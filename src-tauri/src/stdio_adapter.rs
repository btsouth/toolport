//! `--stdio-adapter`: the stdio face of the host daemon (one-gateway-per-host P2.2c).
//!
//! The adapter owns no registry, router, or downstream connections. It renders the
//! host daemon's Streamable HTTP MCP endpoint as a stdio MCP server: every JSON-RPC
//! message the client writes to stdin is POSTed to the daemon's `/mcp`, and every
//! message the daemon sends back (a response body, an SSE frame on the POST reply,
//! or a frame on the long-lived `GET /mcp` listen stream) is written to stdout.
//!
//! The explicit `--stdio-adapter` role never falls back to an in-process gateway.
//! A registry-selected adapter can fall back before it opens a daemon session;
//! transport failures after that point become errors to the client so requests
//! cannot be replayed against another router.
//!
//! Requests run on bounded worker threads, so a client that pipelines a slow call
//! and a fast one is answered in completion order rather than arrival order. The
//! first request runs inline, so `initialize` establishes the session before
//! anything can reference it. A modern (2026-07-28) request declares its version
//! in its own `_meta` and needs no session, so it never waits for one; the adapter
//! mirrors that version and the routing fields into the headers the daemon
//! requires. Notifications stay on the reader thread, which keeps a cancellation
//! ahead of whatever is queued behind it (MCP cancellation is best-effort, so one
//! that loses the race simply does not apply).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::daemon::{DaemonDescriptor, Rendezvous};
use crate::registry;
use crate::topology::CompatKey;

/// The flag that selects the adapter role instead of the in-process gateway.
pub const STDIO_ADAPTER_FLAG: &str = "--stdio-adapter";
/// Internal daemon headers. The daemon accepts these only with its private
/// rendezvous bearer; the public HTTP bridge never trusts them.
pub const ADAPTER_CLIENT_ID_HEADER: &str = "Toolport-Adapter-Client-Id";
pub const ADAPTER_PROFILE_HEADER: &str = "Toolport-Adapter-Profile";
/// Path values are URL-safe base64 so Unicode and platform separators survive
/// HTTP header transport. The daemon's cwd belongs to the first adapter only.
pub const ADAPTER_CWD_HEADER: &str = "Toolport-Adapter-Cwd";
pub const ADAPTER_ROOT_OVERRIDE_HEADER: &str = "Toolport-Adapter-Root-Override";
pub const ADAPTER_DECLARED_ROOT_HEADER: &str = "Toolport-Adapter-Declared-Root";
/// Same per-frame bound the in-process stdio gateway applies to one client frame.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// A single request may legitimately run long (a slow downstream call), so the
/// HTTP budget is generous; the listen stream is separate and reconnects.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// A subscription's reply stays open for the life of the subscription, so it has
/// no overall deadline. The daemon sends a keepalive every 30 seconds; three
/// missed ones mean it is gone.
const SUBSCRIPTION_READ_TIMEOUT: Duration = Duration::from_secs(90);
/// Budget for one idle check-in with the daemon.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the daemon to publish a session id before opening the
/// server-initiated listen stream.
const LISTEN_POLL: Duration = Duration::from_millis(100);
/// Backoff between listen-stream reconnects.
const LISTEN_RECONNECT: Duration = Duration::from_millis(500);
/// How many requests may be in flight against the daemon at once. Matches the
/// in-process gateway's stdio worker cap. Past it the reader runs the request on
/// its own thread rather than waiting for a worker, so the reader can never be
/// held off stdin by a worker that is itself waiting on something the reader has
/// to carry (an elicitation answer, a cancellation).
const MAX_INFLIGHT: usize = 256;
/// On client EOF, how long to let in-flight requests finish before the session is
/// deleted. The client is already gone, so this is a courtesy rather than a wait.
const EOF_GRACE: Duration = Duration::from_secs(5);

/// Whether the command line asked for the adapter role. Kept beside the flag so
/// the help text and the parser cannot disagree.
pub fn adapter_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == STDIO_ADAPTER_FLAG)
}

/// Why daemon preparation failed and whether a standalone gateway is safe.
struct PreparationFailure {
    detail: String,
    /// Only a failed OS spawn proves no daemon was launched by this attempt.
    safe_to_fallback: bool,
}

fn prepare_stdio_adapter() -> Result<(Rendezvous, DaemonDescriptor), PreparationFailure> {
    let dir = registry::conduit_dir().ok_or_else(|| PreparationFailure {
        detail: "no data directory could be resolved".to_string(),
        safe_to_fallback: false,
    })?;
    let compat = CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string());
    let rendezvous = Rendezvous::new(&dir, compat);
    let mut spawn_failed = false;
    let descriptor = rendezvous
        .ensure(|| {
            let result = spawn_daemon();
            spawn_failed = result.is_err();
            result
        })
        .map_err(|detail| PreparationFailure {
            detail,
            safe_to_fallback: spawn_failed,
        })?;
    Ok((rendezvous, descriptor))
}

/// Find or start the same host daemon for the desktop's lightweight HTTP
/// bridge. It has no stdio session, but shares the adapter's election path.
pub fn ensure_host_daemon() -> Result<DaemonDescriptor, String> {
    prepare_stdio_adapter()
        .map(|(_, descriptor)| descriptor)
        .map_err(|failure| failure.detail)
}

fn finish_stdio_adapter(rendezvous: Rendezvous, descriptor: DaemonDescriptor) -> ! {
    match proxy_stdio(rendezvous, descriptor) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("toolport-gateway {STDIO_ADAPTER_FLAG}: {error}");
            std::process::exit(1);
        }
    }
}

/// Run an explicit stdio adapter; an unavailable daemon ends this process.
pub fn run_stdio_adapter() -> ! {
    match prepare_stdio_adapter() {
        Ok((rendezvous, descriptor)) => finish_stdio_adapter(rendezvous, descriptor),
        Err(error) => {
            eprintln!("toolport-gateway {STDIO_ADAPTER_FLAG}: {}", error.detail);
            std::process::exit(1);
        }
    }
}

/// Registry opt-in may fall back only before an adapter session reaches the
/// daemon. Once a descriptor is ready, all later failures stay in adapter mode
/// so a call cannot be retried against a second, in-process router.
pub fn run_selected_stdio_adapter() {
    match prepare_stdio_adapter() {
        Ok((rendezvous, descriptor)) => {
            crate::gatewaylog::append("topology: role=stdio-adapter source=stdio-topology");
            finish_stdio_adapter(rendezvous, descriptor)
        }
        Err(PreparationFailure {
            detail,
            safe_to_fallback: true,
        }) => {
            crate::gatewaylog::append("topology: role=standalone reason=daemon-startup-fallback");
            eprintln!(
                "toolport-gateway: host daemon could not be launched ({detail}); \
                 using the in-process gateway"
            );
        }
        Err(error) => {
            eprintln!(
                "toolport-gateway: host daemon startup was inconclusive ({}); \
                 refusing an in-process fallback",
                error.detail
            );
            std::process::exit(1);
        }
    }
}

/// Start the host daemon as a detached sibling. A new process group keeps the
/// daemon alive when the client tears down the adapter's group, so the next
/// adapter finds it through the rendezvous instead of paying a cold start.
fn spawn_daemon() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not locate this executable: {error}"))?;
    let mut command = Command::new(exe);
    command
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map(|_child| ())
        .map_err(|error| format!("could not start the host daemon: {error}"))
}

/// Shared adapter state: the rendezvous used to (re)find the daemon, the daemon it
/// currently talks to, the negotiated session id, the client handshake to replay if
/// the daemon is replaced, and the one stdout every path writes to.
struct Session {
    rendezvous: Rendezvous,
    descriptor: Mutex<DaemonDescriptor>,
    /// Set when a daemon call failed at the transport level. The next request
    /// re-rendezvouses instead of replaying the call that failed.
    stale: AtomicBool,
    /// Shared by healthy exchanges; taken exclusively while one caller recovers, so
    /// no request runs against the old descriptor or ahead of the replayed handshake.
    gate: RwLock<()>,
    session_id: Mutex<Option<String>>,
    /// The client's `initialize` and its `notifications/initialized`, kept so a
    /// replacement daemon can be given an equivalent session.
    handshake_initialize: Mutex<Option<String>>,
    handshake_initialized: Mutex<Option<String>>,
    stdout: Mutex<std::io::Stdout>,
    client_id: String,
    env_profile: Option<String>,
    cwd: Option<String>,
    root_override: Option<String>,
    /// Roots learned from this client's reply to the daemon's roots/list.
    declared_root: Mutex<Option<String>>,
    roots_request_ids: Mutex<HashSet<String>>,
    /// Open `subscriptions/listen` requests by id, each with the flag the
    /// client's cancellation sets.
    subscriptions: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// Keeps an open subscription cancellable while its reply is relayed.
struct SubscriptionGuard<'a> {
    session: &'a Session,
    key: String,
    cancelled: Arc<AtomicBool>,
}

impl SubscriptionGuard<'_> {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Drop for SubscriptionGuard<'_> {
    fn drop(&mut self) {
        let mut open = self
            .session
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if open
            .get(&self.key)
            .is_some_and(|flag| Arc::ptr_eq(flag, &self.cancelled))
        {
            open.remove(&self.key);
        }
    }
}

impl Session {
    fn new(rendezvous: Rendezvous, descriptor: DaemonDescriptor) -> Self {
        let client_id =
            crate::brand::env_var(crate::brand::CLIENT_ID, crate::brand::CLIENT_ID_LEGACY)
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| format!("adapter-pid-{}", std::process::id()));
        let env_profile =
            crate::brand::env_var(crate::brand::PROFILE, crate::brand::PROFILE_LEGACY)
                .filter(|profile| !profile.trim().is_empty());
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string));
        let root_override = crate::brand::env_var("TOOLPORT_ROOT", "CONDUIT_ROOT")
            .map(|root| root.trim().to_string())
            .filter(|root| !root.is_empty());
        Self {
            rendezvous,
            descriptor: Mutex::new(descriptor),
            stale: AtomicBool::new(false),
            gate: RwLock::new(()),
            session_id: Mutex::new(None),
            handshake_initialize: Mutex::new(None),
            handshake_initialized: Mutex::new(None),
            stdout: Mutex::new(std::io::stdout()),
            client_id,
            env_profile,
            cwd,
            root_override,
            declared_root: Mutex::new(None),
            roots_request_ids: Mutex::new(HashSet::new()),
            subscriptions: Mutex::new(HashMap::new()),
        }
    }

    fn with_identity(&self, request: ureq::Request) -> ureq::Request {
        let request = request.set(ADAPTER_CLIENT_ID_HEADER, &self.client_id);
        let request = match &self.env_profile {
            Some(profile) => request.set(ADAPTER_PROFILE_HEADER, profile),
            None => request,
        };
        let request = match &self.cwd {
            Some(cwd) => request.set(
                ADAPTER_CWD_HEADER,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cwd.as_bytes()),
            ),
            None => request,
        };
        let request = match &self.root_override {
            Some(root) => request.set(
                ADAPTER_ROOT_OVERRIDE_HEADER,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(root.as_bytes()),
            ),
            None => request,
        };
        let declared_root = self
            .declared_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match declared_root {
            Some(root) => request.set(
                ADAPTER_DECLARED_ROOT_HEADER,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(root.as_bytes()),
            ),
            None => request,
        }
    }

    fn remember_roots_request(&self, body: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return;
        };
        if value["method"] == "roots/list" {
            if let Some(id) = value.get("id") {
                let mut pending = self
                    .roots_request_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if pending.len() < 128 {
                    pending.insert(id.to_string());
                }
            }
        }
    }

    fn remember_roots_response(&self, value: &serde_json::Value) {
        // Client requests and daemon requests use independent id sequences.
        // A client request with the same id must not consume the pending roots
        // response before the client answers it.
        if value.get("method").is_some()
            || (value.get("result").is_none() && value.get("error").is_none())
        {
            return;
        }
        let Some(id) = value.get("id") else {
            return;
        };
        let mut pending = self
            .roots_request_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.remove(&id.to_string()) {
            return;
        }
        let Some(roots) = value["result"]["roots"].as_array() else {
            return;
        };
        let root = roots
            .first()
            .and_then(|root| root["uri"].as_str())
            .and_then(crate::downstream::file_uri_to_path);
        *self
            .declared_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = root;
    }

    /// The daemon this adapter is currently talking to.
    fn descriptor(&self) -> DaemonDescriptor {
        self.descriptor
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// The negotiated `Mcp-Session-Id`, if `initialize` has answered yet.
    fn session_id(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|guard| guard.clone())
    }

    /// Write one already-serialized JSON-RPC message to stdout. The newline is the
    /// frame boundary a stdio MCP client reads.
    fn write_message(&self, message: &str) -> Result<(), String> {
        let mut out = self
            .stdout
            .lock()
            .map_err(|_| "stdout lock poisoned".to_string())?;
        writeln!(out, "{message}").map_err(|error| error.to_string())?;
        out.flush().map_err(|error| error.to_string())
    }

    /// Remember the client's handshake so a replacement daemon can be given an
    /// equivalent session. A fresh `initialize` starts it over.
    fn remember_handshake(&self, body: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return;
        };
        match value.get("method").and_then(|method| method.as_str()) {
            Some("initialize") => {
                self.roots_request_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
                *self
                    .declared_root
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                if let Ok(mut initialize) = self.handshake_initialize.lock() {
                    *initialize = Some(body.to_string());
                }
                if let Ok(mut initialized) = self.handshake_initialized.lock() {
                    *initialized = None;
                }
            }
            Some("notifications/initialized") => {
                let has_initialize = self
                    .handshake_initialize
                    .lock()
                    .map(|initialize| initialize.is_some())
                    .unwrap_or(false);
                if has_initialize {
                    if let Ok(mut initialized) = self.handshake_initialized.lock() {
                        *initialized = Some(body.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn open_subscription(&self, id: &serde_json::Value) -> SubscriptionGuard<'_> {
        let key = id.to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), Arc::clone(&cancelled));
        SubscriptionGuard {
            session: self,
            key,
            cancelled,
        }
    }

    /// Stop relaying a subscription the client cancelled. Dropping its stream is
    /// how an HTTP client ends one; the daemon releases the subscription when its
    /// next keepalive finds the stream closed.
    fn cancel_subscription(&self, message: &serde_json::Value) {
        if message["method"] != "notifications/cancelled" {
            return;
        }
        let Some(id) = message["params"].get("requestId") else {
            return;
        };
        if let Some(cancelled) = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id.to_string())
        {
            cancelled.store(true, Ordering::SeqCst);
        }
    }

    /// POST one message to `/mcp`. `forward` writes the daemon's JSON-RPC messages
    /// to stdout as they arrive; a replayed handshake does not, because the client
    /// already has its answer. A transport failure marks the daemon stale and is
    /// returned as-is.
    fn post(&self, body: &str, forward: bool) -> Result<(), String> {
        let message = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
        let id = message.get("id").filter(|id| !id.is_null());
        let expects_reply = id.is_some() && message.get("method").is_some();
        let subscription = match (message["method"].as_str(), id) {
            (Some("subscriptions/listen"), Some(id)) => Some(self.open_subscription(id)),
            _ => None,
        };
        let descriptor = self.descriptor();
        let url = format!("http://{}/mcp", descriptor.endpoint);
        let request = match subscription {
            Some(_) => ureq::AgentBuilder::new()
                .timeout_read(SUBSCRIPTION_READ_TIMEOUT)
                .build()
                .post(&url),
            None => ureq::post(&url).timeout(REQUEST_TIMEOUT),
        };
        let mut request = self.with_identity(
            request
                .set("Authorization", &format!("Bearer {}", descriptor.token))
                .set("Content-Type", "application/json")
                .set("Accept", "application/json, text/event-stream"),
        );
        for (name, value) in modern_headers(&message) {
            request = request.set(&name, &value);
        }
        if let Some(session) = self.session_id() {
            request = request.set("Mcp-Session-Id", &session);
        }
        let response = match request.send_string(body) {
            Ok(response) => response,
            // The daemon answered, just not with 2xx. It is alive, so this is not a
            // recovery trigger; the body is the error the caller should see.
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                // A modern protocol error comes with a 4xx status and a JSON-RPC
                // body. That body is the answer, and the client needs it intact:
                // an unsupported version lists the versions to retry with.
                if is_json_rpc_reply(&body) {
                    return if forward {
                        self.write_message(body.trim())
                    } else {
                        Ok(())
                    };
                }
                // A live profile switch changes the session's bound scope. The
                // daemon then rejects its old id exactly like an expired
                // session. Fail this call once, and replay the handshake on the
                // next request; never replay a call that may have executed.
                if code == 404 && body.contains("unknown or expired Mcp-Session-Id") {
                    self.stale.store(true, Ordering::SeqCst);
                }
                return Err(format!(
                    "the host daemon answered HTTP {code}: {}",
                    body.trim()
                ));
            }
            Err(error) => {
                // The daemon is gone or unreachable. The next request re-rendezvouses;
                // the call that hit this is never retried.
                self.stale.store(true, Ordering::SeqCst);
                return Err(error.to_string());
            }
        };
        if let Some(session) = response.header("Mcp-Session-Id") {
            if let Ok(mut guard) = self.session_id.lock() {
                *guard = Some(session.to_string());
            }
        }
        let is_sse = response
            .header("Content-Type")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("text/event-stream");
        let mut replied = false;
        relay_frames(
            BufReader::new(response.into_reader()),
            is_sse,
            || {
                subscription
                    .as_ref()
                    .is_some_and(SubscriptionGuard::cancelled)
            },
            |frame| {
                replied |= !is_sse || is_reply_to(frame, id);
                if forward {
                    self.write_message(frame)
                } else {
                    Ok(())
                }
            },
        )?;
        if expects_reply && !replied {
            return match &subscription {
                Some(subscription) if subscription.cancelled() => Ok(()),
                Some(_) => Err("the host daemon ended the subscription stream".to_string()),
                None => Err("the host daemon closed the reply without an answer".to_string()),
            };
        }
        Ok(())
    }

    /// Re-rendezvous after a daemon failure, then replay the client's handshake so
    /// the new session is equivalent. The caller holds the write gate.
    fn recover(&self) -> Result<(), String> {
        let descriptor = self
            .rendezvous
            .ensure(spawn_daemon)
            .map_err(|error| format!("the host daemon could not be reached again: {error}"))?;
        if let Ok(mut guard) = self.descriptor.lock() {
            *guard = descriptor;
        }
        // The session belonged to the daemon that went away.
        if let Ok(mut guard) = self.session_id.lock() {
            *guard = None;
        }
        let initialize = self
            .handshake_initialize
            .lock()
            .ok()
            .and_then(|value| value.clone());
        if let Some(body) = initialize {
            self.post(&body, false)?;
        }
        let initialized = self
            .handshake_initialized
            .lock()
            .ok()
            .and_then(|value| value.clone());
        if let Some(body) = initialized {
            self.post(&body, false)?;
        }
        Ok(())
    }

    /// POST one client message. Healthy calls share the read gate and run
    /// concurrently; after a failure, one caller re-finds the daemon under the write
    /// gate while the rest wait, so no request runs against the old descriptor or
    /// ahead of the replayed handshake.
    fn exchange(&self, body: &str) -> Result<(), String> {
        if !self.stale.load(Ordering::SeqCst) {
            let _healthy = self
                .gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.stale.load(Ordering::SeqCst) {
                self.remember_handshake(body);
                return self.post(body, true);
            }
        }
        let _recovering = self
            .gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stale.swap(false, Ordering::SeqCst) {
            if let Err(error) = self.recover() {
                // Let a later request try again rather than staying healthy-looking.
                self.stale.store(true, Ordering::SeqCst);
                return Err(error);
            }
        }
        self.remember_handshake(body);
        self.post(body, true)
    }

    /// Close the daemon-side session on client EOF so its per-session state is
    /// released immediately rather than waiting for a lease TTL.
    fn close(&self) {
        let Some(session) = self.session_id() else {
            return;
        };
        let descriptor = self.descriptor();
        let url = format!("http://{}/mcp", descriptor.endpoint);
        let _ = self
            .with_identity(
                ureq::delete(&url)
                    .set("Authorization", &format!("Bearer {}", descriptor.token))
                    .set("Mcp-Session-Id", &session)
                    .timeout(Duration::from_secs(5)),
            )
            .call();
    }
}

/// The protocol version a modern message declares in its `_meta`. A legacy
/// message has none: its client negotiated once, at `initialize`.
fn declared_version(message: &serde_json::Value) -> Option<&str> {
    message
        .get("params")?
        .get("_meta")?
        .get("io.modelcontextprotocol/protocolVersion")?
        .as_str()
}

/// The headers a modern Streamable HTTP POST must carry, mirrored from the body
/// the client wrote. The daemon rejects a modern request without them, or with
/// values that disagree with the body.
fn modern_headers(message: &serde_json::Value) -> Vec<(String, String)> {
    let Some(version) = declared_version(message) else {
        return Vec::new();
    };
    let mut headers = vec![(
        "MCP-Protocol-Version".to_string(),
        crate::downstream::encode_mcp_header_text(version),
    )];
    headers.extend(crate::downstream::modern_routing_headers(message));
    headers
}

/// Whether a daemon body is a JSON-RPC response, rather than a transport error.
fn is_json_rpc_reply(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|value| {
        value["jsonrpc"] == "2.0"
            && value.get("id").is_some()
            && (value.get("result").is_some() || value.get("error").is_some())
    })
}

/// Whether an SSE frame is the response to `id`, rather than a notification.
fn is_reply_to(frame: &str, id: Option<&serde_json::Value>) -> bool {
    id.is_some()
        && serde_json::from_str::<serde_json::Value>(frame)
            .is_ok_and(|value| value.get("method").is_none() && value.get("id") == id)
}

/// Pass each JSON-RPC message in a daemon reply to `deliver` as it arrives. A JSON
/// body is one message. An SSE body may carry several, and a subscription's stays
/// open, so it is read one line at a time; `stop` is checked at each line,
/// keepalives included, so a cancelled stream is dropped promptly.
fn relay_frames<R: BufRead>(
    mut reader: R,
    is_sse: bool,
    stop: impl Fn() -> bool,
    mut deliver: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    if !is_sse {
        let mut body = Vec::new();
        (&mut reader)
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| error.to_string())?;
        if body.len() > MAX_FRAME_BYTES {
            return Err("the host daemon's reply exceeds the 16 MiB limit".to_string());
        }
        let body = String::from_utf8_lossy(&body);
        let body = body.trim();
        return if body.is_empty() {
            Ok(())
        } else {
            deliver(body)
        };
    }
    while let Some(frame) = read_bounded_line(&mut reader, MAX_FRAME_BYTES)? {
        if stop() {
            return Ok(());
        }
        let ClientFrame::Line(line) = frame else {
            return Err("an event from the host daemon exceeds the 16 MiB limit".to_string());
        };
        if let Some(data) = line
            .strip_prefix("data:")
            .map(str::trim)
            .filter(|data| !data.is_empty())
        {
            deliver(data)?;
        }
    }
    Ok(())
}

/// Releases one slot on drop, so the live count falls even if a worker panics.
struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Claim one slot below `limit`, or `None` when the cap is already reached.
fn try_acquire_inflight(inflight: &Arc<AtomicUsize>, limit: usize) -> Option<InflightGuard> {
    let mut current = inflight.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            return None;
        }
        match inflight.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(InflightGuard(Arc::clone(inflight))),
            Err(actual) => current = actual,
        }
    }
}

/// Runs one exchange per worker thread, bounded, so a slow call cannot stall the
/// reader. Past the cap the reader runs the exchange itself rather than waiting on
/// a worker, because the reader is the only path a server-initiated answer or a
/// cancellation can take back to the daemon. The job is a closure rather than a
/// `Session` method so the dispatch and bounding can be tested without a daemon.
struct Dispatcher {
    run: Arc<dyn Fn(String) + Send + Sync>,
    inflight: Arc<AtomicUsize>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    limit: usize,
}

impl Dispatcher {
    fn new(run: Arc<dyn Fn(String) + Send + Sync>, limit: usize) -> Self {
        Self {
            run,
            inflight: Arc::new(AtomicUsize::new(0)),
            workers: Mutex::new(Vec::new()),
            limit,
        }
    }

    /// Join the workers that have finished, so their resources are reclaimed.
    fn reap(&self) {
        if let Ok(mut workers) = self.workers.lock() {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let handle = workers.swap_remove(index);
                    let _ = handle.join();
                } else {
                    index += 1;
                }
            }
        }
    }

    /// Send one request on a worker thread, or inline when the pool is full.
    fn dispatch(&self, body: String) {
        self.reap();
        match try_acquire_inflight(&self.inflight, self.limit) {
            Some(guard) => {
                let run = Arc::clone(&self.run);
                let handle = std::thread::spawn(move || {
                    let _guard = guard;
                    run(body);
                });
                if let Ok(mut workers) = self.workers.lock() {
                    workers.push(handle);
                }
            }
            // At the cap. Running inline keeps the reader moving and, unlike a
            // blocking join, cannot wait on a worker that needs the reader.
            None => (self.run)(body),
        }
    }

    /// Let in-flight work settle for `grace`, then stop waiting. Anything still
    /// running is abandoned to process exit: the client has already gone away.
    fn drain(&self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while self.inflight.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
            self.reap();
            std::thread::sleep(Duration::from_millis(20));
        }
        self.reap();
    }
}

/// Read from stdin and proxy to the daemon until EOF. A request runs on a worker
/// so a slow call does not hold up the ones behind it, except the first request,
/// which runs inline so `initialize` establishes the session id before anything
/// can reference it. A notification always runs inline, keeping its order against
/// the requests around it. A failed exchange becomes a JSON-RPC error for that
/// request rather than a silent drop or a fallback, and a daemon that went away is
/// re-found before the next request.
fn proxy_stdio(rendezvous: Rendezvous, descriptor: DaemonDescriptor) -> Result<(), String> {
    let session = Arc::new(Session::new(rendezvous, descriptor));
    spawn_listen_stream(Arc::clone(&session));
    spawn_heartbeat(Arc::clone(&session));

    let dispatcher = Dispatcher::new(
        {
            let session = Arc::clone(&session);
            Arc::new(move |body: String| {
                let request = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                if let Err(error) = session.exchange(&body) {
                    report_request_error(&session, &request, &error);
                }
            })
        },
        MAX_INFLIGHT,
    );

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    while let Some(frame) = read_bounded_line(&mut reader, MAX_FRAME_BYTES)? {
        let line = match frame {
            ClientFrame::Oversized => {
                write_error(
                    &session,
                    serde_json::Value::Null,
                    -32600,
                    "request frame exceeds the 16 MiB limit",
                );
                continue;
            }
            ClientFrame::Line(line) => line,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => {
                write_error(&session, serde_json::Value::Null, -32700, "parse error");
                continue;
            }
        };
        session.remember_roots_response(&request);
        session.cancel_subscription(&request);
        let is_request = request.get("id").map(|id| !id.is_null()).unwrap_or(false);
        // A modern request carries everything it needs, so it runs as soon as it
        // is read. A legacy one waits for `initialize` to open the session.
        if is_request && (session.session_id().is_some() || declared_version(&request).is_some()) {
            dispatcher.dispatch(trimmed.to_string());
        } else if let Err(error) = session.exchange(trimmed) {
            report_request_error(&session, &request, &error);
        }
    }
    dispatcher.drain(EOF_GRACE);
    session.close();
    Ok(())
}

/// Surface a failed daemon exchange to the client. A request with an id gets a
/// JSON-RPC error carrying it; a notification has nowhere to answer, so it is
/// logged. Either way the transport problem is stated, never masked by a local
/// retry.
fn report_request_error(session: &Session, request: &serde_json::Value, error: &str) {
    let message = format!("host daemon request failed: {error}");
    match request.get("id") {
        Some(id) if !id.is_null() => write_error(session, id.clone(), -32603, &message),
        _ => {
            eprintln!("toolport-gateway {STDIO_ADAPTER_FLAG}: notification failed: {error}");
        }
    }
}

/// Open the daemon's long-lived `GET /mcp` SSE stream and forward server-initiated
/// messages to the client, reconnecting if it drops. Frames are POSTed back by the
/// client through the normal stdin path, so no correlation table is needed here:
/// the daemon matches the response to its own outstanding request id.
fn spawn_listen_stream(session: Arc<Session>) {
    std::thread::spawn(move || loop {
        let Some(session_id) = session.session_id() else {
            std::thread::sleep(LISTEN_POLL);
            continue;
        };
        let descriptor = session.descriptor();
        let url = format!("http://{}/mcp", descriptor.endpoint);
        let response = session
            .with_identity(
                ureq::get(&url)
                    .set("Authorization", &format!("Bearer {}", descriptor.token))
                    .set("Accept", "text/event-stream")
                    .set("Mcp-Session-Id", &session_id)
                    .timeout(Duration::from_secs(3600)),
            )
            .call();
        match response {
            Ok(response) => {
                let mut reader = BufReader::new(response.into_reader());
                while let Ok(Some(ClientFrame::Line(line))) =
                    read_bounded_line(&mut reader, MAX_FRAME_BYTES)
                {
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if !data.is_empty() {
                            session.remember_roots_request(data);
                            let _ = session.write_message(data);
                        }
                    }
                }
                // The stream ended without an error (a clean drop or a server
                // close). Pause before reconnecting so a server that keeps closing
                // immediately cannot be connected to in a tight loop.
                std::thread::sleep(LISTEN_RECONNECT);
            }
            Err(_) => std::thread::sleep(LISTEN_RECONNECT),
        }
    });
}

/// The daemon leaves once nothing has reached it for its idle grace. A legacy
/// session holds its listen stream open, but a modern client has no standing
/// connection, so the adapter checks in well inside the grace for as long as its
/// client is attached.
fn spawn_heartbeat(session: Arc<Session>) {
    let interval = (crate::daemon::idle_grace() / 5).max(Duration::from_millis(100));
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        let descriptor = session.descriptor();
        let _ = ureq::get(&format!(
            "http://{}{}",
            descriptor.endpoint,
            crate::daemon::IDENTITY_PATH
        ))
        .set("Authorization", &format!("Bearer {}", descriptor.token))
        .timeout(HEARTBEAT_TIMEOUT)
        .call();
    });
}

/// One frame read from the client.
#[derive(Debug, PartialEq, Eq)]
enum ClientFrame {
    /// A complete newline-delimited frame.
    Line(String),
    /// A frame that exceeded the cap; its bytes were drained and dropped.
    Oversized,
}

/// Write a JSON-RPC error to the client.
fn write_error(session: &Session, id: serde_json::Value, code: i64, message: &str) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    });
    let _ = session.write_message(&response.to_string());
}

/// Read one newline-delimited frame, bounded so a client cannot make the adapter
/// allocate without limit. `None` is EOF.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<ClientFrame>, String> {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => {
                return Ok(if buf.is_empty() {
                    None
                } else {
                    Some(ClientFrame::Line(
                        String::from_utf8_lossy(&buf).into_owned(),
                    ))
                });
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok(Some(ClientFrame::Line(
                        String::from_utf8_lossy(&buf).into_owned(),
                    )));
                }
                if buf.len() >= max_bytes {
                    // Drain the rest of this oversized frame so the next read starts
                    // at the next newline, then report it rather than dropping it as
                    // if it were an empty line.
                    loop {
                        let mut discard = [0u8; 1];
                        match reader.read(&mut discard) {
                            Ok(0) | Err(_) => break,
                            Ok(_) if discard[0] == b'\n' => break,
                            Ok(_) => {}
                        }
                    }
                    return Ok(Some(ClientFrame::Oversized));
                }
                buf.push(byte[0]);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_selects_the_adapter_role() {
        assert!(adapter_requested(&["--stdio-adapter".to_string()]));
        assert!(adapter_requested(&[
            "--http".to_string(),
            "--stdio-adapter".to_string()
        ]));
        assert!(!adapter_requested(&["--daemon".to_string()]));
        assert!(!adapter_requested(&[]));
    }

    #[test]
    fn a_bounded_reader_splits_lines_and_reports_eof() {
        let mut reader = std::io::BufReader::new(&b"one\ntwo\n"[..]);
        assert_eq!(
            read_bounded_line(&mut reader, 16).unwrap(),
            Some(ClientFrame::Line("one".to_string()))
        );
        assert_eq!(
            read_bounded_line(&mut reader, 16).unwrap(),
            Some(ClientFrame::Line("two".to_string()))
        );
        assert_eq!(read_bounded_line(&mut reader, 16).unwrap(), None);
    }

    #[test]
    fn an_oversized_frame_is_reported_and_drained() {
        let mut reader = std::io::BufReader::new(&b"aaaaaaaaaaaa\nok\n"[..]);
        assert_eq!(
            read_bounded_line(&mut reader, 4).unwrap(),
            Some(ClientFrame::Oversized)
        );
        assert_eq!(
            read_bounded_line(&mut reader, 4).unwrap(),
            Some(ClientFrame::Line("ok".to_string()))
        );
    }

    #[test]
    fn modern_headers_mirror_the_body() {
        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "wetter_überblick",
                "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" }
            }
        });
        let headers = modern_headers(&call);
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("MCP-Protocol-Version"), Some("2026-07-28"));
        assert_eq!(header("Mcp-Method"), Some("tools/call"));
        assert_eq!(
            header("Mcp-Name"),
            Some(crate::downstream::encode_mcp_header_text("wetter_überblick").as_str()),
            "a non-ASCII name travels in its encoded form"
        );

        let legacy = serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        assert!(
            modern_headers(&legacy).is_empty(),
            "a legacy request declares no version, so it gets no modern headers"
        );
    }

    #[test]
    fn only_a_json_rpc_body_counts_as_a_reply() {
        assert!(is_json_rpc_reply(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32022,"message":"Unsupported"}}"#
        ));
        assert!(is_json_rpc_reply(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#));
        assert!(!is_json_rpc_reply(
            r#"{"error":"unknown or expired Mcp-Session-Id"}"#
        ));
        assert!(!is_json_rpc_reply("gateway busy"));
    }

    #[test]
    fn sse_frames_are_relayed_one_at_a_time() {
        let body = "event: message\r\ndata: {\"a\":1}\r\n\r\n:\r\n\r\ndata: {\"b\":2}\n\n";
        let mut seen = Vec::new();
        relay_frames(
            body.as_bytes(),
            true,
            || false,
            |frame| {
                seen.push(frame.to_string());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, vec![r#"{"a":1}"#, r#"{"b":2}"#]);

        let mut json = Vec::new();
        relay_frames(
            &b" {\"id\":1} \n"[..],
            false,
            || false,
            |frame| {
                json.push(frame.to_string());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(json, vec![r#"{"id":1}"#], "a JSON body is one message");
    }

    #[test]
    fn a_cancelled_stream_stops_before_the_next_frame() {
        let body = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        let cancelled = AtomicBool::new(false);
        let mut seen = Vec::new();
        relay_frames(
            body.as_bytes(),
            true,
            || cancelled.load(Ordering::SeqCst),
            |frame| {
                seen.push(frame.to_string());
                cancelled.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, vec![r#"{"a":1}"#]);
    }

    #[test]
    fn client_request_id_does_not_consume_a_pending_roots_response() {
        let data_dir = std::env::temp_dir();
        let compat = CompatKey::new("test", data_dir.to_string_lossy());
        let session = Session::new(
            Rendezvous::new(&data_dir, compat.clone()),
            DaemonDescriptor::new("127.0.0.1:1", "test-token", &compat),
        );
        let project = data_dir.join("toolport-roots-collision");
        let uri = url::Url::from_file_path(&project)
            .expect("native project URI")
            .to_string();
        session.remember_roots_request(r#"{"jsonrpc":"2.0","id":1,"method":"roots/list"}"#);
        session.remember_roots_response(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}
        }));
        assert_eq!(session.roots_request_ids.lock().unwrap().len(), 1);
        session.remember_roots_response(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": { "roots": [{ "uri": uri }] }
        }));
        assert!(session.roots_request_ids.lock().unwrap().is_empty());
        assert_eq!(
            *session.declared_root.lock().unwrap(),
            crate::downstream::file_uri_to_path(&uri)
        );
    }

    #[test]
    fn a_cancellation_reaches_only_its_own_subscription() {
        let data_dir = std::env::temp_dir();
        let compat = CompatKey::new("test", data_dir.to_string_lossy());
        let session = Session::new(
            Rendezvous::new(&data_dir, compat.clone()),
            DaemonDescriptor::new("127.0.0.1:1", "test-token", &compat),
        );
        let first = session.open_subscription(&serde_json::json!(1));
        let second = session.open_subscription(&serde_json::json!("two"));
        session.cancel_subscription(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": "two" }
        }));
        assert!(!first.cancelled());
        assert!(second.cancelled());

        // A reused id replaces the entry, and the old guard must not remove it.
        let replacement = session.open_subscription(&serde_json::json!(1));
        drop(first);
        session.cancel_subscription(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 1 }
        }));
        assert!(replacement.cancelled());
        drop(replacement);
        drop(second);
        assert!(session.subscriptions.lock().unwrap().is_empty());
    }

    use std::sync::Condvar;

    /// Build a dispatcher whose jobs record into `completed`, with `slow` blocking
    /// until `release` is set.
    fn blocking_dispatcher(
        release: Arc<(Mutex<bool>, Condvar)>,
        completed: Arc<Mutex<Vec<String>>>,
        started: std::sync::mpsc::Sender<String>,
    ) -> Dispatcher {
        Dispatcher::new(
            Arc::new(move |body: String| {
                if body == "slow" {
                    let _ = started.send(body.clone());
                    let (lock, cvar) = &*release;
                    let mut go = lock.lock().unwrap();
                    while !*go {
                        go = cvar.wait(go).unwrap();
                    }
                }
                completed.lock().unwrap().push(body);
            }),
            4,
        )
    }

    #[test]
    fn a_slow_exchange_does_not_block_the_next_dispatch() {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let completed = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let dispatcher =
            blocking_dispatcher(Arc::clone(&release), Arc::clone(&completed), started_tx);

        dispatcher.dispatch("slow".to_string());
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the slow job started");

        dispatcher.dispatch("fast".to_string());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !completed.lock().unwrap().iter().any(|body| body == "fast") {
            assert!(
                Instant::now() < deadline,
                "the fast job never completed while the slow one was held"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        {
            let (lock, cvar) = &*release;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        dispatcher.drain(Duration::from_secs(5));
        let done = completed.lock().unwrap().clone();
        assert!(done.contains(&"slow".to_string()), "{done:?}");
        assert!(done.contains(&"fast".to_string()), "{done:?}");
    }

    #[test]
    fn dispatch_runs_inline_at_the_cap_instead_of_waiting_on_a_worker() {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let second_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let dispatcher = Dispatcher::new(
            {
                let release = Arc::clone(&release);
                let second_ran = Arc::clone(&second_ran);
                Arc::new(move |body: String| {
                    if body == "first" {
                        let _ = started_tx.send(body.clone());
                        let (lock, cvar) = &*release;
                        let mut go = lock.lock().unwrap();
                        while !*go {
                            go = cvar.wait(go).unwrap();
                        }
                    } else {
                        second_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                })
            },
            1,
        );

        dispatcher.dispatch("first".to_string());
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the first job started");

        // The cap is 1 and the worker is held, so `second` runs inline on this
        // thread. dispatch must return once it has run rather than waiting on the
        // held worker; a blocking join here would hang this test.
        dispatcher.dispatch("second".to_string());
        assert!(
            second_ran.load(std::sync::atomic::Ordering::SeqCst),
            "the over-cap request did not run inline"
        );

        {
            let (lock, cvar) = &*release;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        dispatcher.drain(Duration::from_secs(5));
    }
}
