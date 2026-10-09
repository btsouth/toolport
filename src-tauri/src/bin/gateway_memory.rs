//! Reuse glibc response arenas and release free pages after catalog builds.
//! Only the gateway uses this module; desktop binaries and code-mode worker
//! limits are unchanged.

pub fn configure_daemon() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    linux::configure();
}

pub(super) fn serialize(
    response: &super::GatewayResponse,
    sse: bool,
) -> Result<String, serde_json::Error> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if let Some(result) = linux::serialize(response, sse) {
        return result;
    }
    serialize_framed(response, sse)
}

fn serialize_framed(
    response: &super::GatewayResponse,
    sse: bool,
) -> Result<String, serde_json::Error> {
    response.to_json().map(|body| {
        if sse {
            super::mcp_sse_body(&body)
        } else {
            body
        }
    })
}

pub struct AfterBuild(pub usize);

impl Drop for AfterBuild {
    fn drop(&mut self) {
        request(self.0);
    }
}

pub fn request(tools: usize) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    linux::request(tools);
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = tools;
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod linux {
    use std::sync::{mpsc, Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::super::GatewayResponse;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Large replies repeatedly serialized on ephemeral HTTP threads otherwise
    // leave resident free pages spread across their glibc arena tops.
    const MIN_RESPONSE_BYTES: usize = 256 * 1024;
    const SERIALIZER_WAIT: Duration = Duration::from_secs(2);
    static DAEMON: AtomicBool = AtomicBool::new(false);
    type WireResult = Result<String, serde_json::Error>;
    struct SerializeJob {
        response: GatewayResponse,
        sse: bool,
        reply: mpsc::SyncSender<WireResult>,
    }
    static SERIALIZER: OnceLock<Option<mpsc::SyncSender<SerializeJob>>> = OnceLock::new();

    pub fn configure() {
        DAEMON.store(true, Ordering::Relaxed);
    }

    fn serializer_pool() -> Option<mpsc::SyncSender<SerializeJob>> {
        let (sender, receiver) = mpsc::sync_channel::<SerializeJob>(8);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut started = false;
        for index in 0..2 {
            let receiver = Arc::clone(&receiver);
            started |= std::thread::Builder::new()
                .name(format!("gateway-wire-{index}"))
                .spawn(move || loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else {
                        return;
                    };
                    let _ = job
                        .reply
                        .send(super::serialize_framed(&job.response, job.sse));
                })
                .is_ok();
        }
        started.then_some(sender)
    }

    pub(super) fn serialize(response: &GatewayResponse, sse: bool) -> Option<WireResult> {
        if !DAEMON.load(Ordering::Relaxed)
            || response.surface.as_ref()?.json.get().len() < MIN_RESPONSE_BYTES
        {
            return None;
        }
        let sender = SERIALIZER.get_or_init(serializer_pool).as_ref()?;
        Some(serialize_on(sender, response, sse, SERIALIZER_WAIT))
    }

    fn serialize_on(
        sender: &mpsc::SyncSender<SerializeJob>,
        response: &GatewayResponse,
        sse: bool,
        wait: Duration,
    ) -> WireResult {
        let (reply, result) = mpsc::sync_channel(1);
        let job = SerializeJob {
            response: response.clone(),
            sse,
            reply,
        };
        // A full queue or unavailable worker keeps the synchronous path. No
        // catalog locks are held while waiting, and the request owns a fallback.
        if sender.try_send(job).is_ok() {
            if let Ok(result) = result.recv_timeout(wait) {
                return result;
            }
        }
        super::serialize_framed(response, sse)
    }

    const MIN_TOOLS: usize = 256;
    const DEFER: Duration = Duration::from_secs(1);
    const INTERVAL: Duration = Duration::from_secs(5);
    static REQUESTS: OnceLock<Option<mpsc::SyncSender<()>>> = OnceLock::new();

    pub fn request(tools: usize) {
        if tools < MIN_TOOLS {
            return;
        }
        let sender = REQUESTS.get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("gateway-memory".into())
                .spawn(move || {
                    reclaim_loop(
                        receiver,
                        || {
                            // SAFETY: glibc synchronizes allocator access internally.
                            // No gateway/catalog locks are held by this worker.
                            unsafe { libc::malloc_trim(0) };
                        },
                        std::thread::sleep,
                    );
                })
                .ok()
                .map(|_| sender)
        });
        if let Some(sender) = sender {
            // One pending request coalesces a burst without blocking callers.
            // Reclamation is best effort if the worker could not start.
            let _ = sender.try_send(());
        }
    }

    fn delay(last: Option<Instant>, now: Instant) -> Duration {
        // Defer even after a long idle period so build-local temporaries can
        // drop before trimming. Never trim more often than once per interval.
        last.map_or(DEFER, |last| {
            INTERVAL.saturating_sub(now.duration_since(last)).max(DEFER)
        })
    }

    fn reclaim_loop(
        receiver: mpsc::Receiver<()>,
        mut reclaim: impl FnMut(),
        mut pause: impl FnMut(Duration),
    ) {
        let mut last = None;
        while receiver.recv().is_ok() {
            pause(delay(last, Instant::now()));
            // The queue has capacity one. Consume at most one extra request;
            // arrivals during reclamation remain queued for the next interval.
            let _ = receiver.try_recv();
            reclaim();
            last = Some(Instant::now());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn surface_response() -> GatewayResponse {
            let tools = vec![serde_json::json!({
                "name": "fixture__lookup",
                "description": "Quoted \"text\" and UTF-8 é\n".repeat(20_000),
                "inputSchema": {"type":"object", "properties":{"'x-Cwd'":{"type":"string"}}}
            })];
            GatewayResponse {
                envelope: serde_json::json!({
                    "jsonrpc":"2.0", "id":"request-é", "result":{
                        "tools":[], "_meta":{"fixture":true}, "nextCursor":"next"
                    }
                }),
                surface: Some(Arc::new(conduit_lib::savings::SerializedSurface::new(
                    &tools,
                ))),
            }
        }

        #[test]
        fn serializers_preserve_wire_bytes_for_concurrent_request_envelopes() {
            let sender = serializer_pool().unwrap();
            let original = surface_response();
            let mut pending = Vec::new();
            // Eight jobs fit the bounded queue even before workers receive any.
            for id in 0..8 {
                let mut response = original.clone();
                response.envelope["id"] = serde_json::json!(format!("request-{id}-é\n\""));
                let sse = id % 2 == 1;
                let raw = serde_json::to_string(&response).unwrap();
                let expected = if sse {
                    format!("event: message\ndata: {raw}\n\n")
                } else {
                    raw
                };
                let (reply, result) = mpsc::sync_channel(1);
                assert!(sender
                    .try_send(SerializeJob {
                        response,
                        sse,
                        reply
                    })
                    .is_ok());
                pending.push((result, expected));
            }
            for (result, expected) in pending {
                assert_eq!(
                    result
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .unwrap(),
                    expected
                );
            }
        }

        #[test]
        fn serialization_falls_back_for_full_closed_and_stalled_queues() {
            let response = surface_response();
            let expected = serde_json::to_string(&response).unwrap();
            let (sender, receiver) = mpsc::sync_channel(1);
            let (reply, _result) = mpsc::sync_channel(1);
            sender
                .try_send(SerializeJob {
                    response: response.clone(),
                    sse: false,
                    reply,
                })
                .unwrap_or_else(|_| panic!("empty queue"));
            assert_eq!(
                serialize_on(&sender, &response, false, Duration::ZERO).unwrap(),
                expected
            );
            let queued = receiver.try_recv().unwrap();
            assert_eq!(serde_json::to_string(&queued.response).unwrap(), expected);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            drop(receiver);
            assert_eq!(
                serialize_on(&sender, &response, false, Duration::ZERO).unwrap(),
                expected
            );

            let (sender, receiver) = mpsc::sync_channel(1);
            // An expired local wait models an unavailable worker without sleeps.
            assert_eq!(
                serialize_on(&sender, &response, false, Duration::ZERO).unwrap(),
                expected
            );
            let queued = receiver.try_recv().unwrap();
            assert_eq!(serde_json::to_string(&queued.response).unwrap(), expected);
        }

        #[test]
        fn trim_delay_defers_first_and_idle_builds_and_limits_bursts() {
            let now = Instant::now();
            assert_eq!(delay(None, now), DEFER);
            assert_eq!(delay(Some(now), now), INTERVAL);
            assert_eq!(
                delay(Some(now), now + Duration::from_secs(2)),
                Duration::from_secs(3)
            );
            assert_eq!(delay(Some(now), now + Duration::from_secs(5)), DEFER);
            assert_eq!(delay(Some(now), now + Duration::from_secs(30)), DEFER);
        }

        #[test]
        fn trim_worker_coalesces_bursts_without_losing_requests_during_reclaim() {
            let caller = std::thread::current().id();
            let (sender, receiver) = mpsc::sync_channel(1);
            let (trimmed, observed) = mpsc::channel();
            let (resume, parked) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut calls = 0;
                reclaim_loop(
                    receiver,
                    || {
                        assert_ne!(std::thread::current().id(), caller);
                        calls += 1;
                        trimmed.send(calls).unwrap();
                        if calls == 1 {
                            parked.recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                    },
                    |delay| assert!((DEFER..=INTERVAL).contains(&delay)),
                );
                calls
            });
            sender.try_send(()).unwrap();
            assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
            sender.try_send(()).unwrap();
            assert!(matches!(
                sender.try_send(()),
                Err(mpsc::TrySendError::Full(()))
            ));
            resume.send(()).unwrap();
            assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
            drop(sender);
            assert_eq!(worker.join().unwrap(), 2);
        }

        #[test]
        fn trim_worker_coalesces_a_request_received_during_deferral() {
            let (sender, receiver) = mpsc::sync_channel(1);
            sender.try_send(()).unwrap();
            let mut calls = 0;
            let mut sender = Some(sender);
            reclaim_loop(
                receiver,
                || calls += 1,
                move |_| {
                    let sender = sender.take().unwrap();
                    sender.try_send(()).unwrap();
                    // Closing the channel permits one trim then ends the worker.
                    drop(sender);
                },
            );
            assert_eq!(calls, 1);
        }
    }
}
