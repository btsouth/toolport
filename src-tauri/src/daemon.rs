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
use crate::http_client::{RequestHeaderExt as _, ResponseResultExt as _};

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
/// Per-sample network budget for repeated readiness and liveness checks.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// One-shot installer/control requests cannot rely on a later health sample.
/// Give one response the same total budget used to judge a silent live daemon.
const CONTROL_TIMEOUT: Duration = SILENT_RETRY_TIMEOUT;
/// Descriptor publication and cleanup are short filesystem operations. Keep
/// their lock separate from election, which is deliberately held across spawn
/// and readiness polling.
const DESCRIPTOR_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the fast path keeps re-probing a silent endpoint before giving up
/// on it. A loaded machine can outrun [`PROBE_TIMEOUT`] while its daemon is
/// perfectly live. Twenty concurrent adapters exceeded the old six-second
/// budget on macOS CI; silence alone is never evidence against the pointer.
///
/// It is also the bound after which an adapter treats a daemon as wedged and
/// moves its client to a private gateway. The identity probe is answered
/// outside the request worker pool, so this much silence means the process is
/// stopped or its accept path is stuck, not that it is busy with calls.
pub const SILENT_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(50);
/// Operational idle grace: the daemon exits after this long with no requests.
/// A default, not a user setting, in the first release.
pub const DAEMON_IDLE_GRACE: Duration = Duration::from_secs(300);

/// The idle grace in effect: [`DAEMON_IDLE_GRACE`] unless a test shortens it.
/// A daemon inherits the environment of the adapter that spawned it, so both
/// sides read the same value. Never zero: the watchdog polls at a fraction of it.
pub fn idle_grace() -> Duration {
    crate::brand::env_var(
        "TOOLPORT_DAEMON_IDLE_GRACE_MS",
        "CONDUIT_DAEMON_IDLE_GRACE_MS",
    )
    .and_then(|value| value.trim().parse::<u64>().ok())
    .map(|millis| Duration::from_millis(millis.max(1)))
    .unwrap_or(DAEMON_IDLE_GRACE)
}

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
    /// Advertised PID, replaced in memory by the authenticated identity's PID.
    /// Existence never proves a live daemon; definitive death is negative evidence
    /// only for an exact previously authenticated descriptor after connect timeout.
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
    #[serde(default)]
    pub telemetry: Option<crate::telemetry::Health>,
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

/// Remove descriptors, from any build, whose daemon process is gone, so dead
/// daemons do not accumulate pointers in the data directory. A descriptor whose
/// pid is in use stays: the rendezvous probe, not a pid, decides whether it is
/// live. Lock files stay too. They are advisory locks the OS releases when
/// their holder exits and carry no owner, and deleting one another process has
/// open would break the mutual exclusion it provides. Returns how many went.
pub fn clear_dead_descriptors(data_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return 0;
    };
    let mut cleared = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_descriptor = path.extension().is_some_and(|ext| ext == "json")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("daemon-"));
        if !is_descriptor {
            continue;
        }
        let Some(seen) = read_descriptor(&path) else {
            continue;
        };
        if process_exists(seen.pid) {
            continue;
        }
        let Ok(_descriptor_lock) = registry::lock_at_for(&path, DESCRIPTOR_LOCK_TIMEOUT) else {
            continue;
        };
        if read_descriptor(&path).as_ref() == Some(&seen) {
            clear_descriptor_locked(&path);
            cleared += 1;
        }
    }
    cleared
}

/// Whether any process has `pid`. Where that cannot be asked, assume one does,
/// so nothing is removed.
#[cfg(unix)]
pub(crate) fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: signal 0 only checks that the pid exists and may be signalled.
    let signalled = unsafe { libc::kill(pid, 0) } == 0;
    signalled || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(crate) fn process_exists(pid: u32) -> bool {
    crate::gateway_publish::pid_is_running(pid)
}

/// A fresh bearer for the internal endpoint.
pub fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("Could not generate a token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Authenticated `GET /host/identity`. Verifies nothing about compatibility
/// itself; the caller compares the returned identity against its own
/// [`CompatKey`]. This one-shot control request uses the full silence budget;
/// repeated readiness and health samples keep their short per-probe budget.
/// Any transport error is a failed probe, not a hard error.
pub fn probe_identity(descriptor: &DaemonDescriptor) -> Result<DaemonIdentity, String> {
    identity_probe_with_timeout(descriptor, CONTROL_TIMEOUT).map_err(|failure| match failure {
        ProbeFailure::Answered(detail) => detail,
        ProbeFailure::Unreachable => "the daemon endpoint is not reachable".to_string(),
        ProbeFailure::Silent => "the daemon did not answer within the probe budget".to_string(),
    })
}

/// What one authenticated identity probe says about a daemon an adapter is
/// already using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// It answered the handshake.
    Answering,
    /// It accepted the connection, or answered busy, but gave no identity
    /// within the probe budget.
    Silent,
    /// Nothing usable is there any more.
    Gone,
}

/// One bounded liveness check against a daemon this process already trusts.
pub fn probe_health(descriptor: &DaemonDescriptor) -> Health {
    match identity_probe_with_deadlines(descriptor, PROBE_TIMEOUT, true, Some(descriptor)) {
        Ok(_) => Health::Answering,
        Err(ProbeFailure::Silent) => Health::Silent,
        Err(ProbeFailure::Answered(_) | ProbeFailure::Unreachable) => Health::Gone,
    }
}

/// One-line health notes `toolport_status` shows for this process, such as a
/// client moved to a private gateway because the shared daemon stopped answering.
static STATUS_NOTES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Record a note for `toolport_status`. Repeats are kept once.
pub fn add_status_note(note: impl Into<String>) {
    let note = note.into();
    let mut notes = STATUS_NOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !notes.contains(&note) {
        notes.push(note);
    }
}

pub fn status_notes() -> Vec<String> {
    STATUS_NOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
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
    crate::http_client::agent()
        .post(&format!(
            "http://{}{}",
            descriptor.endpoint, SHUTDOWN_IF_IDLE_PATH
        ))
        .config()
        .timeout_global(Some(CONTROL_TIMEOUT))
        .build()
        .set_header("Authorization", &format!("Bearer {}", descriptor.token))
        .send_empty()
        .retain_status_body()
        .map(|_| ())
        .map_err(|error| format!("could not request idle daemon shutdown: {error}"))
}

/// Stale or malformed descriptors are data, not live-process evidence.
fn purge_descriptors(data_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(data_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            (name.starts_with("daemon-") && name.ends_with(".json")).then(|| entry.path())
        })
        .collect()
}

fn purge_descriptor_process(
    pid: u32,
    processes: &[crate::gateway_publish::GatewayProcess],
    owned: impl FnOnce(u32) -> bool,
) -> Result<bool, String> {
    let Some(process) = processes.iter().find(|process| process.pid == pid) else {
        // Process inventory includes known gateway images even when query access is denied.
        return Ok(false);
    };
    match process.is_host_daemon {
        Some(false) => return Ok(false),
        None => {
            return Err("Could not verify this gateway's daemon role; close it and retry".into())
        }
        Some(true) => {}
    }
    if !owned(pid) {
        return Err("Could not verify this daemon belongs to your user and session".into());
    }
    Ok(true)
}

fn verified_purge_descriptor(path: &Path) -> Result<Option<DaemonDescriptor>, String> {
    let Some(descriptor) = read_descriptor(path) else {
        return Ok(None);
    };
    // PID liveness alone is unsafe: it may now be a completely unrelated process.
    let processes = crate::gateway_publish::list_gateway_processes();
    if !purge_descriptor_process(
        descriptor.pid,
        &processes,
        crate::gateway_publish::purge_process_owned,
    )? {
        return Ok(None);
    }
    match attempt_identity_probe(&descriptor) {
        Ok(identity)
            if identity.pid == descriptor.pid
                && identity.compat == descriptor.compat
                && identity.protocol == descriptor.protocol =>
        {
            Ok(Some(descriptor))
        }
        Ok(_) => {
            Err("Live daemon identity does not match its descriptor; close it and retry".into())
        }
        Err(ProbeFailure::Answered(_) | ProbeFailure::Unreachable) => {
            Err("Live daemon could not prove its authenticated identity; close it and retry".into())
        }
        Err(ProbeFailure::Silent) => {
            Err("Daemon did not answer its authenticated identity probe; close it and retry".into())
        }
    }
}

fn probe_purge_idle(descriptor: &DaemonDescriptor) -> Result<(), String> {
    let topology: serde_json::Value = crate::http_client::agent()
        .get(&format!("http://{}{}", descriptor.endpoint, TOPOLOGY_PATH))
        .config()
        .timeout_global(Some(CONTROL_TIMEOUT))
        .build()
        .set_header("Authorization", &format!("Bearer {}", descriptor.token))
        .call()
        .map_err(|error| error.to_string())?
        .body_mut()
        .read_json()
        .map_err(|error| error.to_string())?;
    if topology["pid"].as_u64() != Some(descriptor.pid as u64)
        || topology["compat"].as_str() != Some(descriptor.compat.as_str())
    {
        return Err("Daemon topology does not match its descriptor".into());
    }
    if topology["purgeIdle"].as_bool() != Some(true) {
        return Err("Daemon has active requests, listeners or a service lease, or cannot prove it is idle. Close sessions and retry using the current gateway.".into());
    }
    Ok(())
}

/// Read-only preflight: do not disconnect clients or request shutdown yet.
pub(crate) fn purge_idle_blockers(data_dir: &Path) -> Vec<crate::purge::Leftover> {
    let mut leftovers = Vec::new();
    for path in purge_descriptors(data_dir) {
        let result = verified_purge_descriptor(&path).and_then(|descriptor| {
            descriptor.map_or(Ok(()), |descriptor| probe_purge_idle(&descriptor))
        });
        if let Err(error) = result {
            leftovers.push(crate::purge::Leftover {
                path: path.display().to_string(),
                error,
            });
        }
    }
    leftovers
}

/// Stop only authenticated, idle daemons advertised by this data directory.
pub(crate) fn stop_for_purge(data_dir: &Path) -> Vec<crate::purge::Leftover> {
    let mut leftovers = Vec::new();
    for path in purge_descriptors(data_dir) {
        let result = (|| {
            let Some(descriptor) = verified_purge_descriptor(&path)? else {
                return Ok(());
            };
            probe_purge_idle(&descriptor)?;
            request_shutdown_if_idle(&descriptor)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while process_exists(descriptor.pid) {
                // Recheck image identity too: never wait forever on a reused PID.
                if !purge_descriptor_process(
                    descriptor.pid,
                    &crate::gateway_publish::list_gateway_processes(),
                    crate::gateway_publish::purge_process_owned,
                )? {
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!("Daemon PID {} remains busy or could not exit. Close active sessions and retry.", descriptor.pid));
                }
                std::thread::park_timeout(Duration::from_millis(100));
            }
            Ok(())
        })();
        if let Err(error) = result {
            leftovers.push(crate::purge::Leftover {
                path: path.display().to_string(),
                error,
            });
        }
    }
    leftovers
}

/// Why an identity probe failed, in the terms the rendezvous decides on: what
/// may be cleared, and what may spawn.
#[derive(Debug)]
pub(crate) enum ProbeFailure {
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
/// the port; silence alone says nothing either way. A busy answer (503 or 429)
/// is a live daemon shedding load, so it counts as silence: clearing its
/// pointer would elect a second daemon beside it.
fn classify_probe_error(error: crate::http_client::Error) -> ProbeFailure {
    if let crate::http_client::Error::Transport(transport) = &error {
        if let Some(connect) = crate::http_client::connect_error(transport) {
            return match connect {
                ureq::Error::Timeout(_) => ProbeFailure::Silent,
                ureq::Error::Io(io)
                    if matches!(
                        io.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    ProbeFailure::Silent
                }
                _ => ProbeFailure::Unreachable,
            };
        }
    }
    match &error {
        crate::http_client::Error::Status(429 | 503, _) => ProbeFailure::Silent,
        crate::http_client::Error::Status(..) => ProbeFailure::Answered(error.to_string()),
        crate::http_client::Error::Transport(transport) => match transport {
            ureq::Error::Timeout(_) => ProbeFailure::Silent,
            ureq::Error::ConnectionFailed | ureq::Error::BadUri(_) | ureq::Error::HostNotFound => {
                ProbeFailure::Unreachable
            }
            ureq::Error::Protocol(_)
            | ureq::Error::LargeResponseHeader(..)
            | ureq::Error::Http(_) => ProbeFailure::Answered(error.to_string()),
            ureq::Error::Io(io) => match io.kind() {
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe => ProbeFailure::Unreachable,
                _ => ProbeFailure::Silent,
            },
            _ => ProbeFailure::Silent,
        },
    }
}

/// One authenticated attempt against `GET /host/identity`, keeping the
/// failure kind so the rendezvous can decide on it.
pub(crate) fn attempt_identity_probe(
    descriptor: &DaemonDescriptor,
) -> Result<DaemonIdentity, ProbeFailure> {
    identity_probe_with_timeout(descriptor, PROBE_TIMEOUT)
}

pub(crate) fn identity_probe_with_timeout(
    descriptor: &DaemonDescriptor,
    timeout: Duration,
) -> Result<DaemonIdentity, ProbeFailure> {
    identity_probe_with_deadlines(descriptor, timeout, false, None)
}

pub(crate) fn identity_probe_for_recovery(
    descriptor: &DaemonDescriptor,
    timeout: Duration,
) -> Result<DaemonIdentity, ProbeFailure> {
    identity_probe_with_deadlines(descriptor, timeout, true, None)
}

fn classify_identity_error(
    descriptor: &DaemonDescriptor,
    known: Option<&DaemonDescriptor>,
    error: crate::http_client::Error,
) -> ProbeFailure {
    let connect_timeout = match &error {
        crate::http_client::Error::Transport(transport) => {
            if let Some(connect) = crate::http_client::connect_error(transport) {
                match connect {
                    ureq::Error::Timeout(_) => true,
                    ureq::Error::Io(error) => matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ),
                    _ => false,
                }
            } else {
                matches!(transport, ureq::Error::Timeout(ureq::Timeout::Connect))
            }
        }
        _ => false,
    };
    // A disk PID is not trusted. Only negative evidence for this exact, previously
    // authenticated process can turn an unestablished TCP connection into loss.
    if connect_timeout && known == Some(descriptor) && !process_exists(descriptor.pid) {
        return ProbeFailure::Unreachable;
    }
    classify_probe_error(error)
}

fn identity_probe_with_deadlines(
    descriptor: &DaemonDescriptor,
    timeout: Duration,
    recovery: bool,
    known: Option<&DaemonDescriptor>,
) -> Result<DaemonIdentity, ProbeFailure> {
    let url = format!("http://{}{}", descriptor.endpoint, IDENTITY_PATH);
    let request = crate::http_client::agent()
        .get(&url)
        .set_header("Authorization", &format!("Bearer {}", descriptor.token))
        .config()
        .timeout_global(Some(timeout));
    let request = if recovery {
        request.timeout_connect(Some(timeout))
    } else {
        request
    };
    let response = request
        .build()
        .call()
        .retain_status_body()
        .map_err(|error| classify_identity_error(descriptor, known, error))?;
    response
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json::<DaemonIdentity>()
        .map_err(|e| ProbeFailure::Answered(format!("Daemon identity was not valid JSON: {e}")))
}

/// Why [`Rendezvous::ensure`] could not hand back a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureError {
    /// A daemon owns the pointer but stayed silent for [`SILENT_RETRY_TIMEOUT`].
    /// Its descriptor was left in place and nothing was spawned beside it.
    Unresponsive(DaemonDescriptor),
    /// Anything else: the election, the spawn, or readiness failed.
    Failed(String),
}

impl std::fmt::Display for EnsureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresponsive(descriptor) => f.write_str(&unresponsive_daemon_error(descriptor)),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

impl From<String> for EnsureError {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
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

/// Cancellation shared by recovery stages, with an optional per-stage deadline.
pub(crate) struct RecoveryBudget<'a> {
    deadline: Option<Instant>,
    cancelled: &'a std::sync::atomic::AtomicBool,
}

impl<'a> RecoveryBudget<'a> {
    pub(crate) fn new(timeout: Duration, cancelled: &'a std::sync::atomic::AtomicBool) -> Self {
        Self {
            deadline: Some(Instant::now() + timeout),
            cancelled,
        }
    }

    pub(crate) fn cancellable(cancelled: &'a std::sync::atomic::AtomicBool) -> Self {
        Self {
            deadline: None,
            cancelled,
        }
    }

    fn limited(&self, timeout: Duration) -> Self {
        let deadline = Instant::now() + timeout;
        Self {
            deadline: Some(self.deadline.map_or(deadline, |outer| outer.min(deadline))),
            cancelled: self.cancelled,
        }
    }

    pub(crate) fn remaining(&self) -> Result<Duration, String> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the adapter is closing".into());
        }
        let Some(deadline) = self.deadline else {
            return Ok(Duration::MAX);
        };
        deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "the host daemon recovery deadline expired".into())
    }

    fn lock(&self, path: &Path) -> Result<registry::FileLock, String> {
        loop {
            match registry::lock_at_for(path, self.remaining()?.min(READY_POLL)) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.starts_with("The registry is locked") => {}
                Err(error) => return Err(error),
            }
        }
    }
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
        spawn: impl FnMut() -> Result<(), String>,
    ) -> Result<DaemonDescriptor, EnsureError> {
        self.ensure_with_budget(spawn, None)
    }

    pub(crate) fn ensure_with_budget(
        &self,
        spawn: impl FnMut() -> Result<(), String>,
        budget: Option<&RecoveryBudget<'_>>,
    ) -> Result<DaemonDescriptor, EnsureError> {
        self.ensure_known_with_budget(spawn, budget, None)
    }

    pub(crate) fn ensure_known_with_budget(
        &self,
        mut spawn: impl FnMut() -> Result<(), String>,
        budget: Option<&RecoveryBudget<'_>>,
        known: Option<&DaemonDescriptor>,
    ) -> Result<DaemonDescriptor, EnsureError> {
        let path = self.descriptor_path();
        // A descriptor may appear while we wait for the election lock. If it
        // then stays silent, retry outside the lock so other adapters are not
        // serialized behind one slow identity endpoint. A disappearing pointer
        // gets one more election attempt, bounded against repeated churn.
        for _ in 0..2 {
            let observed_before_lock =
                read_descriptor(&path).filter(|descriptor| descriptor.claims_compat(&self.compat));
            match self.probe_for_reuse_with_budget(&path, budget, known)? {
                Probe::Live(descriptor) => return Ok(descriptor),
                // A silent daemon may still be alive: reuse is unproven, and
                // clearing or spawning beside it is the double-election defect.
                Probe::Silent(descriptor) => return Err(EnsureError::Unresponsive(descriptor)),
                Probe::Gone => {}
            }

            // Elect: `lock_at` appends `.lock` and creates parent directories.
            let lock_base = election_lock_base(&self.data_dir, &self.compat);
            let election = if let Some(budget) = budget {
                budget.limited(ELECTION_TIMEOUT).lock(&lock_base)?
            } else {
                registry::lock_at_for(&lock_base, ELECTION_TIMEOUT)?
            };

            // If another contender published while we waited, leave the lock
            // before probing it. A slow identity response must not make the
            // remaining contenders time out on election lock acquisition.
            let current =
                read_descriptor(&path).filter(|descriptor| descriptor.claims_compat(&self.compat));
            if current.is_some() && current != observed_before_lock {
                drop(election);
                match self.probe_for_reuse_with_budget(&path, budget, known)? {
                    Probe::Live(descriptor) => return Ok(descriptor),
                    Probe::Silent(descriptor) => {
                        return Err(EnsureError::Unresponsive(descriptor));
                    }
                    Probe::Gone => continue,
                }
            }

            // Recheck under the lock: another contender may have started the
            // daemon while we waited. Silence needs the same patience as the
            // fast path, but must not hold the election lock during that wait.
            match self.probe_with_budget(&path, budget, known)? {
                Probe::Live(descriptor) => return Ok(descriptor),
                Probe::Silent(_) => {
                    drop(election);
                    match self.probe_for_reuse_with_budget(&path, budget, known)? {
                        Probe::Live(descriptor) => return Ok(descriptor),
                        Probe::Silent(descriptor) => {
                            return Err(EnsureError::Unresponsive(descriptor));
                        }
                        Probe::Gone => continue,
                    }
                }
                Probe::Gone => {}
            }

            // We are the winner. Drop any stale pointer first so readiness
            // polling cannot mistake it for the daemon we are about to start.
            if let Some(budget) = budget {
                budget.remaining()?;
            }
            if let Some(budget) = budget {
                let _descriptor_lock = budget.limited(DESCRIPTOR_LOCK_TIMEOUT).lock(&path)?;
                budget.remaining()?;
                clear_descriptor_locked(&path);
            } else {
                clear_descriptor(&path);
            }
            if let Some(budget) = budget {
                budget.remaining()?;
            }
            spawn()?;
            // Once spawned, retain the existing readiness window under election.
            // EOF must not release it early and let another adapter double-spawn.
            let not_cancelled = std::sync::atomic::AtomicBool::new(false);
            let startup_budget = budget.map(|_| RecoveryBudget::new(READY_TIMEOUT, &not_cancelled));

            // Wait for readiness while still holding the lock, so no second
            // contender spawns a daemon of its own. Until the daemon is ready,
            // silence and refusals both just mean "not answering yet", so every
            // non-live outcome keeps polling inside the budget.
            let deadline = Instant::now() + READY_TIMEOUT;
            while Instant::now() < deadline {
                if let Probe::Live(descriptor) =
                    self.probe_with_budget(&path, startup_budget.as_ref(), known)?
                {
                    return Ok(descriptor);
                }
                std::thread::sleep(READY_POLL);
            }
            return Err("the daemon did not become ready before the deadline"
                .to_string()
                .into());
        }
        Err("the daemon descriptor changed repeatedly during startup"
            .to_string()
            .into())
    }

    /// Read, claim-check, then prove with the authenticated handshake. A
    /// missing, mismatched, answered-wrong, or unreachable descriptor is "no
    /// live daemon"; a silent endpoint is not ([`Probe::Silent`]). One refusal
    /// is thin evidence for destroying a pointer, so the unreachable case
    /// rechecks once before declaring it stale.
    fn probe_with_budget(
        &self,
        path: &Path,
        budget: Option<&RecoveryBudget<'_>>,
        known: Option<&DaemonDescriptor>,
    ) -> Result<Probe, String> {
        let attempt = |descriptor: &DaemonDescriptor| {
            let timeout = match budget {
                Some(budget) => budget.remaining()?.min(PROBE_TIMEOUT),
                None => PROBE_TIMEOUT,
            };
            Ok::<_, String>(identity_probe_with_deadlines(
                descriptor,
                timeout,
                budget.is_some(),
                known,
            ))
        };
        let Some(mut descriptor) = read_descriptor(path) else {
            return Ok(Probe::Gone);
        };
        if !descriptor.claims_compat(&self.compat) {
            return Ok(Probe::Gone);
        }
        Ok(match attempt(&descriptor)? {
            Ok(identity) if identity.is_compatible_with(&self.compat) => {
                descriptor.pid = identity.pid;
                Probe::Live(descriptor)
            }
            Ok(_) | Err(ProbeFailure::Answered(_)) => Probe::Gone,
            Err(ProbeFailure::Unreachable) => match attempt(&descriptor)? {
                Ok(identity) if identity.is_compatible_with(&self.compat) => {
                    descriptor.pid = identity.pid;
                    Probe::Live(descriptor)
                }
                Err(ProbeFailure::Silent) => Probe::Silent(descriptor),
                _ => Probe::Gone,
            },
            Err(ProbeFailure::Silent) => Probe::Silent(descriptor),
        })
    }

    /// The fast-path probe with bounded patience: give a live-but-slow daemon
    /// the whole [`SILENT_RETRY_TIMEOUT`] to answer before concluding
    /// anything about its pointer.
    fn probe_for_reuse_with_budget(
        &self,
        path: &Path,
        budget: Option<&RecoveryBudget<'_>>,
        known: Option<&DaemonDescriptor>,
    ) -> Result<Probe, String> {
        let deadline = Instant::now() + SILENT_RETRY_TIMEOUT;
        loop {
            match self.probe_with_budget(path, budget, known)? {
                Probe::Silent(_) if Instant::now() < deadline => {}
                outcome => return Ok(outcome),
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
        telemetry: Some(crate::telemetry::health()),
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
                    let body = serde_json::to_string(&DaemonIdentity {
                        telemetry: Some(crate::telemetry::health()),
                        ..identity.clone()
                    })
                    .unwrap_or_else(|_| "{}".to_string());
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
    if let Ok(_descriptor_lock) = registry::lock_at_for(descriptor_path, DESCRIPTOR_LOCK_TIMEOUT) {
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

    fn spawn_exiting_child() -> std::process::Child {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd.exe")
                .args(["/C", "exit", "/B", "0"])
                .spawn()
                .unwrap()
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .unwrap()
        }
    }

    #[test]
    fn only_a_known_dead_process_turns_connect_timeout_into_loss() {
        let mut child = spawn_exiting_child();
        child.wait().unwrap();
        assert!(!process_exists(child.id()));
        let key = CompatKey::new("test", "known-dead-process");
        let mut descriptor = DaemonDescriptor::new("127.0.0.1:9", "known", &key);
        descriptor.pid = child.id();
        let connect_timeout =
            || crate::http_client::Error::Transport(ureq::Error::Timeout(ureq::Timeout::Connect));
        assert!(matches!(
            classify_identity_error(&descriptor, Some(&descriptor), connect_timeout()),
            ProbeFailure::Unreachable
        ));
        for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
            let raw =
                crate::http_client::Error::Transport(ureq::Error::Io(std::io::Error::from(kind)));
            assert!(matches!(
                classify_identity_error(&descriptor, Some(&descriptor), raw),
                ProbeFailure::Silent
            ));
            let wrapped =
                crate::http_client::Error::Transport(crate::http_client::mark_connect_failure(
                    ureq::Error::Io(std::io::Error::from(kind)),
                ));
            assert!(matches!(
                classify_identity_error(&descriptor, Some(&descriptor), wrapped),
                ProbeFailure::Unreachable
            ));
        }
        assert!(matches!(
            classify_identity_error(&descriptor, None, connect_timeout()),
            ProbeFailure::Silent
        ));
        let mut other = descriptor.clone();
        other.token = "different-daemon".into();
        assert!(matches!(
            classify_identity_error(&descriptor, Some(&other), connect_timeout()),
            ProbeFailure::Silent
        ));
        let read_timeout =
            crate::http_client::Error::Transport(ureq::Error::Timeout(ureq::Timeout::RecvBody));
        assert!(matches!(
            classify_identity_error(&descriptor, Some(&descriptor), read_timeout),
            ProbeFailure::Silent
        ));
        for status in ["503 Service Unavailable", "429 Too Many Requests"] {
            descriptor.endpoint = start_status_responder(status);
            assert_eq!(probe_health(&descriptor), Health::Silent);
        }
        descriptor.pid = std::process::id();
        assert!(matches!(
            classify_identity_error(&descriptor, Some(&descriptor), connect_timeout()),
            ProbeFailure::Silent
        ));
    }

    #[test]
    fn successful_identity_corrects_only_the_in_memory_pid() {
        let env = crate::registry::DataDirTestEnv::new("authenticated-pid");
        let key = compat("test", &env.dir);
        let authenticated = start_listener(&env.dir, &key);
        let mut child = spawn_exiting_child();
        child.wait().unwrap();
        assert!(!process_exists(child.id()));
        let mut disk = authenticated.clone();
        disk.pid = child.id();
        let path = descriptor_path(&env.dir, &key);
        write_descriptor(&path, &disk).unwrap();
        let recovered = Rendezvous::new(&env.dir, key)
            .ensure(|| panic!("authenticated live daemon must be reused"))
            .unwrap();
        assert_eq!(recovered, authenticated);
        assert_eq!(read_descriptor(&path), Some(disk.clone()));
        assert_eq!(probe_health(&recovered), Health::Answering);
        let timeout =
            crate::http_client::Error::Transport(ureq::Error::Timeout(ureq::Timeout::Connect));
        assert!(matches!(
            classify_identity_error(&disk, Some(&recovered), timeout),
            ProbeFailure::Silent
        ));
    }

    #[test]
    fn recovery_budget_bounds_election_and_stops_before_startup() {
        let env = crate::registry::DataDirTestEnv::new("recovery-budget-election");
        let key = compat("test", &env.dir);
        let rendezvous = Rendezvous::new(&env.dir, key.clone());
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let election =
            registry::lock_at_for(&election_lock_base(&env.dir, &key), Duration::from_secs(1))
                .unwrap();
        let budget = RecoveryBudget::new(Duration::from_millis(80), &cancelled);
        let started = Instant::now();
        let result = rendezvous.ensure_with_budget(|| panic!("must not spawn"), Some(&budget));
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(election);
        cancelled.store(true, Ordering::SeqCst);
        let budget = RecoveryBudget::new(Duration::from_secs(10), &cancelled);
        assert!(rendezvous
            .ensure_with_budget(|| panic!("must not spawn after EOF"), Some(&budget))
            .is_err());
    }

    #[test]
    fn cancellable_discovery_preserves_each_lock_limit_and_cancellation() {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let control = RecoveryBudget::cancellable(&cancelled);
        let before = Instant::now();
        let election = control.limited(ELECTION_TIMEOUT);
        let descriptor = control.limited(DESCRIPTOR_LOCK_TIMEOUT);
        let after = Instant::now();
        assert!(election.deadline.unwrap() >= before + ELECTION_TIMEOUT);
        assert!(election.deadline.unwrap() <= after + ELECTION_TIMEOUT);
        assert!(descriptor.deadline.unwrap() >= before + DESCRIPTOR_LOCK_TIMEOUT);
        assert!(descriptor.deadline.unwrap() <= after + DESCRIPTOR_LOCK_TIMEOUT);
        let short = RecoveryBudget::new(Duration::from_millis(80), &cancelled);
        assert_eq!(short.limited(ELECTION_TIMEOUT).deadline, short.deadline);
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(election.remaining().unwrap_err(), "the adapter is closing");
        assert_eq!(
            descriptor.remaining().unwrap_err(),
            "the adapter is closing"
        );
    }

    #[test]
    fn cancellable_discovery_reaches_the_existing_silence_fallback() {
        let env = crate::registry::DataDirTestEnv::new("recovery-silence-window");
        let key = compat("test", &env.dir);
        let descriptor = DaemonDescriptor::new(start_stalled_listener(), "silent", &key);
        write_descriptor(&descriptor_path(&env.dir, &key), &descriptor).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let control = RecoveryBudget::cancellable(&cancelled);
        let started = Instant::now();
        let result = Rendezvous::new(&env.dir, key).ensure_with_budget(
            || panic!("must not spawn beside a silent daemon"),
            Some(&control),
        );
        assert_eq!(result.unwrap_err(), EnsureError::Unresponsive(descriptor));
        assert!(started.elapsed() >= SILENT_RETRY_TIMEOUT);
    }

    #[test]
    fn recovery_budget_includes_a_silent_identity_probe() {
        let env = crate::registry::DataDirTestEnv::new("recovery-budget-probe");
        let key = compat("test", &env.dir);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let descriptor =
            DaemonDescriptor::new(listener.local_addr().unwrap().to_string(), "silent", &key);
        write_descriptor(&descriptor_path(&env.dir, &key), &descriptor).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let budget = RecoveryBudget::new(Duration::from_millis(80), &cancelled);
        let started = Instant::now();
        let result = Rendezvous::new(&env.dir, key).ensure_with_budget(
            || panic!("must not spawn beside a silent daemon"),
            Some(&budget),
        );
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn cancelled_startup_retains_election_until_its_daemon_is_ready() {
        let env = crate::registry::DataDirTestEnv::new("recovery-startup-election");
        let key = compat("test", &env.dir);
        let dir = env.dir.clone();
        let worker_key = key.clone();
        let spawns = Arc::new(AtomicUsize::new(0));
        let first_spawns = Arc::clone(&spawns);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            let cancelled = std::sync::atomic::AtomicBool::new(false);
            let budget = RecoveryBudget::new(Duration::from_millis(80), &cancelled);
            Rendezvous::new(&dir, worker_key.clone()).ensure_with_budget(
                || {
                    first_spawns.fetch_add(1, Ordering::SeqCst);
                    let delayed_dir = dir.clone();
                    let delayed_key = worker_key.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(200));
                        start_listener(&delayed_dir, &delayed_key);
                    });
                    cancelled.store(true, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    Ok(())
                },
                Some(&budget),
            )
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = Rendezvous::new(&env.dir, key.clone())
            .ensure(|| {
                spawns.fetch_add(1, Ordering::SeqCst);
                start_listener(&env.dir, &key);
                Ok(())
            })
            .unwrap();
        assert_eq!(first.join().unwrap().unwrap(), second);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
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

    /// A listener that answers every request with `status`, as a saturated
    /// daemon used to answer its own identity probe.
    fn start_status_responder(status: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
            }
        });
        endpoint
    }

    #[test]
    fn a_busy_answer_is_silence_not_a_stale_pointer() {
        let key = CompatKey::new("test", "scratch");
        for status in ["503 Service Unavailable", "429 Too Many Requests"] {
            let descriptor = DaemonDescriptor::new(start_status_responder(status), "busy", &key);
            assert!(
                matches!(
                    attempt_identity_probe(&descriptor),
                    Err(ProbeFailure::Silent)
                ),
                "{status} must not let the rendezvous clear a live daemon's pointer"
            );
            assert_eq!(probe_health(&descriptor), Health::Silent);
        }
        let refused = DaemonDescriptor::new(start_status_responder("401 Unauthorized"), "x", &key);
        assert!(matches!(
            attempt_identity_probe(&refused),
            Err(ProbeFailure::Answered(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dead_daemons_descriptors_are_cleared_and_live_ones_kept() {
        let dir = temp_dir("dead-descriptors");
        let live = DaemonDescriptor::new("127.0.0.1:9", "live", &compat("1.0.0", &dir));
        let mut dead = DaemonDescriptor::new("127.0.0.1:9", "dead", &compat("0.9.0", &dir));
        // Far above any real pid_max, so no process can have it.
        dead.pid = 999_999_999;
        let live_path = descriptor_path(&dir, &compat("1.0.0", &dir));
        let dead_path = descriptor_path(&dir, &compat("0.9.0", &dir));
        write_descriptor(&live_path, &live).unwrap();
        write_descriptor(&dead_path, &dead).unwrap();
        assert_eq!(clear_dead_descriptors(&dir), 1);
        assert!(!dead_path.exists());
        assert_eq!(read_descriptor(&live_path), Some(live));
        let _ = std::fs::remove_dir_all(&dir);
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
    fn one_shot_identity_probe_survives_a_delayed_control_response() {
        delayed_control_response(false);
    }

    #[test]
    fn one_shot_idle_shutdown_survives_a_delayed_control_response() {
        delayed_control_response(true);
    }

    fn delayed_control_response(shutdown: bool) {
        use std::io::{Read as _, Write as _};
        use std::sync::mpsc;
        let dir = temp_dir("probe-delayed-control");
        let key = compat("1.0.0", &dir);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let descriptor =
            DaemonDescriptor::new(listener.local_addr().unwrap().to_string(), "x", &key);
        let identity = DaemonIdentity {
            compat: key.fingerprint(),
            protocol: PROTOCOL_GENERATION,
            pid: std::process::id(),
            gateway_version: "test".into(),
            telemetry: None,
        };
        let expected = (!shutdown).then(|| identity.clone());
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(READY_TIMEOUT)).unwrap();
            let mut scratch = [0; 4096];
            // This fixture needs the request arrival, not the complete request body.
            let _received = stream.read(&mut scratch).unwrap();
            accepted_tx.send(()).unwrap();
            // Hold the real response until the caller has outlived a health
            // sample. Channels control the order; no retry or startup race.
            release_rx.recv_timeout(READY_TIMEOUT).unwrap();
            let body = serde_json::to_string(&identity).unwrap();
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        let (result_tx, result_rx) = mpsc::channel();
        let probe = std::thread::spawn(move || {
            let result = if shutdown {
                request_shutdown_if_idle(&descriptor).map(|()| None)
            } else {
                probe_identity(&descriptor).map(Some)
            };
            result_tx.send(result).unwrap();
        });
        accepted_rx.recv_timeout(READY_TIMEOUT).unwrap();
        let early = result_rx.recv_timeout(PROBE_TIMEOUT + Duration::from_millis(250));
        release_tx.send(()).unwrap();
        server.join().unwrap();
        probe.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "control probe used the health sample budget: {early:?}"
        );
        assert_eq!(
            result_rx.recv_timeout(READY_TIMEOUT).unwrap().unwrap(),
            expected
        );
        let _ = std::fs::remove_dir_all(dir);
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
        for wrapped in [false, true] {
            for error in [
                ureq::Error::Timeout(ureq::Timeout::Connect),
                ureq::Error::Timeout(ureq::Timeout::Global),
                ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
                ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            ] {
                let error = if wrapped {
                    crate::http_client::mark_connect_failure(error)
                } else {
                    error
                };
                assert!(matches!(
                    classify_probe_error(crate::http_client::Error::Transport(error)),
                    ProbeFailure::Silent
                ));
            }
            for kind in [
                std::io::ErrorKind::ConnectionRefused,
                std::io::ErrorKind::ConnectionReset,
            ] {
                let error = ureq::Error::Io(std::io::Error::from(kind));
                let error = if wrapped {
                    crate::http_client::mark_connect_failure(error)
                } else {
                    error
                };
                assert!(matches!(
                    classify_probe_error(crate::http_client::Error::Transport(error)),
                    ProbeFailure::Unreachable
                ));
            }
        }
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
        assert_eq!(
            mode & 0o077,
            0,
            "descriptor must not be group/world readable"
        );
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
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "only one contender may spawn"
        );
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
        assert!(probe_identity(&descriptor)
            .unwrap()
            .is_compatible_with(&compat));
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
            .expect_err("a silent daemon must not be silently replaced")
            .to_string();
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
            telemetry: Some(crate::telemetry::Health::default()),
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
    fn daemon_published_while_waiting_for_election_gets_silent_retry() {
        let dir = temp_dir("delayed-election");
        let key = compat("1.0.0", &dir);
        let election =
            registry::lock_at_for(&election_lock_base(&dir, &key), ELECTION_TIMEOUT).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let worker_dir = dir.clone();
        let worker_key = key.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            Rendezvous::new(&worker_dir, worker_key)
                .ensure(|| Err("must not spawn beside a published daemon".to_string()))
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // Let the worker observe an absent descriptor and block on our lock.
        std::thread::sleep(Duration::from_millis(500));

        let identity = DaemonIdentity {
            compat: key.fingerprint(),
            protocol: PROTOCOL_GENERATION,
            pid: std::process::id(),
            gateway_version: "1.0.0".to_string(),
            telemetry: Some(crate::telemetry::Health::default()),
        };
        let endpoint = start_delayed_responder(Duration::from_secs(6), identity);
        let descriptor = DaemonDescriptor::new(endpoint, "delayed", &key);
        let path = descriptor_path(&dir, &key);
        write_descriptor(&path, &descriptor).unwrap();
        drop(election);

        let reused = worker
            .join()
            .unwrap()
            .expect("a daemon published during election should be reused");
        assert_eq!(reused, descriptor);
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
        assert!(probe_identity(&descriptor)
            .unwrap()
            .is_compatible_with(&compat));
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
        assert!(probe_identity(&descriptor)
            .unwrap()
            .is_compatible_with(&compat));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolvable_host_is_replaced() {
        let dir = temp_dir("unresolvable");
        let compat = compat("1.0.0", &dir);
        let broken = DaemonDescriptor::new("no-such-host.toolport.invalid:9123", "broken", &compat);
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
        assert!(probe_identity(&descriptor)
            .unwrap()
            .is_compatible_with(&compat));
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
    #[test]
    fn purge_malformed_descriptor_and_reused_pid_are_stale() {
        let env = crate::registry::DataDirTestEnv::new("purge_stale_descriptors");
        let malformed = env.dir.join("daemon-broken.json");
        std::fs::write(&malformed, "invalid json").unwrap();
        assert!(verified_purge_descriptor(&malformed).unwrap().is_none());
        let reused = env.dir.join("daemon-reused.json");
        std::fs::write(
            &reused,
            serde_json::to_vec(&DaemonDescriptor {
                endpoint: "127.0.0.1:1".into(),
                token: "fixture".into(),
                pid: std::process::id(),
                compat: "fixture".into(),
                protocol: PROTOCOL_GENERATION,
                created_at_ms: 0,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(verified_purge_descriptor(&reused).unwrap().is_none());
        assert!(purge_idle_blockers(&env.dir).is_empty());
        assert!(stop_for_purge(&env.dir).is_empty());
        assert!(malformed.exists() && reused.exists());
    }

    #[test]
    fn purge_unknown_daemon_role_or_owner_blocks_instead_of_becoming_stale() {
        let mut process = crate::gateway_publish::GatewayProcess {
            pid: 42,
            start_time: None,
            is_host_daemon: None,
            path: None,
            basename: "toolport-gateway.exe".into(),
            parent: None,
        };
        assert!(purge_descriptor_process(42, &[process.clone()], |_| false).is_err());
        process.is_host_daemon = Some(true);
        assert!(purge_descriptor_process(42, &[process.clone()], |_| false).is_err());
        assert!(purge_descriptor_process(42, &[process.clone()], |_| true).unwrap());
        process.is_host_daemon = Some(false);
        assert!(
            !purge_descriptor_process(42, &[process], |_| panic!("proven other role")).unwrap()
        );
        assert!(!purge_descriptor_process(42, &[], |_| panic!("no gateway process")).unwrap());
    }
}
