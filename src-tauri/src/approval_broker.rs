//! App-side HITL approval broker.
//!
//! The Toolport app hosts this broker for legacy clients and code-mode calls, whose wire
//! shape cannot return Toolport's approval as a modern multi-round-trip result. Those
//! paths dial OUT and block for the decision. Modern direct calls use MCP elicitation and
//! do not hold a gateway request. This is the counterpart to the gateway's
//! `request_human_decision` (see `bin/toolport-gateway.rs`).
//!
//! Protocol: the gateway connects (loopback TCP everywhere; on Unix a socket file in a
//! private directory is published as well and preferred by gateways that know of it),
//! opens with a challenge the broker must answer with a proof of the shared token
//! ([`crate::approval::dial_broker`]), then sends one JSON line
//! ([`ApprovalRequest`]) carrying that token, and reads one JSON line back
//! ([`ApprovalDecision`]). Arguments travel over the socket and are never written to
//! disk. The only thing on disk is the endpoint descriptor (address + token). The
//! human decides in the app UI; a fail-closed timeout denies. The challenge is what keeps
//! a process that merely binds the published endpoint after the app has gone from
//! answering in its place (SBS-867); the transport choice is defense in depth on top.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fs2::FileExt;
use serde::Serialize;
use subtle::ConstantTimeEq;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter, Manager};

use crate::approval::{
    ApprovalDecision, ApprovalReason, ApprovalRequest, BrokerStream, EndpointDescriptor,
    PiiReleaseRequest, UrlElicitationRequest, DEFAULT_TIMEOUT_SECS, ENDPOINT_FILE,
};

/// A pending approval as the UI sees it. The auth token is deliberately NOT included.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingView {
    pub id: String,
    pub client: Option<String>,
    pub client_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_label: Option<String>,
    pub server: String,
    pub tool: String,
    pub tool_fingerprint: Option<String>,
    pub reason: ApprovalReason,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_elicitation: Option<UrlElicitationRequest>,
    /// Real values this call would release to a server that never produced them, for the
    /// approver to look at. Never persisted and never sent anywhere but this local UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pii_release: Option<PiiReleaseRequest>,
    /// Wall-clock epoch-millis when this call auto-denies (park time + the fail-closed
    /// timeout). The UI counts down to this exactly, instead of approximating from when
    /// it first saw the request. App and broker share one clock, so it's accurate.
    pub deadline_ms: u64,
}

/// A gateway connection parked waiting for a human decision.
struct Waiter {
    view: PendingView,
    decide: Sender<ApprovalDecision>,
}

struct Inner {
    /// The token a gateway must present (matches the published descriptor).
    token: String,
    /// id -> parked connection. Bounded by `MAX_PENDING`.
    pending: Mutex<HashMap<String, Waiter>>,
    /// Ephemeral per-session "always allow" set of fingerprint-bound `server/tool/fingerprint`
    /// keys. A matching call auto-approves without prompting; cleared on app restart (the
    /// persistent list lives in the registry). "Approve for this session" adds here; "Always
    /// allow" adds here AND to the registry.
    session_allow: Mutex<HashSet<String>>,
    /// Held for the broker lifetime so two desktop shells cannot replace each
    /// other's endpoint or Unix socket during a simultaneous startup.
    _owner_lock: Option<std::fs::File>,
}

/// Cap on simultaneously-pending approvals, so a misbehaving client can't grow the
/// queue without bound. Beyond this, new requests are denied immediately.
const MAX_PENDING: usize = 64;
/// Bound unauthenticated request memory before a gateway proves it has the token.
const MAX_APPROVAL_REQUEST_BYTES: usize = 1024 * 1024;
/// Pending approvals occupy workers while awaiting a decision. Keep bounded
/// headroom for authentication, allowlisted calls, and prompt fail-closed denials.
const MAX_CONNECTION_WORKERS: usize = MAX_PENDING + 32;

struct ConnectionPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_acquire_connection(active: &Arc<AtomicUsize>) -> Option<ConnectionPermit> {
    let mut current = active.load(Ordering::Acquire);
    loop {
        if current >= MAX_CONNECTION_WORKERS {
            return None;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                return Some(ConnectionPermit {
                    active: Arc::clone(active),
                })
            }
            Err(observed) => current = observed,
        }
    }
}

/// Read exactly one newline-terminated request without allowing an unauthenticated
/// peer to grow the allocation indefinitely.
fn read_approval_request<R: BufRead>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut limited = Read::take(reader, (MAX_APPROVAL_REQUEST_BYTES + 1) as u64);
    let read = limited.read_until(b'\n', &mut line)?;
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty approval request",
        ));
    }
    if line.len() > MAX_APPROVAL_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "approval request exceeds size limit",
        ));
    }
    if !line.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "approval request is not newline terminated",
        ));
    }
    Ok(line)
}

/// Constant-time equality for the fixed-length broker token. Token length is public.
fn token_eq(actual: &str, expected: &str) -> bool {
    let (actual, expected) = (actual.as_bytes(), expected.as_bytes());
    if actual.len() != expected.len() {
        return false;
    }
    actual.ct_eq(expected).into()
}

/// The wall-clock epoch-millis deadline for a newly parked approval: now plus the
/// fail-closed timeout. Matches the broker's own `recv_timeout` below, so the UI's
/// countdown to it lands on the same moment the call actually auto-denies.
fn deadline_ms_from_now() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    now + DEFAULT_TIMEOUT_SECS * 1000
}

/// Handle to the broker, managed as Tauri state so the approve/deny commands can reach it.
#[derive(Clone)]
pub struct ApprovalBroker {
    inner: Arc<Inner>,
}

#[derive(Clone)]
struct BrokerHost {
    persistent_allowed: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pending: Arc<dyn Fn(&PendingView) + Send + Sync>,
    resolved: Arc<dyn Fn(&str) + Send + Sync>,
}

#[cfg_attr(
    all(feature = "gtk-desktop", not(feature = "desktop")),
    allow(dead_code)
)]
impl ApprovalBroker {
    pub fn owns_endpoint(&self) -> bool {
        self.inner._owner_lock.is_some()
    }

    /// Remove only the endpoint owned by this broker instance.
    pub fn clear_endpoint(&self) {
        if !self.owns_endpoint() {
            return;
        }
        if let Some(dir) = crate::registry::conduit_dir() {
            if std::fs::remove_file(dir.join(ENDPOINT_FILE)).is_ok() {
                log_broker_event("endpoint descriptor cleared on shutdown");
            }
        }
        #[cfg(unix)]
        if let Some(path) = unix_socket_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Snapshot the pending queue for the UI.
    pub fn list(&self) -> Vec<PendingView> {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|w| w.view.clone())
            .collect()
    }

    /// Deliver a human decision for `id`, returning the resolved call's view (so the caller
    /// can apply an "allow this tool" scope from its server/tool). `Err` if the id is unknown
    /// (already resolved or timed out). Sending is best-effort: a parked connection that
    /// already timed out has dropped its receiver, which is harmless.
    pub fn decide(&self, id: &str, approved: bool) -> Result<PendingView, String> {
        let waiter = self
            .inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        match waiter {
            Some(w) => {
                let d = if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                };
                let _ = w.decide.send(d);
                Ok(w.view)
            }
            None => Err("no pending approval with that id (it may have expired)".into()),
        }
    }

    /// Add a `server/tool` key to the ephemeral session allowlist (auto-approve until the
    /// app restarts). Both "approve for session" and "always allow" add here so the
    /// decision takes effect immediately for later matching calls.
    pub fn add_session_allow(&self, key: String) {
        self.inner
            .session_allow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
    }

    /// Remove a key from the session allowlist (used when the user revokes it).
    pub fn remove_session_allow(&self, key: &str) {
        self.inner
            .session_allow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    /// Snapshot the session allowlist for the UI.
    pub fn session_allowed(&self) -> Vec<String> {
        self.inner
            .session_allow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Whether a key is in the ephemeral session allowlist.
    fn session_contains(&self, key: &str) -> bool {
        self.inner
            .session_allow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }
}

/// Start the broker: generate a token, bind a loopback port, publish the endpoint
/// descriptor into the data dir, and spawn the accept loop. ALWAYS returns a broker so
/// the Tauri commands have state to bind to; if binding/publishing fails, the broker is
/// inert (no listener) and HITL simply never receives approvals - gateways then fail
/// closed on their own (a connect to nothing denies). Never panics.
#[cfg(feature = "desktop")]
pub fn start(app: AppHandle) -> ApprovalBroker {
    let allowed_app = app.clone();
    let pending_app = app.clone();
    let resolved_app = app.clone();
    start_with_host(notification_host(
        BrokerHost {
            persistent_allowed: Arc::new(move |key| registry_allows(&allowed_app, key)),
            pending: Arc::new(move |view| {
                let _ = pending_app.emit("approval-pending", view);
            }),
            resolved: Arc::new(move |id| {
                let _ = resolved_app.emit("approval-resolved", id);
            }),
        },
        Arc::new(move |view, resolved| notify_pending(&app, view, resolved)),
    ))
}

#[cfg(all(target_os = "linux", feature = "gtk-desktop"))]
pub fn start_native() -> ApprovalBroker {
    start_with_host(BrokerHost {
        persistent_allowed: Arc::new(|key| {
            crate::registry::load()
                .map(|registry| registry.is_tool_allowed(key))
                .unwrap_or(false)
        }),
        pending: Arc::new(|_| {}),
        resolved: Arc::new(|_| {}),
    })
}

fn start_with_host(host: BrokerHost) -> ApprovalBroker {
    let mut tok = [0u8; 24];
    let token: String = match getrandom::getrandom(&mut tok) {
        Ok(()) => tok.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => String::new(),
    };
    if existing_broker_is_live() {
        log_broker_event("another Toolport process already owns the approval endpoint");
        return inert_broker(token);
    }
    let Some(owner_lock) = acquire_owner_lock() else {
        log_broker_event("another Toolport process is starting the approval broker");
        return inert_broker(token);
    };
    let broker = ApprovalBroker {
        inner: Arc::new(Inner {
            token: token.clone(),
            pending: Mutex::new(HashMap::new()),
            session_allow: Mutex::new(HashSet::new()),
            _owner_lock: Some(owner_lock),
        }),
    };

    match bind_listeners() {
        Some(bound) => {
            if let Some(dir) = crate::registry::conduit_dir() {
                let desc = EndpointDescriptor {
                    endpoint: bound.endpoint.clone(),
                    token,
                    unix_endpoint: bound.unix_endpoint.clone(),
                };
                let path = dir.join(ENDPOINT_FILE);
                // A stale descriptor (app crashed) points at a dead endpoint, so a gateway
                // connect fails and denies - fail-closed either way. The gateway also
                // re-reads the descriptor and retries once, which self-heals the case
                // where the app restarted and rebound somewhere new. Should some other
                // process bind the stale endpoint instead, the gateway's challenge refuses
                // it before a byte of any request is sent (SBS-867).
                // Written via atomic_write so the HITL endpoint + auth token land
                // owner-only (0600) on Unix rather than world-readable: the token is what
                // the challenge proves, so nothing but the app (or a process that can
                // already read its data dir) may hold it.
                let _ = crate::registry::atomic_write(
                    &path,
                    &serde_json::to_string(&desc).unwrap_or_default(),
                );
                // Record WHERE we published, into the same always-on log the gateway
                // writes its `dir_resolution=` line to. If a client-spawned gateway
                // resolves the data dir differently from the app (MSIX virtualization,
                // a differently-spelled HOME), that mismatch is now a one-line read
                // instead of a multi-hour hunt - it was the root cause of a live
                // "HITL blocks every call but no prompt appears" incident.
                log_broker_event(&format!(
                    "bound {}{}; endpoint published at {}",
                    bound.endpoint,
                    bound
                        .unix_endpoint
                        .as_deref()
                        .map(|u| format!(" and {u}"))
                        .unwrap_or_default(),
                    path.display()
                ));
            } else {
                log_broker_event(
                    "conduit_dir() unavailable; endpoint NOT published (HITL fails closed)",
                );
            }
            // One worker budget across every listener: the cap is on concurrent
            // connections, not per transport.
            let active_workers = Arc::new(AtomicUsize::new(0));
            for listener in bound.listeners {
                let b = broker.clone();
                let host = host.clone();
                let workers = Arc::clone(&active_workers);
                std::thread::spawn(move || accept_loop(listener, b, host, workers));
            }
        }
        None => {
            // Inert broker: HITL never fires and every gateway fails closed. Say so in
            // the log, since "no prompt ever appears" is otherwise a silent mystery.
            log_broker_event("could not bind a loopback listener; HITL fails closed until restart");
        }
    }
    broker
}

fn inert_broker(token: String) -> ApprovalBroker {
    ApprovalBroker {
        inner: Arc::new(Inner {
            token,
            pending: Mutex::new(HashMap::new()),
            session_allow: Mutex::new(HashSet::new()),
            _owner_lock: None,
        }),
    }
}

fn acquire_owner_lock() -> Option<std::fs::File> {
    let dir = crate::registry::conduit_dir()?.join("broker");
    acquire_owner_lock_at(&dir)
}

fn acquire_owner_lock_at(dir: &std::path::Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(dir.join("owner.lock"))
        .ok()?;
    file.try_lock_exclusive().ok()?;
    Some(file)
}

fn existing_broker_is_live() -> bool {
    let Some(path) = crate::registry::conduit_dir().map(|dir| dir.join(ENDPOINT_FILE)) else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(descriptor) = serde_json::from_str::<EndpointDescriptor>(&raw) else {
        return false;
    };
    crate::approval::dial_broker(&descriptor).is_ok()
}

/// Accept on one listener until it errors out for good, handing each connection to a
/// bounded worker. Shared across transports through `active_workers`.
fn accept_loop(
    listener: Listener,
    broker: ApprovalBroker,
    host: BrokerHost,
    active_workers: Arc<AtomicUsize>,
) {
    loop {
        let conn = match listener.accept() {
            Ok(conn) => conn,
            Err(_) => {
                // Transient (EMFILE, EINTR): don't spin flat out on it.
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        let Some(permit) = try_acquire_connection(&active_workers) else {
            // Closing immediately is fail-closed and keeps a slow or
            // stalled peer from consuming an unbounded thread count.
            drop(conn);
            continue;
        };
        let b = broker.clone();
        let host = host.clone();
        let _ = std::thread::Builder::new()
            .name("toolport-approval".into())
            .spawn(move || {
                let _permit = permit;
                handle_conn(conn, b, host);
            });
    }
}

/// What [`bind_listeners`] got: every listener to accept on, plus the endpoint strings to
/// publish for each.
struct Bound {
    listeners: Vec<Listener>,
    /// Loopback TCP, always. The field a pre-socket gateway reads.
    endpoint: String,
    /// The socket file, on Unix when it could be had.
    unix_endpoint: Option<String>,
}

/// The listener the broker accepts on, for one transport.
enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
}

impl Listener {
    fn accept(&self) -> io::Result<BrokerStream> {
        match self {
            Listener::Tcp(l) => l.accept().map(|(s, _)| BrokerStream::Tcp(s)),
            #[cfg(unix)]
            Listener::Unix(l) => l.accept().map(|(s, _)| BrokerStream::Unix(s)),
        }
    }
}

/// Where the Unix broker socket lives: a 0700 directory of its own under the data dir, so
/// no other user can connect to it. The data dir's own mode is whatever the platform gave
/// it, and the socket path ends up in a log line, so the directory carries the guarantee.
#[cfg(unix)]
fn unix_socket_path() -> Option<std::path::PathBuf> {
    Some(
        crate::registry::conduit_dir()?
            .join("broker")
            .join("approval.sock"),
    )
}

/// Bind the broker's listeners. Loopback TCP is required and always published as
/// `endpoint`: gateways that predate the socket field read only that, and long-lived
/// client-spawned gateways outlive app updates, so dropping it would cut them off from
/// approvals until their client restarts them. On Unix a socket file in a private directory
/// is bound as well and published as `unix_endpoint`, which current gateways prefer; if it
/// cannot be had (data dir unavailable, or a path too long for `sun_path`) the broker is
/// TCP-only, which loses defense in depth, not the guarantee - the challenge in
/// [`crate::approval::dial_broker`] protects both the same way. Windows is TCP-only.
fn bind_listeners() -> Option<Bound> {
    let tcp = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = tcp.local_addr().ok()?.port();
    let mut listeners = vec![Listener::Tcp(tcp)];
    let mut unix_endpoint = None;
    #[cfg(unix)]
    match bind_unix_listener() {
        Ok((listener, endpoint)) => {
            listeners.push(listener);
            unix_endpoint = Some(endpoint);
        }
        Err(e) => log_broker_event(&format!("unix socket unavailable ({e}); loopback TCP only")),
    }
    Some(Bound {
        listeners,
        endpoint: format!("127.0.0.1:{port}"),
        unix_endpoint,
    })
}

#[cfg(unix)]
fn bind_unix_listener() -> io::Result<(Listener, String)> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = unix_socket_path().ok_or_else(|| io::Error::other("data dir unavailable"))?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no parent"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // `create` leaves a pre-existing directory's mode alone; make the private mode hold
    // across runs and across anything else that may have created it.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // A socket file survives a crash or a force-kill. Nothing is listening on it (the app
    // is single-instance), so a connect would only ever be refused; unlink it so the bind
    // below does not fail on EADDRINUSE.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok((
        Listener::Unix(listener),
        format!(
            "{}{}",
            crate::approval::UNIX_ENDPOINT_PREFIX,
            path.display()
        ),
    ))
}

/// Append a line to the shared, always-on gateway log, so the broker's bind/publish
/// location sits right next to the gateway's `dir_resolution=` line. Best-effort: a logging
/// failure never touches an approval decision.
fn log_broker_event(msg: &str) {
    crate::gatewaylog::append(&format!("[broker] {msg}"));
}

/// What the broker can settle about a request before a human is involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preflight {
    /// Unauthenticated: answer `Denied` and hang up.
    Deny,
    /// Covered by a fingerprint-bound allow: answer `Approved` without prompting.
    AutoApprove,
    /// Park it and wait for the person (or the fail-closed timeout).
    AskHuman,
}

/// The authentication + auto-approve decision for one request, with no I/O so it can be
/// exercised without a Tauri `AppHandle`. `is_allowed` answers whether a
/// fingerprint-bound allow key is on the session or registry allowlist; it is consulted
/// only for a key this function builds, which is why a legacy broad `server/tool` entry
/// can never match (see the comment below).
fn preflight(
    req: &ApprovalRequest,
    broker_token: &str,
    is_allowed: impl Fn(&str) -> bool,
) -> Preflight {
    // Authenticate: only a process holding our token may register an approval. The empty
    // check is not redundant with `token_eq`: if getrandom failed at startup our own token
    // is empty, and without it every tokenless caller would authenticate.
    if req.token.is_empty() || !token_eq(&req.token, broker_token) {
        return Preflight::Deny;
    }

    // Auto-approve only if the current tool definition matches a fingerprint-bound allow.
    // Legacy broad `server/tool` entries are intentionally ignored: a tool definition that
    // changed since approval should re-prompt instead of inheriting a stale bypass. A
    // request with no fingerprint at all likewise never auto-approves.
    // A PII release is excluded for a stronger reason than a URL elicitation: the allow key
    // binds a TOOL definition, and "this tool may run unprompted" is not consent to hand a
    // particular customer's address to a server that never had it. Auto-approving here would
    // let one earlier "always allow" quietly release every future value to that server.
    if req.url_elicitation.is_none() && req.pii_release.is_none() {
        if let Some(fp) = req.tool_fingerprint.as_deref() {
            let key = crate::approval::fingerprint_allow_key(&req.server, &req.tool, fp);
            if is_allowed(&key) {
                return Preflight::AutoApprove;
            }
        }
    }

    Preflight::AskHuman
}

fn disconnect_decision(reader: &mut impl Read) -> ApprovalDecision {
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return ApprovalDecision::Timeout;
            }
            _ => return ApprovalDecision::Denied,
        }
    }
}

#[cfg(any(feature = "desktop", test))]
fn show_pending_os_toast(
    retractable: bool,
    resolved: std::sync::mpsc::Receiver<()>,
    show_retractable: impl FnOnce(std::sync::mpsc::Receiver<()>),
    show_persistent: impl FnOnce(),
) {
    if retractable {
        show_retractable(resolved);
    } else if resolved.try_recv().is_err() {
        show_persistent();
    }
}

/// Serve one gateway connection: read the request, authenticate it, park it for a human
/// decision (or a fail-closed timeout), and write the decision back.
fn handle_conn(stream: BrokerStream, broker: ApprovalBroker, host: BrokerHost) {
    // Read the request promptly; a slow/stalled sender must not tie up a thread.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = match read_approval_request(&mut reader) {
        Ok(line) => line,
        Err(_) => return,
    };
    // A current gateway opens with a challenge and sends nothing else until it is
    // answered: prove we hold the token, then read the request on the same buffered
    // reader. A gateway from before the handshake sends its request first, and that line
    // IS the request. Either way the request is authenticated below exactly as before;
    // the handshake only adds the other direction (SBS-867).
    if let Some(proof) = crate::approval::answer_challenge(&line, &broker.inner.token) {
        let mut out = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        if writeln!(out, "{proof}").and_then(|_| out.flush()).is_err() {
            return;
        }
        line = match read_approval_request(&mut reader) {
            Ok(line) => line,
            Err(_) => return,
        };
    }
    let req: ApprovalRequest = match serde_json::from_slice(&line) {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut out = stream;
    let deny = |out: &mut BrokerStream| {
        let _ = out.set_write_timeout(Some(Duration::from_secs(10)));
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string(&ApprovalDecision::Denied).unwrap_or_default()
        );
    };

    match preflight(&req, &broker.inner.token, |key| {
        broker.session_contains(key) || (host.persistent_allowed)(key)
    }) {
        Preflight::Deny => {
            deny(&mut out);
            return;
        }
        Preflight::AutoApprove => {
            let _ = out.set_write_timeout(Some(Duration::from_secs(10)));
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string(&ApprovalDecision::Approved).unwrap_or_default()
            );
            return;
        }
        Preflight::AskHuman => {}
    }

    let view = PendingView {
        id: req.id.clone(),
        client: req.client.clone(),
        client_name: crate::clients::trusted_client_name(
            req.client.as_deref(),
            req.client_name.as_deref(),
        ),
        client_label: req.client_label.clone(),
        server: req.server.clone(),
        tool: req.tool.clone(),
        tool_fingerprint: req.tool_fingerprint.clone(),
        reason: req.reason,
        arguments: req.arguments.clone(),
        url_elicitation: req.url_elicitation.clone(),
        pii_release: req.pii_release.clone(),
        // Stamp the deadline now, right before we park on `recv_timeout` below.
        deadline_ms: deadline_ms_from_now(),
    };
    let (tx, rx) = channel::<ApprovalDecision>();
    {
        let mut pending = broker
            .inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending.len() >= MAX_PENDING {
            drop(pending);
            deny(&mut out);
            return;
        }
        // A correlation id identifies exactly one parked call. Replacing an existing
        // waiter would disconnect the first caller and let its cleanup remove the second.
        if pending.contains_key(&req.id) {
            drop(pending);
            deny(&mut out);
            return;
        }
        pending.insert(
            req.id.clone(),
            Waiter {
                view: view.clone(),
                decide: tx,
            },
        );
    }

    // Surface it to the active shell. The poll-based list remains the source of truth.
    (host.pending)(&view);

    // A broker socket owns exactly one prompt. Watch its read side while the
    // decision waits, so an abandoned caller withdraws without the 120s timeout.
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)));
    let watching = broker.clone();
    let id = req.id.clone();
    let disconnected = std::thread::spawn(move || {
        let decision = disconnect_decision(&mut reader);
        if let Some(waiter) = watching
            .inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
        {
            let _ = waiter.decide.send(decision);
        }
    });

    // Block for the human decision or the fail-closed timeout.
    let decision = rx
        .recv_timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .unwrap_or(ApprovalDecision::Timeout);
    // Ensure it's gone (timeout path leaves it; decide() already removed it).
    broker
        .inner
        .pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&req.id);
    (host.resolved)(&req.id);

    let _ = out.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string(&decision).unwrap_or_else(|_| "\"timeout\"".into())
    );
    match &out {
        BrokerStream::Tcp(stream) => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        #[cfg(unix)]
        BrokerStream::Unix(stream) => {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
    let _ = disconnected.join();
}

/// Whether the tool `key` is on the registry's persistent always-allow list. Reads the
/// app-managed registry state; false if unavailable.
#[cfg(feature = "desktop")]
fn registry_allows(app: &AppHandle, key: &str) -> bool {
    app.try_state::<Mutex<crate::registry::Registry>>()
        .map(|s| {
            s.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_tool_allowed(key)
        })
        .unwrap_or(false)
}

/// A resolution can arrive while the OS is still showing the notification.
/// Its queued signal closes that late notification as soon as Notify returns.
#[cfg(any(feature = "desktop", test))]
type ShowPendingNotification =
    Arc<dyn Fn(&PendingView, std::sync::mpsc::Receiver<()>) + Send + Sync>;
#[cfg(any(feature = "desktop", test))]
fn notification_host(host: BrokerHost, show: ShowPendingNotification) -> BrokerHost {
    let notifications = Arc::new(Mutex::new(
        HashMap::<String, std::sync::mpsc::Sender<()>>::new(),
    ));
    let pending = notifications.clone();
    BrokerHost {
        persistent_allowed: host.persistent_allowed,
        pending: Arc::new(move |view| {
            let (tx, rx) = channel();
            pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(view.id.clone(), tx);
            (host.pending)(view);
            show(view, rx);
        }),
        resolved: Arc::new(move |id| {
            if let Some(tx) = notifications
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(id)
            {
                let _ = tx.send(());
            }
            (host.resolved)(id);
        }),
    }
}

pub fn approval_requester(view: &PendingView) -> String {
    let client =
        crate::clients::trusted_client_name(view.client.as_deref(), Some(&view.client_name));
    match view
        .client_label
        .as_deref()
        .filter(|label| *label != client)
    {
        Some(label) => format!(
            "{client} (reports: {})",
            crate::approval::shorten_client_label(label, 40)
        ),
        None => client,
    }
}

pub fn approval_notification(view: &PendingView) -> (&'static str, String) {
    let who = approval_requester(view);
    if let Some(elicitation) = &view.url_elicitation {
        (
            "Toolport: browser action required",
            format!(
                "{} needs a browser action. Requested by {who}. Review it in Toolport.",
                elicitation.origin
            ),
        )
    } else {
        (
            "Toolport: approval required",
            format!(
                "{} / {} needs approval. Requested by {who}.",
                view.server, view.tool
            ),
        )
    }
}

/// Notify the human that a call is held: an OS notification plus
/// taskbar attention on the main window. Best-effort and non-blocking - if either fails (permission
/// off, no window) the in-app overlay is still the source of truth. We flash rather than
/// force-focus so we don't yank the user out of what they're doing.
#[cfg(feature = "desktop")]
fn notify_pending(app: &AppHandle, view: &PendingView, resolved: std::sync::mpsc::Receiver<()>) {
    let (title, body) = approval_notification(view);
    show_pending_os_toast(
        cfg!(target_os = "linux"),
        resolved,
        |resolved| {
            #[cfg(target_os = "linux")]
            std::thread::spawn({
                let body = body.clone();
                move || notify_until_resolved(title, &body, resolved)
            });
            #[cfg(not(target_os = "linux"))]
            let _ = resolved;
        },
        || {
            // The plugin discards the native handle and exposes no per-toast
            // identifier/tag or removal API on Windows/macOS. Keep showing the
            // toast there: a stale notice is preferable to a missed approval.
            #[cfg(not(target_os = "linux"))]
            {
                use tauri_plugin_notification::NotificationExt;
                let _ = app.notification().builder().title(title).body(&body).show();
            }
        },
    );
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
    }
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn notify_until_resolved(title: &str, body: &str, resolved: std::sync::mpsc::Receiver<()>) {
    notify_until_resolved_at_destination(title, body, resolved, "org.freedesktop.Notifications");
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn notify_until_resolved_at_destination(
    title: &str,
    body: &str,
    resolved: std::sync::mpsc::Receiver<()>,
    destination: &str,
) {
    use gio::glib::{self, variant::ToVariant};
    use std::cell::Cell;
    use std::rc::Rc;
    const BUS: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    let context = glib::MainContext::new();
    let _ = context.with_thread_default(|| {
        if resolved.try_recv().is_ok() {
            return;
        }
        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            return;
        };
        let shown = Rc::new(Cell::new(0u32));
        let tracked = shown.clone();
        let signal = bus.signal_subscribe(
            Some(destination),
            Some(BUS),
            Some("NotificationClosed"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, parameters| {
                if let Some((id, _)) = parameters.get::<(u32, u32)>() {
                    if tracked.get() == id {
                        tracked.set(0);
                    }
                }
            },
        );
        let notify = |id: u32, title: &str, body: &str, urgency: u8, expiry: i32| {
            let hints = HashMap::from([("urgency".to_string(), urgency.to_variant())]);
            bus.call_sync(
                Some(destination),
                PATH,
                BUS,
                "Notify",
                Some(
                    &(
                        "Toolport",
                        id,
                        "toolport",
                        title,
                        gio::glib::markup_escape_text(body).as_str(),
                        Vec::<String>::new(),
                        hints,
                        expiry,
                    )
                        .to_variant(),
                ),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|reply| reply.child_value(0).get::<u32>())
        };
        if let Some(id) = notify(0, title, body, 2, -1) {
            shown.set(id);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        loop {
            while context.pending() {
                context.iteration(false);
            }
            if std::time::Instant::now() >= deadline
                || !matches!(
                    resolved.recv_timeout(Duration::from_millis(10)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                )
            {
                break;
            }
        }
        while context.pending() {
            context.iteration(false);
        }
        if shown.get() != 0 {
            // Urgent cards can survive CloseNotification. Replace the tracked id
            // with a short, non-actionable expiry before closing, as GTK does.
            let id = notify(shown.get(), "Approval handled", "", 0, 1000).unwrap_or(shown.get());
            let _ = bus.call_sync(
                Some(destination),
                PATH,
                BUS,
                "CloseNotification",
                Some(&(id,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
            );
        }
        bus.signal_unsubscribe(signal);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p08b_r1_notification_puts_action_before_bounded_report() {
        let view = PendingView {
            id: "1".into(),
            client: Some("adapter:claude-code".into()),
            client_name: "Claude Code".into(),
            client_label: Some("Claude Code 2.1.0".into()),
            server: "github".into(),
            tool: "delete_issue".into(),
            tool_fingerprint: None,
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            url_elicitation: None,
            pii_release: None,
            deadline_ms: 0,
        };
        assert_eq!(approval_notification(&view).1, "github / delete_issue needs approval. Requested by Claude Code (reports: Claude Code 2.1.0).");
        let mut view = view;
        view.client_label = Some("Claude Code".into());
        assert_eq!(approval_requester(&view), "Claude Code");
        view.client_label = Some("界".repeat(80));
        assert_eq!(
            approval_requester(&view),
            format!("Claude Code (reports: {}…)", "界".repeat(39))
        );
    }

    #[test]
    fn p08_broker_read_timeout_is_timeout_and_eof_is_denied() {
        struct Timeout;
        impl Read for Timeout {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            }
        }
        assert_eq!(disconnect_decision(&mut Timeout), ApprovalDecision::Timeout);
        assert_eq!(
            disconnect_decision(&mut io::empty()),
            ApprovalDecision::Denied
        );
    }

    #[test]
    fn p08_non_retractable_platforms_still_show_pending_os_toasts() {
        for platform in ["Windows", "macOS"] {
            let (_tx, rx) = channel();
            let shown = std::cell::Cell::new(false);
            show_pending_os_toast(
                false,
                rx,
                |_| panic!("unexpected Linux path"),
                || shown.set(true),
            );
            assert!(shown.get(), "{platform} must surface a pending approval");
        }
    }

    #[test]
    fn p08_tauri_notification_resolution_withdraws_only_its_toast_even_after_late_show() {
        let (shown_tx, shown_rx) = channel();
        let host = notification_host(
            BrokerHost {
                persistent_allowed: Arc::new(|_| false),
                pending: Arc::new(|_| {}),
                resolved: Arc::new(|_| {}),
            },
            Arc::new(move |view, resolved| {
                shown_tx.send((view.id.clone(), resolved)).unwrap();
            }),
        );
        let first = request("tok", Some("v2:abc"));
        let mut other = first.clone();
        other.id = "other".into();
        let view = |req: ApprovalRequest| PendingView {
            id: req.id,
            client_name: crate::clients::trusted_client_name(
                req.client.as_deref(),
                req.client_name.as_deref(),
            ),
            client: req.client,
            client_label: req.client_label,
            server: req.server,
            tool: req.tool,
            reason: req.reason,
            tool_fingerprint: req.tool_fingerprint,
            arguments: req.arguments,
            url_elicitation: None,
            pii_release: None,
            deadline_ms: deadline_ms_from_now(),
        };
        (host.pending)(&view(first));
        let (_, first) = shown_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        (host.pending)(&view(other));
        let (_, other) = shown_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The same resolved callback used by handle_conn must queue withdrawal
        // while the OS's Notify is still returning its id.
        (host.resolved)("req-1");
        (host.resolved)("req-1");
        first.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            other.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        (host.resolved)("other");
        other.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[cfg(all(feature = "desktop", target_os = "linux"))]
    #[test]
    fn p08_tauri_os_toast_is_replaced_and_closed_on_withdrawal() {
        use gio::glib::{self, variant::ToVariant};
        let (ready_tx, ready_rx) = channel();
        let (calls_tx, calls_rx) = channel();
        let (stop_tx, stop_rx) = channel::<()>();
        let server = std::thread::spawn(move || {
            let context = glib::MainContext::new();
            context.with_thread_default(|| {
                let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).expect("run this notification test in omabox");
                let node = gio::DBusNodeInfo::for_xml("<node><interface name='org.freedesktop.Notifications'><method name='Notify'><arg type='s' direction='in'/><arg type='u' direction='in'/><arg type='s' direction='in'/><arg type='s' direction='in'/><arg type='s' direction='in'/><arg type='as' direction='in'/><arg type='a{sv}' direction='in'/><arg type='i' direction='in'/><arg type='u' direction='out'/></method><method name='CloseNotification'><arg type='u' direction='in'/></method></interface></node>").unwrap();
                let registration = bus.register_object("/org/freedesktop/Notifications", &node.lookup_interface("org.freedesktop.Notifications").unwrap(),
                    move |_, _, _, _, method, parameters, invocation| {
                        calls_tx.send((method.to_string(), parameters)).unwrap();
                        invocation.return_value(Some(&if method == "Notify" { (42u32,).to_variant() } else { ().to_variant() }));
                    }, |_, _, _, _, _| ().to_variant(), |_, _, _, _, _, _| false).unwrap();
                ready_tx.send(bus.unique_name().unwrap().to_string()).unwrap();
                loop {
                    while context.pending() { context.iteration(false); }
                    if !matches!(stop_rx.recv_timeout(Duration::from_millis(2)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) { break; }
                }
                bus.unregister_object(registration).unwrap();
            }).unwrap();
        });
        let destination = ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (resolved_tx, resolved_rx) = channel();
        let worker = std::thread::spawn(move || {
            notify_until_resolved_at_destination(
                "Approval required",
                "s/t",
                resolved_rx,
                &destination,
            )
        });
        let (method, pending) = calls_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(method, "Notify");
        assert_eq!(pending.child_value(1).get::<u32>(), Some(0));
        resolved_tx.send(()).unwrap();
        let (method, handled) = calls_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(method, "Notify");
        assert_eq!(handled.child_value(1).get::<u32>(), Some(42));
        assert_eq!(handled.child_value(7).get::<i32>(), Some(1000));
        let (method, closed) = calls_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(method, "CloseNotification");
        assert_eq!(closed.child_value(0).get::<u32>(), Some(42));
        worker.join().unwrap();
        stop_tx.send(()).unwrap();
        server.join().unwrap();
    }

    fn broker() -> ApprovalBroker {
        ApprovalBroker {
            inner: Arc::new(Inner {
                token: "tok".into(),
                pending: Mutex::new(HashMap::new()),
                session_allow: Mutex::new(HashSet::new()),
                _owner_lock: None,
            }),
        }
    }

    fn park(b: &ApprovalBroker, id: &str) -> std::sync::mpsc::Receiver<ApprovalDecision> {
        let (tx, rx) = channel();
        let view = PendingView {
            id: id.into(),
            client: None,
            client_label: None,
            client_name: "An AI client".into(),
            server: "s".into(),
            tool: "drop".into(),
            tool_fingerprint: Some("v2:abc".into()),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            url_elicitation: None,
            pii_release: None,
            deadline_ms: deadline_ms_from_now(),
        };
        b.inner
            .pending
            .lock()
            .unwrap()
            .insert(id.into(), Waiter { view, decide: tx });
        rx
    }

    #[test]
    fn approve_delivers_then_removes() {
        let b = broker();
        let rx = park(&b, "x");
        assert_eq!(b.list().len(), 1);
        b.decide("x", true).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ApprovalDecision::Approved
        );
        assert!(b.list().is_empty(), "resolved entry should be gone");
    }

    #[test]
    fn deny_delivers_denied() {
        let b = broker();
        let rx = park(&b, "y");
        b.decide("y", false).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ApprovalDecision::Denied
        );
    }

    #[test]
    fn unknown_id_errs() {
        assert!(broker().decide("nope", true).is_err());
    }

    #[test]
    fn deadline_is_about_the_timeout_out() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let d = deadline_ms_from_now();
        let target = DEFAULT_TIMEOUT_SECS * 1000;
        // ~timeout out; allow a few seconds of slack for a slow CI scheduler.
        assert!(
            d >= now + target - 3_000 && d <= now + target + 3_000,
            "deadline {d} not ~{target}ms past {now}"
        );
    }

    #[test]
    fn broker_token_comparison_matches_only_equal_values() {
        assert!(token_eq("token123", "token123"));
        assert!(!token_eq("token123", "token124"));
        assert!(!token_eq("token123", "token1234"));
        assert!(!token_eq("", "token123"));
    }

    #[test]
    fn owner_lock_allows_only_one_broker() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-broker-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = acquire_owner_lock_at(&dir).expect("first broker owns the lock");
        assert!(
            acquire_owner_lock_at(&dir).is_none(),
            "a second broker must not replace the first"
        );
        drop(first);
        assert!(
            acquire_owner_lock_at(&dir).is_some(),
            "the lock must be reusable after shutdown"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shell_neutral_host_observes_pending_and_resolved_lifecycle() {
        let broker = broker();
        let (pending_tx, pending_rx) = channel::<String>();
        let (resolved_tx, resolved_rx) = channel::<String>();
        let host = BrokerHost {
            persistent_allowed: Arc::new(|_| false),
            pending: Arc::new(move |view| {
                let _ = pending_tx.send(view.id.clone());
            }),
            resolved: Arc::new(move |id| {
                let _ = resolved_tx.send(id.to_string());
            }),
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let serving = broker.clone();
        let worker =
            std::thread::spawn(move || handle_conn(BrokerStream::Tcp(server), serving, host));

        writeln!(
            client,
            "{}",
            serde_json::to_string(&request("tok", Some("v2:abc"))).unwrap()
        )
        .unwrap();
        client.flush().unwrap();
        assert_eq!(
            pending_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "req-1"
        );
        broker.decide("req-1", true).unwrap();
        assert_eq!(
            resolved_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "req-1"
        );
        let mut decision = String::new();
        BufReader::new(client).read_line(&mut decision).unwrap();
        assert_eq!(
            serde_json::from_str::<ApprovalDecision>(decision.trim()).unwrap(),
            ApprovalDecision::Approved
        );
        worker.join().unwrap();
    }

    #[test]
    fn p08_caller_disconnect_withdraws_only_its_pending_approval() {
        let broker = broker();
        let (pending_tx, pending_rx) = channel();
        let (resolved_tx, resolved_rx) = channel();
        let host = BrokerHost {
            persistent_allowed: Arc::new(|_| false),
            pending: Arc::new(move |view| {
                pending_tx.send(view.id.clone()).unwrap();
            }),
            resolved: Arc::new(move |id| {
                resolved_tx.send(id.to_string()).unwrap();
            }),
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut workers = Vec::new();
        let mut clients = Vec::new();
        for id in ["abandoned", "other"] {
            let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server, _) = listener.accept().unwrap();
            let serving = broker.clone();
            let host = host.clone();
            workers.push(std::thread::spawn(move || {
                handle_conn(BrokerStream::Tcp(server), serving, host)
            }));
            let mut req = request("tok", Some("v2:abc"));
            req.id = id.to_string();
            writeln!(client, "{}", serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(pending_rx.recv_timeout(Duration::from_secs(1)).unwrap(), id);
            clients.push(client);
        }
        clients[0].shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(
            resolved_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "abandoned"
        );
        assert!(broker.decide("abandoned", true).is_err());
        assert_eq!(broker.list().len(), 1);
        broker.decide("other", true).unwrap();
        assert_eq!(
            resolved_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "other"
        );
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn approval_request_reader_requires_a_bounded_line() {
        let valid = b"{\"token\":\"tok\"}\n";
        assert_eq!(
            read_approval_request(&mut std::io::Cursor::new(valid)).unwrap(),
            valid
        );

        let unterminated = br#"{"token":"tok"}"#;
        assert_eq!(
            read_approval_request(&mut std::io::Cursor::new(unterminated))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let oversized = vec![b'x'; MAX_APPROVAL_REQUEST_BYTES + 1];
        assert_eq!(
            read_approval_request(&mut std::io::Cursor::new(oversized))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn approval_connection_worker_count_is_bounded_and_released() {
        let active = Arc::new(AtomicUsize::new(0));
        let mut permits = Vec::new();
        for _ in 0..MAX_CONNECTION_WORKERS {
            permits.push(try_acquire_connection(&active).expect("worker permit"));
        }
        assert_eq!(active.load(Ordering::Acquire), MAX_CONNECTION_WORKERS);
        assert!(try_acquire_connection(&active).is_none());

        permits.pop();
        assert_eq!(active.load(Ordering::Acquire), MAX_CONNECTION_WORKERS - 1);
        permits.push(try_acquire_connection(&active).expect("released permit is reusable"));
        drop(permits);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    /// A fingerprinted request as a gateway sends it, carrying `token`.
    fn request(token: &str, fingerprint: Option<&str>) -> ApprovalRequest {
        ApprovalRequest {
            token: token.into(),
            id: "req-1".into(),
            client: None,
            client_name: crate::session_observability::current().client_name,
            client_label: None,
            server: "db".into(),
            tool: "drop_table".into(),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            tool_fingerprint: fingerprint.map(str::to_string),
            url_elicitation: None,
            pii_release: None,
        }
    }

    /// The allowlist probe `handle_conn` passes to `preflight`, backed by a fixed key set.
    fn allowing(keys: &[String]) -> impl Fn(&str) -> bool + '_ {
        move |key: &str| keys.iter().any(|k| k == key)
    }

    #[test]
    fn preflight_denies_a_wrong_or_empty_token() {
        let allow_nothing = allowing(&[]);
        assert_eq!(
            preflight(&request("nope", Some("v2:abc")), "tok", &allow_nothing),
            Preflight::Deny
        );
        assert_eq!(
            preflight(&request("", Some("v2:abc")), "tok", &allow_nothing),
            Preflight::Deny
        );
        // Fail closed when getrandom failed at startup and our own token is empty: an
        // empty-vs-empty compare would otherwise authenticate every caller.
        assert_eq!(
            preflight(&request("", Some("v2:abc")), "", &allow_nothing),
            Preflight::Deny
        );
        // Sanity: the same request with the right token gets past auth.
        assert_eq!(
            preflight(&request("tok", Some("v2:abc")), "tok", &allow_nothing),
            Preflight::AskHuman
        );
    }

    #[test]
    fn preflight_auto_approves_only_a_fingerprint_allow() {
        let fp_key = crate::approval::fingerprint_allow_key("db", "drop_table", "v2:abc");
        let allowed = [fp_key];
        assert_eq!(
            preflight(&request("tok", Some("v2:abc")), "tok", allowing(&allowed)),
            Preflight::AutoApprove
        );
        // A different tool definition is a different key, so it re-prompts.
        assert_eq!(
            preflight(&request("tok", Some("v2:xyz")), "tok", allowing(&allowed)),
            Preflight::AskHuman
        );
    }

    #[test]
    fn preflight_ignores_a_legacy_server_tool_allow() {
        // The pre-fingerprint broad key must NOT grant a bypass to a fingerprinted call:
        // a tool definition that changed since approval has to be re-approved.
        let legacy = [crate::approval::allow_key("db", "drop_table")];
        assert_eq!(
            preflight(&request("tok", Some("v2:abc")), "tok", allowing(&legacy)),
            Preflight::AskHuman
        );
    }

    #[test]
    fn preflight_never_auto_approves_an_unfingerprinted_request() {
        // No fingerprint means we cannot prove the definition is the approved one, so no
        // allowlist entry of any shape may bypass the human.
        let every_key = |_: &str| true;
        assert_eq!(
            preflight(&request("tok", None), "tok", every_key),
            Preflight::AskHuman
        );
        // Still denied first on a bad token.
        assert_eq!(
            preflight(&request("bad", None), "tok", every_key),
            Preflight::Deny
        );
    }

    #[test]
    fn preflight_never_auto_approves_a_pii_release() {
        // SBS-696: the allow key binds a TOOL DEFINITION. "This tool may run unprompted" is
        // not consent to hand a specific customer's address to a server that never had it,
        // so an existing allow must not silently release every later value to it.
        let mut req = request("tok", Some("v2:abc"));
        req.reason = ApprovalReason::PiiCrossServer;
        req.pii_release = Some(crate::approval::PiiReleaseRequest {
            server: "mailer".into(),
            values: vec![crate::approval::PiiReleaseValue {
                token: "⟦EMAIL_1⟧".into(),
                value: "ada@example.com".into(),
                origins: vec!["crm".into()],
            }],
        });

        // Every key allowed, and it still asks.
        assert_eq!(preflight(&req, "tok", |_: &str| true), Preflight::AskHuman);
        // Authentication still comes first.
        req.token = "bad".into();
        assert_eq!(preflight(&req, "tok", |_: &str| true), Preflight::Deny);
    }

    #[test]
    fn session_allow_round_trips_and_decide_returns_view() {
        let b = broker();
        let key = crate::approval::fingerprint_allow_key("db", "db__read", "v2:abc");
        assert!(!b.session_contains(&key));
        b.add_session_allow(key.clone());
        assert!(b.session_contains(&key));
        assert_eq!(b.session_allowed(), vec![key.clone()]);
        b.remove_session_allow(&key);
        assert!(!b.session_contains(&key));

        // decide now hands back the resolved view (so the command can apply an allow scope).
        let rx = park(&b, "z");
        let view = b.decide("z", true).unwrap();
        assert_eq!(view.tool, "drop");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ApprovalDecision::Approved
        );
    }
}
