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
//! cannot be replayed against another router. The one request the adapter sends
//! twice is a legacy request the daemon refused for its session, which happens
//! before dispatch (a profile, enabled-set or tool-scope change rebinds the
//! session): it reopens the session and sends that request once more.
//!
//! A daemon that accepts connections but stops answering its identity probe for
//! [`crate::daemon::SILENT_RETRY_TIMEOUT`] is wedged, not busy: the probe is
//! served outside its request workers. Then the adapter never elects a second
//! shared daemon beside it. At startup a registry-selected adapter falls back to
//! the in-process gateway; an explicit adapter, or one already in a session,
//! starts a private gateway process of its own, replays the client's handshake
//! there and sends later requests to it. Calls already waiting on the wedged
//! daemon are left to finish or time out there; none is sent twice.
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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::daemon::{DaemonDescriptor, EnsureError, Rendezvous};
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
/// The internal role an adapter starts for itself when the shared daemon is
/// wedged: a daemon-shaped gateway that is never advertised, announces its
/// descriptor on stdout, and exits when the adapter closes its stdin.
pub const PRIVATE_GATEWAY_FLAG: &str = "--private-gateway";
/// What `toolport_status` says on a private gateway.
pub const PRIVATE_GATEWAY_NOTE: &str = "The shared host daemon stopped answering, so this client \
     runs on a private gateway with its own copies of its servers. Restart the client once the \
     daemon is healthy to share them again.";
/// What `toolport_status` says after the startup fallback to the in-process gateway.
pub const IN_PROCESS_FALLBACK_NOTE: &str = "The shared host daemon was not answering when this \
     client started, so it runs its own in-process gateway with its own copies of its servers. \
     Restart the client once the daemon is healthy to share them again.";
/// Same per-frame bound the in-process stdio gateway applies to one client frame.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// A single request may legitimately run long (a slow downstream call), so the
/// HTTP budget is generous; the listen stream is separate and reconnects.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// A subscription's reply stays open for the life of the subscription, so it has
/// no overall deadline. The daemon sends a keepalive every 30 seconds; three
/// missed ones mean it is gone.
const SUBSCRIPTION_READ_TIMEOUT: Duration = Duration::from_secs(90);
/// How often the adapter checks that its daemon still answers. Also the check-in
/// that keeps an attached daemon from idling out.
const LIVENESS_INTERVAL: Duration = Duration::from_secs(5);
/// A private gateway boots a whole router before it announces itself; a cold
/// daemon on a loaded runner has taken tens of seconds to do the same.
const PRIVATE_GATEWAY_READY_TIMEOUT: Duration = Duration::from_secs(30);
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
    /// A daemon owns the pointer but did not answer. Never replaced, so a
    /// gateway of this client's own does not compete with it for the pointer.
    unresponsive: bool,
}

fn prepare_stdio_adapter() -> Result<(Rendezvous, DaemonDescriptor), PreparationFailure> {
    let dir = registry::conduit_dir().ok_or_else(|| PreparationFailure {
        detail: "no data directory could be resolved".to_string(),
        safe_to_fallback: false,
        unresponsive: false,
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
        .map_err(|error| PreparationFailure {
            detail: error.to_string(),
            safe_to_fallback: spawn_failed,
            unresponsive: matches!(error, EnsureError::Unresponsive(_)),
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

fn finish_stdio_adapter(
    rendezvous: Rendezvous,
    descriptor: DaemonDescriptor,
    private: Option<std::process::Child>,
) -> ! {
    let result = proxy_stdio(rendezvous, descriptor, private);
    match result {
        Ok(()) => crate::telemetry::exit_with(0),
        Err(error) => {
            eprintln!("toolport-gateway {STDIO_ADAPTER_FLAG}: {error}");
            crate::telemetry::exit_with(1);
        }
    }
}

/// Run an explicit stdio adapter; an unavailable daemon ends this process, and
/// a wedged one gets this client a private gateway.
pub fn run_stdio_adapter() -> ! {
    match prepare_stdio_adapter() {
        Ok((rendezvous, descriptor)) => finish_stdio_adapter(rendezvous, descriptor, None),
        Err(error) if error.unresponsive => {
            let rendezvous = match registry::conduit_dir() {
                Some(dir) => Rendezvous::new(
                    &dir,
                    CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string()),
                ),
                None => crate::telemetry::exit_with(1),
            };
            match start_private_gateway(&error.detail) {
                Ok((child, descriptor)) => {
                    finish_stdio_adapter(rendezvous, descriptor, Some(child))
                }
                Err(detail) => {
                    eprintln!(
                        "toolport-gateway {STDIO_ADAPTER_FLAG}: {}; {detail}",
                        error.detail
                    );
                    crate::telemetry::exit_with(1);
                }
            }
        }
        Err(error) => {
            eprintln!("toolport-gateway {STDIO_ADAPTER_FLAG}: {}", error.detail);
            crate::telemetry::exit_with(1);
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
            finish_stdio_adapter(rendezvous, descriptor, None)
        }
        Err(PreparationFailure {
            detail,
            unresponsive: true,
            ..
        }) => {
            crate::gatewaylog::set_role(crate::gatewaylog::Role::Private);
            crate::gatewaylog::append("topology: role=private reason=daemon-unresponsive");
            crate::daemon::add_status_note(IN_PROCESS_FALLBACK_NOTE);
            eprintln!(
                "toolport-gateway: the host daemon is not answering ({detail}); \
                 this client is using its own in-process gateway"
            );
        }
        Err(PreparationFailure {
            detail,
            safe_to_fallback: true,
            ..
        }) => {
            crate::gatewaylog::set_role(crate::gatewaylog::Role::Private);
            crate::gatewaylog::append("topology: role=private reason=daemon-startup-fallback");
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
            crate::telemetry::exit_with(1);
        }
    }
}

/// Start the host daemon as a detached sibling. A new process group keeps the
/// daemon alive when the client tears down the adapter's group, so the next
/// adapter finds it through the rendezvous instead of paying a cold start.
///
/// The adapter must not hand the daemon the environment the AI client launched it
/// with (SEC-04): that environment can hold `AWS_*`, `GITHUB_TOKEN`,
/// `OPENAI_API_KEY` and so on, and the daemon would then serve them to every
/// client that attaches. The daemon is started from a cleared environment plus
/// the same non-secret allowlist a downstream child gets, plus Toolport's own
/// control variables, which it needs to run:
///
/// * `TOOLPORT_DATA_DIR` / `CONDUIT_DATA_DIR` and `TOOLPORT_REGISTRY` /
///   `CONDUIT_REGISTRY` - the data directory and registry to serve, which must
///   match the adapter's.
/// * `TOOLPORT_SECRET_KEY` / `CONDUIT_SECRET_KEY` - the file-backend vault master
///   key; without it the daemon cannot read the secrets it injects into children.
/// * `TOOLPORT_SECRET_*` - per-secret environment fallbacks the daemon reads for
///   a server whose secret is not in the vault.
/// * `TOOLPORT_DISCOVERY`, `TOOLPORT_CODE_MODE`, `TOOLPORT_DEBUG` and their
///   `CONDUIT_` legacy forms - the discovery, code-mode and trace flags the
///   daemon bootstraps from.
/// * `TOOLPORT_PROFILE`, `TOOLPORT_CLIENT_ID` and the legacy pair - the daemon's
///   bootstrap profile and identity; adapters still pass their own per request.
/// * `TOOLPORT_HTTP_TOKEN` and friends - only read in an HTTP mode, kept so a
///   daemon started from an HTTP bridge keeps the same behavior.
///
/// The whole namespace is kept rather than a fixed list so a future control
/// variable cannot silently stop working. None of it reaches the daemon's
/// children: those go through [`crate::downstream::child_environment`], whose
/// allowlist has no `TOOLPORT_`/`CONDUIT_` names. The AppImage bundle variables
/// are kept too, because the daemon is a re-exec of our own bundled payload and
/// needs the bundle's library paths.
fn daemon_environment(
    parent: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, value) in parent {
        if crate::downstream::is_control_env_name(name)
            || crate::downstream::is_allowed_child_env_name(name)
            || crate::hostenv::is_bundled_env_name(name)
        {
            env.insert(name.clone(), value.clone());
        }
    }
    env.into_iter().collect()
}

fn spawn_daemon() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not locate this executable: {error}"))?;
    let parent: std::collections::BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let mut command = Command::new(exe);
    command
        .env_clear()
        .envs(daemon_environment(&parent))
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(daemon_stderr());
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

/// Start this client's own gateway beside a wedged shared daemon. It is never
/// advertised, so no other client can elect it, and it lives exactly as long as
/// the returned child's stdin stays open. `why` is logged with it.
fn start_private_gateway(why: &str) -> Result<(std::process::Child, DaemonDescriptor), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not locate this executable: {error}"))?;
    let parent = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let mut child = Command::new(exe)
        // Use the daemon's boundary even when recovering beside a wedged daemon.
        .env_clear()
        .envs(daemon_environment(&parent))
        .arg(PRIVATE_GATEWAY_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("could not start a private gateway: {error}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (sender, announced) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = sender.send(line);
        // Keep draining so the gateway can never block on a full pipe.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let ready = announced
        .recv_timeout(PRIVATE_GATEWAY_READY_TIMEOUT)
        .map_err(|_| "the private gateway did not announce itself in time".to_string())
        .and_then(|line| {
            serde_json::from_str::<DaemonDescriptor>(line.trim())
                .map_err(|error| format!("the private gateway's announcement was invalid: {error}"))
        })
        .and_then(|descriptor| {
            crate::daemon::probe_identity(&descriptor)
                .map(|_| descriptor)
                .map_err(|error| format!("the private gateway did not answer: {error}"))
        });
    match ready {
        Ok(descriptor) => {
            crate::gatewaylog::append(&format!(
                "topology: role=private-gateway pid={} reason=daemon-unresponsive",
                descriptor.pid
            ));
            eprintln!(
                "toolport-gateway: the host daemon stopped answering ({why}); \
                 this client now uses a private gateway (pid {})",
                descriptor.pid
            );
            Ok((child, descriptor))
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// The daemon's stderr goes to its rotating log in the data directory, so what
/// it reports is kept rather than lost with its detached terminal.
fn daemon_stderr() -> Stdio {
    registry::conduit_dir()
        .and_then(|dir| crate::daemon_log::stderr_file(&dir))
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null)
}

/// The exact error the daemon sends when it refuses a session id. Missing,
/// expired, rescoped and foreign sessions all get it, before any dispatch. The
/// daemon uses this constant too, so the adapter's match cannot drift from it.
pub const SESSION_REFUSED_ERROR: &str = "unknown or expired Mcp-Session-Id; re-initialize";

/// How much of the daemon connection the next exchange must rebuild. Ordered,
/// so the more thorough recovery wins when both are needed.
const HEALTHY: u8 = 0;
/// The daemon answered but refused the session: replay the handshake.
const SESSION_REFUSED: u8 = 1;
/// The daemon could not be reached: re-rendezvous, then replay the handshake.
const DAEMON_LOST: u8 = 2;
/// The daemon stopped answering while alive: move to a private gateway, then
/// replay the handshake.
const DAEMON_WEDGED: u8 = 3;

/// Why an exchange produced no answer for the client.
#[derive(Debug)]
enum ExchangeError {
    /// The daemon refused the session id before dispatching the request, so the
    /// request did not run.
    SessionRefused(String),
    Failed(String),
}

impl ExchangeError {
    fn into_message(self) -> String {
        match self {
            Self::SessionRefused(detail) | Self::Failed(detail) => detail,
        }
    }
}

/// Shared adapter state: the rendezvous used to (re)find the daemon, the daemon it
/// currently talks to, the negotiated session id, the client handshake to replay if
/// the daemon is replaced, and the one stdout every path writes to.
struct Session {
    rendezvous: Rendezvous,
    descriptor: Mutex<DaemonDescriptor>,
    /// What the next request must rebuild first (`HEALTHY`, `SESSION_REFUSED` or
    /// `DAEMON_LOST`). A failed call is never itself replayed at the transport
    /// level; only a request the daemon refused before dispatch is sent again.
    stale: AtomicU8,
    /// Shared by healthy exchanges; taken exclusively while one caller recovers, so
    /// no request runs against the old descriptor or ahead of the replayed handshake.
    /// Replaced when the daemon wedges: calls stuck on it keep the old gate, so
    /// they cannot hold up the move to a private gateway.
    gate: Mutex<Arc<RwLock<()>>>,
    /// Serializes recoveries, which can overlap across a replaced gate.
    recovery: Mutex<()>,
    /// The endpoint the liveness check found wedged, until a recovery moves off it.
    wedged_endpoint: Mutex<Option<String>>,
    /// This client's own gateway, once the shared daemon wedged. Dropping it
    /// closes its stdin, which is what ends it.
    private_gateway: Mutex<Option<std::process::Child>>,
    session_id: Mutex<Option<String>>,
    /// The client's `initialize` and its `notifications/initialized`, kept so a
    /// replacement daemon can be given an equivalent session.
    handshake_initialize: Mutex<Option<String>>,
    handshake_initialized: Mutex<Option<String>>,
    stdout: Mutex<Box<dyn Write + Send>>,
    request_timeout: Duration,
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
    fn new(
        rendezvous: Rendezvous,
        descriptor: DaemonDescriptor,
        private: Option<std::process::Child>,
    ) -> Self {
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
            stale: AtomicU8::new(HEALTHY),
            gate: Mutex::new(Arc::new(RwLock::new(()))),
            recovery: Mutex::new(()),
            wedged_endpoint: Mutex::new(None),
            private_gateway: Mutex::new(private),
            session_id: Mutex::new(None),
            handshake_initialize: Mutex::new(None),
            handshake_initialized: Mutex::new(None),
            stdout: Mutex::new(Box::new(std::io::stdout())),
            request_timeout: REQUEST_TIMEOUT,
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
    /// already has its answer, but an error in its reply fails the replay. A
    /// transport failure marks the daemon lost and is returned as-is.
    fn post(&self, body: &str, forward: bool) -> Result<(), ExchangeError> {
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
            None => ureq::post(&url).timeout(self.request_timeout),
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
        let sent_session = self.session_id();
        if let Some(session) = &sent_session {
            request = request.set("Mcp-Session-Id", session);
        }
        let response = match request.send_string(body) {
            Ok(response) => response,
            // The daemon answered, just not with 2xx. It is alive, so this is not a
            // reason to re-rendezvous; the body is the error the caller should see.
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                // A modern protocol error comes with a 4xx status and a JSON-RPC
                // body. That body is the answer, and the client needs it intact:
                // an unsupported version lists the versions to retry with.
                if is_json_rpc_reply(&body) {
                    return if forward {
                        self.write_message(body.trim())
                            .map_err(ExchangeError::Failed)
                    } else {
                        reply_error(body.trim(), id).map_or(Ok(()), |error| {
                            Err(ExchangeError::Failed(format!(
                                "the host daemon refused the replayed handshake: {error}"
                            )))
                        })
                    };
                }
                let detail = format!("the host daemon answered HTTP {code}: {}", body.trim());
                // A live profile, enabled-set or tool-scope change rebinds the
                // session's scope, and the daemon then refuses the old id exactly
                // like an expired one. It does so before dispatching anything, so
                // the next exchange reopens the session with the replayed
                // handshake, and `exchange` may send this request once more.
                if code == 404 && sent_session.is_some() && is_session_refusal(&body) {
                    self.stale.fetch_max(SESSION_REFUSED, Ordering::SeqCst);
                    return Err(ExchangeError::SessionRefused(detail));
                }
                return Err(ExchangeError::Failed(detail));
            }
            Err(error) => {
                // The daemon is gone or unreachable. The next request re-rendezvouses;
                // the call that hit this may have run, so it is never retried.
                self.stale.fetch_max(DAEMON_LOST, Ordering::SeqCst);
                return Err(ExchangeError::Failed(error.to_string()));
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
        let mut replay_error = None;
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
                    replay_error = replay_error.take().or_else(|| reply_error(frame, id));
                    Ok(())
                }
            },
        )
        .map_err(ExchangeError::Failed)?;
        if let Some(error) = replay_error {
            return Err(ExchangeError::Failed(format!(
                "the host daemon refused the replayed handshake: {error}"
            )));
        }
        if expects_reply && !replied {
            let detail = match &subscription {
                Some(subscription) if subscription.cancelled() => return Ok(()),
                Some(_) => "the host daemon ended the subscription stream",
                None => "the host daemon closed the reply without an answer",
            };
            return Err(ExchangeError::Failed(detail.to_string()));
        }
        Ok(())
    }

    /// The gate in effect now. See [`Session::mark_wedged`] for why it changes.
    fn current_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(
            &self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Called by the liveness check when the daemon at `endpoint` stays silent.
    /// The next request moves this client to a private gateway. Calls already
    /// waiting on the wedged daemon hold the current gate, so a fresh one lets
    /// that move start without waiting for them.
    fn mark_wedged(&self, endpoint: &str) {
        let mut gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.descriptor().endpoint != endpoint {
            return;
        }
        *self
            .wedged_endpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(endpoint.to_string());
        self.stale.fetch_max(DAEMON_WEDGED, Ordering::SeqCst);
        *gate = Arc::new(RwLock::new(()));
    }

    /// Start a private gateway and point this session at it.
    fn use_private_gateway(&self, wedged: &DaemonDescriptor) -> Result<(), String> {
        let (child, descriptor) = start_private_gateway(&format!(
            "the daemon at {} (pid {}) did not answer its identity probe",
            wedged.endpoint, wedged.pid
        ))?;
        *self
            .private_gateway
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(child);
        if let Ok(mut guard) = self.descriptor.lock() {
            *guard = descriptor;
        }
        Ok(())
    }

    /// Open a replacement session by replaying the client's handshake. After a
    /// daemon failure, re-rendezvous first, or start a private gateway when the
    /// daemon is wedged; after a refused session the daemon is alive and keeps
    /// its descriptor. The caller holds the write gate.
    fn recover(&self, stale: u8) -> Result<(), String> {
        let _serial = self
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stale >= DAEMON_WEDGED {
            let wedged = self
                .wedged_endpoint
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let current = self.descriptor();
            if wedged.as_deref() != Some(current.endpoint.as_str()) {
                // An overlapping recovery already moved this client.
                return Ok(());
            }
            self.use_private_gateway(&current)?;
        } else if stale >= DAEMON_LOST {
            match self.rendezvous.ensure(spawn_daemon) {
                Ok(descriptor) => {
                    if let Ok(mut guard) = self.descriptor.lock() {
                        *guard = descriptor;
                    }
                }
                Err(EnsureError::Unresponsive(wedged)) => self.use_private_gateway(&wedged)?,
                Err(error) => {
                    return Err(format!(
                        "the host daemon could not be reached again: {error}"
                    ))
                }
            }
        }
        // The old session is gone either way, and `initialize` must not send it.
        if let Ok(mut guard) = self.session_id.lock() {
            *guard = None;
        }
        let reopen = |error: ExchangeError| {
            format!(
                "the host daemon session could not be reopened: {}",
                error.into_message()
            )
        };
        let initialize = self
            .handshake_initialize
            .lock()
            .ok()
            .and_then(|value| value.clone());
        if let Some(body) = initialize {
            self.post(&body, false).map_err(reopen)?;
            if self.session_id().is_none() {
                return Err(
                    "the host daemon session could not be reopened: no session id was issued"
                        .to_string(),
                );
            }
        }
        let initialized = self
            .handshake_initialized
            .lock()
            .ok()
            .and_then(|value| value.clone());
        if let Some(body) = initialized {
            self.post(&body, false).map_err(reopen)?;
        }
        Ok(())
    }

    /// POST one client message. A request the daemon refused before dispatch,
    /// because its session was rescoped or expired, is sent once more on the
    /// reopened session; a second refusal, and every other failure, goes back to
    /// the client unretried.
    fn exchange(&self, body: &str) -> Result<(), String> {
        match self.exchange_once(body) {
            Err(ExchangeError::SessionRefused(_)) if resendable_after_refusal(body) => {
                self.exchange_once(body).map_err(|error| match error {
                    ExchangeError::SessionRefused(detail) => {
                        format!("the host daemon refused the reopened session too: {detail}")
                    }
                    ExchangeError::Failed(detail) => detail,
                })
            }
            result => result.map_err(ExchangeError::into_message),
        }
    }

    /// One attempt. Healthy calls share the read gate and run concurrently; after a
    /// failure, one caller rebuilds the session under the write gate while the rest
    /// wait, so no request runs against the old descriptor or session, or ahead of
    /// the replayed handshake.
    fn exchange_once(&self, body: &str) -> Result<(), ExchangeError> {
        if self.stale.load(Ordering::SeqCst) == HEALTHY {
            let gate = self.current_gate();
            let _healthy = gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.stale.load(Ordering::SeqCst) == HEALTHY {
                self.remember_handshake(body);
                return self.post(body, true);
            }
        }
        let gate = self.current_gate();
        let _recovering = gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stale = self.stale.swap(HEALTHY, Ordering::SeqCst);
        if stale != HEALTHY {
            if let Err(error) = self.recover(stale) {
                // Let a later request try again rather than staying healthy-looking.
                self.stale.fetch_max(stale, Ordering::SeqCst);
                return Err(ExchangeError::Failed(error));
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

/// Whether a failed daemon POST is the pre-dispatch session refusal, and nothing
/// else: the body must be exactly the daemon's refusal object.
fn is_session_refusal(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|value| {
        value.as_object().is_some_and(|object| object.len() == 1)
            && value["error"] == SESSION_REFUSED_ERROR
    })
}

/// Whether a request refused for its session may be sent once more. Only legacy
/// requests carry a session. `initialize` opens its own; a notification or a
/// reply to the daemon belongs to the session that was refused.
fn resendable_after_refusal(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|message| {
        message.get("id").is_some_and(|id| !id.is_null())
            && message["method"]
                .as_str()
                .is_some_and(|method| method != "initialize")
            && declared_version(&message).is_none()
    })
}

/// The error in a JSON-RPC reply to `id`, if that is what `frame` is.
fn reply_error(frame: &str, id: Option<&serde_json::Value>) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(frame).ok()?;
    if value.get("method").is_some() || value.get("id") != id {
        return None;
    }
    let error = value.get("error")?;
    Some(
        error["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string()),
    )
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
fn proxy_stdio(
    rendezvous: Rendezvous,
    descriptor: DaemonDescriptor,
    private: Option<std::process::Child>,
) -> Result<(), String> {
    let session = Arc::new(Session::new(rendezvous, descriptor, private));
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
/// the daemon matches the response to its own outstanding request id. The daemon
/// sends a keepalive every 30 seconds, so a stream silent for three of them, or
/// one whose daemon this session has moved off, is reopened.
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
                ureq::AgentBuilder::new()
                    .timeout_read(SUBSCRIPTION_READ_TIMEOUT)
                    .build()
                    .get(&url)
                    .set("Authorization", &format!("Bearer {}", descriptor.token))
                    .set("Accept", "text/event-stream")
                    .set("Mcp-Session-Id", &session_id),
            )
            .call();
        match response {
            Ok(response) => {
                let mut reader = BufReader::new(response.into_reader());
                while let Ok(Some(ClientFrame::Line(line))) =
                    read_bounded_line(&mut reader, MAX_FRAME_BYTES)
                {
                    if session.descriptor().endpoint != descriptor.endpoint {
                        break;
                    }
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
///
/// The same check is the liveness test. The daemon answers it outside its
/// request workers, so silence for [`crate::daemon::SILENT_RETRY_TIMEOUT`]
/// means it is wedged, and the next request moves to a private gateway. A
/// daemon that is gone is left to the request path, which re-finds it.
fn spawn_heartbeat(session: Arc<Session>) {
    let interval =
        (crate::daemon::idle_grace() / 5).clamp(Duration::from_millis(100), LIVENESS_INTERVAL);
    std::thread::spawn(move || {
        let mut answered = Instant::now();
        let mut watched = session.descriptor().endpoint;
        loop {
            std::thread::sleep(interval);
            let descriptor = session.descriptor();
            if descriptor.endpoint != watched {
                watched = descriptor.endpoint.clone();
                answered = Instant::now();
            }
            match crate::daemon::probe_health(&descriptor) {
                crate::daemon::Health::Silent
                    if answered.elapsed() >= crate::daemon::SILENT_RETRY_TIMEOUT =>
                {
                    crate::gatewaylog::append(&format!(
                        "adapter: daemon pid {} silent for {}s, moving to a private gateway",
                        descriptor.pid,
                        answered.elapsed().as_secs()
                    ));
                    session.mark_wedged(&descriptor.endpoint);
                    answered = Instant::now();
                }
                crate::daemon::Health::Silent => {}
                crate::daemon::Health::Answering | crate::daemon::Health::Gone => {
                    answered = Instant::now();
                }
            }
        }
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

    /// SEC-04: the daemon must not inherit the client's ambient credentials, but
    /// it does need the whole Toolport control namespace and the locator vars.
    #[test]
    fn daemon_environment_keeps_control_vars_and_drops_ambient_secrets() {
        let parent: std::collections::BTreeMap<String, String> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            ("TOOLPORT_DATA_DIR", "/data"),
            ("TOOLPORT_SECRET_KEY", "vault-key"),
            ("TOOLPORT_FOO", "future-control"),
            ("CONDUIT_BAR", "legacy-control"),
            ("AWS_SECRET_ACCESS_KEY", "aws"),
            ("GITHUB_TOKEN", "ghp"),
            ("OPENAI_API_KEY", "sk"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let env: std::collections::BTreeMap<String, String> =
            daemon_environment(&parent).into_iter().collect();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/u"));
        assert_eq!(
            env.get("TOOLPORT_DATA_DIR").map(String::as_str),
            Some("/data")
        );
        assert_eq!(
            env.get("TOOLPORT_SECRET_KEY").map(String::as_str),
            Some("vault-key")
        );
        assert_eq!(
            env.get("TOOLPORT_FOO").map(String::as_str),
            Some("future-control"),
            "the whole control namespace is kept, not a fixed list"
        );
        assert_eq!(
            env.get("CONDUIT_BAR").map(String::as_str),
            Some("legacy-control")
        );
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.contains_key("GITHUB_TOKEN"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
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
            None,
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
            None,
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

    /// Client output captured in memory instead of the process stdout.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn messages(&self) -> Vec<serde_json::Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    /// One POST the scripted daemon received.
    #[derive(Clone)]
    struct Posted {
        session: Option<String>,
        message: serde_json::Value,
    }

    struct Answer {
        status: u16,
        session: Option<String>,
        body: String,
        delay: Duration,
    }

    impl Answer {
        fn result(to: &Posted, result: serde_json::Value) -> Self {
            Self::body(
                200,
                serde_json::json!({ "jsonrpc": "2.0", "id": to.message["id"], "result": result }),
            )
        }

        fn body(status: u16, body: serde_json::Value) -> Self {
            Self {
                status,
                session: None,
                body: body.to_string(),
                delay: Duration::ZERO,
            }
        }

        fn accepted() -> Self {
            Self {
                status: 202,
                session: None,
                body: String::new(),
                delay: Duration::ZERO,
            }
        }

        fn refused() -> Self {
            Self::body(404, serde_json::json!({ "error": SESSION_REFUSED_ERROR }))
        }

        fn with_session(mut self, session: String) -> Self {
            self.session = Some(session);
            self
        }

        fn after(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    /// A loopback stand-in for the daemon's `/mcp` endpoint that answers each POST
    /// from a script and records what it received.
    struct ScriptedDaemon {
        endpoint: String,
        posted: Arc<Mutex<Vec<Posted>>>,
    }

    impl ScriptedDaemon {
        fn start(script: impl Fn(&Posted) -> Answer + Send + Sync + 'static) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = listener.local_addr().unwrap().to_string();
            let posted = Arc::new(Mutex::new(Vec::new()));
            let script = Arc::new(script);
            let record = Arc::clone(&posted);
            std::thread::spawn(move || {
                for stream in listener.incoming().map_while(Result::ok) {
                    let script = Arc::clone(&script);
                    let record = Arc::clone(&record);
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut length = 0;
                        let mut session = None;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            let line = line.trim_end();
                            if line.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = line.split_once(':') {
                                match name.to_ascii_lowercase().as_str() {
                                    "content-length" => length = value.trim().parse().unwrap(),
                                    "mcp-session-id" => session = Some(value.trim().to_string()),
                                    _ => {}
                                }
                            }
                        }
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).unwrap();
                        let posted = Posted {
                            session,
                            message: serde_json::from_slice(&body).unwrap(),
                        };
                        record.lock().unwrap().push(posted.clone());
                        let answer = script(&posted);
                        std::thread::sleep(answer.delay);
                        let mut head = format!(
                            "HTTP/1.1 {} Scripted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                            answer.status,
                            answer.body.len()
                        );
                        if let Some(session) = answer.session {
                            head.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
                        }
                        let mut stream = stream;
                        let _ = write!(stream, "{head}\r\n{}", answer.body);
                        let _ = stream.flush();
                    });
                }
            });
            Self { endpoint, posted }
        }

        fn posted(&self, method: &str) -> Vec<Posted> {
            self.posted
                .lock()
                .unwrap()
                .iter()
                .filter(|posted| posted.message["method"] == method)
                .cloned()
                .collect()
        }
    }

    fn scripted_session(daemon: &ScriptedDaemon, sink: &Sink, timeout: Duration) -> Session {
        let data_dir = std::env::temp_dir();
        let compat = CompatKey::new("test", data_dir.to_string_lossy());
        let mut session = Session::new(
            Rendezvous::new(&data_dir, compat.clone()),
            DaemonDescriptor::new(daemon.endpoint.as_str(), "test-token", &compat),
            None,
        );
        session.stdout = Mutex::new(Box::new(sink.clone()));
        session.request_timeout = timeout;
        session
    }

    /// Answers `initialize` with a new session id (`s1`, `s2`, ...) each time and
    /// accepts `notifications/initialized`; anything else goes to `call`.
    fn session_daemon(
        call: impl Fn(&Posted) -> Answer + Send + Sync + 'static,
    ) -> (ScriptedDaemon, Arc<AtomicUsize>) {
        let minted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&minted);
        let daemon = ScriptedDaemon::start(move |posted| match posted.message["method"].as_str() {
            Some("initialize") => {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                Answer::result(
                    posted,
                    serde_json::json!({ "protocolVersion": "2024-11-05" }),
                )
                .with_session(format!("s{n}"))
            }
            Some("notifications/initialized") => Answer::accepted(),
            _ => call(posted),
        });
        (daemon, minted)
    }

    fn open(session: &Session) {
        session
            .exchange(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#)
            .unwrap();
        session
            .exchange(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .unwrap();
    }

    fn tool_call(id: u64) -> String {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "write_note", "arguments": { "text": id.to_string() } }
        })
        .to_string()
    }

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn a_refused_session_is_reopened_and_the_request_sent_once_more() {
        let executed = Arc::new(AtomicUsize::new(0));
        let runs = Arc::clone(&executed);
        let (daemon, minted) = session_daemon(move |posted| {
            if posted.session.as_deref() == Some("s1") {
                return Answer::refused();
            }
            runs.fetch_add(1, Ordering::SeqCst);
            Answer::result(posted, serde_json::json!({ "content": [] }))
        });
        let sink = Sink::default();
        let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
        open(&session);

        session.exchange(&tool_call(2)).unwrap();

        assert_eq!(
            executed.load(Ordering::SeqCst),
            1,
            "the call ran exactly once"
        );
        assert_eq!(minted.load(Ordering::SeqCst), 2, "one replacement session");
        let sessions: Vec<_> = daemon
            .posted("tools/call")
            .into_iter()
            .map(|posted| posted.session)
            .collect();
        assert_eq!(sessions, [Some("s1".into()), Some("s2".into())]);
        assert_eq!(daemon.posted("notifications/initialized").len(), 2);
        let replies = sink.messages();
        assert_eq!(
            replies.len(),
            2,
            "the replayed initialize is not forwarded: {replies:?}"
        );
        assert_eq!(replies[1]["id"], 2);
        assert!(replies[1].get("result").is_some());
        assert_eq!(session.stale.load(Ordering::SeqCst), HEALTHY);
    }

    #[test]
    fn a_second_refusal_is_returned_without_another_send() {
        let (daemon, minted) = session_daemon(|_| Answer::refused());
        let sink = Sink::default();
        let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
        open(&session);

        let error = session.exchange(&tool_call(2)).unwrap_err();

        assert!(
            error.contains("refused the reopened session too"),
            "{error}"
        );
        assert_eq!(daemon.posted("tools/call").len(), 2);
        assert_eq!(minted.load(Ordering::SeqCst), 2);
        assert_eq!(
            sink.messages().len(),
            1,
            "only the initialize reply reached the client"
        );
    }

    #[test]
    fn other_failures_are_not_sent_again() {
        for (label, answer, stale) in [
            (
                "another 404",
                Answer::body(404, serde_json::json!({ "error": "not found" })),
                HEALTHY,
            ),
            (
                "a refusal-like body with more fields",
                Answer::body(
                    404,
                    serde_json::json!({ "error": SESSION_REFUSED_ERROR, "detail": "proxy" }),
                ),
                HEALTHY,
            ),
            (
                "a server error",
                Answer::body(500, serde_json::json!({ "error": "boom" })),
                HEALTHY,
            ),
        ] {
            let answer = Mutex::new(Some(answer));
            let (daemon, minted) =
                session_daemon(move |_| answer.lock().unwrap().take().expect("one call only"));
            let sink = Sink::default();
            let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
            open(&session);
            assert!(session.exchange(&tool_call(2)).is_err(), "{label}");
            assert_eq!(daemon.posted("tools/call").len(), 1, "{label}");
            assert_eq!(minted.load(Ordering::SeqCst), 1, "{label}");
            assert_eq!(session.stale.load(Ordering::SeqCst), stale, "{label}");
        }
    }

    #[test]
    fn a_refused_notification_is_not_sent_again() {
        let (daemon, minted) = session_daemon(|_| Answer::refused());
        let sink = Sink::default();
        let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
        open(&session);
        let cancelled =
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#;
        assert!(session.exchange(cancelled).is_err());
        assert_eq!(daemon.posted("notifications/cancelled").len(), 1);
        assert_eq!(minted.load(Ordering::SeqCst), 1);
        assert_eq!(
            session.stale.load(Ordering::SeqCst),
            SESSION_REFUSED,
            "the next request reopens the session first"
        );
    }

    #[test]
    fn a_timed_out_request_is_not_sent_again() {
        let (daemon, _) = session_daemon(|posted| {
            Answer::result(posted, serde_json::json!({})).after(Duration::from_secs(2))
        });
        let sink = Sink::default();
        let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
        open(&session);
        let mut session = session;
        session.request_timeout = Duration::from_millis(300);

        assert!(session.exchange(&tool_call(2)).is_err());
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(daemon.posted("tools/call").len(), 1);
        assert_eq!(session.stale.load(Ordering::SeqCst), DAEMON_LOST);
    }

    #[test]
    fn downstream_json_rpc_errors_are_relayed_once() {
        for status in [200, 400] {
            let (daemon, _) = session_daemon(move |posted| {
                Answer::body(
                    status,
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": posted.message["id"],
                        "error": { "code": -32000, "message": "downstream failed" }
                    }),
                )
            });
            let sink = Sink::default();
            let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
            open(&session);
            session.exchange(&tool_call(2)).unwrap();
            assert_eq!(daemon.posted("tools/call").len(), 1, "HTTP {status}");
            let replies = sink.messages();
            assert_eq!(replies[1]["error"]["message"], "downstream failed");
            assert_eq!(session.stale.load(Ordering::SeqCst), HEALTHY);
        }
    }

    #[test]
    fn a_failed_reopen_is_reported_and_nothing_is_resent() {
        let minted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&minted);
        let daemon = ScriptedDaemon::start(move |posted| match posted.message["method"].as_str() {
            Some("initialize") if count.fetch_add(1, Ordering::SeqCst) == 0 => {
                Answer::result(posted, serde_json::json!({})).with_session("s1".into())
            }
            Some("initialize") => Answer::body(
                200,
                serde_json::json!({
                    "jsonrpc": "2.0", "id": posted.message["id"],
                    "error": { "code": -32602, "message": "unsupported protocol version" }
                }),
            )
            .with_session("s2".into()),
            Some("notifications/initialized") => Answer::accepted(),
            _ => Answer::refused(),
        });
        let sink = Sink::default();
        let session = scripted_session(&daemon, &sink, TEST_TIMEOUT);
        open(&session);

        let error = session.exchange(&tool_call(2)).unwrap_err();

        assert!(error.contains("could not be reopened"), "{error}");
        assert!(error.contains("unsupported protocol version"), "{error}");
        assert_eq!(daemon.posted("tools/call").len(), 1);
        assert_eq!(minted.load(Ordering::SeqCst), 2);
        assert_eq!(
            session.stale.load(Ordering::SeqCst),
            SESSION_REFUSED,
            "a later request tries to reopen again"
        );
    }

    #[test]
    fn concurrent_refusals_reopen_the_session_once() {
        let executed = Arc::new(Mutex::new(Vec::new()));
        let runs = Arc::clone(&executed);
        let (daemon, minted) = session_daemon(move |posted| {
            if posted.session.as_deref() == Some("s1") {
                return Answer::refused().after(Duration::from_millis(100));
            }
            runs.lock()
                .unwrap()
                .push(posted.message["id"].as_u64().unwrap());
            Answer::result(posted, serde_json::json!({ "content": [] }))
        });
        let sink = Sink::default();
        let session = Arc::new(scripted_session(&daemon, &sink, TEST_TIMEOUT));
        open(&session);

        let start = Arc::new(std::sync::Barrier::new(8));
        let callers: Vec<_> = (2..10)
            .map(|id| {
                let session = Arc::clone(&session);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    session.exchange(&tool_call(id))
                })
            })
            .collect();
        for caller in callers {
            caller.join().unwrap().unwrap();
        }

        assert_eq!(minted.load(Ordering::SeqCst), 2, "one replacement session");
        let mut ran = executed.lock().unwrap().clone();
        ran.sort_unstable();
        assert_eq!(
            ran,
            (2..10).collect::<Vec<_>>(),
            "each call ran exactly once"
        );
        assert_eq!(sink.messages().len(), 9);
    }

    #[test]
    fn only_a_refused_legacy_request_may_be_sent_again() {
        assert!(resendable_after_refusal(&tool_call(1)));
        assert!(resendable_after_refusal(
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#
        ));
        for body in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{"roots":[]}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
            "not json",
        ] {
            assert!(!resendable_after_refusal(body), "{body}");
        }
        assert!(is_session_refusal(
            r#"{"error":"unknown or expired Mcp-Session-Id; re-initialize"}"#
        ));
        for body in [
            r#"{"error":"missing Mcp-Session-Id (send initialize first)"}"#,
            r#"{"error":"unknown or expired Mcp-Session-Id; re-initialize","x":1}"#,
            "unknown or expired Mcp-Session-Id; re-initialize",
        ] {
            assert!(!is_session_refusal(body), "{body}");
        }
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
