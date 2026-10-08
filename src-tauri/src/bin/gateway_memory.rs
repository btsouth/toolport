//! Release freed glibc arena pages after large catalog builds. Only the gateway
//! uses this module; desktop binaries and code-mode worker limits are unchanged.

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
    use std::sync::{mpsc, OnceLock};
    use std::time::{Duration, Instant};

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
