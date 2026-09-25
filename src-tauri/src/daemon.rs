//! Host daemon rendezvous: Phase 2 of `docs/design/one-gateway-per-host.md`.
//!
//! Primitives only. A version-keyed descriptor, an authenticated identity
//! handshake, and a cross-process election so exactly one compatible daemon runs
//! per data directory and gateway build. Nothing here changes the default
//! topology: the stdio adapter that speaks the full session protocol lands in a
//! later slice.
//!
//! The pieces deliberately mirror the approval broker's endpoint descriptor
//! (`crate::approval`) and reuse the registry's cross-process lock
//! ([`crate::registry::lock_at`]) rather than inventing new primitives.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::registry;
use crate::topology::CompatKey;

/// Internal adapter/daemon wire generation. Bumped whenever the contract between
/// the two changes; two processes may only share a runtime when this and the
/// compat fingerprint match.
pub const PROTOCOL_GENERATION: u32 = 1;

/// The daemon's internal identity endpoint. Authenticated with the descriptor
/// token; never the user-facing HTTP surface.
pub const IDENTITY_PATH: &str = "/host/identity";
/// Private, bearer-gated counts for the opt-in topology acceptance run.
pub const TOPOLOGY_PATH: &str = "/host/topology";
/// Private lease for the desktop's lightweight public HTTP bridge.
pub const HTTP_SERVICE_LEASE_PATH: &str = "/host/http-service-lease";
/// Ask an unused daemon to leave before replacing its executable during update.
pub const SHUTDOWN_IF_IDLE_PATH: &str = "/host/shutdown-if-idle";

/// Bounded wait for a spawned daemon to publish a reachable descriptor.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the winner may hold the election lock while waiting for readiness.
/// Must exceed everything a lock holder can spend probing: the under-lock
/// recheck (at most two [`PROBE_TIMEOUT`] attempts), [`READY_TIMEOUT`], and
/// one in-flight poll past its deadline, so a blocked contender never gives up
/// first.
const ELECTION_TIMEOUT: Duration = Duration::from_secs(20);
/// Per-probe network budget for the authenticated handshake.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Descriptor publication and cleanup are short filesystem operations. Keep
/// their lock separate from election, which is deliberately held across spawn
/// and readiness polling.
const DESCRIPTOR_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the fast path keeps re-probing a silent endpoint before giving up
/// on it. A loaded machine can outrun [`PROBE_TIMEOUT`] while its daemon is
/// perfectly live. Twenty concurrent adapters exceeded the old six-second
/// budget on macOS CI; silence alone is never evidence against the pointer.
const SILENT_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(50);
/// Operational idle grace: the daemon exits after this long with no requests.
/// A default, not a user setting, in the first release.
pub const DAEMON_IDLE_GRACE: Duration = Duration::from_secs(300);

/// Everything an adapter needs to reach and trust a running daemon. Written
/// atomically into the data directory with user-only permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonDescriptor {
    /// Loopback `host:port` for the internal endpoint.
    pub endpoint: String,
    /// Random bearer both sides hold; only a process that can read the data
    /// directory can obtain it.
    pub token: String,
    /// Daemon PID. Informational only: correctness never depends on a PID or an
    /// open port, only on the authenticated handshake.
    pub pid: u32,
    /// [`CompatKey::fingerprint`] of the daemon.
    pub compat: String,
    /// [`PROTOCOL_GENERATION`] of the daemon.
    pub protocol: u32,
    pub created_at_ms: u128,
}

impl DaemonDescriptor {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>, compat: &CompatKey) -> Self {
        Self {
            endpoint: endpoint.into(),
            token: token.into(),
            pid: std::process::id(),
            compat: compat.fingerprint(),
            protocol: PROTOCOL_GENERATION,
            created_at_ms: now_ms(),
        }
    }

    /// Whether this descriptor claims the same compatibility domain as `compat`.
    /// A claim, not proof; [`probe_identity`] is what proves it.
    pub fn claims_compat(&self, compat: &CompatKey) -> bool {
        self.compat == compat.fingerprint() && self.protocol == PROTOCOL_GENERATION
    }
}

/// What a live daemon returns from [`IDENTITY_PATH`]. The handshake compares this
/// against the expected [`CompatKey`], so a stale file or a port squatter can
/// never be mistaken for a compatible daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonIdentity {
    pub compat: String,
    pub protocol: u32,
    pub pid: u32,
    pub gateway_version: String,
}

impl DaemonIdentity {
    pub fn is_compatible_with(&self, compat: &CompatKey) -> bool {
        self.compat == compat.fingerprint() && self.protocol == PROTOCOL_GENERATION
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The descriptor file for one compatibility domain. Keyed by the compat
/// fingerprint, so a build or data-directory mismatch can never read another
/// domain's pointer.
pub fn descriptor_path(data_dir: &Path, compat: &CompatKey) -> PathBuf {
    data_dir.join(format!("daemon-{}.json", compat.fingerprint()))
}

/// The lock base for the election. `registry::lock_at` appends `.lock` to this
/// path, so the on-disk lock is `daemon-<fingerprint>.lock`.
pub fn election_lock_base(data_dir: &Path, compat: &CompatKey) -> PathBuf {
    data_dir.join(format!("daemon-{}", compat.fingerprint()))
}

pub fn read_descriptor(path: &Path) -> Option<DaemonDescriptor> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Publish a descriptor atomically and restrict it to the owning user. A missing
/// or corrupt file is always treated as "no daemon", never as an error a caller
/// has to handle.
pub fn write_descriptor(path: &Path, descriptor: &DaemonDescriptor) -> Result<(), String> {
    let _descriptor_lock = registry::lock_at_for(path, DESCRIPTOR_LOCK_TIMEOUT)?;
    let raw = serde_json::to_string(descriptor).map_err(|e| e.to_string())?;
    registry::atomic_write(path, &raw)?;
    restrict_to_owner(path)
}

pub fn clear_descriptor(path: &Path) {
    let Ok(_descriptor_lock) = registry::lock_at_for(path, DESCRIPTOR_LOCK_TIMEOUT) else {
        return;
    };
    clear_descriptor_locked(path);
}

fn clear_descriptor_locked(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// A fresh bearer for the internal endpoint.
pub fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("Could not generate a token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Authenticated `GET /host/identity`. Verifies nothing about compatibility
/// itself; the caller compares the returned identity against its own
/// [`CompatKey`]. Any transport error is a failed probe, not a hard error.
pub fn probe_identity(descriptor: &DaemonDescriptor) -> Result<DaemonIdentity, String> {
    attempt_identity_probe(descriptor).map_err(|failure| match failure {
        ProbeFailure::Answered(detail) => detail,
        ProbeFailure::Unreachable => "the daemon endpoint is not reachable".to_string(),
        ProbeFailure::Silent => "the daemon did not answer within the probe budget".to_string(),
    })
}

/// Request a graceful exit. The daemon keeps serving while any session, public
/// service lease, or request is active and withdraws its descriptor before exit.
pub fn request_shutdown_if_idle(descriptor: &DaemonDescriptor) -> Result<(), String> {
    let address = descriptor
        .endpoint
        .parse::<std::net::SocketAddr>()
        .map_err(|_| "daemon endpoint must be a loopback address".to_string())?;
    if !address.ip().is_loopback() {
        return Err("daemon endpoint must be a loopback address".to_string());
    }
    ureq::post(&format!(
        "http://{}{}",
        descriptor.endpoint, SHUTDOWN_IF_IDLE_PATH
    ))
    .timeout(PROBE_TIMEOUT)
    .set("Authorization", &format!("Bearer {}", descriptor.token))
    .call()
    .map(|_| ())
    .map_err(|error| format!("could not request idle daemon shutdown: {error}"))
}

/// Why an identity probe failed, in the terms the rendezvous decides on: what
/// may be cleared, and what may spawn.
#[derive(Debug)]
enum ProbeFailure {
    /// The endpoint answered, but not as a compatible daemon would: a refused
    /// status or a body that is not an identity. A stale pointer.
    Answered(String),
    /// Nothing usable is at the endpoint: the connection is refused or reset,
    /// or the endpoint itself is malformed or unresolvable. Deterministic
    /// failures, every one of them. A stale pointer.
    Unreachable,
    /// The endpoint stayed silent for the whole `PROBE_TIMEOUT`. A daemon may
    /// be alive but wedged, so this is never evidence against its pointer.
    Silent,
}

/// Read the outcome out of a ureq error. A refused or reset connection, or an
/// endpoint that cannot be parsed or resolved, says the pointer is unusable
/// garbage; an answered status or a mangled response says someone else owns
/// the port; silence alone says nothing either way.
fn classify_probe_error(error: ureq::Error) -> ProbeFailure {
    match &error {
        ureq::Error::Status(..) => ProbeFailure::Answered(error.to_string()),
        ureq::Error::Transport(transport) => {
            let io_kind = transport_io_kind(transport);
            match transport.kind() {
                ureq::ErrorKind::ConnectionFailed if is_timeout_io_kind(io_kind) => {
                    ProbeFailure::Silent
                }
                ureq::ErrorKind::ConnectionFailed
                | ureq::ErrorKind::InvalidUrl
                | ureq::ErrorKind::UnknownScheme
                | ureq::ErrorKind::Dns => ProbeFailure::Unreachable,
                ureq::ErrorKind::BadStatus | ureq::ErrorKind::BadHeader => {
                    ProbeFailure::Answered(error.to_string())
                }
                ureq::ErrorKind::Io => match io_kind {
                    Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                        ProbeFailure::Silent
                    }
                    Some(
                        std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe,
                    ) => ProbeFailure::Unreachable,
                    _ => ProbeFailure::Silent,
                },
                _ => ProbeFailure::Silent,
            }
        }
    }
}

/// Find an I/O cause anywhere in ureq's transport error chain. Connect
/// timeouts are wrapped as `ConnectionFailed`, while read timeouts are `Io`;
/// rendezvous must treat both as silence rather than evidence of a dead daemon.
fn transport_io_kind(transport: &ureq::Transport) -> Option<std::io::ErrorKind> {
    use std::error::Error as _;
    let mut source = transport.source();
    while let Some(error) = source {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        source = error.source();
    }
    None
}

fn is_timeout_io_kind(kind: Option<std::io::ErrorKind>) -> bool {
    matches!(
        kind,
        Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
    )
}

/// One authenticated attempt against `GET /host/identity`, keeping the
/// failure kind so the rendezvous can decide on it.
fn attempt_identity_probe(descriptor: &DaemonDescriptor) -> Result<DaemonIdentity, ProbeFailure> {
    let url = format!("http://{}{}", descriptor.endpoint, IDENTITY_PATH);
    let response = ureq::get(&url)
        .set("Authorization", &format!("Bearer {}", descriptor.token))
        .timeout(PROBE_TIMEOUT)
        .call()
        .map_err(classify_probe_error)?;
    response
        .into_json::<DaemonIdentity>()
        .map_err(|e| ProbeFailure::Answered(format!("Daemon identity was not valid JSON: {e}")))
}

/// One compatibility domain's rendezvous: the data directory and the build it may
/// share a runtime with.
///
/// The correctness rule lives here: only a proven-live daemon is reused, only
/// a proven-stale pointer is cleared, and an endpoint that stays silent is
/// neither — it may be a live daemon too wedged to answer.
pub struct Rendezvous {
    data_dir: PathBuf,
    compat: CompatKey,
}

impl Rendezvous {
    pub fn new(data_dir: impl Into<PathBuf>, compat: CompatKey) -> Self {
        Self {
            data_dir: data_dir.into(),
            compat,
        }
    }

    pub fn descriptor_path(&self) -> PathBuf {
        descriptor_path(&self.data_dir, &self.compat)
    }

    /// Return the reachable compatible daemon's descriptor, starting one if
    /// needed. `spawn` is the caller's way to launch `--daemon` (a detached child
    /// in production, an in-process thread in tests). Exactly one caller in a
    /// concurrent cold start runs `spawn`.
    pub fn ensure(
        &self,
        mut spawn: impl FnMut() -> Result<(), String>,
    ) -> Result<DaemonDescriptor, String> {
        let path = self.descriptor_path();
        match self.probe_for_reuse(&path) {
            Probe::Live(descriptor) => return Ok(descriptor),
            // A silent daemon may still be alive: reuse is unproven, and
            // clearing or spawning beside it is the double-election defect.
            // Name it and fail; if it really is gone, its idle watchdog
            // clears the pointer on the way out.
            Probe::Silent(descriptor) => return Err(unresponsive_daemon_error(&descriptor)),
            Probe::Gone => {}
        }

        // Elect: `lock_at` appends `.lock` and creates parent directories.
        let lock_base = election_lock_base(&self.data_dir, &self.compat);
        let _election = registry::lock_at_for(&lock_base, ELECTION_TIMEOUT)?;

        // Recheck under the lock: another contender may have started the daemon
        // while we waited.
        match self.probe(&path) {
            Probe::Live(descriptor) => return Ok(descriptor),
            Probe::Silent(descriptor) => return Err(unresponsive_daemon_error(&descriptor)),
            Probe::Gone => {}
        }

        // We are the winner. Drop any stale pointer first so readiness polling
        // cannot mistake it for the daemon we are about to start.
        clear_descriptor(&path);
        spawn()?;

        // Wait for readiness while still holding the lock, so no second
        // contender spawns a daemon of its own. Until the daemon is ready,
        // silence and refusals both just mean "not answering yet", so every
        // non-live outcome keeps polling inside the budget.
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if let Probe::Live(descriptor) = self.probe(&path) {
                return Ok(descriptor);
            }
            std::thread::sleep(READY_POLL);
        }
        Err("the daemon did not become ready before the deadline".to_string())
    }

    /// Read, claim-check, then prove with the authenticated handshake. A
    /// missing, mismatched, answered-wrong, or unreachable descriptor is "no
    /// live daemon"; a silent endpoint is not ([`Probe::Silent`]). One refusal
    /// is thin evidence for destroying a pointer, so the unreachable case
    /// rechecks once before declaring it stale.
    fn probe(&self, path: &Path) -> Probe {
        let Some(descriptor) = read_descriptor(path) else {
            return Probe::Gone;
        };
        if !descriptor.claims_compat(&self.compat) {
            return Probe::Gone;
        }
        match attempt_identity_probe(&descriptor) {
            Ok(identity) if identity.is_compatible_with(&self.compat) => Probe::Live(descriptor),
            Ok(_) | Err(ProbeFailure::Answered(_)) => Probe::Gone,
            Err(ProbeFailure::Unreachable) => match attempt_identity_probe(&descriptor) {
                Ok(identity) if identity.is_compatible_with(&self.compat) => Probe::Live(descriptor),
                Err(ProbeFailure::Silent) => Probe::Silent(descriptor),
                _ => Probe::Gone,
            },
            Err(ProbeFailure::Silent) => Probe::Silent(descriptor),
        }
    }

    /// The fast-path probe with bounded patience: give a live-but-slow daemon
    /// the whole [`SILENT_RETRY_TIMEOUT`] to answer before concluding
    /// anything about its pointer.
    fn probe_for_reuse(&self, path: &Path) -> Probe {
        let deadline = Instant::now() + SILENT_RETRY_TIMEOUT;
        loop {
            match self.probe(path) {
                Probe::Silent(_) if Instant::now() < deadline => {}
                outcome => return outcome,
            }
            std::thread::sleep(READY_POLL);
        }
    }
}

/// What one look at the descriptor file concluded. The distinction carries the
/// correctness rule above: only `Gone` justifies clearing the pointer, and
/// only `Live` justifies reusing it.
enum Probe {
    /// A compatible daemon answered the authenticated handshake.
    Live(DaemonDescriptor),
    /// The pointer is stale: nothing answers, or something else does. The
    /// rendezvous may clear it and elect a new daemon.
    Gone,
    /// The endpoint stayed silent for the whole probe budget. A daemon may be
    /// alive but wedged; never clear or spawn beside it on this evidence.
    Silent(DaemonDescriptor),
}

/// The error a persistent silence becomes: never a clearing, never a second
/// daemon, and a name the operator can act on.
fn unresponsive_daemon_error(descriptor: &DaemonDescriptor) -> String {
    format!(
        "the daemon at {} (pid {}) did not answer its identity probe; its descriptor was left in place",
        descriptor.endpoint, descriptor.pid
    )
}

/// The daemon side of the handshake. Binds an ephemeral loopback port, publishes
/// the descriptor, signals `ready`, then serves identity requests until the
/// process exits. Blocking on purpose: the daemon owns this thread.
pub fn serve_identity(
    descriptor_path: &Path,
    compat: &CompatKey,
    token: String,
    ready: Option<std::sync::mpsc::Sender<DaemonDescriptor>>,
    idle_timeout: Option<Duration>,
) -> Result<(), String> {
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| format!("Could not bind the daemon endpoint: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or("the daemon endpoint was not an IP socket")?
        .port();
    let identity = DaemonIdentity {
        compat: compat.fingerprint(),
        protocol: PROTOCOL_GENERATION,
        pid: std::process::id(),
        gateway_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let descriptor = DaemonDescriptor::new(format!("127.0.0.1:{port}"), token.clone(), compat);
    write_descriptor(descriptor_path, &descriptor)?;
    if let Some(sender) = ready {
        let _ = sender.send(descriptor.clone());
    }

    // Poll so an idle daemon can exit without a request to wake it; a real
    // request still returns immediately.
    let mut last_activity = Instant::now();
    loop {
        match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(request)) => {
                last_activity = Instant::now();
                let is_identity = request.method() == &tiny_http::Method::Get
                    && request.url().split('?').next() == Some(IDENTITY_PATH);
                let authorized = request.headers().iter().any(|header| {
                    header.field.equiv("Authorization")
                        && header.value.as_str() == format!("Bearer {token}")
                });
                let response = if !is_identity {
                    text_response(404, "not found")
                } else if !authorized {
                    text_response(401, "unauthorized")
                } else {
                    let body = serde_json::to_string(&identity).unwrap_or_else(|_| "{}".to_string());
                    tiny_http::Response::from_string(body)
                        .with_status_code(200)
                        .with_header(json_header())
                };
                let _ = request.respond(response);
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if idle_timeout.is_some_and(|grace| last_activity.elapsed() >= grace) {
            break;
        }
    }
    // Leave no stale pointer behind for the next rendezvous to trip over —
    // but only ours. If another daemon has already published over this path,
    // the file is the survivor's, and deleting it would strand every later
    // rendezvous with the wrong daemon.
    if let Ok(_descriptor_lock) =
        registry::lock_at_for(descriptor_path, DESCRIPTOR_LOCK_TIMEOUT)
    {
        if let Some(current) = read_descriptor(descriptor_path) {
            if current.token == token && current.endpoint == descriptor.endpoint {
                clear_descriptor_locked(descriptor_path);
            }
        }
    }
    Ok(())
}

fn text_response(code: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body.to_string()).with_status_code(code)
}

fn json_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_dir(label: &str) -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!(
            "toolport-daemon-{label}-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn compat(version: &str, dir: &Path) -> CompatKey {
        CompatKey::new(version, dir.display().to_string())
    }

    #[test]
    fn shutdown_request_rejects_non_loopback_descriptors_before_sending_bearer() {
        let key = CompatKey::new("test", "scratch");
        for endpoint in ["192.0.2.1:9", "not-a-socket"] {
            let descriptor = DaemonDescriptor::new(endpoint, "private-token", &key);
            assert!(request_shutdown_if_idle(&descriptor).is_err());
        }
    }

    /// Start an in-process identity listener and return its descriptor.
    fn start_listener(dir: &Path, compat: &CompatKey) -> DaemonDescriptor {
        let path = descriptor_path(dir, compat);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let compat = compat.clone();
        std::thread::spawn(move || {
            let _ = serve_identity(&path, &compat, new_token().unwrap(), Some(ready_tx), None);
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    /// A TCP listener that accepts connections and then stalls, holding each
    /// socket open without ever answering: a daemon alive enough to own its
    /// port, but wedged past the probe budget.
    fn start_stalled_listener() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
            }
        });
        endpoint
    }

    /// Simulate a live daemon whose identity endpoint is delayed by cold-start
    /// load beyond the old six-second retry budget.
    fn start_delayed_responder(delay: Duration, identity: DaemonIdentity) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let body = serde_json::to_string(&identity).unwrap();
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        endpoint
    }

    /// A TCP listener that answers every request with a 200 and a body that is
    /// not an identity: a port squatter the handshake must reject, then route
    /// around. The response closes the connection so probes never pool.
    fn start_garbage_responder() -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\noops!",
                );
            }
        });
        endpoint
    }

    #[test]
    fn descriptor_path_is_keyed_by_compat() {
        let dir = temp_dir("keyed");
        let a = compat("1.0.0", &dir);
        let b = compat("1.0.1", &dir);
        let c = compat("1.0.0", &temp_dir("keyed-other"));
        assert_ne!(descriptor_path(&dir, &a), descriptor_path(&dir, &b));
        assert_ne!(descriptor_path(&dir, &a), descriptor_path(&dir, &c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_returns_a_compatible_identity() {
        let dir = temp_dir("probe-ok");
        let compat = compat("1.0.0", &dir);
        let descriptor = start_listener(&dir, &compat);
        let identity = probe_identity(&descriptor).unwrap();
        assert!(identity.is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_rejects_a_wrong_token() {
        let dir = temp_dir("probe-token");
        let compat = compat("1.0.0", &dir);
        let mut descriptor = start_listener(&dir, &compat);
        descriptor.token = "not-the-token".to_string();
        assert!(probe_identity(&descriptor).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_timeouts_are_silence_not_evidence_of_a_dead_daemon() {
        assert!(is_timeout_io_kind(Some(std::io::ErrorKind::TimedOut)));
        assert!(is_timeout_io_kind(Some(std::io::ErrorKind::WouldBlock)));
        assert!(!is_timeout_io_kind(Some(
            std::io::ErrorKind::ConnectionRefused
        )));
    }

    #[test]
    fn identity_from_one_build_is_not_compatible_with_another() {
        let dir = temp_dir("probe-compat");
        let running = compat("1.0.0", &dir);
        let other = compat("2.0.0", &dir);
        let descriptor = start_listener(&dir, &running);
        let identity = probe_identity(&descriptor).unwrap();
        assert!(identity.is_compatible_with(&running));
        assert!(!identity.is_compatible_with(&other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perms");
        let compat = compat("1.0.0", &dir);
        let descriptor = start_listener(&dir, &compat);
        assert!(descriptor.claims_compat(&compat));
        let mode = std::fs::metadata(descriptor_path(&dir, &compat))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "descriptor must not be group/world readable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_cold_starts_elect_one_daemon() {
        let dir = temp_dir("election");
        let compat = compat("1.0.0", &dir);
        let spawns = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let dir = dir.clone();
            let compat = compat.clone();
            let spawns = Arc::clone(&spawns);
            handles.push(std::thread::spawn(move || {
                let rendezvous = Rendezvous::new(&dir, compat.clone());
                rendezvous
                    .ensure(move || {
                        spawns.fetch_add(1, Ordering::SeqCst);
                        let _ = start_listener(&dir, &compat);
                        Ok(())
                    })
                    .map(|d| d.endpoint)
            }));
        }

        let endpoints: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("every contender reaches a daemon"))
            .collect();
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "only one contender may spawn");
        assert!(
            endpoints.windows(2).all(|w| w[0] == w[1]),
            "all contenders must observe the same daemon"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_descriptor_is_replaced() {
        let dir = temp_dir("stale");
        let compat = compat("1.0.0", &dir);
        // A descriptor pointing at a closed port, as after a daemon crash.
        let stale = DaemonDescriptor::new("127.0.0.1:1", "stale", &compat);
        write_descriptor(&descriptor_path(&dir, &compat), &stale).unwrap();

        let rendezvous = Rendezvous::new(&dir, compat.clone());
        let descriptor = rendezvous
            .ensure(|| {
                let _ = start_listener(&dir, &compat);
                Ok(())
            })
            .unwrap();
        assert_ne!(descriptor.endpoint, "127.0.0.1:1");
        assert!(probe_identity(&descriptor).unwrap().is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_daemon_exits_and_clears_its_descriptor() {
        let dir = temp_dir("idle");
        let compat = compat("1.0.0", &dir);
        let path = descriptor_path(&dir, &compat);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread_path = path.clone();
        let thread_compat = compat.clone();
        std::thread::spawn(move || {
            let _ = serve_identity(
                &thread_path,
                &thread_compat,
                new_token().unwrap(),
                Some(ready_tx),
                Some(Duration::from_millis(150)),
            );
            let _ = done_tx.send(());
        });
        let _ = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(path.exists(), "the descriptor should exist while idle");
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("an idle daemon should exit after its grace period");
        assert!(!path.exists(), "idle exit must clear the descriptor");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresponsive_daemon_is_never_cleared_or_duplicated() {
        let dir = temp_dir("silent");
        let compat = compat("1.0.0", &dir);
        let path = descriptor_path(&dir, &compat);
        let wedged = DaemonDescriptor::new(start_stalled_listener(), "wedged", &compat);
        write_descriptor(&path, &wedged).unwrap();

        let spawns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawns);
        let closure_dir = dir.clone();
        let closure_compat = compat.clone();
        let rendezvous = Rendezvous::new(&dir, compat.clone());
        let started = Instant::now();
        let error = rendezvous
            .ensure(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = start_listener(&closure_dir, &closure_compat);
                Ok(())
            })
            .expect_err("a silent daemon must not be silently replaced");
        assert!(
            error.contains(&wedged.endpoint),
            "the error must name the unresponsive daemon: {error}"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "a silent daemon must not be duplicated"
        );
        assert_eq!(
            read_descriptor(&path),
            Some(wedged),
            "a silent daemon's descriptor must survive untouched"
        );
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the bounded retry must stay bounded: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delayed_live_daemon_is_reused_after_the_old_probe_budget() {
        let dir = temp_dir("delayed");
        let key = compat("1.0.0", &dir);
        let identity = DaemonIdentity {
            compat: key.fingerprint(),
            protocol: PROTOCOL_GENERATION,
            pid: std::process::id(),
            gateway_version: "1.0.0".to_string(),
        };
        let endpoint = start_delayed_responder(Duration::from_secs(8), identity);
        let descriptor = DaemonDescriptor::new(endpoint, "delayed", &key);
        let path = descriptor_path(&dir, &key);
        write_descriptor(&path, &descriptor).unwrap();

        let spawns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawns);
        let reused = Rendezvous::new(&dir, key)
            .ensure(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Err("must not spawn beside a live daemon".to_string())
            })
            .expect("a delayed but live daemon should be reused");
        assert_eq!(reused, descriptor);
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        assert_eq!(read_descriptor(&path), Some(descriptor));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_answer_is_replaced() {
        let dir = temp_dir("garbage");
        let compat = compat("1.0.0", &dir);
        let squatter = DaemonDescriptor::new(start_garbage_responder(), "squatter", &compat);
        write_descriptor(&descriptor_path(&dir, &compat), &squatter).unwrap();

        let spawns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawns);
        let closure_dir = dir.clone();
        let closure_compat = compat.clone();
        let rendezvous = Rendezvous::new(&dir, compat.clone());
        let descriptor = rendezvous
            .ensure(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = start_listener(&closure_dir, &closure_compat);
                Ok(())
            })
            .expect("a squatter's pointer is stale and gets replaced");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_ne!(descriptor.endpoint, squatter.endpoint);
        assert!(probe_identity(&descriptor).unwrap().is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_endpoint_is_replaced() {
        let dir = temp_dir("malformed");
        let compat = compat("1.0.0", &dir);
        let broken = DaemonDescriptor::new("not a valid endpoint", "broken", &compat);
        write_descriptor(&descriptor_path(&dir, &compat), &broken).unwrap();

        let spawns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawns);
        let closure_dir = dir.clone();
        let closure_compat = compat.clone();
        let rendezvous = Rendezvous::new(&dir, compat.clone());
        let descriptor = rendezvous
            .ensure(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = start_listener(&closure_dir, &closure_compat);
                Ok(())
            })
            .expect("an endpoint that cannot parse is garbage, not a live daemon");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_ne!(descriptor.endpoint, broken.endpoint);
        assert!(probe_identity(&descriptor).unwrap().is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolvable_host_is_replaced() {
        let dir = temp_dir("unresolvable");
        let compat = compat("1.0.0", &dir);
        let broken =
            DaemonDescriptor::new("no-such-host.toolport.invalid:9123", "broken", &compat);
        write_descriptor(&descriptor_path(&dir, &compat), &broken).unwrap();

        let spawns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawns);
        let closure_dir = dir.clone();
        let closure_compat = compat.clone();
        let rendezvous = Rendezvous::new(&dir, compat.clone());
        let descriptor = rendezvous
            .ensure(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = start_listener(&closure_dir, &closure_compat);
                Ok(())
            })
            .expect("a host that cannot resolve is garbage, not a live daemon");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_ne!(descriptor.endpoint, broken.endpoint);
        assert!(probe_identity(&descriptor).unwrap().is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exiting_daemon_does_not_clear_a_successors_descriptor() {
        let dir = temp_dir("successor");
        let compat = compat("1.0.0", &dir);
        let path = descriptor_path(&dir, &compat);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread_path = path.clone();
        let thread_compat = compat.clone();
        std::thread::spawn(move || {
            let _ = serve_identity(
                &thread_path,
                &thread_compat,
                new_token().unwrap(),
                Some(ready_tx),
                Some(Duration::from_millis(150)),
            );
            let _ = done_tx.send(());
        });
        let _ = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Another daemon takes over the pointer while this one is serving.
        let successor = DaemonDescriptor::new("127.0.0.1:9", "successor", &compat);
        write_descriptor(&path, &successor).unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the serving daemon exits after its grace");
        assert_eq!(
            read_descriptor(&path),
            Some(successor),
            "an exiting daemon must not clear a successor's pointer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn separate_compat_domains_elect_separate_daemons() {
        let dir = temp_dir("partition");
        let a = compat("1.0.0", &dir);
        let b = compat("2.0.0", &dir);
        let spawns = Arc::new(AtomicUsize::new(0));

        let mut endpoints = Vec::new();
        for key in [&a, &b] {
            let counter = Arc::clone(&spawns);
            let closure_dir = dir.clone();
            let closure_key = key.clone();
            let rendezvous = Rendezvous::new(&dir, key.clone());
            let descriptor = rendezvous
                .ensure(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let _ = start_listener(&closure_dir, &closure_key);
                    Ok(())
                })
                .expect("each domain elects its own daemon");
            assert!(descriptor.claims_compat(key));
            endpoints.push(descriptor.endpoint);
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        assert_ne!(endpoints[0], endpoints[1]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
