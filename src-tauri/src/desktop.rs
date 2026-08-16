//! Tauri desktop shell: tray, webview IPC commands, approval broker, HTTP bridge.

use std::io::ErrorKind;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Listener, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;

use crate::approval;
use crate::approval_broker;
use crate::audit;
use crate::catalog;
use crate::clients;
use crate::downstream::{resolve_root_token, DownstreamServer, StdioTransport};
use crate::inspect;
use crate::integrity;
use crate::oauth;
use crate::registry::{
    self, arg_looks_secret, redact_url_userinfo, FolderProfile, Profile, Registry, ServerEntry,
};
use crate::remote;
use crate::router;
use crate::routines;
use crate::savings;
use crate::searchtrace;
use crate::secrets;
use crate::stacks;
use crate::teams;
use crate::usage_report;
use crate::vendors;

type RegistryState = Mutex<Registry>;

struct TrayMenuState {
    pending_approvals: MenuItem<tauri::Wry>,
}

const OAUTH_LOCK_LEASE_SECS: u64 = 180;
const OAUTH_LOCK_WAIT_SECS: u64 = 30;
const OAUTH_LOCK_POLL_MS: u64 = 250;

struct OAuthFlowLock {
    path: std::path::PathBuf,
    attempt_id: String,
    succeeded: bool,
}

impl OAuthFlowLock {
    fn mark_succeeded(&mut self) {
        self.succeeded = true;
    }
}

struct AuthMutationLock {
    path: std::path::PathBuf,
}

impl Drop for AuthMutationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for OAuthFlowLock {
    fn drop(&mut self) {
        // SBS-842: waiters treat a completion file as "the other process
        // authenticated". Only claim success after tokens are vaulted;
        // otherwise write an explicit failure so a concurrent waiter
        // returns Err, not Ok(()).
        let completion = oauth_completion_path(&self.path, &self.attempt_id);
        let status = if self.succeeded { "ok" } else { "failed" };
        // Written atomically (temp file + rename): `fs::write` truncates first, so a
        // waiter polling every OAUTH_LOCK_POLL_MS could open the file between the
        // truncate and the bytes landing and read zero bytes. A rename makes the
        // completion file appear only once it is whole, so no waiter ever sees a
        // half-written verdict, not even if this process dies mid-write.
        let _ = registry::atomic_write(
            &completion,
            &format!(
                "status={status}\ndone={}\npid={}\n",
                now_unix_secs(),
                std::process::id()
            ),
        );
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone)]
struct OAuthLockSnapshot {
    modified: SystemTime,
    content: String,
    attempt_id: Option<String>,
}

impl OAuthLockSnapshot {
    fn instance_key(&self) -> String {
        let modified = self
            .modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{modified}:{}", self.content)
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn oauth_attempt_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn oauth_completion_path(path: &std::path::Path, attempt_id: &str) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("oauth.lock");
    path.with_file_name(format!("{name}.{attempt_id}.done"))
}

fn oauth_lock_contents(attempt_id: &str) -> String {
    format!(
        "attempt_id={attempt_id}
pid={}
started={}
lease_secs={}
",
        std::process::id(),
        now_unix_secs(),
        OAUTH_LOCK_LEASE_SECS
    )
}

fn parse_lock_attempt_id(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.strip_prefix("attempt_id=")
            .or_else(|| line.strip_prefix("nonce="))
            .map(ToOwned::to_owned)
    })
}

fn read_oauth_lock_snapshot(path: &std::path::Path) -> Result<Option<OAuthLockSnapshot>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not stat oauth lock file: {e}")),
    };
    let modified = meta
        .modified()
        .map_err(|e| format!("could not read oauth lock timestamp: {e}"))?;
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read oauth lock file: {e}"))?;
    let attempt_id = parse_lock_attempt_id(&content);
    Ok(Some(OAuthLockSnapshot {
        modified,
        content,
        attempt_id,
    }))
}

fn lock_snapshot_is_expired(snapshot: &OAuthLockSnapshot) -> bool {
    let Ok(elapsed) = snapshot.modified.elapsed() else {
        return false;
    };
    elapsed.as_secs() >= OAUTH_LOCK_LEASE_SECS
}

/// Test-only: production reads the file's CONTENT (see [`read_oauth_completion`]),
/// because existence alone is what wrongly reported a failed drop as success.
#[cfg(test)]
fn completion_exists(path: &std::path::Path, attempt_id: &str) -> bool {
    oauth_completion_path(path, attempt_id).exists()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OAuthCompletion {
    Succeeded,
    Failed,
}

/// `None` means "no verdict yet", never "failed": an empty or unrecognised file is a
/// file we caught mid-write (or one written by a build we do not know), and a waiter
/// that turned that into `Failed` would tell the user sign-in failed while the other
/// window was busy vaulting a token. Waiters keep polling on `None` and fall back to
/// the wait timeout, so an unreadable file costs a slow error, not a wrong one.
fn read_oauth_completion(path: &std::path::Path, attempt_id: &str) -> Option<OAuthCompletion> {
    let content = std::fs::read_to_string(oauth_completion_path(path, attempt_id)).ok()?;
    if content.lines().any(|line| line.trim() == "status=failed") {
        Some(OAuthCompletion::Failed)
    } else if content.lines().any(|line| line.trim() == "status=ok") {
        Some(OAuthCompletion::Succeeded)
    } else if content.contains("done=") {
        // Pre-SBS-842 files had no status= and were written on every drop.
        Some(OAuthCompletion::Succeeded)
    } else {
        None
    }
}

fn oauth_waiter_outcome(path: &std::path::Path, attempt_id: &str) -> Option<Result<(), String>> {
    match read_oauth_completion(path, attempt_id)? {
        OAuthCompletion::Succeeded => Some(Ok(())),
        OAuthCompletion::Failed => Some(Err(
            "another Toolport process failed to complete OAuth for this server".into(),
        )),
    }
}

fn try_replace_stale_lock(
    path: &std::path::Path,
    observed: &OAuthLockSnapshot,
    contender_contents: &str,
    contender_attempt_id: &str,
) -> Result<bool, String> {
    let Some(current) = read_oauth_lock_snapshot(path)? else {
        return Ok(false);
    };
    if current.instance_key() != observed.instance_key() {
        return Ok(false);
    }
    let _ = std::fs::remove_file(oauth_completion_path(path, contender_attempt_id));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("could not rewrite stale oauth lock file: {e}"))?;
    use std::io::Write;
    file.write_all(contender_contents.as_bytes())
        .map_err(|e| format!("could not write oauth lock file: {e}"))?;
    file.flush()
        .map_err(|e| format!("could not flush oauth lock file: {e}"))?;
    Ok(true)
}

fn oauth_lock_key(server_id: &str, url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn oauth_lock_path(server_id: &str, url: &str) -> Result<std::path::PathBuf, String> {
    let dir = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    let locks = dir.join("oauth-locks");
    std::fs::create_dir_all(&locks)
        .map_err(|e| format!("could not create oauth lock directory: {e}"))?;
    Ok(locks.join(format!("{}.lock", oauth_lock_key(server_id, url))))
}

fn auth_mutation_lock_path(server_id: &str) -> Result<std::path::PathBuf, String> {
    let dir = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    let locks = dir.join("oauth-locks");
    std::fs::create_dir_all(&locks)
        .map_err(|e| format!("could not create oauth lock directory: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    Ok(locks.join(format!("auth-write-{:x}.lock", hasher.finalize())))
}

fn try_acquire_auth_mutation_lock(
    path: &std::path::Path,
) -> Result<Option<AuthMutationLock>, String> {
    let mutation_id = oauth_attempt_id();
    let contents = format!(
        "mutation_id={mutation_id}\npid={}\nstarted={}\nlease_secs={}\n",
        std::process::id(),
        now_unix_secs(),
        OAUTH_LOCK_LEASE_SECS
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(contents.as_bytes())
                .map_err(|e| format!("could not write auth mutation lock file: {e}"))?;
            Ok(Some(AuthMutationLock {
                path: path.to_path_buf(),
            }))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let Some(observed) = read_oauth_lock_snapshot(path)? else {
                return Ok(None);
            };
            if lock_snapshot_is_expired(&observed)
                && try_replace_stale_lock(
                    path,
                    &observed,
                    &contents,
                    &mutation_id,
                )?
            {
                return Ok(Some(AuthMutationLock {
                    path: path.to_path_buf(),
                }));
            }
            Ok(None)
        }
        Err(e) => Err(format!("could not create auth mutation lock file: {e}")),
    }
}

fn acquire_auth_mutation_lock(server_id: &str) -> Result<AuthMutationLock, String> {
    let path = auth_mutation_lock_path(server_id)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(OAUTH_LOCK_WAIT_SECS);
    loop {
        if let Some(lock) = try_acquire_auth_mutation_lock(&path)? {
            return Ok(lock);
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "another Toolport process is updating credentials for this server; timed out waiting for it to finish"
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS));
    }
}

fn try_acquire_oauth_lock(path: &std::path::Path) -> Result<Option<OAuthFlowLock>, String> {
    let attempt_id = oauth_attempt_id();
    let contents = oauth_lock_contents(&attempt_id);
    let _ = std::fs::remove_file(oauth_completion_path(path, &attempt_id));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(contents.as_bytes())
                .map_err(|e| format!("could not write oauth lock file: {e}"))?;
            Ok(Some(OAuthFlowLock {
                path: path.to_path_buf(),
                attempt_id,
                succeeded: false,
            }))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let Some(observed) = read_oauth_lock_snapshot(path)? else {
                return Ok(None);
            };
            if lock_snapshot_is_expired(&observed)
                && try_replace_stale_lock(path, &observed, &contents, &attempt_id)?
            {
                return Ok(Some(OAuthFlowLock {
                    path: path.to_path_buf(),
                    attempt_id,
                    succeeded: false,
                }));
            }
            Ok(None)
        }
        Err(e) => Err(format!("could not create oauth lock file: {e}")),
    }
}

fn acquire_or_wait_oauth_lock(
    _server_id: &str,
    url: &str,
) -> Result<Option<OAuthFlowLock>, String> {
    let path = oauth_lock_path(_server_id, url)?;
    acquire_or_wait_oauth_lock_at(&path)
}

fn acquire_or_wait_oauth_lock_at(path: &std::path::Path) -> Result<Option<OAuthFlowLock>, String> {
    let mut observed_attempt_id: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(OAUTH_LOCK_WAIT_SECS);
    loop {
        if let Some(lock) = try_acquire_oauth_lock(path)? {
            if let Some(attempt_id) = &observed_attempt_id {
                if let Some(outcome) = oauth_waiter_outcome(path, attempt_id) {
                    drop(lock);
                    return outcome.map(|()| None);
                }
            }
            return Ok(Some(lock));
        }
        if let Some(snapshot) = read_oauth_lock_snapshot(path)? {
            if let Some(attempt_id) = snapshot.attempt_id {
                observed_attempt_id = Some(attempt_id);
            }
        }
        if let Some(attempt_id) = &observed_attempt_id {
            if let Some(outcome) = oauth_waiter_outcome(path, attempt_id) {
                return outcome.map(|()| None);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "another Toolport process is already running OAuth for this server; timed out waiting for it to finish"
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS));
    }
}

/// Tracks the optional `toolport-gateway --http` child the app supervises so
/// HTTP/OpenAPI clients (Open WebUI and the like) can connect with one click,
/// no terminal. Only one runs at a time; the app kills it on exit.
#[derive(Default)]
struct HttpBridge {
    child: Option<std::process::Child>,
    port: Option<u16>,
    token: Option<String>,
}
type HttpBridgeState = Mutex<HttpBridge>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateHttpBridgeIntent {
    port: u16,
    token: String,
}

fn update_http_bridge_intent_path() -> Option<std::path::PathBuf> {
    registry::resolved_path()?
        .parent()
        .map(|dir| dir.join("http-bridge-update-resume.json"))
}

fn save_update_http_bridge_intent(intent: &UpdateHttpBridgeIntent) -> Result<(), String> {
    let path = update_http_bridge_intent_path()
        .ok_or_else(|| "could not resolve the HTTP bridge update state path".to_string())?;
    save_update_http_bridge_intent_to(&path, intent)
}

fn save_update_http_bridge_intent_to(
    path: &std::path::Path,
    intent: &UpdateHttpBridgeIntent,
) -> Result<(), String> {
    let json = serde_json::to_string(intent).map_err(|error| error.to_string())?;
    registry::atomic_write(path, &json)
}

fn load_update_http_bridge_intent() -> Result<Option<UpdateHttpBridgeIntent>, String> {
    let Some(path) = update_http_bridge_intent_path() else {
        return Ok(None);
    };
    load_update_http_bridge_intent_from(&path)
}

fn load_update_http_bridge_intent_from(
    path: &std::path::Path,
) -> Result<Option<UpdateHttpBridgeIntent>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|error| {
            format!("could not read the HTTP bridge update state: {error}")
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read the HTTP bridge update state: {error}")),
    }
}

fn clear_update_http_bridge_intent() -> Result<(), String> {
    let Some(path) = update_http_bridge_intent_path() else {
        return Ok(());
    };
    clear_update_http_bridge_intent_at(&path)
}

fn clear_update_http_bridge_intent_at(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not clear the HTTP bridge update state: {error}"
        )),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpBridgeStatus {
    running: bool,
    port: Option<u16>,
    url: Option<String>,
    /// The bearer token the client must send (Authorization: Bearer ...). Shown
    /// in the UI to copy; required on every request to the endpoint.
    token: Option<String>,
}

impl HttpBridgeStatus {
    fn new(port: Option<u16>, token: Option<String>) -> Self {
        HttpBridgeStatus {
            running: port.is_some(),
            url: port.map(|p| format!("http://localhost:{p}")),
            port,
            token,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeResult {
    server_id: String,
    ok: bool,
    tool_count: usize,
    error: Option<String>,
    /// The failure looks like missing credentials (a remote 401/403, or a stdio
    /// server with secret env vars that aren't vaulted) - so the fix is to
    /// authenticate, not to debug. Drives the "Needs sign-in" UI.
    auth_required: bool,
}

/// True if this server declares secret env vars that don't yet have a vaulted value.
/// A failed vault read is NOT "missing" (SBS-789): a locked keychain must not flip
/// the UI to "Needs sign-in" and invite the user to re-enter a credential that is
/// in fact stored — only a confirmed `Ok(None)` counts.
fn missing_secret(server: &ServerEntry) -> bool {
    server.env.iter().any(|e| {
        e.secret
            && e.value.is_none()
            && matches!(secrets::get_secret_result(&server.id, &e.key), Ok(None))
    })
}

/// Connect to one server (stdio or remote), injecting any vaulted secrets, and
/// return the live connection (its tools are already listed). Shared by the
/// health probe and the tool playground - the running gateway is a separate
/// process, so the app connects on demand for these one-off operations.
fn connect_server(server: &ServerEntry) -> Result<DownstreamServer, String> {
    if let Some(command) = &server.command {
        let mut env: Vec<(String, String)> = Vec::new();
        for e in &server.env {
            if let Some(v) = &e.value {
                env.push((e.key.clone(), v.clone()));
            } else if e.secret {
                // Distinguish "never saved" from "couldn't read it" so we don't
                // silently launch a server without its key (which then fails with
                // its own cryptic message). Surface the real reason instead.
                match secrets::get_secret_result(&server.id, &e.key) {
                    Ok(Some(v)) => env.push((e.key.clone(), v)),
                    Ok(None) => {
                        return Err(format!(
                            "missing secret '{}': add its value under this server's secrets",
                            e.key
                        ))
                    }
                    Err(err) => {
                        return Err(format!(
                            "could not read secret '{}' from the keychain: {err}",
                            e.key
                        ))
                    }
                }
            }
        }
        // The probe/playground has no upstream client, so ${ROOT} has no root to
        // resolve against and falls back to the default cwd (issue #239).
        let cwd = server.cwd.as_deref().and_then(|c| resolve_root_token(c, None));
        let t = StdioTransport::spawn(command, &server.args, &env, cwd.as_deref())?;
        DownstreamServer::connect(server.id.clone(), Box::new(t))
    } else if server.url.is_some() {
        remote::connect_remote(server)
    } else {
        Err("no command or url".to_string())
    }
}

/// Connect to one server and report whether it came up and how many tools it has.
fn probe_one(server: &ServerEntry) -> ProbeResult {
    match connect_server(server) {
        Ok(ds) => ProbeResult {
            server_id: server.id.clone(),
            ok: true,
            tool_count: ds.tools.len(),
            error: None,
            auth_required: false,
        },
        // A stdio server that spawned but didn't list tools is very likely missing
        // its key; a remote 401/403 is an auth error outright.
        Err(e) => ProbeResult {
            server_id: server.id.clone(),
            ok: false,
            tool_count: 0,
            auth_required: remote::is_auth_error(&e) || missing_secret(server),
            error: Some(e),
        },
    }
}

/// Connect to a possibly-unsaved server entry and report whether it came up and
/// how many tools it exposes. Backs the "Test connection" button in the add/edit
/// dialog, so the user learns a server is broken before saving it. Never
/// persists anything; secret values the user typed ride in on `entry.env`, and
/// for an edit the entry keeps its id so already-vaulted secrets resolve.
#[tauri::command]
async fn test_server(entry: ServerEntry) -> Result<ProbeResult, String> {
    tauri::async_runtime::spawn_blocking(move || probe_one(&entry))
        .await
        .map_err(|e| e.to_string())
}

/// Probe every supported MCP client and return its current server configuration.
#[tauri::command]
async fn detect_clients(
    state: State<'_, RegistryState>,
) -> Result<Vec<clients::DetectedClient>, String> {
    // Reads several config files and scans plugin dirs - off the UI thread.
    let mut list = tauri::async_runtime::spawn_blocking(clients::detect_clients)
        .await
        .map_err(|e| e.to_string())?;
    // Ownership record lives on the registry (SOU-406).
    let managed = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .client_managed_entries
        .clone();
    clients::apply_entry_states(&mut list, &managed);
    Ok(list)
}

#[tauri::command]
fn get_registry(state: State<RegistryState>) -> Registry {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

fn server_from_detected(server: &clients::McpServer, client_id: &str) -> ServerEntry {
    ServerEntry {
        id: String::new(),
        name: server.name.clone(),
        transport: server.transport.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        // We only know env var names (values are never read into the UI layer).
        // Imported env vars are treated as secrets to be vaulted later.
        env: server
            .env_keys
            .iter()
            .map(|key| registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: true,
            })
            .collect(),
        url: server.url.clone(),
        source: Some(format!("imported:{client_id}")),
        disabled_tools: vec![],
        cwd: None,
        client_credentials: None,
        unknown_fields: serde_json::Map::new(),
    }
}

/// Servers to add to the registry from a set of detected clients: both a
/// client's main config servers and its plugin-detected ones (e.g. Cursor/Roo
/// project-level scans), skipping the gateway's own entry and anything with the
/// same import key (checked against `existing` plus whatever this same call has
/// already picked). Package runners use their full package spec as that key, so
/// distinct scoped packages that share a friendly display name are retained and
/// given unique registry ids by `Registry::add_server`.
/// The onboarding banner promises a count across both server sources (see
/// `importableServers` in `src/lib/types.ts`), so this must actually cover
/// both or it silently under-imports relative to what was promised.
fn servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
) -> Vec<ServerEntry> {
    let mut picked: Vec<ServerEntry> = Vec::new();
    let mut import_keys: std::collections::HashSet<String> = existing
        .servers
        .iter()
        .map(|server| {
            clients::import_dedupe_key(&server.name, server.command.as_deref(), &server.args)
        })
        .collect();
    for client in detected {
        for server in client.servers.iter().chain(client.plugin_servers.iter()) {
            let entry = server_from_detected(server, &client.id);
            // Recognize the gateway by command path too, not just the "conduit"
            // name: an entry registered under any other name (a leftover from
            // before the rename, a manual add, whatever) still points straight
            // at our own binary, and importing it risks the gateway proxying
            // itself. See is_gateway_server's doc comment - this is the exact
            // contract it promises but this call site wasn't honoring.
            if clients::is_gateway_server(&entry) {
                continue;
            }
            let key = clients::import_dedupe_key(&entry.name, entry.command.as_deref(), &entry.args);
            if import_keys.insert(key) {
                picked.push(entry);
            }
        }
    }
    picked
}

fn selected_servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
    selected: Option<&std::collections::HashSet<String>>,
) -> Vec<ServerEntry> {
    servers_to_import(detected, existing)
        .into_iter()
        .filter(|server| {
            selected.map_or(true, |keys| {
                keys.contains(&clients::import_dedupe_key(
                    &server.name,
                    server.command.as_deref(),
                    &server.args,
                ))
            })
        })
        .collect()
}

/// Pull selected servers from every detected client into the registry. Omitting
/// `selected` preserves the legacy import-all behavior; callers that preview
/// first pass the opaque import keys they explicitly confirmed.
#[tauri::command]
async fn import_servers(
    state: State<'_, RegistryState>,
    selected: Option<Vec<String>>,
) -> Result<Registry, String> {
    let detected = tauri::async_runtime::spawn_blocking(clients::detect_clients)
        .await
        .map_err(|e| e.to_string())?;
    let selected: Option<std::collections::HashSet<String>> =
        selected.map(|keys| keys.into_iter().collect());
    let (reg, _) = write_registry(state.inner(), |reg| {
        for server in selected_servers_to_import(&detected, reg, selected.as_ref()) {
            reg.add_server(server);
        }
        Ok(())
    })?;
    Ok(reg)
}

/// Parse a pasted config snippet and return the detected server(s) with
/// env-var values included. Used by the Add Server dialog's "paste config" feature.
#[tauri::command]
fn parse_server_snippet(text: String) -> Result<Vec<clients::ParsedSnippetServer>, String> {
    const MAX_SNIPPET_BYTES: usize = 256 * 1024;
    if text.len() > MAX_SNIPPET_BYTES {
        return Err(format!(
            "Snippet is {} KB; limit is {} KB. Paste a single server config, not an entire file.",
            text.len() / 1024,
            MAX_SNIPPET_BYTES / 1024,
        ));
    }
    clients::parse_snippet(&text)
}

#[tauri::command]
fn add_server(state: State<RegistryState>, entry: ServerEntry) -> Result<Registry, String> {
    let (reg, id) = write_registry(state.inner(), |reg| Ok(reg.add_server(entry)))?;
    // Warm the launcher for the entry we just added, found by its assigned id (a concurrent
    // add under the lock could otherwise make `last` a different server).
    if let Some(saved) = reg.servers.iter().find(|s| s.id == id) {
        prewarm_launcher(saved);
    }
    Ok(reg)
}

/// Fire-and-forget spawn of a just-added download-then-run server (npx, uvx, ...)
/// so the launcher fetches its package now, in the background, instead of on the
/// first health probe or gateway connect. Lenient about env: missing secrets are
/// skipped rather than fatal - the child may exit complaining about them, but by
/// then the package is already in the launcher's cache, which is the whole point.
/// The connect result is deliberately ignored; the real probe reports health.
fn prewarm_launcher(server: &ServerEntry) {
    if server.needs_team_enable_review() {
        return;
    }
    let Some(command) = server.command.clone() else {
        return;
    };
    if !crate::downstream::is_download_launcher(&command, &server.args) {
        return;
    }
    let server = server.clone();
    std::thread::spawn(move || {
        let mut env: Vec<(String, String)> = Vec::new();
        for e in &server.env {
            match e.value.clone() {
                Some(v) => env.push((e.key.clone(), v)),
                // Only secret entries have vaulted values; a plain unset var
                // must not pick up a same-named secret from the vault.
                None if !e.secret => {}
                None => match secrets::get_secret_result(&server.id, &e.key) {
                    Ok(Some(v)) => env.push((e.key.clone(), v)),
                    // Unset stays lenient (the point is warming the download
                    // cache), but a failed vault read aborts (SBS-789): don't
                    // hand the child a half-real environment while the keychain
                    // is locked — the real probe will retry with the truth.
                    Ok(None) => {}
                    Err(_) => return,
                },
            }
        }
        let cwd = server.cwd.as_deref().and_then(|c| resolve_root_token(c, None));
        if let Ok(t) = StdioTransport::spawn(&command, &server.args, &env, cwd.as_deref()) {
            // Attempting the handshake keeps the child alive until the download
            // finishes (dropping the transport kills it), and warms it end-to-end
            // when the server actually comes up.
            let _ = DownstreamServer::connect(server.id.clone(), Box::new(t));
        }
    });
}

#[tauri::command]
fn update_server(state: State<RegistryState>, entry: ServerEntry) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| reg.update_server(entry))?;
    Ok(reg)
}

#[tauri::command]
fn remove_server(state: State<RegistryState>, id: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| reg.remove_server(&id))?;
    Ok(reg)
}

/// `reviewed` is the caller asserting the member saw the Teams review dialog for
/// THIS definition. It defaults to false, which is the point: `set_all_enabled`
/// already filters these out (`Registry::set_all_enabled`), but this single-server
/// path had no gate at all, so the renderer's `needsTeamEnableReview` was the only
/// thing standing between a team-pushed local command and a running process. That
/// check cannot resolve DNS, so it missed a LAN URL written as a hostname, and any
/// future caller of this command would have inherited the same hole. Deciding it
/// here means the frontend heuristic is now an affordance rather than the boundary.
#[tauri::command]
fn set_server_enabled(
    state: State<RegistryState>,
    profile_id: String,
    server_id: String,
    enabled: bool,
    reviewed: Option<bool>,
) -> Result<Registry, String> {
    let reviewed = reviewed.unwrap_or(false);
    let (reg, _) = write_registry(state.inner(), |reg| {
        // Checked inside the write closure so it sees the registry that will be
        // persisted: a team_sync_wait replace landing between a pre-lock check and
        // this write could swap the entry for one that needs review.
        refuse_unreviewed_team_enable(reg, &server_id, enabled, reviewed)?;
        reg.set_server_enabled(&profile_id, &server_id, enabled)
    })?;
    Ok(reg)
}

/// The gate half of `set_server_enabled`, split out so the write-closure behavior
/// is unit-testable without a Tauri `State`.
fn refuse_unreviewed_team_enable(
    reg: &Registry,
    server_id: &str,
    enabled: bool,
    reviewed: bool,
) -> Result<(), String> {
    if enabled
        && !reviewed
        && reg
            .servers
            .iter()
            .any(|s| s.id == server_id && s.needs_team_enable_review())
    {
        return Err(
            "this team server runs a local command or private address; enable it from Teams after review"
                .into(),
        );
    }
    Ok(())
}

#[tauri::command]
fn set_all_enabled(
    state: State<RegistryState>,
    profile_id: String,
    enabled: bool,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_all_enabled(&profile_id, enabled)
    })?;
    Ok(reg)
}

#[tauri::command]
fn create_profile(state: State<RegistryState>, name: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.add_profile(&name);
        Ok(())
    })?;
    Ok(reg)
}

#[tauri::command]
fn delete_profile(state: State<RegistryState>, id: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| reg.remove_profile(&id))?;
    Ok(reg)
}

#[tauri::command]
fn set_active_profile(state: State<RegistryState>, id: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| reg.set_active_profile(&id))?;
    Ok(reg)
}

/// Replace the folder -> profile auto-routing mappings (SOU-188). A gateway serving a client
/// whose reported project root is under a mapped path auto-scopes to that profile. Returns
/// the saved registry so the UI reflects the persisted list.
#[tauri::command]
fn set_folder_profiles(
    state: State<RegistryState>,
    mappings: Vec<FolderProfile>,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_folder_profiles(mappings);
        Ok(())
    })?;
    Ok(reg)
}

/// Set (or clear) a profile's tool-granular scope for one server (SOU-189). `tools = Some(list)`
/// narrows that server to exactly those original tool names within the profile; `None` (or an
/// empty list) clears it, restoring all tools on that server. Returns the saved registry.
#[tauri::command]
fn set_profile_server_tools(
    state: State<RegistryState>,
    profile_id: String,
    server_id: String,
    tools: Option<Vec<String>>,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_profile_server_tools(&profile_id, &server_id, tools)
    })?;
    Ok(reg)
}

/// Write a server set into a client's config (backs up first). Not yet called by
/// the UI; reserved for bulk operations.
#[tauri::command]
fn write_to_client(
    client_id: String,
    servers: Vec<ServerEntry>,
) -> Result<clients::WriteOutcome, String> {
    clients::write_servers(&client_id, &servers)
}

/// Refuse to overwrite a hand-edited gateway entry unless `force` is true (SOU-406).
fn refuse_if_customized(state: &RegistryState, client_id: &str, force: bool) -> Result<(), String> {
    if force {
        return Ok(());
    }
    let mut list = clients::detect_clients();
    let managed = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .client_managed_entries
        .clone();
    clients::apply_entry_states(&mut list, &managed);
    if list
        .iter()
        .find(|c| c.id == client_id)
        .is_some_and(|c| c.entry_state == clients::GatewayEntryState::Customized)
    {
        return Err(
            "This client's Toolport entry has a custom configuration. Confirm to \
             reset it to the default gateway, or leave it as-is."
                .into(),
        );
    }
    Ok(())
}

/// Install the Toolport gateway into a client (one click "connect to Toolport").
/// `profile` scopes that client to one profile (None = all enabled servers).
/// `transport` is `"stdio"` (default) or `"sharedHttp"` (SOU-407).
/// When the live entry is user-customized, pass `force: true` after the UI confirms
/// overwrite (SOU-406); otherwise the install is refused.
///
/// Runs on the blocking pool, not the GTK main loop (SBS-818): the shared-HTTP
/// path reaches the vault through `ensure_client_http_token`, and a Secret
/// Service call is a synchronous D-Bus round trip that stalls window controls
/// and the tray menu on a slow or locked keyring (SBS-813, SBS-812). Reading the
/// client's config in `refuse_if_customized` and writing it in `clients::*` are
/// blocking file IO for the same reason.
#[tauri::command]
async fn install_gateway(
    app: AppHandle,
    client_id: String,
    profile: Option<String>,
    force: Option<bool>,
    transport: Option<String>,
) -> Result<clients::WriteOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        let bridge = app.state::<HttpBridgeState>();
        refuse_if_customized(state.inner(), &client_id, force.unwrap_or(false))?;
        let transport = transport
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("stdio");
        let outcome = if transport.eq_ignore_ascii_case("sharedHttp")
            || transport.eq_ignore_ascii_case("shared_http")
        {
            // Ensure the supervised bridge is up, mint/reuse a per-client bearer, write
            // native remote or mcp-remote into the client config (SOU-407).
            let status = start_http_bridge_at(bridge.inner(), None)?;
            let port = status.port.unwrap_or(8765);
            let url = format!("http://127.0.0.1:{port}/mcp");
            let token = ensure_client_http_token(state.inner(), &client_id, profile.as_deref())?;
            let spec = clients::SharedHttpSpec { url, token };
            clients::install_gateway_shared_http(&client_id, profile.as_deref(), &spec)?
        } else {
            clients::install_gateway(&client_id, profile.as_deref())?
        };
        // Record the scope we just wrote into the client's config, so the UI can show
        // and re-apply this client's effective scope without re-reading the config.
        // A concrete profile is stored by name; "no profile" is recorded as an
        // explicit-unscoped marker (not a removal) so a running gateway drops its old
        // scope live instead of falling back to its boot-time CONDUIT_PROFILE. The client
        // config was already written above (outside the lock); only the registry record
        // goes through the locked load-modify-save.
        let scope: Option<String> = profile
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let managed = outcome.managed.clone();
        write_registry(state.inner(), |reg| {
            match scope.as_deref() {
                Some(p) => reg.set_client_scope(&client_id, Some(p)),
                None => reg.set_client_unscoped(&client_id),
            }
            if let Some(m) = managed {
                reg.set_client_managed_entry(&client_id, m);
            }
            Ok(())
        })?;
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("install task join failed: {e}"))?
}

/// Mint or reuse a bearer token for a managed shared-HTTP client install.
/// Plaintext lives in the OS keychain; only the hash is in `http_clients`.
///
/// When reusing a vaulted token, rewrite the row's `profile` if the caller asked
/// for a different scope — Shared HTTP scope is derived solely from that row
/// (WS3-5), not from `client_scopes` / TOOLPORT_PROFILE env.
fn ensure_client_http_token(
    state: &RegistryState,
    client_id: &str,
    profile: Option<&str>,
) -> Result<String, String> {
    ensure_client_http_token_with(state, client_id, profile, crate::secrets::get_secret_result)
}

/// [`ensure_client_http_token`] with the vault read injected.
///
/// The vault server id is fixed here, so the reserved-namespace trick the other
/// fail-closed tests use cannot reach it. Injecting the read is how
/// [`revoke_client_http_token_with`] solves the same problem in this file.
fn ensure_client_http_token_with(
    state: &RegistryState,
    client_id: &str,
    profile: Option<&str>,
    read_vaulted_token: impl FnOnce(&str, &str) -> Result<Option<String>, String>,
) -> Result<String, String> {
    const VAULT_SERVER: &str = "__toolport_http_clients__";
    let http_id = format!("client:{client_id}");
    let desired_profile = profile.unwrap_or("").trim().to_string();
    // Reuse vaulted token when we still have a matching http_clients row.
    //
    // A failed vault READ must not fall through to minting a replacement. `get_secret`
    // collapses a read error into `None`, so a locked or flaky keychain looked exactly
    // like "this client has no bearer yet": the mint path below then overwrites the
    // vaulted copy AND `retain`s the client's `http_clients` row away for a new one.
    // The bearer the client is already configured with now hashes to no row, so every
    // request it makes 401s until the user reconnects that client by hand. Only a
    // confirmed "nothing vaulted" may mint (SBS-840 class).
    if let Some(existing) = read_vaulted_token(VAULT_SERVER, client_id)
        // Complete sentence, capitalized: `install_gateway`'s caller renders this
        // verbatim in a toast (`ClientDetail.tsx` `toastError(`${e}`)`), so a bare
        // lowercase fragment would reach the user untethered.
        .map_err(|e| format!("Could not read the saved token for {client_id}: {e}"))?
    {
        let hash = registry::sha256_hex(&existing);
        let mut matched = false;
        let mut profile_stale = false;
        {
            let reg = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(row) = reg
                .http_clients
                .iter()
                .find(|c| c.id == http_id && c.token_sha256 == hash)
            {
                matched = true;
                profile_stale = row.profile != desired_profile;
            }
        }
        if matched {
            if profile_stale {
                write_registry(state, |reg| {
                    if let Some(row) = reg
                        .http_clients
                        .iter_mut()
                        .find(|c| c.id == http_id && c.token_sha256 == hash)
                    {
                        row.profile = desired_profile;
                    }
                    Ok(())
                })?;
            }
            return Ok(existing);
        }
    }
    let token = random_hex()?;
    crate::secrets::set_secret(VAULT_SERVER, client_id, &token)?;
    write_registry(state, |reg| {
        reg.http_clients.retain(|c| c.id != http_id);
        reg.http_clients.push(registry::HttpClient {
            id: http_id,
            label: format!("Client: {client_id}"),
            token_sha256: registry::sha256_hex(&token),
            profile: desired_profile,
        });
        Ok(())
    })?;
    Ok(token)
}

/// Drop the managed shared-HTTP bearer for this client (registry row, then vault).
///
/// `delete_secret` already treats a missing vault entry as success (WS3-4), so
/// Disconnect is not blocked when the bearer was never stored. A real vault or
/// `http_clients` persist failure is returned so uninstall cannot report Ok
/// while the token is still live (SBS-845).
fn revoke_client_http_token(state: &RegistryState, client_id: &str) -> Result<(), String> {
    revoke_client_http_token_with(state, client_id, crate::secrets::delete_secret)
}

/// [`revoke_client_http_token`] with the vault delete injected.
///
/// Split out because the real vault is not reachable on every platform the
/// tests run on: headless Linux CI has no Secret Service, and the macOS
/// data-protection keychain needs a signed build (the same reason
/// `secrets::tests::set_get_delete_round_trip` is `ignore`d there). A test that
/// called it would assert on the machine rather than on this function, so tests
/// pass a stub while production passes [`crate::secrets::delete_secret`].
fn revoke_client_http_token_with(
    state: &RegistryState,
    client_id: &str,
    delete_vaulted_token: impl FnOnce(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    const VAULT_SERVER: &str = "__toolport_http_clients__";
    let http_id = format!("client:{client_id}");
    // Drop the registry row FIRST. `resolve_http_caller` authenticates a bearer
    // through `http_client_for_token`, which matches the row's `token_sha256`;
    // the vault copy is never consulted on the auth path. So the row is the
    // thing that grants access, and removing it revokes the bearer even if the
    // vault step below then fails. The reverse order fails open: a vault error
    // would return early with the row still registered and the bearer still
    // authenticating. An orphaned vault entry is a hygiene problem; a live
    // bearer after a failed revoke is a security one.
    write_registry(state, |reg| {
        reg.http_clients.retain(|c| c.id != http_id);
        Ok(())
    })?;
    // A failed persist above means nothing was revoked, so the vault copy is
    // deliberately left alone: the bearer it belongs to is still registered.
    // Past this point the bearer is already dead, but a vault failure is still
    // returned, because uninstall must not report a clean Disconnect while a
    // copy of the token is left on the machine.
    delete_vaulted_token(VAULT_SERVER, client_id)?;
    Ok(())
}

/// Remove the Toolport gateway from a client.
///
/// Runs on the blocking pool for the same reason as [`install_gateway`]
/// (SBS-818): `revoke_client_http_token` deletes the vaulted bearer, which is a
/// synchronous Secret Service round trip on Linux.
#[tauri::command]
async fn uninstall_gateway(
    app: AppHandle,
    client_id: String,
) -> Result<clients::WriteOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        let outcome = clients::uninstall_gateway(&client_id)?;
        // Config entry is gone; also revoke the bridge bearer so a leftover backup or
        // stale token cannot keep authenticating (WS3-4). A vault or registry
        // failure must fail Disconnect — the bearer is still live (SBS-845).
        revoke_client_http_token(state.inner(), &client_id)?;
        write_registry(state.inner(), |reg| {
            reg.set_client_scope(&client_id, None);
            reg.clear_client_managed_entry(&client_id);
            Ok(())
        })?;
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("uninstall task join failed: {e}"))?
}

/// 24 random bytes (192 bits) as hex, for a bearer token or a unique id.
fn random_hex() -> Result<String, String> {
    let mut buf = [0u8; 24];
    getrandom::getrandom(&mut buf).map_err(|e| format!("could not generate randomness: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AddedHttpClient {
    registry: Registry,
    /// The plaintext bearer token. Shown once; only its SHA-256 is stored.
    token: String,
}

/// Register an HTTP-bridge client: generate a bearer token, store its hash plus
/// the chosen scope, and return the plaintext token once. The client pastes the
/// token as its API key; the multi-tenant bridge resolves it to this profile per
/// request, so several HTTP clients on one bridge get different server sets.
#[tauri::command]
fn add_http_client(
    state: State<RegistryState>,
    label: String,
    profile: Option<String>,
) -> Result<AddedHttpClient, String> {
    let token = random_hex()?;
    let id = random_hex()?;
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.http_clients.push(registry::HttpClient {
            id,
            label: label.trim().to_string(),
            token_sha256: registry::sha256_hex(&token),
            profile: profile.unwrap_or_default().trim().to_string(),
        });
        Ok(())
    })?;
    Ok(AddedHttpClient {
        registry: reg,
        token,
    })
}

/// Remove a registered HTTP-bridge client (revokes its token).
#[tauri::command]
fn remove_http_client(state: State<RegistryState>, id: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.http_clients.retain(|c| c.id != id);
        Ok(())
    })?;
    Ok(reg)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrateResult {
    registry: Registry,
    /// How many of the client's servers were newly imported into Toolport.
    imported: usize,
    /// Names of the servers moved out of the client's config.
    moved: Vec<String>,
}

/// Import the servers a client directly manages before its config is replaced
/// with the Toolport gateway. Gateway identities must be skipped before they
/// reach `moved`, otherwise migration reports moving a server it never imported.
fn import_client_servers_for_migration(
    reg: &mut Registry,
    client: &clients::DetectedClient,
) -> (usize, Vec<String>) {
    let mut imported = 0;
    let mut moved = Vec::new();
    for server in &client.servers {
        if clients::detected_is_gateway(server) {
            continue;
        }
        moved.push(server.name.clone());
        let exists = reg
            .servers
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(&server.name));
        if !exists {
            reg.add_server(server_from_detected(server, &client.id));
            imported += 1;
        }
    }
    (imported, moved)
}

/// Migrate a client to Toolport: import its directly-configured servers into the
/// registry, then rewrite the client's config to contain only the Toolport
/// gateway (optionally scoped to `profile`). The client is left managing nothing
/// directly - everything routes through Toolport. Backs the config up first.
///
/// Plugin servers (read-only, outside the config file) are left untouched.
/// When the live gateway entry is user-customized, pass `force: true` after the
/// UI confirms overwrite (SOU-406); otherwise migration is refused before any
/// config rewrite.
#[tauri::command]
async fn migrate_client(
    state: State<'_, RegistryState>,
    bridge: State<'_, HttpBridgeState>,
    client_id: String,
    profile: Option<String>,
    force: Option<bool>,
    transport: Option<String>,
) -> Result<MigrateResult, String> {
    // Guard before import or rewrite so a hand-edited gateway entry is not wiped.
    refuse_if_customized(state.inner(), &client_id, force.unwrap_or(false))?;

    let detected = tauri::async_runtime::spawn_blocking(clients::detect_clients)
        .await
        .map_err(|e| e.to_string())?;
    let client = detected
        .into_iter()
        .find(|c| c.id == client_id)
        .ok_or_else(|| format!("Unknown client '{client_id}'"))?;

    // Import the client's servers under the lock (a fresh load-modify-save).
    let (_, (imported, moved)) = write_registry(state.inner(), |reg| {
        Ok(import_client_servers_for_migration(reg, &client))
    })?;

    // Rewrite the client to only the gateway (backs up first). Honor transport so
    // migrate does not silently force stdio when the UI chose Shared HTTP (WS3-2).
    let transport = transport
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("stdio");
    let migrate_write = if transport.eq_ignore_ascii_case("sharedHttp")
        || transport.eq_ignore_ascii_case("shared_http")
    {
        let status = start_http_bridge_at(bridge.inner(), None)?;
        let port = status.port.unwrap_or(8765);
        let url = format!("http://127.0.0.1:{port}/mcp");
        let token = ensure_client_http_token(state.inner(), &client_id, profile.as_deref())?;
        let spec = clients::SharedHttpSpec { url, token };
        clients::migrate_to_gateway_with_transport(&client_id, profile.as_deref(), Some(&spec))?
    } else {
        clients::migrate_to_gateway(&client_id, profile.as_deref())?
    };

    // Record the scope now that the client config was rewritten to the gateway.
    // "No profile" becomes an explicit-unscoped marker (not a removal) so a live
    // re-scope to "all servers" applies without restarting the client.
    let scope: Option<String> = profile
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let managed = migrate_write.managed;
    let (registry, _) = write_registry(state.inner(), |reg| {
        match scope.as_deref() {
            Some(p) => reg.set_client_scope(&client_id, Some(p)),
            None => reg.set_client_unscoped(&client_id),
        }
        if let Some(m) = managed {
            reg.set_client_managed_entry(&client_id, m);
        }
        Ok(())
    })?;

    Ok(MigrateResult {
        registry,
        imported,
        moved,
    })
}

/// Store a secret env value in the OS keychain and mark it on the server entry
/// (the value itself never enters the registry file).
#[tauri::command]
async fn set_secret(
    app: AppHandle,
    server_id: String,
    key: String,
    value: String,
) -> Result<Registry, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        // Serialize the whole keychain+registry pair, not just the registry half.
        // These commands used to be synchronous and therefore ran one at a time on
        // the GTK main loop; on the blocking pool they are genuinely concurrent, so
        // a `delete_secret` landing between this keychain write and the registry
        // write below would leave the registry advertising a secret the keychain no
        // longer holds. Same keyed lock `set_auth_token` already uses.
        let _mutation = acquire_auth_mutation_lock(&server_id)?;
        // Keychain write first (external to the registry, so outside the lock), then record
        // that the secret exists + bump the generation on the FRESH value under the lock.
        secrets::set_secret(&server_id, &key, &value)?;
        let (reg, _) = write_registry(state.inner(), |reg| {
            if let Some(server) = reg.servers.iter_mut().find(|s| s.id == server_id) {
                match server.env.iter_mut().find(|e| e.key == key) {
                    Some(ev) => {
                        ev.secret = true;
                        ev.value = None;
                    }
                    None => server.env.push(registry::EnvVar {
                        key,
                        value: None,
                        secret: true,
                    }),
                }
            }
            reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
            Ok(())
        })?;
        Ok(reg)
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// Remove a secret from the keychain and drop the env var from the server entry.
#[tauri::command]
async fn delete_secret(
    app: AppHandle,
    server_id: String,
    key: String,
) -> Result<Registry, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        // Held across both halves: see the note in `set_secret`.
        let _mutation = acquire_auth_mutation_lock(&server_id)?;
        secrets::delete_secret(&server_id, &key)?;
        let (reg, _) = write_registry(state.inner(), |reg| {
            if let Some(server) = reg.servers.iter_mut().find(|s| s.id == server_id) {
                server.env.retain(|e| e.key != key);
            }
            reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
            Ok(())
        })?;
        Ok(reg)
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// Did the user supply a new client secret, and what exactly should be stored?
///
/// Whitespace decides only whether the field was left blank ("keep the stored
/// one"). The value itself is stored VERBATIM: generated secrets are opaque, and
/// one that legitimately begins or ends with whitespace would otherwise be
/// silently altered, rejected by the token endpoint as `invalid_client`, and
/// impossible to correct through the UI since the field cannot be read back.
fn supplied_secret(input: Option<String>) -> Option<String> {
    input.filter(|s| !s.trim().is_empty())
}

/// Configure the headless OAuth client-credentials flow for an HTTP server
/// (SBS-524).
///
/// The secret goes to the keychain; only the non-secret client id, auth method
/// and scopes are written to the registry. Deliberately not routed through
/// [`set_secret`], which records an env var: this credential is not an env var,
/// and surfacing it as one would put it in the server's environment listing.
#[tauri::command]
async fn set_client_credentials(
    app: AppHandle,
    server_id: String,
    client_id: String,
    client_secret: Option<String>,
    token_endpoint_auth_method: Option<String>,
    scope: Option<String>,
) -> Result<Registry, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        set_client_credentials_blocking(&state, server_id, client_id, client_secret, token_endpoint_auth_method, scope)
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

fn set_client_credentials_blocking(
    state: &RegistryState,
    server_id: String,
    client_id: String,
    client_secret: Option<String>,
    token_endpoint_auth_method: Option<String>,
    scope: Option<String>,
) -> Result<Registry, String> {
    // The reset/registry/keychain ordering below is deliberate, and on the
    // blocking pool a concurrent `clear_client_credentials` can interleave with
    // it -- clearing the registry entry before this call's final keychain write,
    // which would strand a client secret with nothing pointing at it. Taken here
    // rather than in the command so a direct caller cannot skip it.
    let _mutation = acquire_auth_mutation_lock(&server_id)?;
    let client_id = client_id.trim().to_string();
    if client_id.is_empty() {
        return Err("a client id is required for client-credentials auth".into());
    }
    // Trim at the boundary, not just in the UI. `ClientAuthMethod::parse` trims
    // before matching, so an untrimmed method would validate here and then be
    // persisted with the whitespace still on it; scope is sent to the token
    // endpoint verbatim, where padding can be rejected.
    let blank_to_none =
        |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let token_endpoint_auth_method = blank_to_none(token_endpoint_auth_method);
    let scope = blank_to_none(scope);
    // Reject an unknown method here rather than at connect time, so a typo is a
    // dialog error instead of a failed connection later.
    if let Some(method) = token_endpoint_auth_method.as_deref() {
        // Unusable as well as unrecognised. `private_key_jwt` parses but is not
        // implemented, so persisting it produces a server that fails closed at
        // every connect. Team import already refuses it; this is the same rule at
        // the other entry point.
        if !oauth::ClientAuthMethod::parse(method).is_some_and(|m| m.is_implemented()) {
            return Err(format!(
                "unsupported token endpoint auth method {method:?}; expected                  client_secret_basic or client_secret_post (private_key_jwt is                  not implemented yet, see SBS-599)"
            ));
        }
    }
    // Validate the id BEFORE touching the keychain. Writing first and erroring
    // afterwards would leave an orphaned secret with nothing referencing it and
    // no path to clean it up. The closure below re-checks under the lock, which
    // is what actually guarantees consistency; this is purely so the common
    // typo case cannot leave a credential behind.
    {
        let reg = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !reg.servers.iter().any(|s| s.id == server_id) {
            return Err(format!("no server with id {server_id:?}"));
        }
    }
    // An empty secret means "keep the vaulted one", so editing scopes does not
    // require re-entering the credential. Resolve it here but do not write yet.
    let secret_to_store = supplied_secret(client_secret);
    if secret_to_store.is_none()
        && secrets::get_secret_result(&server_id, secrets::CLIENT_SECRET_KEY)?.is_none()
    {
        return Err("no client secret is stored for this server yet; enter one".into());
    }
    // Any config change invalidates a token minted under the old settings.
    remote::reset_client_credentials(&server_id)?;

    let (reg, _) = write_registry(state, |reg| {
        // Fail loudly on an unknown id. The keychain write above already happened,
        // so silently skipping the registry half would leave a stored secret with
        // no configuration pointing at it, and report success.
        let Some(server) = reg.servers.iter_mut().find(|s| s.id == server_id) else {
            return Err(format!("no server with id {server_id:?}"));
        };
        let mut existing = server.client_credentials.take().unwrap_or_default();
        // Preserve a newer build's fields, but never a credential smuggled in
        // through them -- e.g. a `clientSecret` hand-written into registry.json,
        // which saving here would otherwise re-persist and hand to team export.
        existing.strip_secret_fields();
        server.client_credentials = Some(registry::ClientCredentials {
            client_id: client_id.clone(),
            token_endpoint_auth_method: token_endpoint_auth_method.clone(),
            scope: scope.clone(),
            unknown_fields: existing.unknown_fields,
        });
        reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
        Ok(())
    })?;
    // Secret LAST, so no earlier failure can leave one vaulted with nothing
    // referencing it and no way to reach it from the UI. If this write is the
    // thing that fails, the config exists without a secret, which surfaces at
    // connect as "no client secret is vaulted" and can simply be retried -- a
    // visible, recoverable state rather than an invisible orphan.
    if let Some(secret) = secret_to_store {
        secrets::set_secret(&server_id, secrets::CLIENT_SECRET_KEY, &secret)?;
    }
    Ok(reg)
}

/// Remove client-credentials auth from a server: the vaulted secret, the minted
/// access token, and the registry config.
#[tauri::command]
async fn clear_client_credentials(
    app: AppHandle,
    server_id: String,
) -> Result<Registry, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        clear_client_credentials_blocking(&state, server_id)
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

fn clear_client_credentials_blocking(
    state: &RegistryState,
    server_id: String,
) -> Result<Registry, String> {
    // Paired with `set_client_credentials_blocking`: see the note there.
    let _mutation = acquire_auth_mutation_lock(&server_id)?;
    // Reset first, so a failure here preserves the credential and the removal can
    // be retried.
    remote::reset_client_credentials(&server_id)?;
    // Then the config, and only then the secret. This is the OPPOSITE order to
    // `set_client_credentials`, deliberately: there, a stranded secret is one the
    // user still has in hand, so failing visibly is the better trade. Here the
    // secret may be unrecoverable -- it is never shown again and may have to be
    // re-issued by the authorization server -- so it must not be destroyed until
    // the config is durably gone. If the delete is what fails, the result is a
    // stale keychain entry with nothing pointing at it, and Remove can be run
    // again; that is strictly better than losing a credential the user cannot get
    // back.
    let (reg, _) = write_registry(state, |reg| {
        let Some(server) = reg.servers.iter_mut().find(|s| s.id == server_id) else {
            return Err(format!("no server with id {server_id:?}"));
        };
        server.client_credentials = None;
        reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
        Ok(())
    })?;
    secrets::delete_secret(&server_id, secrets::CLIENT_SECRET_KEY)?;
    Ok(reg)
}

/// Whether a client secret is vaulted for this server, so the UI can show
/// "configured" without ever reading the value back.
#[tauri::command]
async fn has_client_secret(server_id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(secrets::get_secret_result(&server_id, secrets::CLIENT_SECRET_KEY)?.is_some())
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// The most recent tool-call audit entries (newest first).
#[tauri::command]
async fn get_audit_log(limit: usize) -> Result<Vec<serde_json::Value>, String> {
    // Async, like every polled reader here: Activity invokes these every few
    // seconds, and as sync commands the file reads ran on the GTK main loop,
    // where a large log made window controls intermittently dead on Linux
    // (SBS-813). An unreadable log or a join failure must reject so Activity
    // can show error/retry instead of "No tool calls yet" (SBS-873).
    tauri::async_runtime::spawn_blocking(move || {
        audit::read_recent(limit).map_err(|e| format!("Couldn't read the activity log: {e}"))
    })
    .await
    .map_err(|e| format!("activity log task join failed: {e}"))?
}

/// Aggregate the full retained audit log into per-server call/error/latency stats for
/// the observability dashboard. Bounded by the log's byte cap, so totals are real.
#[tauri::command]
async fn audit_stats() -> Result<serde_json::Value, String> {
    // Rejects on the same unreadable log that makes `get_audit_log` reject. A
    // `null` here only hides the dashboard, which reads as "no data" rather
    // than "the read failed" (SBS-873).
    tauri::async_runtime::spawn_blocking(|| {
        audit::stats().map_err(|e| format!("Couldn't read the activity log: {e}"))
    })
    .await
    .map_err(|e| format!("activity stats task join failed: {e}"))?
}

/// Recent tool-definition integrity events (newest first): a previously-approved
/// tool whose definition changed (rug-pull signal) or a known server that added a
/// tool. Powers the in-app security notices.
#[tauri::command]
async fn get_security_events(limit: usize) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        integrity::read_recent(limit).map_err(|e| format!("Couldn't read security events: {e}"))
    })
    .await
    .map_err(|e| format!("security events task join failed: {e}"))?
}

/// Cumulative tool-definition tokens that lazy discovery has kept out of clients'
/// context, summed from the local savings log for the in-app counter.
#[tauri::command]
async fn savings_summary() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(savings::summary)
        .await
        .unwrap_or(serde_json::Value::Null)
}

/// How many trailing gateway-log lines the diagnostics bundle includes.
const DIAG_LOG_LINES: usize = 200;

/// A shareable diagnostics blob for bug reports: Toolport version + OS, a
/// secrets-stripped registry summary, and the tail of the always-on gateway log.
/// Safe to paste into a public issue, secret values live in the OS keychain and
/// are never included; env vars are listed by key name only.
#[tauri::command]
async fn gather_diagnostics() -> String {
    tauri::async_runtime::spawn_blocking(gather_diagnostics_blocking)
        .await
        .unwrap_or_else(|e| format!("diagnostics task join failed: {e}"))
}

fn gather_diagnostics_blocking() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Toolport diagnostics");
    let _ = writeln!(out, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "os: {} {}", std::env::consts::OS, std::env::consts::ARCH);

    // A load failure is exactly what a bug report needs to surface, not a
    // silently-empty registry from unwrap_or_default.
    match registry::load() {
        Ok(reg) => out.push_str(&registry_summary(&reg)),
        Err(e) => {
            let _ = writeln!(out, "\nregistry: failed to load: {e}");
        }
    }

    let _ = writeln!(out, "\ngateway log (last {DIAG_LOG_LINES} lines):");
    out.push_str(&gateway_log_tail(DIAG_LOG_LINES));
    out
}

/// Format the registry for a diagnostics bundle: settings, servers (on/off plus
/// launch target), and profiles. Secret-safe: env vars are listed by key name
/// only (with a `(secret)` marker), never their values.
fn registry_summary(reg: &Registry) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let active = reg.active_profile_id();
    let _ = writeln!(out, "\nsettings:");
    let _ = writeln!(out, "  lazy discovery: {}", reg.lazy_discovery);
    let global_mode = reg
        .discovery_mode
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| {
            if reg.lazy_discovery {
                "lazy".into()
            } else {
                "full".into()
            }
        });
    let _ = writeln!(out, "  discovery mode: {global_mode} (global)");
    if !reg.client_discovery.is_empty() {
        let mut overrides: Vec<String> = reg
            .client_discovery
            .iter()
            .map(|(id, mode)| format!("{id}={mode}"))
            .collect();
        overrides.sort();
        let _ = writeln!(out, "  per-client discovery: {}", overrides.join(", "));
    }
    let _ = writeln!(out, "  deny destructive: {}", reg.deny_destructive);
    let _ = writeln!(out, "  active profile: {active}");

    let _ = writeln!(out, "\nservers ({}):", reg.servers.len());
    for s in &reg.servers {
        let on = if reg.is_enabled(&active, &s.id) { "on" } else { "off" };
        let target = match (&s.command, &s.url) {
            (Some(cmd), _) => safe_command_target(cmd, &s.args),
            (None, Some(url)) => redact_url_userinfo(url),
            _ => String::new(),
        };
        let _ = writeln!(out, "  [{on}] {} ({}) {}", s.id, s.transport, target);
        if !s.env.is_empty() {
            let keys: Vec<String> = s
                .env
                .iter()
                .map(|e| {
                    if e.secret {
                        format!("{} (secret)", e.key)
                    } else {
                        e.key.clone()
                    }
                })
                .collect();
            let _ = writeln!(out, "        env: {}", keys.join(", "));
        }
    }

    let _ = writeln!(out, "\nprofiles ({}):", reg.profiles.len());
    for p in &reg.profiles {
        let _ = writeln!(out, "  {}: [{}]", p.name, p.enabled_server_ids.join(", "));
    }
    out
}

fn safe_command_target(cmd: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(redact_arg_for_sharing(cmd));
    parts.extend(args.iter().map(|arg| redact_arg_for_sharing(arg)));
    parts.join(" ").trim().to_string()
}

fn redact_arg_for_sharing(arg: &str) -> String {
    if arg_looks_secret(arg) {
        "<redacted>".to_string()
    } else {
        redact_url_userinfo(arg)
    }
}

/// The last `n` lines of the always-on gateway log, or a friendly note when it
/// hasn't been written yet (no client has connected through the gateway).
fn gateway_log_tail(n: usize) -> String {
    let Some(path) = registry::gateway_log_path() else {
        return "(log path unavailable)\n".to_string();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => last_lines(&text, n),
        _ => "(no gateway log yet, connect a client to populate it)\n".to_string(),
    }
}

/// The last `n` lines of `text`, newline-terminated. Returns everything when the
/// text has fewer than `n` lines.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut tail = lines[start..].join("\n");
    if !tail.is_empty() {
        tail.push('\n');
    }
    tail
}

/// How long to wait for one server's probe before giving up on it. Generous
/// enough for an `npx` first-run package install, but bounded so a single hung
/// server can't leave its row "checking" forever. Issue #252.
const PROBE_TIMEOUT: Duration = Duration::from_secs(90);

/// Probe one server, never blocking longer than `PROBE_TIMEOUT`. On timeout the
/// underlying probe thread is left to finish or die on its own; we return a
/// timed-out result so the row resolves instead of spinning indefinitely.
fn probe_one_bounded(server: &ServerEntry) -> ProbeResult {
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let _ = tx.send(probe_one(&s));
    });
    rx.recv_timeout(PROBE_TIMEOUT).unwrap_or_else(|_| ProbeResult {
        server_id: server.id.clone(),
        ok: false,
        tool_count: 0,
        error: Some(format!("timed out after {}s", PROBE_TIMEOUT.as_secs())),
        auth_required: false,
    })
}

/// Connect to each enabled server in the active profile and report health + tool
/// count. Emits a `server-probed` event per server the moment it finishes, so the
/// UI resolves each row independently instead of waiting for the slowest - a cold
/// `npx` install used to leave the whole grid "checking" for 30-60s. Still returns
/// the full batch for callers that want it. Issue #252.
#[tauri::command]
async fn probe_servers(
    app: tauri::AppHandle,
    state: State<'_, RegistryState>,
) -> Result<Vec<ProbeResult>, String> {
    // Snapshot which servers to probe, then drop the lock before any I/O.
    let servers: Vec<ServerEntry> = {
        let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.enabled_servers()
            .into_iter()
            .filter(|s| !clients::is_gateway_server(s))
            .cloned()
            .collect()
    };
    // One worker thread per server. Each emits its result as soon as it's ready
    // (the UI listens for `server-probed`), then contributes to the returned batch.
    tauri::async_runtime::spawn_blocking(move || {
        let handles: Vec<_> = servers
            .into_iter()
            .map(|s| {
                let app = app.clone();
                std::thread::spawn(move || {
                    let result = probe_one_bounded(&s);
                    let _ = app.emit("server-probed", &result);
                    result
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Snapshot one server out of the registry by id (dropping the lock before I/O).
/// Playground connects must not spawn a team-review server the member has not
/// enabled — the Teams confirm is otherwise frontend-only.
fn playground_server(state: &RegistryState, server_id: &str) -> Result<ServerEntry, String> {
    let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = reg
        .servers
        .iter()
        .find(|s| s.id == server_id)
        .cloned()
        .ok_or_else(|| format!("server '{server_id}' not found"))?;
    if server.needs_team_enable_review() {
        let pid = reg.active_profile_id();
        if !reg.is_enabled(&pid, &server.id) {
            return Err(
                "this team server runs a local command or private address; enable it from Teams after review"
                    .into(),
            );
        }
    }
    Ok(server)
}

/// List the tools one server exposes (raw MCP tool objects: name, description,
/// inputSchema). Connects on demand and disconnects when the connection drops.
/// Powers the tool playground's tool picker.
#[tauri::command]
async fn list_server_tools(
    state: State<'_, RegistryState>,
    server_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || connect_server(&server).map(|ds| ds.tools))
        .await
        .map_err(|e| e.to_string())?
}

/// Invoke one tool on a server with the given arguments and return its raw MCP
/// result (`{ content, isError }`). Connects on demand and records the call in
/// the audit log, just like a call routed through the gateway.
#[tauri::command]
async fn call_tool(
    state: State<'_, RegistryState>,
    server_id: String,
    tool: String,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut ds = connect_server(&server)?;
        let started = std::time::Instant::now();
        let result = ds.call(&tool, arguments).map_err(|e| e.to_string());
        let ms = started.elapsed().as_millis() as u64;
        // Mirror the gateway's success accounting: a result with isError=true is
        // a failed call even though the transport round-tripped fine.
        let ok = result
            .as_ref()
            .map(|r| !r.get("isError").and_then(|v| v.as_bool()).unwrap_or(false))
            .unwrap_or(false);
        // A transport error carries its own message; capture it so Activity can
        // show why a playground call failed, not just that it did.
        let err = result.as_ref().err().map(|e| e.to_string());
        // The in-app tool playground: a local action by the desktop user, so it's
        // unattributed (client identity is only meaningful for registered HTTP clients).
        audit::record_timed(&server.id, &tool, ok, Some(ms), err.as_deref(), None);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// List the resources a server advertises (uri, name, mimeType). Connects on
/// demand; empty if the server declares no resources capability. Powers the
/// playground's Resources tab.
#[tauri::command]
async fn list_server_resources(
    state: State<'_, RegistryState>,
    server_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        connect_server(&server).map(|mut ds| {
            ds.load_resources_prompts();
            ds.resources
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// List the prompts a server advertises (name, description, arguments). Connects
/// on demand; empty if the server declares no prompts capability. Powers the
/// playground's Prompts tab.
#[tauri::command]
async fn list_server_prompts(
    state: State<'_, RegistryState>,
    server_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        connect_server(&server).map(|mut ds| {
            ds.load_resources_prompts();
            ds.prompts
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Read one resource by its uri and return the raw MCP result (`{ contents }`).
/// Connects on demand. Playground.
#[tauri::command]
async fn read_resource(
    state: State<'_, RegistryState>,
    server_id: String,
    uri: String,
) -> Result<serde_json::Value, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut ds = connect_server(&server)?;
        ds.read_resource(&uri).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Get one prompt by name with arguments, returning the raw MCP result
/// (`{ messages }`). Connects on demand. Playground.
#[tauri::command]
async fn get_prompt(
    state: State<'_, RegistryState>,
    server_id: String,
    name: String,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let server = playground_server(state.inner(), &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut ds = connect_server(&server)?;
        ds.get_prompt(&name, arguments).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Enable or disable a single tool on a server. The gateway hides disabled
/// tools from `tools/list` and rejects calls to them; the change propagates live
/// via the registry watcher. Returns the updated registry.
#[tauri::command]
fn set_tool_enabled(
    state: State<RegistryState>,
    server_id: String,
    tool: String,
    enabled: bool,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_tool_enabled(&server_id, &tool, enabled)
    })?;
    Ok(reg)
}

/// Pin (or unpin) a tool as a lazy-discovery prerequisite: search always surfaces a
/// pinned tool with its full schema, regardless of the query's match score, so a
/// load-bearing tool is never hidden. Propagates live via the registry watcher.
#[tauri::command]
fn set_tool_pinned(
    state: State<RegistryState>,
    server_id: String,
    tool: String,
    pinned: bool,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_tool_pinned(&server_id, &tool, pinned);
        Ok(())
    })?;
    Ok(reg)
}

/// Flip the global destructive-tool deny switch. When on, the gateway hides and
/// blocks every tool annotated `destructiveHint: true` across all servers.
#[tauri::command]
fn set_deny_destructive(state: State<RegistryState>, deny: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_deny_destructive(deny);
        Ok(())
    })?;
    Ok(reg)
}

/// Toggle per-call confirmation for destructive tools. When enabled, the gateway
/// intercepts each destructive tool call, returns a preview with a token, and
/// requires `conduit_confirm { token }` to proceed. Mutually exclusive with
/// `deny_destructive` (confirm turns deny off).
#[tauri::command]
fn set_confirm_destructive(state: State<RegistryState>, confirm: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_confirm_destructive(confirm);
        Ok(())
    })?;
    Ok(reg)
}

/// Toggle human-in-the-loop approval. When on, a gated tool call (destructive, or from an
/// untrusted-provenance server) is HELD until a person approves or denies it in the app,
/// via the approval broker. Distinct from confirm-destructive (which the agent re-confirms).
#[tauri::command]
fn set_human_approval(state: State<RegistryState>, on: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_human_approval(on);
        Ok(())
    })?;
    Ok(reg)
}

/// The tool calls currently held awaiting a human decision (for the Pending Approvals UI).
/// Polled by the frontend; the `approval-pending` / `approval-resolved` events prompt a refresh.
#[tauri::command]
fn list_pending_approvals(
    broker: State<approval_broker::ApprovalBroker>,
) -> Vec<approval_broker::PendingView> {
    broker.list()
}

/// Approve or deny a held tool call by id. The parked gateway call then runs (approve) or is
/// refused (deny). `scope` (on approve) controls whether future calls to the same tool skip
/// the prompt: `once` (default, remember nothing), `session` (until the app restarts), or
/// `always` (persisted). `Err` if the id is unknown (already resolved or timed out).
#[tauri::command]
fn decide_approval(
    broker: State<approval_broker::ApprovalBroker>,
    state: State<RegistryState>,
    id: String,
    approved: bool,
    scope: String,
) -> Result<(), String> {
    let view = broker.decide(&id, approved)?;
    if approved && scope != "once" && view.url_elicitation.is_none() {
        // Persist only when we can bind the allow to the current definition
        // fingerprint. If it's unavailable (the tool is no longer resolvable),
        // the call itself already went through via `decide` above; we simply
        // can't remember it, so degrade to a one-time approval rather than
        // returning an error for a decision that already succeeded.
        if let Some(fp) = view.tool_fingerprint.as_deref() {
            let key = approval::fingerprint_allow_key(&view.server, &view.tool, fp);
            broker.add_session_allow(key.clone());
            if scope == "always" {
                write_registry(state.inner(), |reg| {
                    reg.allow_tool(key);
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

/// Strong routine candidates the gateway queued for the passive Settings area.
/// Polled by the frontend; the `routine-suggestion` event prompts a refresh.
#[tauri::command]
fn list_routine_suggestions(
    broker: State<approval_broker::ApprovalBroker>,
) -> Vec<routines::RoutineSuggestion> {
    broker.list_suggestions()
}

/// Persist a queued suggestion. The user's click IS the persistence authorization:
/// the card showed the same disclosure the approval prompt would (name, dependencies,
/// risk, provenance, collapsible source), so no second prompt fires. Everything still
/// passes the store's own validation and the equivalence dedupe, and the routine
/// watcher advertises the result to every client.
#[tauri::command]
fn approve_routine_suggestion(
    broker: State<approval_broker::ApprovalBroker>,
    fingerprint: String,
    name: String,
    description: Option<String>,
) -> Result<routines::RoutineDefinition, String> {
    let suggestion = broker
        .suggestion(&fingerprint)
        .ok_or_else(|| "no queued suggestion with that fingerprint".to_string())?;
    suggestion.validate()?;
    let started = std::time::Instant::now();
    // Equivalent-definition dedupe: an agent-initiated save may have landed the same
    // definition already; treat that as success rather than a duplicate.
    if let Some(existing) = routines::find_by_definition_fingerprint(&fingerprint)? {
        broker.remove_suggestion(&fingerprint);
        return Ok(existing);
    }
    let definition = routines::new_promoted_definition(
        name,
        description.filter(|text| !text.trim().is_empty()),
        suggestion.source,
        suggestion.input_schema,
        suggestion.limits,
        suggestion.evidence,
    )?;
    let saved = routines::append_immutable(definition)?;
    audit::record_routine(
        "save",
        saved.id(),
        saved.content_hash(),
        true,
        Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        Some("app_suggestion"),
        None,
    );
    broker.remove_suggestion(&fingerprint);
    Ok(saved)
}

/// Drop a queued suggestion and keep the same definition out for this app run.
#[tauri::command]
fn dismiss_routine_suggestion(broker: State<approval_broker::ApprovalBroker>, fingerprint: String) {
    broker.dismiss_suggestion(&fingerprint);
}

/// A tool allowed to skip human approval, for the Settings "Allowed tools" list.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AllowedTool {
    key: String,
    server: String,
    tool: String,
    /// true = persisted ("always"); false = only for this app session.
    persistent: bool,
}

/// Tools currently allowed to skip human approval: persistent ("always allow") from the
/// registry, plus this session's temporary allows from the broker.
#[tauri::command]
fn list_allowed_tools(
    state: State<RegistryState>,
    broker: State<approval_broker::ApprovalBroker>,
) -> Vec<AllowedTool> {
    let persistent = {
        let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.human_approval_allow.clone()
    };
    // Only fingerprint-bound `server/tool/<fingerprint>` keys still auto-approve;
    // the broker ignores legacy broad `server/tool` entries, so they must not be
    // surfaced here as active allows (that would misreport an inert entry as live).
    let parse = |key: &str| -> Option<(String, String)> {
        let mut parts = key.splitn(3, '/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(s), Some(t), Some(_fp)) => Some((s.to_string(), t.to_string())),
            _ => None,
        }
    };
    let mut out: Vec<AllowedTool> = persistent
        .iter()
        .filter_map(|k| {
            let (server, tool) = parse(k)?;
            Some(AllowedTool {
                key: k.clone(),
                server,
                tool,
                persistent: true,
            })
        })
        .collect();
    for k in broker.session_allowed() {
        if !persistent.contains(&k) {
            if let Some((server, tool)) = parse(&k) {
                out.push(AllowedTool {
                    key: k,
                    server,
                    tool,
                    persistent: false,
                });
            }
        }
    }
    out
}

/// Revoke an allowed tool (re-require approval): drop it from both the persistent registry
/// list and this session's allowlist.
#[tauri::command]
fn revoke_allowed_tool(
    state: State<RegistryState>,
    broker: State<approval_broker::ApprovalBroker>,
    key: String,
) -> Result<(), String> {
    write_registry(state.inner(), |reg| {
        reg.revoke_tool(&key);
        Ok(())
    })?;
    broker.remove_session_allow(&key);
    Ok(())
}

/// Set (or clear) a per-tool exposure override, keyed by `(server, original tool)`: rename
/// the tool and/or replace its description as clients see it (the latter locally neutralizes
/// a poisoned description). Empty/blank name and description clears the override. The call
/// still routes to the original downstream tool; gateways pick up the change via the registry
/// watcher.
#[tauri::command]
fn set_tool_override(
    state: State<RegistryState>,
    server: String,
    tool: String,
    name: Option<String>,
    description: Option<String>,
) -> Result<Registry, String> {
    let norm = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_tool_override(
            server,
            tool,
            registry::ToolOverride {
                name: norm(name),
                description: norm(description),
            },
        );
        Ok(())
    })?;
    Ok(reg)
}

/// Remove a tool's exposure override, restoring the server's own name and description.
#[tauri::command]
fn clear_tool_override(
    state: State<RegistryState>,
    server: String,
    tool: String,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.clear_tool_override(&server, &tool);
        Ok(())
    })?;
    Ok(reg)
}

/// Toggle live request/response inspection. When enabled, the gateway captures each
/// tool call's args + result into a small, separate, ephemeral local ring
/// (`inspect.jsonl`, last 50 calls, each body size-capped) that the Activity view can
/// show. Off by default; the governance audit log is never touched by this. Turning
/// it off in the UI should also clear the ring (see `clear_inspect_log`).
#[tauri::command]
fn set_live_inspect(state: State<RegistryState>, enabled: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_live_inspect(enabled);
        Ok(())
    })?;
    Ok(reg)
}

/// The most recent live-inspection captures (newest first): each tool call's args and
/// result, only present while live inspection has been on. Empty when off/unused.
#[tauri::command]
async fn get_inspect_log(limit: usize) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        inspect::read_recent(limit).map_err(|e| format!("Couldn't read the inspector log: {e}"))
    })
    .await
    .map_err(|e| format!("inspector log task join failed: {e}"))?
}

/// Clear the live-inspection ring (delete `inspect.jsonl`), so no captured args/results
/// linger. Called when the user turns live inspection off. Surfaces a real removal
/// failure so the UI never confirms a delete that did not happen.
#[tauri::command]
fn clear_inspect_log() -> Result<(), String> {
    inspect::try_clear().map_err(|e| format!("Couldn't clear the inspector log: {e}"))
}

/// Recent lazy-discovery search traces (newest first): what the model searched for,
/// which tools matched, and the tool-definition tokens the results cost vs. loading
/// the whole catalog. The in-path proof that lazy discovery is working. Empty when
/// nothing has searched yet.
#[tauri::command]
async fn get_search_traces(limit: usize) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        searchtrace::read_recent(limit).map_err(|e| format!("Couldn't read search traces: {e}"))
    })
    .await
    .map_err(|e| format!("search traces task join failed: {e}"))?
}

/// Clear the search-trace log (delete `search-trace.jsonl`).
#[tauri::command]
fn clear_search_traces() -> Result<(), String> {
    searchtrace::try_clear().map_err(|e| format!("Couldn't clear the search traces: {e}"))
}

/// Clear all retained local activity in one confirmed action: the audit log, discovery
/// search traces, live-inspection captures, and the savings tally (including its
/// carry-forward total). Each is a local, irreversible delete; the logs re-create
/// themselves on the next event. Backs the Activity view's "Clear retained activity".
///
/// Attempts every log even if one fails (so a single locked file doesn't leave the
/// rest un-cleared), then reports exactly which could not be removed. Never confirms a
/// delete that did not happen: a leftover sensitive log must not read as "cleared".
#[tauri::command]
fn clear_activity_logs() -> Result<(), String> {
    let mut failed = Vec::new();
    if audit::try_clear().is_err() {
        failed.push("audit log");
    }
    if searchtrace::try_clear().is_err() {
        failed.push("search traces");
    }
    if inspect::try_clear().is_err() {
        failed.push("inspector captures");
    }
    if savings::try_clear().is_err() {
        failed.push("savings");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("Couldn't clear: {}", failed.join(", ")))
    }
}

/// One exposed tool's verifiable identity: the model-visible alias joined back to its
/// source server + the profiles that enable it, plus the integrity fingerprint and
/// when the definition was first seen / last changed. This is the "capability
/// provenance" view: prefixing helps the model pick a tool, this helps a human verify
/// what actually crossed the boundary.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ToolIdentity {
    /// Model-visible exposed name (the integrity pin key).
    alias: String,
    /// Resolved source server id, or empty if the alias couldn't be attributed (a
    /// renamed tool whose alias no longer carries its `server__` prefix; its exact
    /// provenance needs the deeper gateway integration, tracked separately).
    server_id: String,
    server_name: String,
    /// Names of the profiles whose enabled set includes this server.
    profiles: Vec<String>,
    /// Upstream tool name, taken as the alias suffix after `server__`.
    upstream: String,
    /// Version-prefixed fingerprint of the pinned definition (drift detection compares
    /// against this exact value).
    fingerprint: String,
    first_seen: u64,
    last_changed: u64,
    quarantined: bool,
}

/// Assemble the identity rows. Pure (no state/IO) so the alias->server attribution is
/// unit-testable.
fn build_tool_identities(
    baselines: &std::collections::BTreeMap<String, integrity::ToolBaseline>,
    quarantined: &std::collections::BTreeSet<String>,
    servers: &[ServerEntry],
    profiles: &[Profile],
) -> Vec<ToolIdentity> {
    // Exposed prefix (sanitize_segment(id)) -> server. Matching by the KNOWN prefixes
    // (longest wins) is robust against a server id that itself contains `__`, unlike a
    // naive split on the first separator.
    let prefixed: Vec<(String, &ServerEntry)> = servers
        .iter()
        .map(|s| (router::sanitize_segment(&s.id), s))
        .collect();
    baselines
        .iter()
        .map(|(alias, base)| {
            let mut server: Option<&ServerEntry> = None;
            let mut upstream = String::new();
            let mut best_len = 0usize;
            for (prefix, srv) in &prefixed {
                if let Some(rest) = alias
                    .strip_prefix(prefix.as_str())
                    .and_then(|r| r.strip_prefix("__"))
                {
                    if prefix.len() > best_len || server.is_none() {
                        best_len = prefix.len();
                        server = Some(srv);
                        upstream = rest.to_string();
                    }
                }
            }
            let (server_id, server_name) =
                server.map(|s| (s.id.clone(), s.name.clone())).unwrap_or_default();
            let profile_names = if server_id.is_empty() {
                Vec::new()
            } else {
                profiles
                    .iter()
                    .filter(|p| p.enabled_server_ids.contains(&server_id))
                    .map(|p| p.name.clone())
                    .collect()
            };
            ToolIdentity {
                alias: alias.clone(),
                server_id,
                server_name,
                profiles: profile_names,
                upstream,
                fingerprint: base.fingerprint.clone(),
                first_seen: base.first_seen,
                last_changed: base.last_changed,
                quarantined: quarantined.contains(alias),
            }
        })
        .collect()
}

/// The capability-provenance table: every pinned tool's identity for the active
/// newest-changed first. Aggregates pins across all profiles, because the gateway keys
/// pins by the CONDUIT_PROFILE it ran under (often None -> tool-pins.json), which need
/// not equal the app's active profile. Empty until the gateway has pinned a baseline.
#[tauri::command]
fn list_tool_identities(state: State<RegistryState>) -> Result<Vec<ToolIdentity>, String> {
    let reg = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ids = build_tool_identities(
        &integrity::all_baselines()?,
        &integrity::all_quarantined_names()?,
        &reg.servers,
        &reg.profiles,
    );
    ids.sort_by(|a, b| b.last_changed.cmp(&a.last_changed).then(a.alias.cmp(&b.alias)));
    Ok(ids)
}

/// Toggle quarantine-on-drift. When enabled, the gateway hides and blocks a high-risk
/// tool (poisoned definition, or a destructive tool whose definition changed/appeared)
/// that drifts from its pinned baseline, until the user re-approves it.
#[tauri::command]
fn set_quarantine_on_drift(state: State<RegistryState>, on: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.quarantine_on_drift = on;
        Ok(())
    })?;
    Ok(reg)
}

/// Toggle opt-in block-on-injection (SOU-345). When enabled, a high-confidence injection
/// hit fails the tool call instead of only labeling the content (scanning runs even if
/// the separate content-defense label toggle is off). Org force (`forceBlockOnInjection`)
/// can still enable this via the team overlay.
#[tauri::command]
fn set_block_on_injection(state: State<RegistryState>, on: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.block_on_injection = on;
        Ok(())
    })?;
    Ok(reg)
}

/// Toggle PII pseudonymization of tool results (SBS-346).
#[tauri::command]
fn set_pii_redaction(state: State<RegistryState>, on: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.pii_redaction = on;
        Ok(())
    })?;
    Ok(reg)
}

/// Tools currently quarantined (blocked after a high-risk drift), across all profiles.
///
/// Also fires an OS notification when a **new** entry appears after the first baseline
/// poll (SOU-305 option 1). Quarantine is decided in the gateway process; the app only
/// learns by polling, so this is the cheapest "notify when it happens while the app is
/// running" path. First call only seeds the seen-set so restarting the app with an
/// already-quarantined tool does not re-notify.
#[tauri::command]
async fn list_quarantined(app: AppHandle) -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let list = integrity::all_quarantined()?;
        notify_new_quarantines(&app, &list);
        Ok(list)
    })
    .await
    .map_err(|e| format!("quarantine read task join failed: {e}"))?
}

/// Keys of quarantine entries we have already observed this process. `None` = not
/// baselined yet (first poll seeds without notifying).
static QUARANTINE_SEEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn quarantine_entry_key(rec: &serde_json::Value) -> String {
    let profile = rec.get("profile").and_then(|v| v.as_str()).unwrap_or("");
    let tool = rec.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
    let ts = rec.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
    format!("{profile}\0{tool}@{ts}")
}

fn notify_new_quarantines(app: &AppHandle, list: &[serde_json::Value]) {
    let keys: std::collections::HashSet<String> =
        list.iter().map(quarantine_entry_key).collect();
    let mut guard = QUARANTINE_SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_mut() {
        None => {
            // First poll: baseline only.
            *guard = Some(keys);
        }
        Some(seen) => {
            let mut newcomers: Vec<&serde_json::Value> = list
                .iter()
                .filter(|rec| !seen.contains(&quarantine_entry_key(rec)))
                .collect();
            if !newcomers.is_empty() {
                // Newest first for the body when several land in one poll.
                newcomers.sort_by_key(|r| {
                    std::cmp::Reverse(r.get("ts").and_then(|v| v.as_u64()).unwrap_or(0))
                });
                let title = if newcomers.len() == 1 {
                    "Toolport: tool quarantined".to_string()
                } else {
                    format!("Toolport: {} tools quarantined", newcomers.len())
                };
                let body = newcomers
                    .iter()
                    .take(3)
                    .map(|r| {
                        let tool = r.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                        let detail = r
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .or_else(|| r.get("reason").and_then(|v| v.as_str()))
                            .unwrap_or("high-risk change");
                        format!("{tool}: {detail}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = app
                    .notification()
                    .builder()
                    .title(title)
                    .body(body)
                    .show();
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
            *seen = keys;
        }
    }
}

/// Re-approve a quarantined tool so the gateway re-exposes it. Re-saving the registry
/// nudges the gateway (which watches it) to rebuild and re-read the smaller set.
#[tauri::command]
fn release_quarantine(
    state: State<RegistryState>,
    profile: String,
    tool: String,
) -> Result<(), String> {
    let prof = if profile.is_empty() {
        None
    } else {
        Some(profile.as_str())
    };
    if !integrity::release(prof, &tool)
        .map_err(|e| format!("Could not re-approve {tool}: {e}"))?
    {
        // Idempotent across app/gateway instances: another process may have released the
        // same tool after this UI last polled. Only report failure when the persisted store
        // still says it is blocked (or cannot be read), which also preserves the useful
        // error for a tamper release whose accepted pin could not be saved.
        let still_blocked = integrity::quarantined(prof)
            .map_err(|e| format!("Could not verify re-approval for {tool}: {e}"))?;
        if still_blocked.contains(&tool) {
            return Err(format!(
                "Could not re-approve {tool}; its quarantine record or integrity pin could not be updated"
            ));
        }
    }
    // The quarantine release lives in the separate tool-pins file; the former blind re-save
    // here was only a gateway mtime-nudge (which the no-op guard usually swallowed anyway)
    // and it could revert a concurrent gateway/team write (SOU-23). Refresh the cache
    // instead of blind-writing the possibly-stale snapshot.
    reload_into_state(state.inner())?;
    Ok(())
}

/// Re-approve every quarantined tool for a profile in one action.
///
/// A lost integrity baseline blocks the whole catalog at once, which on a real
/// install is thousands of tools. Recovering through `release_quarantine` means one
/// IPC round trip, one cross-process lock and two store writes per tool, so the
/// only recovery the UI offered did not finish in practice. `integrity::release_all`
/// does the same repair with a single pass. Tools whose captured definition could
/// not be read stay blocked and come back in `skipped`, so this can never expose a
/// tool without re-establishing its baseline.
#[tauri::command]
async fn release_all_quarantine(
    app: AppHandle,
    profile: String,
) -> Result<integrity::ReleaseAllOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RegistryState>();
        let prof = if profile.is_empty() {
            None
        } else {
            Some(profile.as_str())
        };
        let outcome = integrity::release_all(prof)
            .map_err(|e| format!("Could not re-approve the blocked tools: {e}"))?;
        // Same reasoning as `release_quarantine`: refresh the cache rather than
        // blind-writing a possibly stale snapshot over a concurrent gateway write.
        reload_into_state(state.inner())?;
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("re-approval task join failed: {e}"))?
}

/// Set lazy discovery globally. The gateway reads this from the registry, so it
/// takes effect for every client (including ones that don't forward env vars).
/// Clients pick it up the next time they (re)spawn the gateway.
#[tauri::command]
fn set_lazy_discovery(state: State<RegistryState>, lazy: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_lazy_discovery(lazy);
        Ok(())
    })?;
    Ok(reg)
}

/// Enable or disable server-side "code mode" (the `toolport_run_script` meta-tool). The
/// gateway reads this from the registry and refreshes it live on the next watcher tick, so
/// it applies to every client without forwarding an env var. On by default (SOU-397);
/// pass `enabled: false` as the kill switch.
#[tauri::command]
fn set_code_mode(state: State<RegistryState>, enabled: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.code_mode = enabled;
        Ok(())
    })?;
    Ok(reg)
}

/// Opt into agent-requested Routine persistence. The gateway refreshes this setting live,
/// but every save remains separately gated by content-bound human approval.
#[tauri::command]
fn set_allow_routine_writes(state: State<RegistryState>, allow: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.allow_routine_writes = allow;
        Ok(())
    })?;
    Ok(reg)
}

/// Opt into agent control: lets an agent enable or disable servers through the
/// gateway's `conduit_enable_server` / `conduit_disable_server` tools. Off by
/// default; the destructive-tool safety switch stays user-only regardless of this.
#[tauri::command]
fn set_allow_agent_control(state: State<RegistryState>, allow: bool) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.allow_agent_control = allow;
        Ok(())
    })?;
    Ok(reg)
}

/// Set (or clear) a client's discovery-mode override. `mode` is `"full" | "lazy" |
/// "grouped"`; `None` (or "inherit"/unknown) clears it so the client inherits the global
/// mode. The gateway resolves this live via `CONDUIT_CLIENT_ID`, so the change applies
/// without reinstalling the client.
#[tauri::command]
fn set_client_discovery(
    state: State<RegistryState>,
    client_id: String,
    mode: Option<String>,
) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| {
        reg.set_client_discovery(&client_id, mode.as_deref());
        Ok(())
    })?;
    Ok(reg)
}

/// Flush the in-memory registry to disk so the teams module (which reads the registry
/// file) operates on the current state, then refresh the in-memory state from disk
/// after the team operation merged into it.
/// Load-modify-save the registry under the cross-process lock (mutating a FRESH on-disk
/// copy), then sync the in-memory cache to the persisted result. The in-process mutex is
/// held across the whole op so app threads serialize on it before the file lock; together
/// with `registry::update` this stops any app command from reverting a concurrent gateway
/// or team-sync write (SOU-23). Returns the new registry and `f`'s value.
fn write_registry<T>(
    state: &RegistryState,
    f: impl FnOnce(&mut Registry) -> Result<T, String>,
) -> Result<(Registry, T), String> {
    #[cfg(test)]
    if FAIL_NEXT_REGISTRY_WRITE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err("injected registry write failure".into());
    }
    let mut guard = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (reg, out) = registry::update(f)?;
    *guard = reg.clone();
    bump_registry_generation();
    Ok((reg, out))
}

/// Process-global test hook for SBS-845. Tests that set it must hold
/// [`REVOKE_HOOK_LOCK`] so a leftover flag cannot leak into another case.
/// The vault half of the revoke needs no hook: it is injected instead, via
/// [`revoke_client_http_token_with`].
#[cfg(test)]
static FAIL_NEXT_REGISTRY_WRITE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static REVOKE_HOOK_LOCK: Mutex<()> = Mutex::new(());

/// Bumped every time the in-memory registry cache is replaced, while the
/// `RegistryState` mutex is held.
///
/// Exists so the disk watcher can tell "nothing changed the cache while I was
/// reading the file" from "something did" (SOU-329). A module-level counter rather
/// than another piece of managed state because the registry is already a
/// process-wide singleton and threading a second handle through ~70 command call
/// sites would be a lot of churn for one comparison.
///
/// Every *write* happens under the `RegistryState` mutex, which is what makes the
/// comparison meaningful. Reads are not all locked: the pre-load sample is taken
/// deliberately outside it, since taking the lock there would reintroduce the very
/// hold-across-IO this design avoids. Only the comparison at publish time is locked,
/// and that is the one that has to be exact.
///
/// `SeqCst` is stronger than this strictly needs - a stale unlocked sample can only
/// cause a false mismatch, which drops a load rather than clobbers one, so `Relaxed`
/// would also be correct. It is kept because that argument is a subtlety a later edit
/// could quietly invalidate, and the counter is touched once per registry write and
/// once per 1500 ms watcher tick, so the ordering costs nothing measurable.
static REGISTRY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn registry_generation() -> u64 {
    REGISTRY_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

fn bump_registry_generation() {
    REGISTRY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Refresh the in-memory cache from disk. Formerly `flush_to_disk`, which PUSHED the
/// in-memory snapshot to disk; that blind write could revert a concurrent gateway/team
/// change (SOU-23), and it is now unnecessary because every mutation persists immediately
/// via `write_registry`, so the in-memory copy never holds unsaved changes. Pulling instead
/// keeps the cache current for the team flows without ever clobbering the file.
fn refresh_from_disk(state: &RegistryState) -> Result<(), String> {
    reload_into_state(state).map(|_| ())
}

/// Pull the registry from disk into the cache.
///
/// Guarded exactly like the watcher, and for the same reason: the disk load happens
/// before the mutex is taken, so a `write_registry` that persists and caches B in
/// that window would otherwise be overwritten here by the earlier A. This is the
/// same defect as SOU-329 in a second function, reachable through `refresh_from_disk`
/// and the team-sync paths rather than through the file watcher.
///
/// On a lost race the caller gets the cached registry, which is the newer of the
/// two, so the return value is always the authoritative current state.
fn reload_into_state(state: &RegistryState) -> Result<Registry, String> {
    reload_with(state, registry::load)
}

/// The body of [`reload_into_state`], with the disk read left as a parameter.
///
/// Split out purely so a test can drive the real sample -> load -> publish -> fall back
/// sequence: `registry::load` reads the user's actual registry file, so a test calling
/// `reload_into_state` could neither choose what comes back nor land a competing write
/// inside the window. Passing the load in lets the test do both, by performing the racing
/// write from inside the closure - which is exactly where the real race happens.
///
/// This is the only body; `reload_into_state` is a one-line delegation. Inlining a disk
/// load back into it would mean deleting that delegation, which is the visible edit this
/// arrangement is meant to force.
fn reload_with(
    state: &RegistryState,
    load: impl FnOnce() -> Result<Registry, String>,
) -> Result<Registry, String> {
    let sampled = registry_generation();
    let fresh = load()?;
    if publish_if_unchanged(state, sampled, &fresh) {
        return Ok(fresh);
    }
    // Lost the race: the cache holds the newer value, so return that rather than
    // handing the caller the disk read we just declined to publish.
    let guard = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(guard.clone())
}

/// Publish a registry read from disk, unless the cache moved underneath the read.
///
/// Shared by the file watcher and by [`reload_into_state`]: both read the file
/// outside the mutex and then publish under it, so both have the same window and
/// must not have two copies of the guard that could drift apart.
///
/// The watcher reads the file outside the mutex (loading it under the lock would
/// hold the registry across file IO on every change). That leaves a window: the
/// watcher samples disk state A, a command writes B to both disk and cache, and the
/// watcher then assigns its stale A over B and emits `registry-changed` with A. The
/// UI shows the reverted state and the cache disagrees with the file until something
/// else touches it.
///
/// `sampled` is the generation read *before* the load. Any in-memory write in the
/// meantime bumps it, so a mismatch means the cache is now fresher than what we
/// read, and the load is dropped. Nothing is lost: that write persisted to disk
/// too, so its own mtime change brings the watcher back with the newer content on
/// the next tick.
///
/// Returns whether the value was applied, so the caller only emits on a real change.
fn publish_if_unchanged(state: &RegistryState, sampled: u64, fresh: &Registry) -> bool {
    let mut guard = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry_generation() != sampled {
        return false;
    }
    *guard = fresh.clone();
    bump_registry_generation();
    true
}

/// Result of a connect (or a pending-join poll). `status` is "connected" (joined; `registry`
/// is the fresh merged state), "pending" (an approval-gated link — poll `request_token` via
/// `team_join_poll`), "denied", or "unknown". The frontend switches on `status`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamConnectResult {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry: Option<Registry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_token: Option<String>,
}

/// Join a Toolport Teams server with an invite or join-link code. A normal code vaults the
/// member token in the OS keychain, pulls the team's server set, and merges it into the local
/// registry non-destructively. An approval-gated link instead returns `status: "pending"` with
/// a `request_token` the frontend polls (nothing is stored locally until an admin approves).
#[tauri::command]
async fn team_connect(
    app: tauri::AppHandle,
    state: State<'_, RegistryState>,
    server_url: String,
    invite_code: String,
    member_name: Option<String>,
) -> Result<TeamConnectResult, String> {
    refresh_from_disk(state.inner())?;
    // Same reason as team_sync: a synchronous command runs on Tauri's main (UI) thread, and
    // teams::connect does a blocking network join + first config pull. Run it off-thread so
    // clicking "Connect" to join a team doesn't freeze the whole app until the join returns.
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        teams::connect(&server_url, &invite_code, member_name.as_deref())
    })
    .await
    .map_err(|e| format!("connect task join failed: {e}"))??;
    match outcome {
        teams::ConnectOutcome::Connected(review) => {
            let fresh = reload_into_state(state.inner())?;
            nudge_gateway(state.inner());
            // Team config adds local/stdio + LAN servers OFF (the member reviews + enables them)
            // and refuses link-local/metadata URLs. Surface both so the state is never a mystery.
            emit_team_review(&app, review);
            Ok(TeamConnectResult {
                status: "connected",
                registry: Some(fresh),
                request_token: None,
            })
        }
        teams::ConnectOutcome::Pending { request_token } => Ok(TeamConnectResult {
            status: "pending",
            registry: None,
            request_token: Some(request_token),
        }),
    }
}

/// Poll a pending, approval-gated join. The frontend calls this on an interval after
/// `team_connect` returned `status: "pending"`, handing back the `request_token` (and the same
/// `member_name`). On approval it finalizes exactly like a direct connect and returns the fresh
/// registry; otherwise it reports still-pending, denied, or unknown (expired/invalid).
#[tauri::command]
async fn team_join_poll(
    app: tauri::AppHandle,
    state: State<'_, RegistryState>,
    server_url: String,
    request_token: String,
    member_name: Option<String>,
) -> Result<TeamConnectResult, String> {
    let poll = tauri::async_runtime::spawn_blocking(move || {
        teams::poll_join(&server_url, &request_token, member_name.as_deref())
    })
    .await
    .map_err(|e| format!("poll task join failed: {e}"))??;
    match poll {
        teams::JoinPoll::Connected(review) => {
            let fresh = reload_into_state(state.inner())?;
            nudge_gateway(state.inner());
            emit_team_review(&app, review);
            Ok(TeamConnectResult {
                status: "connected",
                registry: Some(fresh),
                request_token: None,
            })
        }
        teams::JoinPoll::Pending => Ok(TeamConnectResult {
            status: "pending",
            registry: None,
            request_token: None,
        }),
        teams::JoinPoll::Denied => Ok(TeamConnectResult {
            status: "denied",
            registry: None,
            request_token: None,
        }),
        teams::JoinPoll::Unknown => Ok(TeamConnectResult {
            status: "unknown",
            registry: None,
            request_token: None,
        }),
    }
}

/// Pull the latest team config and re-merge it. A no-op when nothing changed.
///
/// `async` + `spawn_blocking` is load-bearing, not stylistic: a synchronous Tauri command
/// runs on the main (UI) thread, and the config pull is a blocking network call. The
/// long-poll variant below blocks for up to 30s per cycle, and the member's background loop
/// re-invokes it continuously, so as a sync command it froze the whole app ("Not Responding")
/// for anyone connected to a team and starved every other command (probe_servers, etc.).
/// Running the blocking pull on a worker thread keeps the event loop free.
#[tauri::command]
async fn team_sync(
    app: tauri::AppHandle,
    state: State<'_, RegistryState>,
) -> Result<Registry, String> {
    refresh_from_disk(state.inner())?;
    let result = tauri::async_runtime::spawn_blocking(teams::sync_now)
        .await
        .map_err(|e| format!("sync task join failed: {e}"))??;
    finish_sync(&app, state.inner(), result)
}

/// Long-polling sync for the member's background loop: the config pull parks on the server
/// for up to `wait_secs` (clamped) and returns the instant the team config view changes, so
/// a dashboard policy edit enforces in ~1s instead of at the next interval. Otherwise
/// identical to [`team_sync`]; the frontend re-invokes it in a loop. See [`team_sync`] for
/// why the blocking pull must run off the main thread.
#[tauri::command]
async fn team_sync_wait(
    app: tauri::AppHandle,
    state: State<'_, RegistryState>,
    wait_secs: u64,
) -> Result<Registry, String> {
    refresh_from_disk(state.inner())?;
    let wait = wait_secs.min(30);
    let result = tauri::async_runtime::spawn_blocking(move || teams::sync_wait(wait))
        .await
        .map_err(|e| format!("sync task join failed: {e}"))??;
    finish_sync(&app, state.inner(), result)
}

/// Apply a sync result to the shared registry state and tell the UI what happened. Shared by
/// the immediate ([`team_sync`]) and long-polling ([`team_sync_wait`]) commands.
fn finish_sync(
    app: &tauri::AppHandle,
    state: &RegistryState,
    result: teams::SyncResult,
) -> Result<Registry, String> {
    match result {
        teams::SyncResult::Removed => {
            // The member was removed; sync already cleared the local team. Reload so the UI
            // drops the team, and tell it why so it can surface a notice rather than the raw
            // error the config pull used to throw.
            let fresh = reload_into_state(state)?;
            nudge_gateway(state);
            let _ = app.emit("team-removed", serde_json::json!({}));
            Ok(fresh)
        }
        teams::SyncResult::Ok { applied, .. } => {
            let outcome = applied.map(|(_, o)| o).unwrap_or_default();
            let fresh = reload_into_state(state)?;
            nudge_gateway(state);
            emit_team_review(app, outcome);
            Ok(fresh)
        }
    }
}

/// Tell the UI how a team config landed: how many servers need the member's review (they
/// run a local command or hit a LAN URL, so they're added OFF) and how many were blocked
/// outright (link-local / cloud-metadata URLs). Only fires when there's something to say.
fn emit_team_review(app: &tauri::AppHandle, outcome: teams::MergeOutcome) {
    if outcome.review > 0 || outcome.blocked > 0 {
        let _ = app.emit(
            "team-servers-review",
            serde_json::json!({ "review": outcome.review, "blocked": outcome.blocked }),
        );
    }
}

/// Member-facing Team Instructions status (spec W4): the org content on this machine, its
/// version, and each installed client's on-disk state. `None` when the team has no active
/// instructions. Read-only. Async + `spawn_blocking` because it scans every installed client's
/// rules file, which must not run on the UI thread.
#[tauri::command]
async fn team_instructions_status() -> Option<teams::InstructionsStatusView> {
    tauri::async_runtime::spawn_blocking(teams::instructions_status)
        .await
        .ok()
        .flatten()
}

/// Leave the team: remove its merged servers, clear the connection and the token.
#[tauri::command]
fn team_disconnect(state: State<RegistryState>) -> Result<Registry, String> {
    refresh_from_disk(state.inner())?;
    teams::disconnect()?;
    let fresh = reload_into_state(state.inner())?;
    nudge_gateway(state.inner());
    Ok(fresh)
}

/// Admin: replace only the team's shared server list with the current local set (own servers
/// only, secret values never sent). Remote instructions and policy fields are preserved, and
/// an optimistic-concurrency conflict is returned rather than overwriting another admin.
#[tauri::command]
async fn team_push_preview(state: State<'_, RegistryState>) -> Result<teams::PushPreview, String> {
    refresh_from_disk(state.inner())?;
    tauri::async_runtime::spawn_blocking(teams::preview_push_current)
        .await
        .map_err(|e| format!("push preview task join failed: {e}"))?
}

#[tauri::command]
async fn team_push(
    state: State<'_, RegistryState>,
    base_version: i64,
    local_fingerprint: String,
) -> Result<i64, String> {
    refresh_from_disk(state.inner())?;
    // push_current does a blocking GET + PUT to the team server; keep it off the main thread.
    tauri::async_runtime::spawn_blocking(move || {
        teams::push_current(base_version, &local_fingerprint)
    })
        .await
        .map_err(|e| format!("push task join failed: {e}"))?
}

/// Re-save the registry to bump its mtime. The running gateway watches that file
/// and rebuilds on change, so freshly-vaulted credentials take effect (and the
/// server's tools flow to connected clients) without a manual restart.
/// Refresh the in-memory cache from disk. Formerly a blind re-save meant to bump the
/// registry mtime so the gateway would reload; that reverted concurrent gateway/team writes
/// (SOU-23) and, because the no-op guard skips a same-content save, rarely bumped the mtime
/// anyway. A real change (e.g. `bump_secrets_generation`) advances the file under the lock
/// and triggers the gateway reload on its own.
fn nudge_gateway(state: &RegistryState) {
    let _ = reload_into_state(state);
}

/// Bump [`Registry::secrets_generation`] and save under the lock so gateways reload even
/// when only the keychain changed. Increments the FRESH on-disk value (not a stale `+1`) so
/// a concurrent bump from another writer isn't lost.
fn bump_secrets_generation(state: &RegistryState) {
    let _ = write_registry(state, |reg| {
        reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
        Ok(())
    });
}

#[tauri::command]
fn take_registry_recovery_notice() -> Option<registry::RegistryRecoveryNotice> {
    registry::take_recovery_notice()
}

/// Store a bearer token for an http server (used as `Authorization: Bearer ...`).
#[tauri::command]
async fn set_auth_token(app: AppHandle, server_id: String, token: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _mutation = acquire_auth_mutation_lock(&server_id)?;
        // A manually pasted bearer replaces any prior OAuth session. Keeping stale
        // refresh metadata could otherwise overwrite the user's token later.
        remote::clear_oauth_state(&server_id)?;
        secrets::set_secret(&server_id, secrets::HTTP_AUTH_KEY, &token)?;
        bump_secrets_generation(app.state::<RegistryState>().inner());
        Ok(())
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

#[tauri::command]
async fn clear_auth_token(app: AppHandle, server_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _mutation = acquire_auth_mutation_lock(&server_id)?;
        // Remove refresh metadata first so a second-write failure cannot leave state
        // that silently recreates the bearer token the user asked to delete.
        remote::clear_oauth_state(&server_id)?;
        secrets::delete_secret(&server_id, secrets::HTTP_AUTH_KEY)?;
        bump_secrets_generation(app.state::<RegistryState>().inner());
        Ok(())
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// Errs on a failed vault read instead of reporting `false` (SBS-789): a locked
/// keychain must not make a vaulted token look like "never authenticated".
#[tauri::command]
async fn has_auth_token(server_id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(secrets::get_secret_result(&server_id, secrets::HTTP_AUTH_KEY)?.is_some())
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// Figure out what a remote server needs to connect (none / oauth / token) and
/// how to get it. Runs off the UI thread (it makes a network call).
#[tauri::command]
async fn probe_auth(url: String) -> vendors::AuthInfo {
    tauri::async_runtime::spawn_blocking(move || vendors::probe_auth(&url))
        .await
        .unwrap_or_else(|_| vendors::AuthInfo::fallback())
}

/// Run the OAuth 2.1 browser flow for a remote server and vault the resulting
/// access token (and refresh token). Runs on a blocking worker so the UI thread
/// stays responsive while the user completes sign-in in their browser.
#[tauri::command]
async fn authenticate_oauth(
    state: State<'_, RegistryState>,
    server_id: String,
    url: String,
) -> Result<(), String> {
    let Some(mut lock) = acquire_or_wait_oauth_lock(&server_id, &url)? else {
        // Another process completed the OAuth flow for this same server while we waited.
        return Ok(());
    };
    let resource = url.clone();
    let res = tauri::async_runtime::spawn_blocking(move || oauth::authenticate(&url))
        .await
        .map_err(|e| e.to_string())??;
    let _mutation = acquire_auth_mutation_lock(&server_id)?;
    // Persist refresh metadata first. If the access-token write then fails, the
    // next refresh still has the new state and can recover; the reverse order can
    // strand a new access token beside an invalidated old refresh token.
    remote::store_oauth_state(
        &server_id,
        Some(res.issuer),
        &res.token_endpoint,
        &res.client_id,
        res.refresh_token,
        Some(resource),
        res.scope,
        res.issued_at,
        res.expires_at,
    )?;
    secrets::set_secret(&server_id, secrets::HTTP_AUTH_KEY, &res.access_token)?;
    bump_secrets_generation(state.inner());
    lock.mark_succeeded();
    Ok(())
}

/// The popular catalog (the curated set).
#[tauri::command]
fn popular_catalog() -> Vec<catalog::CatalogEntry> {
    catalog::popular()
}

/// Curated stacks: role-based bundles of catalog servers (each resolved to full
/// entries with credential hints) for the guided one-flow setup.
#[tauri::command]
fn list_stacks() -> Vec<stacks::Stack> {
    stacks::stacks()
}

/// Search the official MCP Registry for servers to add. Network call, so it runs
/// on a blocking worker. Empty query returns popular/recent servers.
#[tauri::command]
async fn search_catalog(query: String) -> Result<Vec<catalog::CatalogEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || catalog::search(&query))
        .await
        .map_err(|e| e.to_string())?
}

/// Which of a server's env keys currently have a value stored in the keychain.
///
/// Errs on a failed vault read instead of reporting `(key, false)` (SBS-841): a
/// locked keychain must not make a vaulted env secret look like "not stored".
#[tauri::command]
async fn secret_status(server_id: String, keys: Vec<String>) -> Result<Vec<(String, bool)>, String> {
    // Async, like every keychain command here: a Secret Service read is a
    // synchronous D-Bus round trip that can stall for seconds on a locked or
    // slow keyring, and as sync commands they ran on the GTK main loop,
    // freezing window controls while a dialog probed the vault (SBS-813).
    //
    // Errs rather than returning an empty list on a worker failure, and rather
    // than mapping a failed per-key read to `false`, for the same reason
    // `has_auth_token` / `has_client_secret` err (SBS-789 / SBS-722 / SBS-841):
    // the dialog treats a resolved list as authoritative and would mark every
    // key unvaulted, while its `catch` leaves the badges alone. `get_secret`
    // swallows a locked or failed keyring into `None`; the presence probe must
    // use `get_secret_result` so that becomes `Err`. The polled readers below
    // can absorb a panic as an empty result because they run again in seconds;
    // this is a one-shot probe.
    //
    // All-or-nothing on purpose, rather than a per-key `Option<bool>`. The
    // realistic failures are process-wide (locked keyring, no Secret Service,
    // denied keychain access), so a per-key answer would report "unknown" for
    // every key anyway, while pushing a third state through the dialog's
    // `vaulted` map at every use site - badge, placeholder, Remove button. One
    // `Err` maps to the one "couldn't check the keychain" warning the dialog
    // now shows, and keeps the shape of `has_auth_token` / `has_client_secret`.
    tauri::async_runtime::spawn_blocking(move || {
        keys.into_iter()
            .map(|k| {
                let present = secrets::get_secret_result(&server_id, &k)?.is_some();
                Ok((k, present))
            })
            .collect::<Result<Vec<_>, String>>()
    })
    .await
    .map_err(|e| format!("keychain task join failed: {e}"))?
}

/// Open Toolport's data directory (registry, logs, audit) in the OS file manager,
/// so users can back it up or inspect it.
#[tauri::command]
fn open_data_dir() -> Result<(), String> {
    let dir = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("could not open the data directory: {e}"))?;
    Ok(())
}

/// Serialize the user's servers into a shareable setup (server definitions only,
/// never secret values). A teammate imports this and adds their own keys, so a
/// curated server set can be shared without leaking any credentials. An optional
/// name/description lets the sharer label the set.
#[tauri::command]
fn export_config(
    state: State<RegistryState>,
    name: Option<String>,
    description: Option<String>,
    server_ids: Option<Vec<String>>,
) -> Result<String, String> {
    let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    serde_json::to_string_pretty(&build_export(
        &reg,
        name.as_deref(),
        description.as_deref(),
        server_ids.as_deref(),
    ))
    .map_err(|e| e.to_string())
}

/// Write a shareable setup to a file on disk (the path comes from a save dialog).
/// Same content as export_config; just easier to hand to a teammate than a paste.
#[tauri::command]
fn export_config_to_path(
    state: State<RegistryState>,
    path: String,
    name: Option<String>,
    description: Option<String>,
    server_ids: Option<Vec<String>>,
) -> Result<(), String> {
    let json = {
        let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        serde_json::to_string_pretty(&build_export(
            &reg,
            name.as_deref(),
            description.as_deref(),
            server_ids.as_deref(),
        ))
        .map_err(|e| e.to_string())?
    };
    std::fs::write(&path, json).map_err(|e| format!("Couldn't write the file: {e}"))
}

/// Export the audit log to a file (path from a save dialog). `format` is "csv" or
/// "json". CSV is formula-injection-safe (see `audit::to_csv`) since tool names and
/// error text come from untrusted downstream servers. Exports the full retained
/// log, which the audit module already caps.
#[tauri::command]
fn export_audit_to_path(path: String, format: String) -> Result<(), String> {
    let entries = audit::read_recent(usize::MAX)
        .map_err(|e| format!("Couldn't read the activity log: {e}"))?;
    let body = if format == "csv" {
        audit::to_csv(&entries)
    } else {
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?
    };
    std::fs::write(&path, body).map_err(|e| format!("Couldn't write the file: {e}"))
}

/// Public endpoint that turns a shared setup into a `toolport.app/s/<id>` link.
const SHARE_ENDPOINT: &str = "https://toolport.app/api/share";

/// POST a shareable setup (the secret-stripped JSON from `export_config`) to the
/// share service and return the short link to copy. The service stores it with a
/// 90-day TTL and renders a preview page; secrets are never in the payload.
#[tauri::command]
async fn share_stack(setup_json: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Read;
        let resp = ureq::post(SHARE_ENDPOINT)
            .timeout(std::time::Duration::from_secs(20))
            .set("content-type", "application/json")
            .send_string(&setup_json)
            .map_err(|e| format!("couldn't reach the share service: {e}"))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(64 * 1024)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
        body.get("url")
            .and_then(|u| u.as_str())
            .map(str::to_string)
            .ok_or_else(|| "the share service did not return a link".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Holds a share id captured from a toolport:// (or legacy conduit://) deep link
/// that arrived before the UI was ready (cold start). The frontend claims it on mount.
type PendingShare = Mutex<Option<String>>;

/// Remembers an approvals tray request that arrived before the frontend listener.
#[derive(Default)]
struct PendingTrayApprovals(Mutex<TrayApprovalsDelivery>);

#[derive(Default)]
struct TrayApprovalsDelivery {
    frontend_ready: bool,
    pending: bool,
}

/// Queue a request until the frontend is ready, or tell the caller it is safe
/// to emit live. The decision and state change are atomic with the readiness claim.
fn should_emit_tray_approvals(state: &PendingTrayApprovals) -> bool {
    let mut delivery = state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if delivery.frontend_ready {
        true
    } else {
        delivery.pending = true;
        false
    }
}

fn claim_pending_tray_approvals(state: &PendingTrayApprovals) -> bool {
    let mut delivery = state
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    delivery.frontend_ready = true;
    std::mem::take(&mut delivery.pending)
}

/// Parse a `toolport://import?s=<id>` (or legacy `conduit://…`) deep link into its
/// share id. Tolerates an optional trailing slash after the host; the id must look
/// like a share id.
fn parse_share_url(url: &str) -> Option<String> {
    let after = url
        .strip_prefix("toolport://")
        .or_else(|| url.strip_prefix("conduit://"))?;
    let after = after.strip_prefix("import")?;
    let query = after.trim_start_matches('/').strip_prefix('?')?;
    query.split('&').find_map(|pair| {
        let v = pair.strip_prefix("s=")?;
        let id: String = v.chars().take(64).collect();
        if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            Some(id)
        } else {
            None
        }
    })
}

/// Resolve a shared-stack id (from a deep link) by fetching its setup JSON from
/// the share service; the frontend then previews it like any other import.
#[tauri::command]
async fn fetch_shared_setup(id: String) -> Result<String, String> {
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("invalid share id".to_string());
    }
    let url = format!("{SHARE_ENDPOINT}?id={id}");
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Read;
        let resp = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(20))
            .call()
            .map_err(|e| format!("couldn't reach the share service: {e}"))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(128 * 1024)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Claim a share id captured from a deep link before the UI was listening.
#[tauri::command]
fn take_pending_shared(state: State<PendingShare>) -> Option<String> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Claim an approvals tray request captured before the UI was listening.
#[tauri::command]
fn take_pending_tray_approvals(state: State<PendingTrayApprovals>) -> bool {
    claim_pending_tray_approvals(state.inner())
}

/// Deliver a shared-stack id from a deep link to the UI: stash it so a cold start
/// can claim it on mount, reveal the window from every tray/minimized state, and
/// emit the live event for a running app. Idempotent enough that delivering the
/// same id twice just re-opens it.
fn deliver_shared_import(handle: &tauri::AppHandle, id: String) {
    if let Some(st) = handle.try_state::<PendingShare>() {
        *st.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.clone());
    }
    show_main_window(handle);
    let _ = handle.emit("import-shared", id);
}

/// Import a shared setup. Adds servers not already present (by name); secret
/// values are never included, so each new server is left for the user to vault.
#[tauri::command]
fn import_config(state: State<RegistryState>, json: String) -> Result<Registry, String> {
    let (reg, _) = write_registry(state.inner(), |reg| apply_import(reg, &json))?;
    Ok(reg)
}

/// Read a shared-setup file from disk (path from an open dialog), capped so a
/// malicious or accidental huge file can't OOM the app. The contents go to the UI
/// for a preview/confirm step; nothing is imported here.
#[tauri::command]
fn read_setup_file(path: String) -> Result<String, String> {
    const MAX_SETUP_BYTES: u64 = 4 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_SETUP_BYTES {
            return Err("That file is too large to be a Toolport setup.".to_string());
        }
    }
    std::fs::read_to_string(&path).map_err(|e| format!("Couldn't read the file: {e}"))
}

/// One server a shared setup would add. The UI shows the exact command/args/url so
/// the user reviews what an (attacker-controllable) shared config will run before
/// accepting it - enabling a server later spawns its command.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportItem {
    /// Stable only for the current detected-import preview. Shared setup previews
    /// have no key because they are confirmed as one complete document.
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    name: String,
    transport: String,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    /// False if a server with this name already exists (the import would skip it).
    is_new: bool,
}

/// Show exactly what the bulk client import would add without changing the
/// registry. The same import key is accepted by `import_servers` after review.
#[tauri::command]
async fn preview_import_servers(
    state: State<'_, RegistryState>,
) -> Result<Vec<ImportItem>, String> {
    let detected = tauri::async_runtime::spawn_blocking(clients::detect_clients)
        .await
        .map_err(|e| e.to_string())?;
    let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(servers_to_import(&detected, &reg)
        .into_iter()
        .map(|server| ImportItem {
            key: Some(clients::import_dedupe_key(
                &server.name,
                server.command.as_deref(),
                &server.args,
            )),
            name: server.name,
            transport: server.transport,
            command: server.command,
            args: server.args,
            url: server.url,
            is_new: true,
        })
        .collect())
}

/// Parse a shared setup and report what it WOULD add, without importing anything.
#[tauri::command]
fn preview_import(state: State<RegistryState>, json: String) -> Result<Vec<ImportItem>, String> {
    #[derive(serde::Deserialize)]
    struct Doc {
        servers: Vec<ServerEntry>,
    }
    let doc: Doc = serde_json::from_str(&json)
        .map_err(|e| format!("That doesn't look like a Toolport setup: {e}"))?;
    let reg = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(doc
        .servers
        .into_iter()
        .map(|s| {
            let is_new = !reg
                .servers
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case(&s.name));
            ImportItem {
                key: None,
                name: s.name,
                transport: s.transport,
                command: s.command,
                args: s.args,
                url: s.url,
                is_new,
            }
        })
        .collect())
}

/// Build a shareable setup document: server definitions only, with the gateway
/// entry excluded and every secret value stripped. Pure, so the never-leak-a-key
/// invariant is testable without Tauri state.
fn build_export(
    reg: &Registry,
    name: Option<&str>,
    description: Option<&str>,
    server_ids: Option<&[String]>,
) -> serde_json::Value {
    // When a selection is given, share only those servers (by stable id); otherwise
    // share them all. Lets a user share a focused "stack" instead of everything.
    let include: Option<std::collections::HashSet<&str>> =
        server_ids.map(|ids| ids.iter().map(String::as_str).collect());
    let servers: Vec<ServerEntry> = reg
        .servers
        .iter()
        .filter(|s| !clients::is_gateway_server(s))
        .filter(|s| {
            include
                .as_ref()
                .map(|set| set.contains(s.id.as_str()))
                .unwrap_or(true)
        })
        .map(|s| {
            let mut s = s.clone();
            s.id = String::new();
            for e in &mut s.env {
                e.value = None; // never share env values
            }
            // Some servers take credentials inline in args (e.g. a Postgres
            // connection string with a password). Redact those too, so a shared
            // setup never leaks a secret the env-stripping above wouldn't catch.
            for a in &mut s.args {
                if arg_looks_secret(a) {
                    *a = "<redacted>".to_string();
                }
            }
            // A remote server's URL can carry inline credentials
            // (`https://user:pass@host`); strip them too - the env/arg passes miss
            // the `url` field, which would otherwise leak through the share link.
            if let Some(u) = &s.url {
                s.url = Some(redact_url_userinfo(u));
            }
            s
        })
        .collect();
    let mut doc = serde_json::json!({ "kind": "conduit-setup", "version": 1, "servers": servers });
    if let Some(n) = name.map(str::trim).filter(|s| !s.is_empty()) {
        doc["name"] = serde_json::json!(n);
    }
    if let Some(d) = description.map(str::trim).filter(|s| !s.is_empty()) {
        doc["description"] = serde_json::json!(d);
    }
    doc
}

/// Merge a shared setup into the registry: add servers not already present (by
/// name, case-insensitive), stripping any secret values. Pure (no Tauri state).
fn apply_import(reg: &mut Registry, json: &str) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Doc {
        servers: Vec<ServerEntry>,
    }
    let doc: Doc = serde_json::from_str(json)
        .map_err(|e| format!("That doesn't look like a Toolport setup: {e}"))?;
    for mut s in doc.servers {
        if reg.servers.iter().any(|e| e.name.eq_ignore_ascii_case(&s.name)) {
            continue;
        }
        s.id = String::new();
        for e in &mut s.env {
            e.value = None;
        }
        s.source = Some("shared".to_string());
        reg.add_server(s);
    }
    Ok(())
}

/// Mutable loop state for [`watch_registry_for_app`] / [`watch_registry_tick`].
struct RegistryWatchLoop {
    /// Last-seen registry-file mtime after a successful load. Advanced on
    /// identical / applied / lost-race so we do not reload every tick. A
    /// transient `load_from` failure does not consume it (issue #695).
    last_mtime: Option<SystemTime>,
    /// Serialized form of the last applied registry. Identical JSON (an mtime-only
    /// bump) skips emit but still advances `last_mtime`.
    last_json: String,
    /// Last load-failure string, used with `consecutive_failures` to log the
    /// first failure, changed errors, and periodic reminders without spamming.
    last_error: Option<String>,
    /// Consecutive failed loads for bounded exponential retry delay.
    consecutive_failures: u32,
}

impl RegistryWatchLoop {
    /// Seed comparison from the registry value already applied to the app. The
    /// mtime deliberately starts empty so the first tick validates disk through
    /// the same load/publish path as every later change.
    fn from_state(state: &RegistryState) -> Self {
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self {
            last_mtime: None,
            last_json: serde_json::to_string(&*guard).unwrap_or_default(),
            last_error: None,
            consecutive_failures: 0,
        }
    }
}

/// What one watcher iteration did. Extracted so tests can drive a tick without
/// the infinite sleep loop or a Tauri `AppHandle` (issue #695).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryWatchTick {
    Unchanged,
    LoadFailed,
    Identical,
    LostRace,
    Applied,
}

fn clear_watch_failure(loop_state: &mut RegistryWatchLoop) {
    loop_state.consecutive_failures = 0;
    if loop_state.last_error.take().is_some() {
        eprintln!("toolport: registry reload recovered");
    }
}

/// One iteration of the desktop registry watcher (no sleep).
///
/// `mtime`, `load`, and `emit` are parameters so a test can present a changed
/// mtime, fail the first load, keep that mtime, and succeed on the second tick
/// — the path `watch_registry_for_app` used to skip forever (issue #695).
fn watch_registry_tick(
    state: &RegistryState,
    loop_state: &mut RegistryWatchLoop,
    mtime: impl FnOnce() -> Option<SystemTime>,
    load: impl FnOnce() -> Result<Registry, String>,
    emit: impl FnOnce(&Registry),
) -> RegistryWatchTick {
    let cur = mtime();
    if cur == loop_state.last_mtime {
        // A previously failing change may have been rolled back to the last
        // applied cursor. There is nothing left to retry, so return to the
        // normal poll interval without claiming a successful recovery.
        loop_state.consecutive_failures = 0;
        loop_state.last_error = None;
        return RegistryWatchTick::Unchanged;
    }
    // Sampled BEFORE the load, so any in-memory write racing this read is
    // visible as a mismatch when we go to apply it (SOU-329).
    let sampled = registry_generation();
    let fresh = match load() {
        Ok(r) => r,
        Err(e) => {
            loop_state.consecutive_failures = loop_state.consecutive_failures.saturating_add(1);
            let error_changed = loop_state.last_error.as_deref() != Some(e.as_str());
            if watch_failure_should_log(loop_state.consecutive_failures, error_changed) {
                eprintln!(
                    "toolport: registry reload failed ({} consecutive failures; will retry): {e}",
                    loop_state.consecutive_failures
                );
            }
            loop_state.last_error = Some(e);
            return RegistryWatchTick::LoadFailed;
        }
    };
    let fresh_json = serde_json::to_string(&fresh).unwrap_or_default();
    if fresh_json == loop_state.last_json {
        // Identical content (e.g. an mtime bump to nudge the gateway): consume
        // the cursor so we do not reload every tick, but do not emit.
        loop_state.last_mtime = cur;
        clear_watch_failure(loop_state);
        return RegistryWatchTick::Identical;
    }
    if !publish_if_unchanged(state, sampled, &fresh) {
        // A command wrote a newer registry while we were reading. Consume
        // `last_mtime` so a retry cannot publish this stale load over the
        // winner (SOU-329). Leave `last_json` alone so this content is not
        // remembered as applied, and do not emit: the winning write persisted
        // to disk, so its mtime change brings us back with the newer value.
        loop_state.last_mtime = cur;
        clear_watch_failure(loop_state);
        return RegistryWatchTick::LostRace;
    }
    loop_state.last_mtime = cur;
    loop_state.last_json = fresh_json;
    clear_watch_failure(loop_state);
    emit(&fresh);
    RegistryWatchTick::Applied
}

fn watch_retry_delay(consecutive_failures: u32) -> Duration {
    const BASE_MS: u64 = 1500;
    const CAP_MS: u64 = 60_000;
    let exponent = consecutive_failures.saturating_sub(1).min(6);
    Duration::from_millis((BASE_MS << exponent).min(CAP_MS))
}

fn watch_failure_should_log(consecutive_failures: u32, error_changed: bool) -> bool {
    consecutive_failures > 0
        && (consecutive_failures == 1 || error_changed || consecutive_failures % 20 == 0)
}

/// Watch the registry file and mirror external changes (e.g. an agent enabling a
/// server through the gateway) back into the app's in-memory state, then nudge the
/// UI to refetch. Without this, a gateway-written change would be invisible to the
/// app and clobbered by its next save. Polls mtime (the gateway uses the same
/// approach), skips identical touches so an mtime-only bump doesn't churn the UI,
/// and backs off repeated load failures without consuming their mtime.
fn watch_registry_for_app(handle: tauri::AppHandle) {
    let Some(path) = registry::resolved_path() else {
        return;
    };
    let mtime = |p: &std::path::Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    let mut loop_state = RegistryWatchLoop::from_state(&handle.state::<RegistryState>());
    loop {
        let registry_state = handle.state::<RegistryState>();
        let _ = watch_registry_tick(
            &registry_state,
            &mut loop_state,
            || mtime(&path),
            || registry::load_from(&path),
            |fresh| {
                let _ = handle.emit("registry-changed", fresh);
            },
        );
        std::thread::sleep(watch_retry_delay(loop_state.consecutive_failures));
    }
}

/// Reap the child if it has already exited; returns true if it is still alive.
fn http_bridge_alive(bridge: &mut HttpBridge) -> bool {
    let alive = match bridge.child.as_mut() {
        Some(child) => !matches!(child.try_wait(), Ok(Some(_))),
        None => false,
    };
    if !alive {
        bridge.child = None;
        bridge.port = None;
        bridge.token = None;
    }
    alive
}

/// Stop client-spawned gateway processes before an in-app update (all platforms).
/// MCP clients stay open; only `toolport-gateway` / `conduit-gateway` children exit.
///
/// The supervised HTTP endpoint is the one gateway Toolport can recreate itself.
/// Capture its port before reaping so a failed install can explicitly recover it;
/// client-owned stdio gateways must be recreated by their owning applications.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateShutdownReport {
    #[serde(flatten)]
    reaper: crate::gateway_publish::ReapReport,
    http_bridge_port: Option<u16>,
    /// Failures managing Toolport's owned HTTP child or its durable recovery
    /// marker. Kept separate from `ReapReport::failed`, whose entries are
    /// external process labels and drive different user guidance.
    lifecycle_errors: Vec<String>,
}

#[tauri::command]
fn stop_spawned_gateways(bridge: State<HttpBridgeState>) -> UpdateShutdownReport {
    enum OwnedBridgeStop {
        Absent,
        Stopped(u16),
        FailedWithIntent { port: u16, error: String },
        FailedBeforeIntent(String),
    }

    let owned_bridge = {
        let mut bridge = bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if http_bridge_alive(&mut bridge) {
            match (bridge.port, bridge.token.clone()) {
                (Some(port), Some(token)) => {
                    let intent = UpdateHttpBridgeIntent { port, token };
                    match save_update_http_bridge_intent(&intent) {
                        Ok(()) => {
                            let mut child = bridge.child.take().expect("live bridge has a child");
                            let stopped = match child.kill() {
                                Ok(()) => child.wait().map(|_| ()),
                                Err(kill_error) => match child.try_wait() {
                                    Ok(Some(_)) => Ok(()),
                                    Ok(None) => Err(kill_error),
                                    Err(wait_error) => Err(wait_error),
                                },
                            };
                            match stopped {
                                Ok(()) => {
                                    bridge.port = None;
                                    bridge.token = None;
                                    OwnedBridgeStop::Stopped(port)
                                }
                                Err(error) => {
                                    bridge.child = Some(child);
                                    OwnedBridgeStop::FailedWithIntent {
                                        port,
                                        error: format!(
                                            "Toolport HTTP endpoint on port {port}: {error}"
                                        ),
                                    }
                                }
                            }
                        }
                        Err(error) => OwnedBridgeStop::FailedBeforeIntent(error),
                    }
                }
                _ => OwnedBridgeStop::FailedBeforeIntent(
                    "live HTTP endpoint is missing its port or token".to_string(),
                ),
            }
        } else {
            OwnedBridgeStop::Absent
        }
    };

    // Never launch the kill-all pass when Toolport could not first persist the
    // exact recovery identity for its owned endpoint. The global enumerator can
    // see and kill that child too; proceeding would destroy connectivity without
    // a trustworthy port/token from which to recover it.
    if let OwnedBridgeStop::FailedBeforeIntent(error) = &owned_bridge {
        return UpdateShutdownReport {
            reaper: crate::gateway_publish::ReapReport {
                ..Default::default()
            },
            http_bridge_port: None,
            lifecycle_errors: vec![error.clone()],
        };
    }

    let reaper = crate::gateway_publish::stop_spawned_gateways();
    let mut lifecycle_errors = Vec::new();
    let http_bridge_port = match owned_bridge {
        OwnedBridgeStop::Absent => None,
        OwnedBridgeStop::Stopped(port) => Some(port),
        OwnedBridgeStop::FailedWithIntent { port, error } => {
            lifecycle_errors.push(error);
            // The durable intent was written before touching the owned child.
            // Keep it on a partial shutdown failure: the global reaper may still
            // stop that process, and recovery must then restore the exact port
            // and bearer token. If it stayed live, recovery verifies the same
            // endpoint and can safely clear the intent.
            Some(port)
        }
        OwnedBridgeStop::FailedBeforeIntent(_) => unreachable!("returned before global reaper"),
    };
    UpdateShutdownReport {
        reaper,
        http_bridge_port,
        lifecycle_errors,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRecoveryReport {
    /// True only when an HTTP endpoint existed before shutdown and is available
    /// again. External stdio clients are deliberately not represented as restored.
    http_bridge_recovered: bool,
    /// Endpoint availability and marker cleanup are separate facts. A failed
    /// delete must not turn a verified live endpoint into a false "not restored"
    /// message, but the retained marker still needs to be surfaced.
    cleanup_warning: Option<String>,
}

/// Restore the transport Toolport owns after an update aborts. External clients
/// own their gateway stdin/stdout pipes, so fabricating replacement children here
/// would create disconnected processes and a false recovery claim.
#[tauri::command]
fn recover_update_gateways(
    bridge: State<HttpBridgeState>,
    http_bridge_port: Option<u16>,
) -> Result<UpdateRecoveryReport, String> {
    let Some(port) = http_bridge_port else {
        return Ok(UpdateRecoveryReport::default());
    };
    let intent = load_update_http_bridge_intent()?
        .ok_or_else(|| "HTTP bridge update state is missing; start it again from Settings".to_string())?;
    if intent.port != port {
        return Err(format!(
            "HTTP bridge update state expected port {}, not {port}",
            intent.port
        ));
    }
    ensure_http_bridge_at(bridge.inner(), port, Some(intent.token))?;
    let cleanup_warning = clear_update_http_bridge_intent().err();
    Ok(UpdateRecoveryReport {
        http_bridge_recovered: true,
        cleanup_warning,
    })
}

/// Apps still launching an obsolete gateway, accumulated across reaper passes.
///
/// Held rather than recomputed on demand because the advice is only knowable from
/// a pre-kill process table: once a pass has stopped the obsolete gateways, a fresh
/// query reads a table with the evidence already removed.
///
/// The merge is a **union keyed by client pid**, never a replace. #542 shipped an
/// unconditional write and the failure was immediate: the launch pass correctly
/// recorded "restart Claude" and killed the process, the user opened Settings and
/// clicked **Run** (the obvious next action), Claude had not made a tool call yet so
/// the new snapshot was empty, and the empty snapshot overwrote the good advice. The
/// panel vanished and the UI claimed "No old gateway processes found" while Claude
/// was still pinned to an obsolete binary, for the rest of the session.
///
/// Entries expire on their own terms: a pid that is no longer running means the user
/// restarted that app, which is the only evidence of compliance that actually exists.
/// Absence of a respawned gateway is not evidence, because a client that simply has
/// not made a tool call yet looks identical.
#[derive(Default)]
struct RestartAdvice(Mutex<Vec<crate::gateway_publish::ClientNeedingRestart>>);

impl RestartAdvice {
    /// Fold one pass's pre-kill findings into the stored set and return the result.
    fn merge(
        &self,
        fresh: Vec<crate::gateway_publish::ClientNeedingRestart>,
    ) -> Vec<crate::gateway_publish::ClientNeedingRestart> {
        let mut stored = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in fresh {
            // One row per app to act on. A client that respawned two different
            // obsolete gateways is still one restart.
            if !stored.iter().any(|e| e.client_pid == entry.client_pid) {
                stored.push(entry);
            }
        }
        stored.retain(|e| crate::gateway_publish::pid_is_running(e.client_pid));
        stored.clone()
    }

    fn current(&self) -> Vec<crate::gateway_publish::ClientNeedingRestart> {
        let mut stored = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stored.retain(|e| crate::gateway_publish::pid_is_running(e.client_pid));
        stored.clone()
    }
}

/// What a reaper run did and what the user still has to do about it.
///
/// One payload rather than a killed-list plus a separate advice query, so the two
/// cannot describe different moments: any second call necessarily observes a table
/// the first one already changed.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReapOutcome {
    killed: Vec<String>,
    /// Processes that matched but could not be stopped. Previously dropped, which
    /// made "found nothing stale" and "found something and failed to kill it"
    /// indistinguishable in the UI (#542 review).
    failed: Vec<String>,
    needs_restart: Vec<crate::gateway_publish::ClientNeedingRestart>,
}

/// Stop obsolete gateway processes (older versions / stale paths), keeping the
/// current resolved binary. Safe to run any time; used from Settings and launch.
#[tauri::command]
fn stop_stale_gateways(
    bridge: State<HttpBridgeState>,
    advice: State<RestartAdvice>,
) -> ReapOutcome {
    reap_stale_and_restore_bridge(bridge.inner(), advice.inner())
}

/// Apps that need restarting, without running a reaper pass.
#[tauri::command]
fn clients_needing_restart(
    advice: State<RestartAdvice>,
) -> Vec<crate::gateway_publish::ClientNeedingRestart> {
    advice.current()
}

/// Log a reaper pass. `failed` is logged separately from `killed` so "found nothing"
/// and "found something and could not stop it" are distinguishable in a support log.
fn log_reap_outcome(kind: &str, outcome: &ReapOutcome) {
    if !outcome.killed.is_empty() {
        eprintln!(
            "toolport: {kind} stopped {} stale gateway process(es): {}",
            outcome.killed.len(),
            outcome.killed.join("; ")
        );
    }
    if !outcome.failed.is_empty() {
        eprintln!(
            "toolport: {kind} could not stop {} gateway process(es): {}",
            outcome.failed.len(),
            outcome.failed.join("; ")
        );
    }
    if !outcome.needs_restart.is_empty() {
        eprintln!(
            "toolport: {} app(s) are still launching an obsolete gateway and need restarting: {}",
            outcome.needs_restart.len(),
            outcome
                .needs_restart
                .iter()
                .map(|c| format!("{} (pid {}) -> {}", c.client, c.client_pid, c.gateway))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

/// Entries whose client has not already been announced.
///
/// A later pass reads the merged advice, which by design still holds everything an
/// earlier pass found, so it must be filtered before it becomes a second toast for
/// the same app. Pure so the "only new clients interrupt the user" rule is pinned by
/// a test rather than by the shape of a closure.
fn unannounced(
    already: &[u32],
    current: Vec<crate::gateway_publish::ClientNeedingRestart>,
) -> Vec<crate::gateway_publish::ClientNeedingRestart> {
    current
        .into_iter()
        .filter(|c| !already.contains(&c.client_pid))
        .collect()
}

/// Tell the frontend which apps need restarting, if any.
fn announce_restart_needed(
    handle: &tauri::AppHandle,
    needs_restart: &[crate::gateway_publish::ClientNeedingRestart],
) {
    if needs_restart.is_empty() {
        return;
    }
    if let Err(e) = handle.emit("gateway-restart-needed", needs_restart) {
        eprintln!("toolport: could not emit gateway-restart-needed: {e}");
    }
}

/// Run the stale reaper, then bring the supervised HTTP bridge back if the reaper
/// stopped it (SOU-418 / SOU-432).
///
/// The reaper is *supposed* to kill a bridge running a replaced binary - that is the
/// whole point of SOU-414, and on Linux `2ba9f95` guarantees it, because an exe ending
/// in ` (deleted)` deliberately misses keep-paths after an in-place upgrade. But nothing
/// respawns it: `http_bridge_alive` only clears the tracking state, so HTTP/OpenAPI
/// clients would stay dark until someone re-opened Settings. Restarting on the same port
/// keeps SOU-414's guarantee (the old image dies) without stranding the transport.
///
/// Connected clients are unaffected by the new process: they authenticate with the
/// per-client bearers in `http_clients`, not the bridge's own env token.
fn reap_stale_and_restore_bridge(bridge: &HttpBridgeState, advice: &RestartAdvice) -> ReapOutcome {
    let mut extra_keep = Vec::new();
    if let Some(p) = clients::resolve_gateway_path() {
        extra_keep.push(p);
    }
    // Port of a bridge that is alive *before* the reap; None means there is nothing
    // to restore and the reaper's outcome is final.
    let was_serving = {
        let mut b = bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        http_bridge_alive(&mut b).then(|| (b.port, b.token.clone()))
    };
    let report = crate::gateway_publish::reap_stale(&extra_keep);
    // Merged from this pass's pre-kill snapshot, which is the only moment it can be
    // known. Union, not replace: see `RestartAdvice`.
    let mut failed = report.failed.clone();
    for remaining in &report.remaining {
        if !failed.contains(remaining) {
            failed.push(remaining.clone());
        }
    }
    let outcome = ReapOutcome {
        killed: report.killed.clone(),
        // Include the final alive set in the existing failure surface so Settings
        // cannot report success merely because the OS accepted a termination request.
        failed,
        needs_restart: advice.merge(report.needs_restart),
    };
    if let Some((Some(port), token)) = was_serving {
        let still_serving = {
            let mut b = bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            http_bridge_alive(&mut b)
        };
        if !still_serving {
            match ensure_http_bridge_at(bridge, port, token) {
                Ok(()) => eprintln!(
                    "toolport: restarted the HTTP endpoint on port {port} after the stale \
                     reaper stopped its previous (replaced) binary"
                ),
                Err(last) => eprintln!(
                    "toolport: the stale reaper stopped the HTTP endpoint on port {port} and it \
                     could not be restarted: {last}. Start it again from Settings."
                ),
            }
        }
    }
    outcome
}

/// Ensure the supervised HTTP bridge is available on its previous port. The
/// just-stopped child may need a moment to release the listener, so retry within a
/// short, deterministic bound. Idempotent when the tracked bridge is already alive.
fn ensure_http_bridge_at(
    state: &HttpBridgeState,
    port: u16,
    token: Option<String>,
) -> Result<(), String> {
    let tracked_live = {
        let mut bridge = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if http_bridge_alive(&mut bridge) {
            if bridge.port == Some(port)
                && token
                    .as_ref()
                    .is_none_or(|expected| bridge.token.as_ref() == Some(expected))
            {
                true
            } else {
                return Err(format!(
                    "another Toolport HTTP endpoint is already running on port {}",
                    bridge.port.unwrap_or_default()
                ));
            }
        } else {
            false
        }
    };
    if tracked_live {
        let expected = token.as_deref().ok_or_else(|| {
            "could not verify the tracked HTTP endpoint without its bearer token".to_string()
        })?;
        return http_bridge_identity_ready(port, expected)
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "the tracked Toolport HTTP endpoint on port {port} did not pass authenticated readiness"
                )
            });
    }

    let mut last = String::new();
    for attempt in 0..5 {
        match start_http_bridge_with_token_at(state, Some(port), token.clone()) {
            Ok(status)
                if status.port == Some(port)
                    && token
                        .as_ref()
                        .is_none_or(|expected| status.token.as_ref() == Some(expected)) =>
            {
                return Ok(());
            }
            Ok(status) => {
                return Err(format!(
                    "another Toolport HTTP endpoint is already running on port {}",
                    status.port.unwrap_or_default()
                ));
            }
            Err(error) => last = error,
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Err(last)
}

/// Start `toolport-gateway --http <port>` as a supervised child so HTTP/OpenAPI
/// clients can connect. Idempotent: if it's already running, returns the current
/// status; otherwise spawns the bundled gateway binary and tracks it.
#[tauri::command]
fn start_http_bridge(
    state: State<HttpBridgeState>,
    port: Option<u16>,
) -> Result<HttpBridgeStatus, String> {
    start_http_bridge_at(state.inner(), port)
}

/// Body of [`start_http_bridge`], taking the state directly so non-command callers
/// (the reaper's bridge restore) can reach it without a `State` wrapper.
fn start_http_bridge_at(
    state: &HttpBridgeState,
    port: Option<u16>,
) -> Result<HttpBridgeStatus, String> {
    // Every ordinary/user-initiated start supersedes an interrupted-update
    // marker, including install/migration paths that call this helper directly.
    // Recovery paths use the token-preserving lower-level function instead.
    clear_update_http_bridge_intent()?;
    start_http_bridge_with_token_at(state, port, None)
}

fn http_bridge_identity_ready(port: u16, token: &str) -> bool {
    use std::io::Read as _;

    let response = match ureq::get(&format!("http://127.0.0.1:{port}/"))
        .timeout(Duration::from_millis(300))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(response) if response.status() == 200 => response,
        _ => return false,
    };
    let mut body = String::new();
    response
        .into_reader()
        .take(4 * 1024)
        .read_to_string(&mut body)
        .is_ok()
        && body.starts_with("Toolport gateway (HTTP mode).")
}

fn start_http_bridge_with_token_at(
    state: &HttpBridgeState,
    port: Option<u16>,
    token: Option<String>,
) -> Result<HttpBridgeStatus, String> {
    let port = port.unwrap_or(8765);
    let mut bridge = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if http_bridge_alive(&mut bridge) {
        return Ok(HttpBridgeStatus::new(bridge.port, bridge.token.clone()));
    }
    // Fail fast if the port is already taken (another instance, or a stray
    // gateway). Otherwise the child would just exit on the bind error and we'd
    // wrongly report success while the user is actually talking to whatever
    // already owns the port.
    if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        return Err(format!(
            "Port {port} is already in use. Stop whatever is using it, then try again."
        ));
    }
    let bin = clients::resolve_gateway_path()
        .ok_or_else(|| "toolport-gateway binary not found next to the app".to_string())?;
    // Auto-generate a bearer token the client must send on every request.
    // Without it, any local process (including a web page open in the user's
    // browser) could POST to the port and run their tools.
    let token = match token {
        Some(token) => token,
        None => {
            let mut tok = [0u8; 24];
            getrandom::getrandom(&mut tok)
                .map_err(|e| format!("could not generate a token: {e}"))?;
            tok.iter().map(|b| format!("{b:02x}")).collect()
        }
    };
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("--http")
        .arg(port.to_string())
        // Prefer TOOLPORT_*; also set CONDUIT_* so a mixed-version gateway binary
        // still authenticates during an upgrade window.
        .env("TOOLPORT_HTTP_TOKEN", &token)
        .env("CONDUIT_HTTP_TOKEN", &token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Don't flash a console window on Windows.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the HTTP bridge: {e}"))?;
    // Confirm the spawned process is the authenticated Toolport gateway, not
    // merely that some listener won a bind race on this port. The preserved
    // bearer is high-entropy identity evidence and the fixed root response proves
    // the peer completed Toolport's auth and routing path.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "The HTTP endpoint exited on startup ({status}). Is port {port} already in use?"
            ));
        }
        if http_bridge_identity_ready(port, &token) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "The HTTP endpoint did not come up on port {port} within 5s."
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bridge.child = Some(child);
    bridge.port = Some(port);
    bridge.token = Some(token.clone());
    Ok(HttpBridgeStatus::new(Some(port), Some(token)))
}

/// Stop the supervised HTTP bridge child, if any.
#[tauri::command]
fn stop_http_bridge(state: State<HttpBridgeState>) -> Result<HttpBridgeStatus, String> {
    let mut bridge = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Clear the resume marker before stopping the live endpoint. If cleanup is
    // denied, leave the endpoint running and report the error; otherwise a later
    // launch could silently resurrect something the user explicitly disabled.
    clear_update_http_bridge_intent()?;
    if let Some(mut child) = bridge.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    bridge.port = None;
    bridge.token = None;
    Ok(HttpBridgeStatus::new(None, None))
}

fn resume_http_bridge_after_update(state: &HttpBridgeState) -> Result<bool, String> {
    let Some(intent) = load_update_http_bridge_intent()? else {
        return Ok(false);
    };
    ensure_http_bridge_at(state, intent.port, Some(intent.token))?;
    if let Err(error) = clear_update_http_bridge_intent() {
        eprintln!(
            "toolport: restored the HTTP endpoint, but could not clear its update resume state: {error}"
        );
    }
    Ok(true)
}

/// Report whether the HTTP bridge is running, reaping it if it has exited.
#[tauri::command]
fn http_bridge_status(state: State<HttpBridgeState>) -> Result<HttpBridgeStatus, String> {
    let mut bridge = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    http_bridge_alive(&mut bridge);
    Ok(HttpBridgeStatus::new(bridge.port, bridge.token.clone()))
}

/// macOS only: show the Dock icon when a window is visible, and drop it (Accessory
/// activation policy) when the app is only in the menu bar, so Toolport is never in
/// both the Dock and the menu bar at once. No-op on Windows/Linux, which have no
/// such concept and keep their normal taskbar/tray behavior.
#[cfg(target_os = "macos")]
fn set_dock_icon_visible(app: &AppHandle, visible: bool) {
    let policy = if visible {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    };
    let _ = app.set_activation_policy(policy);
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon_visible(_app: &AppHandle, _visible: bool) {}

/// Wayland workaround (SBS-813): a window created hidden and shown later has a
/// stale input region under tao 0.35, so the native titlebar buttons ignore
/// clicks until something forces a surface reconfigure (the user's repro:
/// maximize + restore heals it). Nudge the size by one pixel and back to force
/// that reconfigure invisibly. Fixed upstream in tao 0.36 (tauri-apps/tao#1218,
/// ships with Tauri 2.12) — remove this when the dependency bump lands.
#[cfg(target_os = "linux")]
static WAYLAND_NUDGE_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "linux")]
fn nudge_wayland_input_region(w: &tauri::WebviewWindow) {
    use std::sync::atomic::Ordering;

    if std::env::var("WAYLAND_DISPLAY").is_err() {
        return; // X11 sessions are unaffected.
    }
    // One nudge at a time. `show_main_window` runs on every tray click, second
    // instance, and approval reveal, and each nudge drives the window through
    // maximize -> unmaximize -> set_size over about a second. Overlapping runs
    // would fight each other: one reads `prior` while another has the window
    // maximized, then restores that maximized size as the "real" one. A window
    // already visible enough to be re-shown does not need a second heal anyway.
    if WAYLAND_NUDGE_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    // The reconfigure only heals a MAPPED surface, and `show()` has not mapped
    // it yet when this runs — an immediate resize is a no-op for the bug. Give
    // the compositor a beat to map the window first, off the main thread so a
    // slow map never delays the reveal itself.
    let w = w.clone();
    std::thread::spawn(move || {
        // Cleared however this thread leaves, including the early returns below.
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                WAYLAND_NUDGE_RUNNING
                    .store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _done = Done;
        std::thread::sleep(std::time::Duration::from_millis(150));
        // A maximized window already has a fresh configure (that's why the
        // manual maximize/restore workaround heals the buttons) — and it's
        // also the state we must not disturb. Fullscreen is the same story with
        // a worse failure: maximize/unmaximize on a fullscreen surface drops
        // fullscreen and leaves the user in a windowed frame they never asked
        // for. Both states are already configured, so there is nothing to heal.
        if w.is_maximized().unwrap_or(false) || w.is_fullscreen().unwrap_or(false) {
            return;
        }
        // A plain 1px resize was tested and does NOT heal the input region;
        // only the maximize state change does. The flick can be briefly
        // visible — the lesser evil next to dead window controls.
        //
        // Unmaximize restores broken geometry on this compositor (oversized,
        // titlebar off-screen), so remember the real size and put it back
        // explicitly, then re-center (a no-op where the compositor owns
        // placement, correct everywhere else).
        let prior = w.inner_size().ok();
        let _ = w.maximize();
        std::thread::sleep(std::time::Duration::from_millis(80));
        let _ = w.unmaximize();
        // Unmaximize completes asynchronously (a compositor configure
        // round-trip), and its restore geometry lands AFTER any set_size
        // issued immediately — overwriting it with a broken oversized frame.
        // Wait for the state to actually flip before enforcing a size.
        let mut settled = false;
        for attempt in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if !w.is_maximized().unwrap_or(true) {
                settled = true;
                break;
            }
            // Re-issue periodically: the maximize has already happened, so
            // giving up here would leave the user staring at a maximized window
            // they never asked for. A dropped request is the likely cause, and
            // unmaximize on an already-restored window is a no-op.
            if attempt % 5 == 4 {
                let _ = w.unmaximize();
            }
        }
        if !settled {
            // Out of retries. Restoring the size is unsafe now (the window is
            // still maximized, so the geometry would be wrong), but leaving it
            // maximized is not an option either: one last request, then stop.
            let _ = w.unmaximize();
            return;
        }
        // Clamp the remembered size to the current monitor's usable area. The
        // window-state plugin can hold a size that no longer fits (a broken
        // restore geometry persisted on quit, or a saved state from a larger
        // monitor); restoring it verbatim opens the window below the fold.
        // Clamping here also heals the persisted state: the plugin saves the
        // clamped size on the next quit.
        let target = prior.map(|mut s| {
            if let Ok(Some(mon)) = w.current_monitor() {
                let m = mon.size();
                s.width = s.width.min(m.width * 95 / 100);
                s.height = s.height.min(m.height * 85 / 100);
            }
            s
        });
        if let Some(size) = target {
            for _ in 0..10 {
                let _ = w.set_size(size);
                std::thread::sleep(std::time::Duration::from_millis(50));
                if w.inner_size().map(|s| s == size).unwrap_or(false) {
                    break;
                }
            }
        }
        let _ = w.center();
    });
}

#[cfg(not(target_os = "linux"))]
fn nudge_wayland_input_region(_w: &tauri::WebviewWindow) {}

/// Bring the main window back to the foreground (from the tray, a re-launch, or an
/// approval). Un-hides, un-minimizes, and focuses so it works from every hidden state.
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        // A visible window means the app should own a Dock icon again (macOS).
        set_dock_icon_visible(app, true);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        nudge_wayland_input_region(&w);
        // Tell the frontend the window is visible again so the team-sync loop resumes and does
        // an immediate catch-up poll. The webview's Page Visibility API doesn't report Tauri
        // tray show/hide on Windows, so this event is the authoritative signal (see the
        // team-sync effect in App.tsx and `main_window_visible`).
        let _ = app.emit("team-window-visible", true);
    }
}

/// Whether the main window is currently shown (vs hidden to the tray). Seeds the frontend
/// team-sync loop's visibility gate on mount - live changes come via the `team-window-visible`
/// event emitted from show/hide. Defaults to visible if the window is missing or the platform
/// query fails, so sync never wedges off on an unexpected error.
#[tauri::command]
fn main_window_visible(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(true)
}

/// Reflect the pending-approval count on the tray tooltip, so a glance at the tray
/// tells you something is waiting even with the window hidden (complements the OS
/// notification the broker already fires). Best-effort.
fn update_tray_tooltip(app: &AppHandle) {
    let pending = app
        .try_state::<approval_broker::ApprovalBroker>()
        .map(|b| b.list().len())
        .unwrap_or(0);
    if let Some(menu) = app.try_state::<TrayMenuState>() {
        let _ = menu
            .pending_approvals
            .set_text(format!("Pending approvals ({pending})"));
    }
    if let Some(tray) = app.tray_by_id("main") {
        let tip = if pending > 0 {
            format!(
                "Toolport - {pending} request{} awaiting action",
                if pending == 1 { "" } else { "s" }
            )
        } else {
            "Toolport".to_string()
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}

/// The first time the window is closed to the tray, tell the user it's still running
/// (so a background HITL gate isn't a surprise) and how to fully quit. Once ever: a
/// marker file in the data dir gates it.
fn maybe_show_tray_hint(app: &AppHandle) {
    let Some(dir) = registry::conduit_dir() else {
        return;
    };
    let marker = dir.join(".tray-hint-shown");
    if marker.exists() {
        return;
    }
    let _ = std::fs::write(&marker, b"1");
    let _ = app
        .notification()
        .builder()
        .title("Toolport is still running")
        .body(
            "It stays in your tray so it can hold tool calls for your approval. \
             Quit it any time from the tray icon.",
        )
        .show();
}

/// Build the system-tray (Windows) / menu-bar (macOS) icon. The menu exposes the two
/// background actions users need without hunting through a hidden window: pending
/// approvals and update checks. Quit fully exits (the run-loop's Exit handler tears
/// down the HTTP bridge); closing the window only hides it (see the window-event
/// handler), so the gateway/broker keep running and HITL stays live.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "tray_open", "Open Toolport", true, None::<&str>)?;
    let approvals = MenuItem::with_id(
        app,
        "tray_approvals",
        "Pending approvals (0)",
        true,
        None::<&str>,
    )?;
    let check_updates = MenuItem::with_id(
        app,
        "tray_check_updates",
        "Check for updates",
        true,
        None::<&str>,
    )?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "tray_quit", "Quit Toolport", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &approvals, &check_updates, &sep, &quit],
    )?;
    app.manage(TrayMenuState {
        pending_approvals: approvals,
    });

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Toolport")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray_open" => show_main_window(app),
            "tray_approvals" => {
                let should_emit = app
                    .try_state::<PendingTrayApprovals>()
                    .is_none_or(|state| should_emit_tray_approvals(state.inner()));
                show_main_window(app);
                if should_emit {
                    let _ = app.emit("tray-open-approvals", ());
                }
            }
            "tray_check_updates" => {
                show_main_window(app);
                let _ = app.emit("tray-check-updates", ());
            }
            "tray_quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    // macOS: use a monochrome glyph rendered as a template image, so the menu bar
    // tints it to match every other status item (white on the dark bar) instead of
    // showing the full-color app icon. Every other platform keeps the colored icon.
    #[cfg(target_os = "macos")]
    {
        let glyph = tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray-mac-template.png"
        ))?;
        builder = builder.icon(glyph).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Launch-at-login enable. On Linux this writes the XDG autostart entry with
/// `$APPIMAGE` when set, so AppImage sessions do not register the FUSE mount
/// (`/tmp/.mount_*`) that `current_exe` returns. Other platforms keep the
/// autostart plugin (`current_exe`).
#[tauri::command]
fn enable_launch_at_login(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::autostart::enable_linux(&app.package_info().name);
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().enable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn disable_launch_at_login(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::autostart::disable_linux(&app.package_info().name);
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn is_launch_at_login_enabled(app: AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    {
        return crate::autostart::is_enabled_linux(&app.package_info().name);
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().is_enabled().map_err(|e| e.to_string())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // `generate_context!()` must expand exactly once in this crate: on macOS dev
    // builds each expansion embeds a `_EMBED_INFO_PLIST` byte section, and two
    // collide at codegen ("symbol is already defined"). Both startup paths share
    // this one Context; exactly one of them consumes it.
    let context = tauri::generate_context!();
    let registry = match registry::load() {
        Ok(registry) => registry,
        Err(error) => {
            run_registry_startup_failure(error, context);
            return;
        }
    };

    // Migrate legacy keychain secrets into the data-protection keychain (the
    // team-scoped shared access group) in the background. On macOS, older versions
    // of Toolport stored secrets as per-app-ACL keychain items that trigger a
    // password prompt every time a freshly-signed app/gateway reads them. This
    // moves each value into the data-protection keychain, which the separately
    // signed gateway reads with NO prompt across updates — read each value, write +
    // verify the data-protection copy, then delete the legacy item (no secret is
    // lost; an item that can't move is left in place). Guarded by a marker file so
    // it runs once. Only the app runs this (the gateway is read-only). Best-effort:
    // failures are logged but never block startup.
    {
        let reg = registry.clone();
        std::thread::spawn(move || {
            // Collect every secret key the registry knows about: env vars marked
            // secret, plus the reserved keys for remote servers and team tokens.
            let mut keys: Vec<(String, String)> = Vec::new();
            for server in &reg.servers {
                for e in &server.env {
                    if e.secret {
                        keys.push((server.id.clone(), e.key.clone()));
                    }
                }
                if server.url.is_some() {
                    // Remote servers store both the auth token and OAuth state.
                    keys.push((server.id.clone(), secrets::HTTP_AUTH_KEY.to_string()));
                    keys.push((server.id.clone(), remote::OAUTH_STATE_KEY.to_string()));
                }
            }
            // Team member token (one global slot, not per-server).
            keys.push((
                teams::TEAM_TOKEN_SERVER.to_string(),
                teams::TEAM_TOKEN_KEY.to_string(),
            ));
            let report = secrets::migrate_secrets_to_dpk(&keys);
            if report.migrated > 0 || report.failed > 0 {
                eprintln!(
                    "conduit: keychain migration complete ({} entries moved to data-protection keychain, {} failed, {} not found)",
                    report.migrated, report.failed, report.not_found
                );
            }
        });
    }

    if !registry.live_inspect {
        inspect::clear();
    }

    tauri::Builder::default()
        // Single-instance must be registered first. With its `deep-link` feature
        // a second launch carrying a conduit:// URL is forwarded to the deep-link
        // plugin's on_open_url (set up below); here we just focus the window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A second launch (or clicking the app while it's hidden in the tray) should
            // bring the window back, not just focus a hidden window.
            show_main_window(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Launch at login (opt-in via Settings). `--hidden` is passed on auto-launch so
        // the app starts to the tray without flashing a window (see setup()).
        // Linux AppImage sessions do not use this plugin's path: Settings goes
        // through enable_launch_at_login, which writes $APPIMAGE (SBS-844).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        // Remember where the user put the window (SBS-144). Without this every cold
        // start reopens at the fixed 1240x820 centered geometry from tauri.conf.json,
        // undoing a resize or a move on every quit, reboot, and update relaunch.
        //
        // VISIBLE is skipped deliberately: the window is configured `visible: false` and
        // shown by `setup()` (or kept hidden for a `--hidden` autostart into the tray).
        // Letting the plugin restore visibility would fight that and flash a window on
        // every launch-at-login.
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        - tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .manage(Mutex::new(registry))
        .manage(Mutex::new(HttpBridge::default()))
        .manage(PendingShare::default())
        .manage(PendingTrayApprovals::default())
        .manage(RestartAdvice::default())
        .invoke_handler(tauri::generate_handler![
            detect_clients,
            get_registry,
            take_registry_recovery_notice,
            import_servers,
            preview_import_servers,
            parse_server_snippet,
            add_server,
            update_server,
            remove_server,
            set_server_enabled,
            create_profile,
            delete_profile,
            set_active_profile,
            set_folder_profiles,
            set_profile_server_tools,
            write_to_client,
            install_gateway,
            uninstall_gateway,
            migrate_client,
            set_secret,
            delete_secret,
            set_client_credentials,
            clear_client_credentials,
            has_client_secret,
            secret_status,
            get_audit_log,
            audit_stats,
            get_security_events,
            savings_summary,
            gather_diagnostics,
            probe_servers,
            test_server,
            list_server_tools,
            list_server_resources,
            list_server_prompts,
            read_resource,
            get_prompt,
            add_http_client,
            remove_http_client,
            call_tool,
            set_tool_enabled,
            set_tool_pinned,
            set_deny_destructive,
            set_confirm_destructive,
            set_human_approval,
            list_pending_approvals,
            decide_approval,
            list_routine_suggestions,
            approve_routine_suggestion,
            dismiss_routine_suggestion,
            list_allowed_tools,
            revoke_allowed_tool,
            set_tool_override,
            clear_tool_override,
            set_live_inspect,
            get_inspect_log,
            clear_inspect_log,
            get_search_traces,
            clear_search_traces,
            clear_activity_logs,
            list_tool_identities,
            set_quarantine_on_drift,
            set_block_on_injection,
            set_pii_redaction,
            list_quarantined,
            release_quarantine,
            release_all_quarantine,
            set_lazy_discovery,
            set_code_mode,
            set_allow_routine_writes,
            set_allow_agent_control,
            set_client_discovery,
            team_connect,
            team_join_poll,
            team_sync,
            team_sync_wait,
            main_window_visible,
            team_instructions_status,
            team_disconnect,
            team_push_preview,
            team_push,
            set_auth_token,
            clear_auth_token,
            has_auth_token,
            authenticate_oauth,
            probe_auth,
            popular_catalog,
            list_stacks,
            search_catalog,
            open_data_dir,
            set_all_enabled,
            export_config,
            export_config_to_path,
            export_audit_to_path,
            share_stack,
            fetch_shared_setup,
            take_pending_shared,
            take_pending_tray_approvals,
            import_config,
            read_setup_file,
            preview_import,
            start_http_bridge,
            stop_http_bridge,
            http_bridge_status,
            stop_spawned_gateways,
            recover_update_gateways,
            stop_stale_gateways,
            clients_needing_restart,
            enable_launch_at_login,
            disable_launch_at_login,
            is_launch_at_login_enabled,
        ])
        // Close-to-tray: the window's X hides it instead of quitting, so the gateway and
        // approval broker keep running (HITL only works while the app is alive). Quit is
        // explicit, from the tray menu. A one-time notification explains it the first time.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    // Hidden to the tray => menu-bar only, so drop the Dock icon (macOS).
                    set_dock_icon_visible(window.app_handle(), false);
                    // Tell the frontend the window is hidden so the team-sync loop parks and
                    // stops polling the team server (each poll would otherwise keep a
                    // scale-to-zero Postgres awake). Resumes via show_main_window's emit.
                    let _ = window.app_handle().emit("team-window-visible", false);
                    maybe_show_tray_hint(window.app_handle());
                }
            }
        })
        .setup(|app| {
            let handle = app.handle();

            // AppImage launch-at-login: if a previous session registered the
            // ephemeral FUSE mount, rewrite Exec to $APPIMAGE (SBS-844).
            #[cfg(target_os = "linux")]
            crate::autostart::repair_linux(&app.package_info().name);

            // Build the tray icon, then show the window - unless launched with `--hidden`
            // (auto-start at login), in which case we start straight to the tray. The
            // window is created hidden (visible:false) so a normal launch never flashes.
            build_tray(handle)?;
            let start_hidden = std::env::args().any(|a| a == "--hidden");
            if start_hidden {
                // Auto-start at login goes straight to the tray: menu-bar only, no
                // Dock icon until the user opens the window.
                set_dock_icon_visible(handle, false);
            } else {
                show_main_window(handle);
            }

            // Keep the tray tooltip's pending-approval count fresh as calls are held and
            // resolved (the broker emits these; the window may be hidden in the tray).
            let h = handle.clone();
            app.listen("approval-pending", move |_| update_tray_tooltip(&h));
            let h = handle.clone();
            app.listen("approval-resolved", move |_| update_tray_tooltip(&h));

            // Mirror external registry changes (an agent toggling a server through
            // the gateway) back into the app and the UI, in a background thread.
            let handle = app.handle().clone();
            std::thread::spawn(move || watch_registry_for_app(handle));

            // One-time, idempotent migrations on launch:
            //   1. Rename the data-dir leaf Conduit → Toolport when safe (legacy
            //      installs keep working on the old leaf until this succeeds).
            //   2. Publish the versioned gateway binary into that dir.
            //   3. Re-point client configs that still use the conduit entry name,
            //      CONDUIT_* env keys, conduit-gateway binary, or the old data-dir
            //      path. Surgical + backed up; a no-op once every client is current.
            // Handle for the migration thread: the reaper at the end of it needs
            // HttpBridgeState so a stopped bridge can be brought back (SOU-418).
            let migrate_handle = app.handle().clone();
            std::thread::spawn(move || {
                // Prefer a quiet data-dir rename before publishing/repointing so the
                // new bin path is under Toolport and client configs get that path.
                // Gateways holding files open may block the rename; we then keep the
                // legacy leaf and still repoint names/env keys.
                if let Some(migrated) = registry::migrate_legacy_data_dir() {
                    eprintln!(
                        "toolport: migrated data directory to {}",
                        migrated.display()
                    );
                }
                if let Some(published) = crate::gateway_publish::publish_bundled_gateway() {
                    eprintln!(
                        "toolport: published client gateway at {}",
                        published.display()
                    );
                }
                // An in-app update deliberately stops the supervised HTTP child
                // before replacing files. The durable intent carries its exact
                // port and bearer across relaunch, so the newly installed app
                // restores connectivity without rotating credentials.
                match resume_http_bridge_after_update(
                    migrate_handle.state::<HttpBridgeState>().inner(),
                ) {
                    Ok(true) => eprintln!(
                        "toolport: restored the HTTP endpoint after the in-app update"
                    ),
                    Ok(false) => {}
                    Err(error) => eprintln!(
                        "toolport: could not restore the HTTP endpoint after the in-app update: {error}"
                    ),
                }
                // Ownership map from disk (this launch thread has no RegistryState handle).
                let managed_snapshot = registry::load()
                    .map(|r| r.client_managed_entries)
                    .unwrap_or_default();
                let repoint = clients::repoint_stale_gateways(&managed_snapshot);
                if !repoint.repointed.is_empty() {
                    let ids: Vec<&str> = repoint
                        .repointed
                        .iter()
                        .map(|(id, _)| id.as_str())
                        .collect();
                    eprintln!(
                        "toolport: re-pointed {} client config(s) to the renamed gateway: {}",
                        repoint.repointed.len(),
                        ids.join(", ")
                    );
                    // Refresh ownership records for everything we rewrote (SOU-406).
                    let _ = registry::update(|reg| {
                        for (id, entry) in &repoint.repointed {
                            reg.set_client_managed_entry(id, entry.clone());
                        }
                        Ok(())
                    });
                }
                if !repoint.customized.is_empty() {
                    eprintln!(
                        "toolport: left {} client config(s) alone (custom configuration): {}",
                        repoint.customized.len(),
                        repoint.customized.join(", ")
                    );
                }
                if !repoint.failed.is_empty() {
                    // A client that needed migrating and could not be written stays
                    // on a superseded gateway until someone notices. Keep it
                    // distinguishable from "nothing to do" at the call site too,
                    // not just in the gateway log.
                    eprintln!(
                        "toolport: FAILED to re-point {} client config(s); they will keep \
                         launching their previous gateway: {}",
                        repoint.failed.len(),
                        repoint
                            .failed
                            .iter()
                            .map(|(id, why)| format!("{id} ({why})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                // Stop obsolete gateway processes. Path-based identity on all OS
                // (SOU-414); not gated on repoint (SOU-306).
                //
                // Reaping alone does NOT always deliver new gateway code. A client reads
                // its config once at its own startup and caches the spawn command. Where
                // that cached path is replaced in place the next spawn picks up the new
                // binary and the reap is sufficient. Where the path is one an upgrade
                // never rewrites (a versioned filename, or an install location the app has
                // since moved away from) the client relaunches the same obsolete binary
                // and only restarting that application fixes it. Do not restate the old
                // claim that no agent restart is needed; it was false for every client
                // observed in the SOU-418 smoke pass (SOU-435).
                //
                // Pass resolve_gateway_path so the nested macOS helper /
                // AppImage stable path is kept even when publish is Windows-only.
                //
                // Run once immediately, then again after a short delay so a client that
                // race-respawns an old path between repoint and the first kill is cleaned up
                // without another full app restart.
                //
                // Both passes go through `reap_stale_and_restore_bridge` so a supervised
                // HTTP bridge the reaper stops (correctly, when its binary was replaced)
                // comes back instead of leaving HTTP/OpenAPI clients dark (SOU-418).
                //
                // The FIRST pass is the one that can see the restart advice: it runs
                // before anything has been killed, so its snapshot still holds the
                // obsolete gateways their clients spawned. The delayed pass contributes
                // whatever raced in after it, which the union in `RestartAdvice`
                // folds together rather than overwriting.
                let advice_state = migrate_handle.state::<RestartAdvice>();
                let stale = reap_stale_and_restore_bridge(
                    migrate_handle.state::<HttpBridgeState>().inner(),
                    advice_state.inner(),
                );
                log_reap_outcome("stale reaper", &stale);
                announce_restart_needed(&migrate_handle, &stale.needs_restart);
                // What the user has already been told about. The delayed pass reads
                // the MERGED list, which by design still contains everything the
                // first pass found, so announcing it wholesale would toast the same
                // apps twice three seconds apart. Only genuinely new clients are
                // worth interrupting for; the Settings panel is the durable view.
                let already_announced: Vec<u32> =
                    stale.needs_restart.iter().map(|c| c.client_pid).collect();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let again = reap_stale_and_restore_bridge(
                        migrate_handle.state::<HttpBridgeState>().inner(),
                        migrate_handle.state::<RestartAdvice>().inner(),
                    );
                    log_reap_outcome("delayed reaper", &again);
                    let newly_found = unannounced(&already_announced, again.needs_restart);
                    announce_restart_needed(&migrate_handle, &newly_found);

                    // Only now delete old gateway binaries (SOU-484). Both reaper
                    // passes have run, so clients that were going to respawn an
                    // obsolete image have done so and are recorded in the advice;
                    // deleting one a client still spawns would break it outright
                    // rather than leave it on old code. Last, and deliberately
                    // after the delay, because every input this needs is evidence
                    // the passes above produced.
                    let advised = migrate_handle
                        .state::<RestartAdvice>()
                        .current()
                        .into_iter()
                        .map(|c| c.gateway)
                        .collect();
                    crate::gateway_publish::prune_published_gateways(
                        clients::referenced_gateway_paths(),
                        advised,
                    );
                });
            });

            // Start the human-approval broker: it publishes a loopback endpoint that every
            // gateway process dials into, and holds gated tool calls until the user approves
            // or denies them here. Always managed so the approve/deny commands have state.
            let broker = approval_broker::start(app.handle().clone());
            app.manage(broker);

            // toolport://import?s=<id> (and legacy conduit://) deep links open the
            // shared-stack import. The installer registers the schemes; we also
            // register at runtime so they work unpackaged (dev). Three delivery
            // paths are covered:
            //   - cold start (app launched by the link): the URL is in this
            //     process's launch args, read via get_current();
            //   - already running (second launch): the single-instance plugin
            //     forwards the URL to on_open_url;
            //   - macOS: the OS delivers via on_open_url at launch and runtime.
            // Cold starts can arrive before the UI is listening, so the id is also
            // stashed for the frontend to claim on mount (take_pending_shared).
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                {
                    let _ = app.deep_link().register("toolport");
                    let _ = app.deep_link().register("conduit");
                }

                // Cold start: the URL(s) the app was launched with.
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    for url in urls {
                        if let Some(id) = parse_share_url(url.as_str()) {
                            deliver_shared_import(app.handle(), id);
                        }
                    }
                }

                // While running (and macOS launch): delivered as an event.
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        if let Some(id) = parse_share_url(url.as_str()) {
                            deliver_shared_import(&handle, id);
                        }
                    }
                });
            }
            Ok(())
        })
        .build(context)
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Never orphan the HTTP bridge: kill the supervised child on exit.
            if matches!(
                event,
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
            ) {
                if let Some(state) = app_handle.try_state::<HttpBridgeState>() {
                    let mut bridge =
                        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(mut child) = bridge.child.take() {
                        let _ = child.kill();
                    }
                }
            }
            // On FINAL exit only (not a cancelable ExitRequested), remove the approval
            // endpoint descriptor so a gateway dialing after we're gone reads no broker
            // (a clean Unreachable) rather than connecting to the dead port we left behind.
            if matches!(event, tauri::RunEvent::Exit) {
                approval_broker::clear_endpoint();
            }
        });
}

/// Start only the native recovery dialog when the registry cannot be loaded safely. The
/// normal app (and therefore every mutating command) is never initialized with an invented
/// empty registry. Closing the dialog exits; the user can then resolve a stuck lock or restore
/// one of the preserved backup/unreadable files before relaunching (SOU-331).
fn run_registry_startup_failure(error: String, context: tauri::Context) {
    let message = registry_startup_failure_message(registry::resolved_path().as_deref(), &error);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            app.dialog()
                .message(message)
                .title("Toolport could not start safely")
                .kind(MessageDialogKind::Error)
                .buttons(MessageDialogButtons::OkCustom("Close Toolport".to_string()))
                .show(move |_| handle.exit(1));
            Ok(())
        })
        .run(context)
        .expect("error while showing the Toolport registry recovery dialog");
}

fn registry_startup_failure_message(path: Option<&std::path::Path>, error: &str) -> String {
    let path = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the Toolport data directory".to_string());
    format!(
        "Toolport could not safely load its registry, so it stopped before showing or saving an empty configuration. Your registry was not replaced.\n\nClose any other Toolport processes and try again. If the problem continues, restore a registry backup or move the unreadable registry aside, then reopen Toolport.\n\nRegistry: {path}\n\nError: {error}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arg_looks_secret, redact_url_userinfo};
    use registry::EnvVar;

    fn unique_update_test_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "toolport-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn update_http_bridge_intent_round_trips_exact_token_and_clear_failures_surface() {
        let dir = unique_update_test_dir("update-bridge-intent");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resume.json");
        let intent = UpdateHttpBridgeIntent {
            port: 9876,
            token: "preserved-secret-token".to_string(),
        };

        save_update_http_bridge_intent_to(&path, &intent).unwrap();
        let loaded = load_update_http_bridge_intent_from(&path)
            .unwrap()
            .expect("intent exists");
        assert_eq!(loaded.port, intent.port);
        assert_eq!(loaded.token, intent.token);
        clear_update_http_bridge_intent_at(&path).unwrap();
        assert!(load_update_http_bridge_intent_from(&path).unwrap().is_none());

        let directory_marker = dir.join("not-a-file");
        std::fs::create_dir(&directory_marker).unwrap();
        assert!(clear_update_http_bridge_intent_at(&directory_marker).is_err());
        std::fs::remove_dir(&directory_marker).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn update_http_bridge_readiness_requires_the_exact_bearer_and_identity() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv().unwrap();
                let authorized = request.headers().iter().any(|header| {
                    header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer exact-secret"
                });
                let response = if authorized {
                    tiny_http::Response::from_string("Toolport gateway (HTTP mode).\n")
                        .with_status_code(200)
                } else {
                    tiny_http::Response::from_string("not authorized").with_status_code(401)
                };
                request.respond(response).unwrap();
            }
        });

        assert!(!http_bridge_identity_ready(port, "wrong-secret"));
        assert!(http_bridge_identity_ready(port, "exact-secret"));
        handle.join().unwrap();
    }

    #[test]
    fn tray_approvals_requests_choose_exactly_one_delivery_mode() {
        let pending = PendingTrayApprovals::default();

        // Before the frontend claims readiness, queue without emitting.
        assert!(!should_emit_tray_approvals(&pending));
        assert!(claim_pending_tray_approvals(&pending));
        assert!(!claim_pending_tray_approvals(&pending));

        // Once ready, deliver live without leaving a queued duplicate.
        assert!(should_emit_tray_approvals(&pending));
        assert!(!claim_pending_tray_approvals(&pending));
    }

    #[test]
    fn registry_startup_failure_is_blocking_and_preserves_the_real_path_and_error() {
        let path = std::path::Path::new(r"C:\Toolport\registry.json");
        let message = registry_startup_failure_message(Some(path), "Corrupt registry: bad json");
        assert!(message.contains("stopped before showing or saving an empty configuration"));
        assert!(message.contains(r"C:\Toolport\registry.json"));
        assert!(message.contains("Corrupt registry: bad json"));
        assert!(message.contains("Your registry was not replaced"));
    }

    /// A failed keychain read must surface as an error, never as "no secret
    /// stored": `Ok(false)` sends the UI down the first-time path and blocks a
    /// user whose secret really is vaulted (SBS-722). The reserved internal
    /// namespace is the one deterministic way to make `get_secret_result` fail
    /// on every platform, and `secrets::get_secret` swallows exactly that error
    /// into `None` -- which is the regression this pins against.
    #[test]
    fn has_client_secret_reports_a_failed_read_as_an_error_not_missing() {
        // The command went async (SBS-813); the probe semantics under test are
        // unchanged, so drive the future to completion on the runtime.
        let result =
            tauri::async_runtime::block_on(has_client_secret("__toolport_internal__".to_string()));
        assert!(
            result.is_err(),
            "a failed secret read must propagate, not resolve to a boolean: {result:?}"
        );
    }

    /// Same fail-closed contract as `has_client_secret` (SBS-841): a locked or
    /// otherwise failed vault read must not resolve to `(key, false)`, which the
    /// Secrets dialog treats as "not vaulted" and would overwrite. The reserved
    /// internal namespace is the deterministic way to make `get_secret_result`
    /// fail on every platform; `get_secret` swallows that into `None`.
    #[test]
    fn secret_status_reports_a_failed_read_as_an_error_not_missing() {
        let result = tauri::async_runtime::block_on(secret_status(
            "__toolport_internal__".to_string(),
            vec!["SOME_KEY".to_string()],
        ));
        assert!(
            result.is_err(),
            "a failed secret read must propagate, not resolve to unvaulted: {result:?}"
        );
    }

    /// An unreadable activity log must reject `audit_stats`, not resolve to
    /// `null` (SBS-873). Activity hides the dashboard on `null`, so a failed
    /// read looked like "no data" while `get_audit_log` on the same file
    /// rejected.
    #[test]
    fn audit_stats_rejects_an_unreadable_activity_log() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = unique_update_test_dir("audit-stats-unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        let _override = crate::registry::DataDirOverride::set(&dir);
        let path = audit::audit_path().expect("audit path under override");
        // IsADirectory: the log path exists but cannot be read as a file.
        std::fs::create_dir_all(&path).unwrap();

        let result = tauri::async_runtime::block_on(audit_stats());
        assert!(
            result.is_err(),
            "a failed activity-log read must reject, not resolve to null: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the contract: a missing log is an honest empty
    /// dashboard, so a first run still resolves.
    #[test]
    fn audit_stats_resolves_when_the_activity_log_is_missing() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = unique_update_test_dir("audit-stats-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let _override = crate::registry::DataDirOverride::set(&dir);
        let path = audit::audit_path().expect("audit path under override");
        assert!(!path.exists(), "fixture must not create the log");

        let stats = tauri::async_runtime::block_on(audit_stats())
            .expect("a missing log is an empty dashboard, not a failure");
        assert!(stats.is_object(), "expected aggregated stats: {stats}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn github_with_secret() -> ServerEntry {
        ServerEntry {
            id: "gh".into(),
            name: "GitHub".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec![],
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: Some("sk-live-xyz".into()),
                secret: true,
            }],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn probe_one_bounded_passes_through_a_fast_failure_well_under_the_timeout() {
        // A bogus command fails to spawn immediately, so the bounded wrapper must
        // return that result promptly (nowhere near PROBE_TIMEOUT) and carry the
        // server id - it only times out for a genuinely hung probe.
        let mut server = plain_server("bogus", "Bogus");
        server.command = Some("toolport-no-such-binary-xyz".into());
        let start = std::time::Instant::now();
        let r = probe_one_bounded(&server);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "a fast failure must not wait on the timeout"
        );
        assert!(!r.ok);
        assert_eq!(r.server_id, "bogus");
    }

    #[test]
    fn unreviewed_team_stdio_enable_is_refused_in_write_closure() {
        let mut reg = Registry::default();
        let mut s = plain_server("team-tool", "Team tool");
        s.source = Some("team:acme".into());
        reg.servers.push(s);

        let err = refuse_unreviewed_team_enable(&reg, "team-tool", true, false)
            .expect_err("stdio team server must not enable without review");
        assert!(err.contains("enable it from Teams after review"), "{err}");
        // The consent path (reviewed=true), disabling, and non-team servers pass.
        assert!(refuse_unreviewed_team_enable(&reg, "team-tool", true, true).is_ok());
        assert!(refuse_unreviewed_team_enable(&reg, "team-tool", false, false).is_ok());
        reg.servers[0].source = None;
        assert!(refuse_unreviewed_team_enable(&reg, "team-tool", true, false).is_ok());
    }

    fn plain_server(id: &str, name: &str) -> ServerEntry {
        ServerEntry {
            id: id.into(),
            name: name.into(),
            transport: "stdio".into(),
            command: Some("x".into()),
            args: vec![],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    fn detected_mcp_server(name: &str) -> clients::McpServer {
        clients::McpServer {
            name: name.into(),
            transport: "stdio".into(),
            command: Some("x".into()),
            args: vec![],
            env_keys: vec![],
            url: None,
        }
    }

    fn detected_mcp_server_with_command(name: &str, command: &str) -> clients::McpServer {
        clients::McpServer {
            command: Some(command.into()),
            ..detected_mcp_server(name)
        }
    }

    fn detected_mcp_server_with_args(
        name: &str,
        command: &str,
        args: &[&str],
    ) -> clients::McpServer {
        clients::McpServer {
            command: Some(command.into()),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            ..detected_mcp_server(name)
        }
    }

    fn detected_client(
        id: &str,
        servers: Vec<&str>,
        plugin_servers: Vec<&str>,
    ) -> clients::DetectedClient {
        clients::DetectedClient {
            id: id.into(),
            name: id.into(),
            uses_connectors: false,
            config_path: String::new(),
            config_exists: true,
            app_present: true,
            servers: servers.into_iter().map(detected_mcp_server).collect(),
            plugin_servers: plugin_servers.into_iter().map(detected_mcp_server).collect(),
            gateway_installed: false,
            entry_state: clients::GatewayEntryState::Absent,
            error: None,
        }
    }

    #[test]
    fn servers_to_import_includes_plugin_detected_servers() {
        // The onboarding banner promises a count across BOTH client.servers and
        // client.plugin_servers (see importableServers in src/lib/types.ts); the
        // import used to only walk client.servers, silently dropping every
        // plugin-detected one (e.g. Cursor/Roo project-level scans) and leaving
        // the actual import far short of the promised count.
        let detected = vec![detected_client(
            "cursor",
            vec!["node_repl"],
            vec!["linear", "github", "figma"],
        )];
        let reg = Registry::default();
        let picked = servers_to_import(&detected, &reg);
        let names: std::collections::HashSet<_> =
            picked.iter().map(|s| s.name.to_lowercase()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains("node_repl"));
        assert!(names.contains("linear"));
        assert!(names.contains("github"));
        assert!(names.contains("figma"));
    }

    #[test]
    fn servers_to_import_dedupes_by_name_across_clients_and_sources() {
        let detected = vec![
            detected_client("cursor", vec!["Linear"], vec!["linear"]),
            detected_client("claude-code", vec!["linear"], vec![]),
        ];
        let reg = Registry::default();
        let picked = servers_to_import(&detected, &reg);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name.to_lowercase(), "linear");
    }

    #[test]
    fn selected_servers_to_import_respects_the_reviewed_keys() {
        let detected = vec![detected_client("cursor", vec!["linear", "github"], vec![])];
        let selected = std::collections::HashSet::from(["name:github".to_string()]);
        let picked = selected_servers_to_import(&detected, &Registry::default(), Some(&selected));
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "github");
    }

    #[test]
    fn servers_to_import_keeps_distinct_packages_with_the_same_friendly_name() {
        // Bare package-runner entries use the package-derived friendly name. The
        // scopes below both become "weather", but the runner invocations target
        // different packages and must survive bulk import as separate entries.
        let mut client = detected_client("cursor", vec![], vec![]);
        client.servers = vec![
            detected_mcp_server_with_args("weather", "npx", &["-y", "@acme/mcp-weather"]),
            detected_mcp_server_with_args("weather", "npx", &["-y", "@other/mcp-weather"]),
        ];
        let mut reg = Registry::default();
        let picked = servers_to_import(&[client], &reg);
        assert_eq!(picked.len(), 2);
        assert!(picked.iter().all(|server| server.name == "weather"));

        for server in picked {
            reg.add_server(server);
        }
        let ids: std::collections::HashSet<_> =
            reg.servers.iter().map(|server| server.id.as_str()).collect();
        assert_eq!(ids, std::collections::HashSet::from(["weather", "weather-2"]));
    }

    #[test]
    fn servers_to_import_keeps_same_package_under_distinct_names() {
        // A multi-account setup runs the SAME package twice under different
        // names (e.g. a personal and a work token). Keying on the package alone
        // would collapse them and silently drop one; the name tiebreaker keeps
        // both while still distinguishing different packages (see the test above).
        let mut client = detected_client("claude", vec![], vec![]);
        client.servers = vec![
            detected_mcp_server_with_args("github-personal", "npx", &["-y", "@mcp/server-github"]),
            detected_mcp_server_with_args("github-work", "npx", &["-y", "@mcp/server-github"]),
        ];
        let picked = servers_to_import(&[client], &Registry::default());
        assert_eq!(picked.len(), 2);
        let names: std::collections::HashSet<_> =
            picked.iter().map(|server| server.name.as_str()).collect();
        assert_eq!(
            names,
            std::collections::HashSet::from(["github-personal", "github-work"])
        );
    }

    #[test]
    fn servers_to_import_skips_existing_and_own_gateway_entry() {
        let detected = vec![detected_client(
            "cursor",
            vec!["already-here", clients::GATEWAY_ENTRY_NAME],
            vec!["new-one"],
        )];
        let mut reg = Registry::default();
        reg.add_server(plain_server("x", "already-here"));
        let picked = servers_to_import(&detected, &reg);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "new-one");
    }

    #[test]
    fn servers_to_import_skips_gateway_registered_under_a_different_name() {
        // Regression: a real config had the gateway entry named "toolport"
        // (not "conduit"), pointing at toolport-gateway.exe - a leftover from
        // before the rename or a manual add. The name-only check let it through
        // and imported the gateway as if it were a normal downstream server,
        // which risks the gateway proxying itself if ever enabled.
        let mut client = detected_client("claude-code", vec!["linear"], vec![]);
        client.servers.push(detected_mcp_server_with_command(
            "toolport",
            r"C:\Users\x\AppData\Local\Toolport\toolport-gateway.exe",
        ));
        let reg = Registry::default();
        let picked = servers_to_import(&[client], &reg);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name.to_lowercase(), "linear");
    }

    #[test]
    fn migration_filter_skips_gateway_identities_before_reporting_moved() {
        let mut client = detected_client("test-client", vec![], vec![]);
        client.servers = vec![
            // Name-based identity: the legacy gateway name is not the current
            // GATEWAY_ENTRY_NAME and must not regress to a current-name-only check.
            detected_mcp_server_with_command("conduit", "manual-wrapper"),
            // Command-based identity: an arbitrary slot name still points at the
            // gateway binary and would recurse if imported as a downstream server.
            detected_mcp_server_with_command(
                "stale",
                r"C:\Users\x\AppData\Local\Toolport\toolport-gateway.exe",
            ),
        ];
        let mut reg = Registry::default();

        let (imported, moved) = import_client_servers_for_migration(&mut reg, &client);

        assert_eq!(imported, 0);
        assert!(moved.is_empty());
        assert!(reg.servers.is_empty());
    }

    #[test]
    fn oauth_lock_serializes_concurrent_attempts() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("conduit-oauth-lock-{unique}.lock"));
        let lock1 = try_acquire_oauth_lock(&path)
            .expect("first lock should not fail")
            .expect("first lock should be acquired");
        assert!(
            try_acquire_oauth_lock(&path)
                .expect("second lock should not fail")
                .is_none(),
            "second concurrent lock must wait"
        );
        drop(lock1);
        let lock2 = try_acquire_oauth_lock(&path)
            .expect("third lock should not fail")
            .expect("lock should be available after release");
        drop(lock2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_mutation_lock_serializes_token_and_oauth_writes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("toolport-auth-write-{unique}.lock"));
        let first = try_acquire_auth_mutation_lock(&path)
            .expect("first mutation lock should not fail")
            .expect("first mutation lock should be acquired");
        assert!(
            try_acquire_auth_mutation_lock(&path)
                .expect("second mutation lock should not fail")
                .is_none(),
            "a concurrent manual-token or OAuth write must wait"
        );
        drop(first);
        let second = try_acquire_auth_mutation_lock(&path)
            .expect("lock reacquisition should not fail")
            .expect("lock should be available after release");
        drop(second);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn oauth_lock_key_is_stable_and_scoped() {
        let a = oauth_lock_key("srv-1", "https://mcp.example.com");
        let b = oauth_lock_key("srv-1", "https://mcp.example.com");
        let c = oauth_lock_key("srv-2", "https://mcp.example.com");
        assert_eq!(a, b, "same server identity must map to same lock key");
        assert_ne!(a, c, "different server identity must map to different lock keys");
    }

    #[test]
    fn oauth_waiter_uses_attempt_completion_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("conduit-oauth-lock-{unique}.lock"));

        let lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let attempt_id = lock.attempt_id.clone();

        let stale_attempt = format!("old-attempt-{unique}");
        let stale_done = oauth_completion_path(&path, &stale_attempt);
        std::fs::write(&stale_done, "done=1").expect("stale completion should be writable");
        assert!(
            !completion_exists(&path, &attempt_id),
            "completion from a prior attempt must not satisfy current waiter"
        );

        drop(lock);
        assert!(
            completion_exists(&path, &attempt_id),
            "lock drop should mark the specific attempt complete"
        );

        let _ = std::fs::remove_file(stale_done);
        let _ = std::fs::remove_file(oauth_completion_path(&path, &attempt_id));
        let _ = std::fs::remove_file(path);
    }

    fn unique_oauth_lock_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("conduit-oauth-lock-{label}-{unique}.lock"))
    }

    fn cleanup_oauth_lock(path: &std::path::Path, attempt_ids: &[&str]) {
        for attempt_id in attempt_ids {
            let _ = std::fs::remove_file(oauth_completion_path(path, attempt_id));
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn oauth_lock_drop_without_success_is_a_failed_completion() {
        let path = unique_oauth_lock_path("fail-drop");
        let lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let attempt_id = lock.attempt_id.clone();
        drop(lock);
        assert_eq!(
            read_oauth_completion(&path, &attempt_id),
            Some(OAuthCompletion::Failed),
            "Drop without mark_succeeded must not look like a finished sign-in"
        );
        assert_eq!(
            oauth_waiter_outcome(&path, &attempt_id)
                .expect("failure completion should produce an outcome")
                .expect_err("waiter must not treat a failed flow as success"),
            "another Toolport process failed to complete OAuth for this server"
        );
        cleanup_oauth_lock(&path, &[&attempt_id]);
    }

    #[test]
    fn oauth_lock_drop_after_success_is_an_ok_completion() {
        let path = unique_oauth_lock_path("ok-drop");
        let mut lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let attempt_id = lock.attempt_id.clone();
        lock.mark_succeeded();
        drop(lock);
        assert_eq!(
            read_oauth_completion(&path, &attempt_id),
            Some(OAuthCompletion::Succeeded)
        );
        assert!(
            oauth_waiter_outcome(&path, &attempt_id)
                .expect("success completion should produce an outcome")
                .is_ok(),
            "a vaulted first flow must still let a waiter treat the attempt as done"
        );
        cleanup_oauth_lock(&path, &[&attempt_id]);
    }

    #[test]
    fn oauth_waiter_returns_err_when_first_flow_fails() {
        let path = unique_oauth_lock_path("fail-wait");
        let lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let first_attempt = lock.attempt_id.clone();
        let wait_path = path.clone();
        let waiter = std::thread::spawn(move || acquire_or_wait_oauth_lock_at(&wait_path));
        // Let the waiter observe the live lock before we fail it (SBS-842).
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS * 2));
        drop(lock);
        let outcome = waiter.join().expect("waiter thread should finish");
        let err = match outcome {
            Err(e) => e,
            Ok(None) => panic!("failed first flow must not report waiter success"),
            Ok(Some(_)) => panic!("failed first flow must not start a second browser"),
        };
        assert!(
            err.contains("failed to complete OAuth"),
            "unexpected waiter error: {err}"
        );
        cleanup_oauth_lock(&path, &[&first_attempt]);
    }

    #[test]
    fn oauth_waiter_returns_ok_none_when_first_flow_succeeds() {
        let path = unique_oauth_lock_path("ok-wait");
        let mut lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let first_attempt = lock.attempt_id.clone();
        let wait_path = path.clone();
        let waiter = std::thread::spawn(move || acquire_or_wait_oauth_lock_at(&wait_path));
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS * 2));
        lock.mark_succeeded();
        drop(lock);
        let outcome = waiter.join().expect("waiter thread should finish");
        match outcome {
            Ok(None) => {}
            Ok(Some(_)) => panic!(
                "successful first flow should let the waiter finish without a second browser"
            ),
            Err(e) => panic!("successful first flow should not error the waiter: {e}"),
        }
        cleanup_oauth_lock(&path, &[&first_attempt]);
    }

    #[test]
    fn truncated_completion_file_is_not_a_failure_verdict() {
        let path = unique_oauth_lock_path("torn-read");
        let attempt_id = "attempt-torn-read";
        let completion = oauth_completion_path(&path, attempt_id);
        // Every shape a reader can catch while a writer is mid-write: the file
        // exists but the verdict is not in it yet.
        for partial in ["", "\n", "status=", "status=o"] {
            std::fs::write(&completion, partial).expect("partial completion should be writable");
            assert_eq!(
                read_oauth_completion(&path, attempt_id),
                None,
                "a partially written completion ({partial:?}) must read as no verdict yet, not as failure"
            );
            assert!(
                oauth_waiter_outcome(&path, attempt_id).is_none(),
                "a partially written completion ({partial:?}) must not resolve the waiter"
            );
        }
        cleanup_oauth_lock(&path, &[attempt_id]);
    }

    #[test]
    fn oauth_waiter_keeps_polling_through_a_torn_completion_file() {
        let path = unique_oauth_lock_path("torn-wait");
        let mut lock = try_acquire_oauth_lock(&path)
            .expect("lock acquisition should not fail")
            .expect("lock should be acquired");
        let first_attempt = lock.attempt_id.clone();
        let wait_path = path.clone();
        let waiter = std::thread::spawn(move || acquire_or_wait_oauth_lock_at(&wait_path));
        // Let the waiter latch the live attempt id off the lock file.
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS * 2));
        // Stand in for the truncate half of a non-atomic completion write: the file
        // is there, the bytes are not.
        std::fs::write(oauth_completion_path(&path, &first_attempt), "")
            .expect("torn completion should be writable");
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS * 3));
        // ...and only now does the first window finish vaulting its tokens.
        lock.mark_succeeded();
        drop(lock);
        match waiter.join().expect("waiter thread should finish") {
            Ok(None) => {}
            Ok(Some(_)) => panic!("successful first flow should not start a second browser"),
            Err(e) => panic!("a torn completion read must not be reported as a failed sign-in: {e}"),
        }
        cleanup_oauth_lock(&path, &[&first_attempt]);
    }

    #[test]
    fn stale_lock_replace_requires_same_observed_instance() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("conduit-oauth-lock-{unique}.lock"));

        let observed_id = format!("observed-{unique}");
        let fresh_id = format!("fresh-owner-{unique}");
        let contender_id = format!("contender-{unique}");
        std::fs::write(&path, oauth_lock_contents(&observed_id))
            .expect("initial lock write should work");
        let observed = read_oauth_lock_snapshot(&path)
            .expect("snapshot read should work")
            .expect("snapshot should exist");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, oauth_lock_contents(&fresh_id))
            .expect("fresh lock write should work");

        let replaced = try_replace_stale_lock(
            &path,
            &observed,
            &oauth_lock_contents(&contender_id),
            &contender_id,
        )
        .expect("replace check should not error");
        assert!(
            !replaced,
            "stale cleanup must not clobber a newly replaced lock"
        );

        let current = std::fs::read_to_string(&path).expect("current lock should be readable");
        assert!(
            current.contains(&format!("attempt_id={fresh_id}")),
            "fresh lock instance must remain intact"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tool_identities_attribute_alias_to_server_and_profiles() {
        use std::collections::{BTreeMap, BTreeSet};
        let servers = vec![
            plain_server("gh", "GitHub"),
            plain_server("my-server", "My Server"),
        ];
        let profiles = vec![Profile {
            id: "default".into(),
            name: "Default".into(),
            enabled_server_ids: vec!["gh".into()],
            tool_scope: Default::default(),
        }];
        let mut baselines = BTreeMap::new();
        let bl = |fp: &str, fs: u64, lc: u64| integrity::ToolBaseline {
            fingerprint: fp.into(),
            first_seen: fs,
            last_changed: lc,
        };
        baselines.insert("gh__create_issue".to_string(), bl("v2:abc", 100, 200));
        baselines.insert("my_server__do_thing".to_string(), bl("v2:def", 50, 60));
        baselines.insert("orphan_alias".to_string(), bl("v2:ghi", 1, 2));
        let quarantined: BTreeSet<String> =
            ["gh__create_issue".to_string()].into_iter().collect();

        let ids = build_tool_identities(&baselines, &quarantined, &servers, &profiles);
        let get = |a: &str| ids.iter().find(|i| i.alias == a).cloned().unwrap();

        let gh = get("gh__create_issue");
        assert_eq!(gh.server_id, "gh");
        assert_eq!(gh.server_name, "GitHub");
        assert_eq!(gh.upstream, "create_issue");
        assert_eq!(gh.profiles, vec!["Default".to_string()]);
        assert_eq!(gh.fingerprint, "v2:abc");
        assert!(gh.quarantined);

        // The REAL server id ("my-server") is recovered even though its exposed prefix
        // is the sanitized "my_server". Not enabled in any profile -> empty profiles.
        let my = get("my_server__do_thing");
        assert_eq!(my.server_id, "my-server");
        assert_eq!(my.upstream, "do_thing");
        assert!(my.profiles.is_empty());
        assert!(!my.quarantined);

        // An alias matching no server prefix is honestly left unattributed, not guessed.
        let orphan = get("orphan_alias");
        assert_eq!(orphan.server_id, "");
        assert_eq!(orphan.server_name, "");
        assert!(orphan.profiles.is_empty());
    }

    #[test]
    fn export_strips_secrets_and_excludes_gateway() {
        let mut reg = Registry::default();
        reg.add_server(github_with_secret());
        reg.add_server(ServerEntry {
            id: String::new(),
            name: "conduit".into(),
            transport: "stdio".into(),
            command: Some("conduit-gateway".into()),
            args: vec![],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });

        let doc = build_export(&reg, Some("Team setup"), Some("Our shared servers"), None);
        let serialized = serde_json::to_string(&doc).unwrap();
        // The secret value must never appear in a shared setup.
        assert!(!serialized.contains("sk-live-xyz"));
        let servers = doc["servers"].as_array().unwrap();
        // Gateway entry excluded.
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["env"][0]["value"], serde_json::Value::Null);
        // Optional label is carried through.
        assert_eq!(doc["name"], "Team setup");
        assert_eq!(doc["description"], "Our shared servers");

        // Selective share: an id filter includes only the matching servers, so a
        // user can share a focused stack instead of their whole setup.
        let shared_id = reg
            .servers
            .iter()
            .find(|server| server.name == "GitHub")
            .unwrap()
            .id
            .clone();
        let subset = build_export(&reg, None, None, Some(&[shared_id]));
        assert_eq!(subset["servers"].as_array().unwrap().len(), 1);
        let empty = build_export(&reg, None, None, Some(&["does-not-exist".to_string()]));
        assert_eq!(empty["servers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn export_selection_uses_stable_ids_and_an_explicit_snapshot() {
        let mut reg = Registry::default();
        let mut first = github_with_secret();
        first.name = "Duplicate".into();
        first.command = Some("first-command".into());
        reg.add_server(first);
        let mut second = github_with_secret();
        second.name = "Duplicate".into();
        second.command = Some("second-command".into());
        reg.add_server(second);

        let first_id = reg
            .servers
            .iter()
            .find(|server| server.command.as_deref() == Some("first-command"))
            .unwrap()
            .id
            .clone();
        let snapshot = vec![first_id];

        // A same-name server is not accidentally selected, and a later registry
        // addition cannot widen the explicit snapshot.
        let mut later = github_with_secret();
        later.name = "Added later".into();
        later.command = Some("later-command".into());
        reg.add_server(later);
        let doc = build_export(&reg, None, None, Some(&snapshot));
        let exported = doc["servers"].as_array().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0]["command"], "first-command");

        let empty = build_export(&reg, None, None, Some(&[]));
        assert!(empty["servers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn redact_url_userinfo_strips_credentials_only() {
        // Password AND token-as-username are both stripped; the host/path survive.
        assert_eq!(
            redact_url_userinfo("https://user:s3cr3t@mcp.example.com/mcp"),
            "https://<redacted>@mcp.example.com/mcp"
        );
        assert_eq!(
            redact_url_userinfo("https://gh_tok3n@api.example.com/v1?x=1"),
            "https://<redacted>@api.example.com/v1?x=1"
        );
        // No userinfo -> unchanged (host with '@' only after a '/' is not authority).
        assert_eq!(
            redact_url_userinfo("https://api.githubcopilot.com/mcp/"),
            "https://api.githubcopilot.com/mcp/"
        );
        assert_eq!(
            redact_url_userinfo("https://host.example.com/path/u@v"),
            "https://host.example.com/path/u@v"
        );
        // Non-URL input is returned verbatim.
        assert_eq!(redact_url_userinfo("not a url"), "not a url");
    }

    #[test]
    fn export_redacts_url_embedded_credentials() {
        // A remote server whose URL carries inline creds must not leak them in a share.
        let mut reg = Registry::default();
        reg.add_server(ServerEntry {
            id: "remote".into(),
            name: "Remote".into(),
            transport: "http".into(),
            command: None,
            args: vec![],
            env: vec![],
            url: Some("https://user:hunter2@mcp.example.com/mcp".into()),
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });
        let doc = build_export(&reg, None, None, None);
        let serialized = serde_json::to_string(&doc).unwrap();
        assert!(!serialized.contains("hunter2"), "url password leaked: {serialized}");
        assert_eq!(
            doc["servers"][0]["url"].as_str().unwrap(),
            "https://<redacted>@mcp.example.com/mcp"
        );
    }

    #[test]
    fn export_redacts_inline_secret_args() {
        // The connection-URI and inline-credential heuristics.
        assert!(arg_looks_secret("postgresql://admin:hunter2@db.example.com:5432/app"));
        assert!(arg_looks_secret("--dsn=postgres://u:p@h/db"));
        assert!(arg_looks_secret("PASSWORD=hunter2"));
        assert!(arg_looks_secret("Authorization: token=abc123"));
        assert!(arg_looks_secret("Authorization: Bearer sk-live-secret"));
        assert!(arg_looks_secret("authorization:Basic Zm9vOmJhcg=="));
        assert!(arg_looks_secret("Bearer sk-live-secret"));
        assert!(arg_looks_secret("Basic dXNlcjpwYXNz"));
        assert!(arg_looks_secret("Digest username=admin,response=secret"));
        assert!(arg_looks_secret("Proxy-Authorization: Basic dXNlcjpwYXNz"));
        // Legitimate args must NOT be redacted.
        assert!(!arg_looks_secret("-y"));
        assert!(!arg_looks_secret("@modelcontextprotocol/server-postgres"));
        assert!(!arg_looks_secret("--stdio"));
        assert!(!arg_looks_secret("https://api.githubcopilot.com/mcp/")); // no userinfo

        let mut reg = Registry::default();
        reg.add_server(ServerEntry {
            id: "pg".into(),
            name: "PostgreSQL".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-postgres".into(),
                "postgresql://admin:hunter2@db.example.com:5432/app".into(),
                "Authorization: Bearer sk-live-secret".into(),
            ],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });
        let doc = build_export(&reg, None, None, None);
        let serialized = serde_json::to_string(&doc).unwrap();
        // The password must never appear in a shared setup.
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("sk-live-secret"));
        let args = doc["servers"][0]["args"].as_array().unwrap();
        // Benign args are kept; only the credential-bearing one is redacted.
        assert_eq!(args[0], "-y");
        assert_eq!(args[1], "@modelcontextprotocol/server-postgres");
        assert_eq!(args[2], "<redacted>");
        assert_eq!(args[3], "<redacted>");
    }

    #[test]
    fn secret_arg_never_survives_export_then_import() {
        // End-to-end invariant: a credential pasted into args must not leak
        // through the full share path (export -> serialize -> import elsewhere).
        let mut reg = Registry::default();
        reg.add_server(ServerEntry {
            id: "pg".into(),
            name: "PostgreSQL".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "postgresql://admin:hunter2@db.example.com/app".into(),
            ],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });
        let json = serde_json::to_string(&build_export(&reg, None, None, None)).unwrap();
        let mut recipient = Registry::default();
        apply_import(&mut recipient, &json).unwrap();
        let imported = recipient
            .servers
            .iter()
            .find(|s| s.name == "PostgreSQL")
            .expect("server imported");
        assert!(
            imported.args.iter().all(|a| !a.contains("hunter2")),
            "secret leaked through export+import"
        );
        assert!(imported.args.iter().any(|a| a == "<redacted>"));
    }

    #[test]
    fn import_dedups_by_name_and_nulls_secrets() {
        let mut reg = Registry::default();
        reg.add_server(github_with_secret());
        let doc = r#"{"kind":"conduit-setup","version":1,"servers":[
            {"name":"github","transport":"stdio","command":"npx"},
            {"name":"Stripe","transport":"http","url":"https://x",
             "env":[{"key":"K","value":"shh","secret":true}]}
        ]}"#;
        apply_import(&mut reg, doc).unwrap();

        // "github" is deduped case-insensitively; only Stripe is added.
        assert_eq!(
            reg.servers
                .iter()
                .filter(|s| s.name.eq_ignore_ascii_case("github"))
                .count(),
            1
        );
        let stripe = reg.servers.iter().find(|s| s.name == "Stripe").unwrap();
        assert_eq!(stripe.env[0].value, None);
        assert_eq!(stripe.source.as_deref(), Some("shared"));
    }

    #[test]
    fn import_rejects_garbage() {
        let mut reg = Registry::default();
        assert!(apply_import(&mut reg, "{not json").is_err());
    }

    #[test]
    fn parse_share_url_extracts_id() {
        assert_eq!(
            parse_share_url("toolport://import?s=071g6i3h5f5g6h2i"),
            Some("071g6i3h5f5g6h2i".to_string())
        );
        // Legacy scheme still accepted so existing share links keep working.
        assert_eq!(
            parse_share_url("conduit://import?s=071g6i3h5f5g6h2i"),
            Some("071g6i3h5f5g6h2i".to_string())
        );
        // Tolerate a trailing slash after the host, and pick s out of many params.
        assert_eq!(
            parse_share_url("toolport://import/?ref=x&s=abc123"),
            Some("abc123".to_string())
        );
        // Reject the wrong action, missing id, and non-alphanumeric ids.
        assert_eq!(parse_share_url("conduit://other?s=abc"), None);
        assert_eq!(parse_share_url("toolport://import?x=1"), None);
        assert_eq!(parse_share_url("conduit://import?s=../etc"), None);
        assert_eq!(parse_share_url("https://example.com?s=abc"), None);
    }

    #[test]
    fn last_lines_returns_the_tail() {
        let text = "a\nb\nc\nd\ne";
        // Fewer requested than available: just the tail, newline-terminated.
        assert_eq!(last_lines(text, 2), "d\ne\n");
        // More requested than available: everything.
        assert_eq!(last_lines(text, 99), "a\nb\nc\nd\ne\n");
        // Empty input stays empty (no stray newline).
        assert_eq!(last_lines("", 10), "");
    }

    #[test]
    fn diagnostics_lists_env_keys_but_never_values() {
        let mut reg = Registry::default();
        reg.add_server(github_with_secret());
        let s = registry_summary(&reg);
        // The key is shown (with a secret marker) so a report says what's set...
        assert!(s.contains("TOKEN (secret)"), "got: {s}");
        // ...but the secret value itself must never appear in a pasted report.
        assert!(!s.contains("sk-live-xyz"), "secret value leaked: {s}");
        // The launch command is present for debugging.
        assert!(s.contains("(stdio) npx"), "missing launch line: {s}");
    }

    #[test]
    fn diagnostics_redacts_inline_arg_and_url_secrets() {
        let mut reg = Registry::default();
        reg.add_server(ServerEntry {
            id: "pg".into(),
            name: "Postgres".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec![
                "@modelcontextprotocol/server-postgres".into(),
                "postgresql://admin:hunter2@db.example.com/app".into(),
                "--token=sk-live-xyz".into(),
                "https://api.example.com/path".into(),
            ],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });
        reg.add_server(ServerEntry {
            id: "remote".into(),
            name: "Remote".into(),
            transport: "http".into(),
            command: None,
            args: vec![],
            env: vec![],
            url: Some("https://user:hunter2@mcp.example.com/mcp".into()),
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });

        let s = registry_summary(&reg);
        assert!(!s.contains("hunter2"), "secret value leaked: {s}");
        assert!(!s.contains("sk-live-xyz"), "secret token leaked: {s}");
        assert!(s.contains("<redacted>"), "missing redaction marker: {s}");
        assert!(s.contains("https://api.example.com/path"), "safe URL was over-redacted: {s}");
    }

    // ----- SOU-435: restart advice survives later passes ----------------------

    fn advice_entry(pid: u32, client: &str) -> crate::gateway_publish::ClientNeedingRestart {
        crate::gateway_publish::ClientNeedingRestart {
            client: client.into(),
            client_pid: pid,
            gateway: "toolport-gateway-1.9.4.exe".into(),
        }
    }

    /// A pid guaranteed to be running for the length of the test.
    fn live_pid() -> u32 {
        std::process::id()
    }

    /// A pid guaranteed not to be running. Near the top of the numeric range, so it
    /// is not a pid any real process on the machine would have been assigned.
    fn dead_pid() -> u32 {
        u32::MAX - 1
    }

    /// The exact #542 failure, as a test.
    ///
    /// Launch pass finds "restart Claude" and kills the process. The user opens
    /// Settings and clicks Run. Claude has not made a tool call yet, so that pass
    /// finds nothing. The old code wrote unconditionally, so the empty result erased
    /// the advice and the UI then claimed nothing was stale while Claude was still
    /// pinned to an obsolete binary.
    #[test]
    fn an_empty_later_pass_does_not_erase_earlier_advice() {
        let advice = RestartAdvice::default();
        let pid = live_pid();

        let after_launch = advice.merge(vec![advice_entry(pid, "claude.exe")]);
        assert_eq!(after_launch.len(), 1, "launch pass records the advice");

        // The Settings button: a second pass whose snapshot is empty.
        let after_button = advice.merge(Vec::new());
        assert_eq!(
            after_button,
            after_launch,
            "an empty pass must not erase advice the user has not acted on yet"
        );
        assert_eq!(
            advice.current().len(),
            1,
            "and the panel must still have something to show"
        );
    }

    /// A second live pid, for union cases that need two distinct surviving entries.
    /// Killed by the caller; the merge only ever asks whether the pid is running.
    fn spawn_live_child() -> std::process::Child {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/c", "ping", "-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a short-lived helper process")
    }

    /// Union across SEPARATE passes is the behaviour `RestartAdvice` exists for:
    /// the delayed launch pass must add what it finds without discarding what the
    /// first pass already recorded.
    #[test]
    fn merge_unions_apps_found_by_different_passes() {
        let advice = RestartAdvice::default();
        let mut child = spawn_live_child();
        let other_pid = child.id();

        let first = advice.merge(vec![advice_entry(live_pid(), "claude.exe")]);
        assert_eq!(first.len(), 1);

        // A later pass finds a genuinely different app. Both must survive.
        let merged = advice.merge(vec![advice_entry(other_pid, "grok.exe")]);
        let mut names: Vec<&str> = merged.iter().map(|c| c.client.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["claude.exe", "grok.exe"],
            "a later pass adds to the advice instead of replacing it"
        );

        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn merge_keeps_one_row_per_app() {
        let advice = RestartAdvice::default();
        let pid = live_pid();

        advice.merge(vec![advice_entry(pid, "claude.exe")]);
        // The same app seen by a later pass is still one restart to perform.
        let merged = advice.merge(vec![advice_entry(pid, "claude.exe")]);
        assert_eq!(merged.len(), 1);

        // Same app, second obsolete gateway: still one thing for the user to do.
        let mut second_gateway = advice_entry(pid, "claude.exe");
        second_gateway.gateway = "toolport-gateway-1.9.5.exe".into();
        let merged = advice.merge(vec![second_gateway]);
        assert_eq!(
            merged.len(),
            1,
            "advice is keyed by client pid, so one row per app regardless of gateway count"
        );
    }

    /// Compliance is only observable through the pid disappearing. Absence of a
    /// respawned gateway is not evidence: a client that has not made a tool call yet
    /// looks exactly the same.
    #[test]
    fn advice_expires_once_the_user_restarts_the_app() {
        let advice = RestartAdvice::default();
        let stored = advice.merge(vec![advice_entry(dead_pid(), "claude.exe")]);
        assert!(
            stored.is_empty(),
            "a client pid that is no longer running means the app was restarted"
        );
        assert!(advice.current().is_empty());
    }

    #[test]
    fn expiry_is_per_entry_not_all_or_nothing() {
        let advice = RestartAdvice::default();
        let merged = advice.merge(vec![
            advice_entry(live_pid(), "claude.exe"),
            advice_entry(dead_pid(), "grok.exe"),
        ]);
        assert_eq!(merged.len(), 1, "only the restarted app drops out");
        assert_eq!(merged[0].client, "claude.exe");
    }

    /// The delayed launch pass reads the merged advice, so without this filter the
    /// same app would toast twice three seconds apart (Copilot review, PR #622).
    #[test]
    fn only_newly_seen_clients_are_announced_again() {
        let first = advice_entry(101, "claude.exe");
        let second = advice_entry(202, "grok.exe");
        let already = vec![first.client_pid];

        // The delayed pass sees the merged list: the first app plus a new one.
        let merged = vec![first.clone(), second.clone()];
        assert_eq!(
            unannounced(&already, merged),
            vec![second],
            "only the app the user has not been told about interrupts them again"
        );

        // Nothing new raced in: the merged list is entirely old news.
        assert!(
            unannounced(&already, vec![first]).is_empty(),
            "re-announcing stored advice would double-toast the same app"
        );

        // Nothing announced yet (an empty first pass): everything is new.
        assert_eq!(unannounced(&[], vec![advice_entry(303, "cursor.exe")]).len(), 1);
    }

    // ----- SOU-329: the watcher must not clobber a fresher cache ---------------

    /// The generation counter is process-wide, so these tests must not interleave
    /// with each other or an unrelated bump would look like a racing write.
    static GEN_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Tagged via `active_profile_id`, a field `Registry` already carries, so the
    /// test does not need a `Default` impl on a production type it is not testing.
    fn registry_named(tag: &str) -> Registry {
        Registry {
            active_profile_id: Some(tag.to_string()),
            ..Registry::default()
        }
    }

    fn only_server(reg: &Registry) -> &str {
        reg.active_profile_id.as_deref().unwrap_or("")
    }

    /// The race from SOU-329, in order: the watcher reads disk state A outside the
    /// mutex, a command writes B to both disk and cache, and the watcher then tries
    /// to publish its now-stale A. Before the generation guard this overwrote B and
    /// emitted `registry-changed` with A, so the UI showed the reverted state and
    /// the cache disagreed with the file until something else touched it.
    #[test]
    fn a_racing_write_beats_a_stale_watcher_load() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("before"));

        // Watcher samples the generation, then reads the file (slow, unlocked).
        let sampled = registry_generation();
        let from_disk = registry_named("stale-A");

        // A command writes B in the meantime: cache updated under the mutex, and
        // the generation bumped, exactly as `write_registry` does.
        {
            let mut guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = registry_named("fresh-B");
            bump_registry_generation();
        }

        assert!(
            !publish_if_unchanged(&state, sampled, &from_disk),
            "a load that predates a cache write must be dropped, not published"
        );
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            only_server(&guard),
            "fresh-B",
            "the newer in-memory write must survive the watcher"
        );
    }

    /// The ordinary case this must not break: another process (gateway, team sync)
    /// changed the file and nothing touched the cache, so the watcher publishes it.
    #[test]
    fn an_uncontended_watcher_load_is_applied() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("before"));

        let sampled = registry_generation();
        let from_disk = registry_named("from-another-process");

        assert!(
            publish_if_unchanged(&state, sampled, &from_disk),
            "a disk change with no competing cache write is exactly what the watcher is for"
        );
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(only_server(&guard), "from-another-process");
    }

    /// The same race reached through a second caller. `reload_into_state` also loads
    /// disk before taking the mutex, so `refresh_from_disk` and the team-sync paths
    /// could restore a stale cache even after the watcher was fixed. Found by review
    /// on #624, not by the original SOU-329 report.
    ///
    /// Drives the real body via `reload_with` and lands the competing write from
    /// inside the load closure, i.e. in the actual window between the generation
    /// sample and the publish. An earlier version of this test open-coded that
    /// sequence and called `publish_if_unchanged` directly; it passed with the whole
    /// guard stripped out of `reload_into_state`, because it never ran that function.
    #[test]
    fn the_reload_path_drops_a_load_older_than_the_cache() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("before"));

        let returned = reload_with(&state, || {
            // Runs where the disk read runs: after the generation sample, before the
            // publish. A command persisting and caching B lands right here.
            let mut guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = registry_named("fresh-B");
            bump_registry_generation();
            Ok(registry_named("stale-A"))
        })
        .expect("a lost race is not an error");

        assert_eq!(
            only_server(&returned),
            "fresh-B",
            "the caller must get the newer cached registry, not the stale disk read"
        );
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            only_server(&guard),
            "fresh-B",
            "refresh_from_disk must not be able to restore stale cached state"
        );
    }

    /// The uncontended reload: nothing touches the cache during the load, so the disk
    /// value is published and handed back.
    #[test]
    fn the_reload_path_applies_an_uncontended_load() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("before"));

        let returned = reload_with(&state, || Ok(registry_named("from-disk")))
            .expect("an uncontended reload must succeed");

        assert_eq!(only_server(&returned), "from-disk");
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(only_server(&guard), "from-disk");
    }

    /// Applying must itself bump the generation, or a second stale load sampled at
    /// the same moment would still be able to overwrite the value just published.
    #[test]
    fn applying_advances_the_generation() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("before"));

        // Two watcher ticks sample the same generation, as they would if both read
        // the file before either published.
        let sampled = registry_generation();
        assert!(publish_if_unchanged(&state, sampled, &registry_named("first")));
        assert!(
            !publish_if_unchanged(&state, sampled, &registry_named("second")),
            "the second load is stale relative to the first and must be dropped"
        );
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(only_server(&guard), "first");
    }

    // ----- issue #695: a failed load must not consume the mtime cursor --------

    fn watch_mtime(offset_secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + offset_secs)
    }

    fn json_of(reg: &Registry) -> String {
        serde_json::to_string(reg).unwrap()
    }

    fn seeded_watch(tag: &str, mtime: SystemTime) -> RegistryWatchLoop {
        RegistryWatchLoop {
            last_mtime: Some(mtime),
            last_json: json_of(&registry_named(tag)),
            last_error: None,
            consecutive_failures: 0,
        }
    }

    fn tick_watch(
        state: &RegistryState,
        loop_state: &mut RegistryWatchLoop,
        mtime: Option<SystemTime>,
        load: impl FnOnce() -> Result<Registry, String>,
        emitted: &std::cell::RefCell<Vec<String>>,
    ) -> RegistryWatchTick {
        watch_registry_tick(
            state,
            loop_state,
            || mtime,
            load,
            |r| {
                emitted.borrow_mut().push(only_server(r).to_string());
            },
        )
    }

    fn cache_tag(state: &RegistryState) -> String {
        only_server(
            &state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .to_string()
    }

    /// The defect in #695: the watcher recorded a new mtime before `load_from`
    /// succeeded. A lock timeout (or corrupt file with no backup) then left the
    /// next tick seeing the same mtime and skipping forever, so a later successful
    /// read of that same file never published.
    #[test]
    fn registry_watch_retries_same_mtime_after_transient_load_failure() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("old"));
        let mut loop_state = seeded_watch("old", watch_mtime(0));
        let emitted = std::cell::RefCell::new(Vec::new());

        let first = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || {
                Err(
                    "The registry is locked by another Toolport process (os error); try again."
                        .into(),
                )
            },
            &emitted,
        );
        assert_eq!(first, RegistryWatchTick::LoadFailed);
        assert_eq!(loop_state.consecutive_failures, 1);
        assert_eq!(
            loop_state.last_mtime,
            Some(watch_mtime(0)),
            "a failed load must not consume the change"
        );
        assert_eq!(loop_state.last_json, json_of(&registry_named("old")));
        assert!(emitted.borrow().is_empty(), "a failed load must not emit");
        assert_eq!(cache_tag(&state), "old");

        let second = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Ok(registry_named("new")),
            &emitted,
        );
        assert_eq!(
            second,
            RegistryWatchTick::Applied,
            "the same mtime must be retried once the lock is free"
        );
        assert_eq!(loop_state.last_mtime, Some(watch_mtime(1)));
        assert_eq!(loop_state.last_json, json_of(&registry_named("new")));
        assert_eq!(loop_state.consecutive_failures, 0);
        assert_eq!(emitted.borrow().as_slice(), ["new"]);
        assert_eq!(cache_tag(&state), "new");
        assert!(
            loop_state.last_error.is_none(),
            "a successful apply clears the logged error so a later failure can print again"
        );
    }

    /// An mtime-only bump (the gateway nudge) must consume the cursor so we do
    /// not reload every tick, and must not churn the UI.
    #[test]
    fn registry_watch_identical_content_advances_mtime_without_emitting() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = registry_named("old");
        let state: RegistryState = Mutex::new(old.clone());
        let mut loop_state = seeded_watch("old", watch_mtime(0));
        let emitted = std::cell::RefCell::new(Vec::new());

        let outcome = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Ok(old.clone()),
            &emitted,
        );
        assert_eq!(outcome, RegistryWatchTick::Identical);
        assert_eq!(loop_state.last_mtime, Some(watch_mtime(1)));
        assert_eq!(loop_state.last_json, json_of(&old));
        assert!(emitted.borrow().is_empty());

        let mut loaded = false;
        let quiet = watch_registry_tick(
            &state,
            &mut loop_state,
            || Some(watch_mtime(1)),
            || {
                loaded = true;
                Ok(old.clone())
            },
            |_| panic!("identical content must not emit"),
        );
        assert_eq!(quiet, RegistryWatchTick::Unchanged);
        assert!(
            !loaded,
            "a consumed identical mtime must not load again until the file moves"
        );
    }

    /// A SOU-329 lost race must consume the mtime. Retrying the same cursor
    /// would publish stale disk over the newer cache the generation guard just
    /// protected. The winner persisted, so a later mtime still reaches load.
    #[test]
    fn registry_watch_lost_race_consumes_mtime_and_keeps_newer_cache() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("old"));
        let mut loop_state = seeded_watch("old", watch_mtime(0));
        let emitted = std::cell::RefCell::new(Vec::new());

        let failed = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Err("registry locked".into()),
            &emitted,
        );
        assert_eq!(failed, RegistryWatchTick::LoadFailed);

        let lost = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || {
                let mut guard = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *guard = registry_named("fresh-B");
                bump_registry_generation();
                Ok(registry_named("stale-A"))
            },
            &emitted,
        );
        assert_eq!(lost, RegistryWatchTick::LostRace);
        assert_eq!(
            loop_state.last_mtime,
            Some(watch_mtime(1)),
            "a dropped publish must consume the mtime so stale disk cannot retry"
        );
        assert_eq!(loop_state.last_json, json_of(&registry_named("old")));
        assert_eq!(loop_state.consecutive_failures, 0);
        assert!(loop_state.last_error.is_none());
        assert!(emitted.borrow().is_empty());
        assert_eq!(cache_tag(&state), "fresh-B");

        let mut loaded = false;
        let quiet = watch_registry_tick(
            &state,
            &mut loop_state,
            || Some(watch_mtime(1)),
            || {
                loaded = true;
                Ok(registry_named("from-disk"))
            },
            |_| panic!("a lost race must not emit on the same mtime"),
        );
        assert_eq!(quiet, RegistryWatchTick::Unchanged);
        assert!(
            !loaded,
            "a consumed lost-race mtime must not load again until the file moves"
        );
        assert_eq!(cache_tag(&state), "fresh-B");

        let persisted = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(2)),
            || Ok(registry_named("fresh-B")),
            &emitted,
        );
        assert_eq!(
            persisted,
            RegistryWatchTick::Applied,
            "the winner's later mtime must still be loadable"
        );
        assert_eq!(loop_state.last_mtime, Some(watch_mtime(2)));
        assert_eq!(emitted.borrow().as_slice(), ["fresh-B"]);
        assert_eq!(cache_tag(&state), "fresh-B");
    }

    /// The app loads the registry before the watcher thread starts. If another
    /// process persists B in that startup window, the watcher's first tick must
    /// compare disk against the already-applied in-memory A and publish B. A
    /// separate priming load used to remember B without applying it, leaving the
    /// UI stale indefinitely.
    #[test]
    fn registry_watch_startup_disk_change_is_applied_on_first_tick() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("startup-A"));
        let mut loop_state = RegistryWatchLoop::from_state(&state);
        let emitted = std::cell::RefCell::new(Vec::new());

        let outcome = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Ok(registry_named("disk-B")),
            &emitted,
        );

        assert_eq!(outcome, RegistryWatchTick::Applied);
        assert_eq!(loop_state.last_mtime, Some(watch_mtime(1)));
        assert_eq!(loop_state.last_json, json_of(&registry_named("disk-B")));
        assert_eq!(emitted.borrow().as_slice(), ["disk-B"]);
        assert_eq!(cache_tag(&state), "disk-B");
    }

    #[test]
    fn registry_watch_retry_delay_is_bounded() {
        assert_eq!(watch_retry_delay(0), Duration::from_millis(1500));
        assert_eq!(watch_retry_delay(1), Duration::from_millis(1500));
        assert_eq!(watch_retry_delay(2), Duration::from_millis(3000));
        assert_eq!(watch_retry_delay(3), Duration::from_millis(6000));
        assert_eq!(watch_retry_delay(6), Duration::from_millis(48000));
        assert_eq!(watch_retry_delay(7), Duration::from_millis(60000));
        assert_eq!(watch_retry_delay(u32::MAX), Duration::from_millis(60000));
    }

    #[test]
    fn registry_watch_failure_diagnostics_are_throttled() {
        assert!(!watch_failure_should_log(0, false));
        assert!(watch_failure_should_log(1, false));
        assert!(!watch_failure_should_log(2, false));
        assert!(!watch_failure_should_log(19, false));
        assert!(watch_failure_should_log(20, false));
        assert!(watch_failure_should_log(2, true));
    }

    #[test]
    fn registry_watch_reverted_mtime_clears_pending_failure_backoff() {
        let state: RegistryState = Mutex::new(registry_named("old"));
        let mut loop_state = seeded_watch("old", watch_mtime(0));
        let emitted = std::cell::RefCell::new(Vec::new());

        let failed = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Err("registry locked".into()),
            &emitted,
        );
        assert_eq!(failed, RegistryWatchTick::LoadFailed);

        let mut loaded = false;
        let reverted = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(0)),
            || {
                loaded = true;
                Ok(registry_named("should-not-load"))
            },
            &emitted,
        );
        assert_eq!(reverted, RegistryWatchTick::Unchanged);
        assert!(!loaded);
        assert_eq!(loop_state.consecutive_failures, 0);
        assert!(loop_state.last_error.is_none());
    }

    /// A failed first tick must leave the startup cursor empty and retry the same
    /// observed mtime once the registry becomes readable.
    #[test]
    fn registry_watch_startup_failure_retries_same_mtime() {
        let _serial = GEN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state: RegistryState = Mutex::new(registry_named("startup"));
        let mut loop_state = RegistryWatchLoop::from_state(&state);
        let emitted = std::cell::RefCell::new(Vec::new());

        let first = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Err("The registry is locked by another Toolport process; try again.".into()),
            &emitted,
        );
        assert_eq!(first, RegistryWatchTick::LoadFailed);
        assert_eq!(loop_state.last_mtime, None);
        assert_eq!(loop_state.last_json, json_of(&registry_named("startup")));
        assert_eq!(loop_state.consecutive_failures, 1);
        assert!(emitted.borrow().is_empty());

        let second = tick_watch(
            &state,
            &mut loop_state,
            Some(watch_mtime(1)),
            || Ok(registry_named("from-disk")),
            &emitted,
        );
        assert_eq!(second, RegistryWatchTick::Applied);
        assert_eq!(loop_state.last_mtime, Some(watch_mtime(1)));
        assert_eq!(loop_state.consecutive_failures, 0);
        assert_eq!(emitted.borrow().as_slice(), ["from-disk"]);
        assert_eq!(cache_tag(&state), "from-disk");
        assert!(loop_state.last_error.is_none());
    }

    /// SBS-524: whitespace decides only whether the field was blank; the value
    /// itself must reach the keychain byte-for-byte. Trimming it would corrupt an
    /// opaque secret that legitimately carries edge whitespace, and the resulting
    /// `invalid_client` would be uncorrectable from a field that cannot be read back.
    #[test]
    fn supplied_secret_keeps_the_value_verbatim() {
        assert_eq!(
            supplied_secret(Some("  s3cret  ".into())).as_deref(),
            Some("  s3cret  ")
        );
        assert_eq!(supplied_secret(Some("s3cret".into())).as_deref(), Some("s3cret"));
        // Blank in any form means "keep the vaulted one".
        assert_eq!(supplied_secret(Some(String::new())), None);
        assert_eq!(supplied_secret(Some("   ".into())), None);
        assert_eq!(supplied_secret(Some("\t\n".into())), None);
        assert_eq!(supplied_secret(None), None);
    }

    // ----- SBS-845: Disconnect must not succeed while the bearer is still live --
    //
    // These drive `revoke_client_http_token_with` rather than the real vault:
    // headless Linux CI has no Secret Service, so a real `delete_secret` fails
    // there and every case would assert on the runner instead of on the revoke
    // logic. What is under test is which failures propagate and what each one
    // leaves behind, so the vault result is injected.

    fn fail_next_registry_write() {
        FAIL_NEXT_REGISTRY_WRITE.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn clear_revoke_hooks() {
        FAIL_NEXT_REGISTRY_WRITE.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Scratch data dir + seeded `http_clients` row. Holds the process-global
    /// hook lock and `data_dir_test_lock` so persist and injected failures cannot
    /// interleave with another test.
    struct RevokeFixture {
        _hooks: std::sync::MutexGuard<'static, ()>,
        _data_dir: std::sync::MutexGuard<'static, ()>,
        _override: crate::registry::DataDirOverride,
        state: RegistryState,
        client_id: String,
        http_id: String,
    }

    impl Drop for RevokeFixture {
        fn drop(&mut self) {
            clear_revoke_hooks();
        }
    }

    impl RevokeFixture {
        fn new(label: &str) -> Self {
            let hooks = REVOKE_HOOK_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clear_revoke_hooks();
            let data_dir = crate::registry::data_dir_test_lock();
            let dir = unique_update_test_dir(label);
            std::fs::create_dir_all(&dir).unwrap();
            let over = crate::registry::DataDirOverride::set(&dir);
            let client_id = "cursor".to_string();
            let http_id = format!("client:{client_id}");
            let mut reg = Registry::default();
            reg.http_clients.push(registry::HttpClient {
                id: http_id.clone(),
                label: format!("Client: {client_id}"),
                token_sha256: registry::sha256_hex("leftover-bearer"),
                profile: String::new(),
            });
            registry::save(&reg).unwrap();
            Self {
                _hooks: hooks,
                _data_dir: data_dir,
                _override: over,
                state: Mutex::new(reg),
                client_id,
                http_id,
            }
        }

        fn http_row_present(&self) -> bool {
            let on_disk = registry::load().expect("scratch registry loads");
            on_disk.http_clients.iter().any(|c| c.id == self.http_id)
        }
    }

    /// SBS-840 class, the instance left behind by that sweep. A failed vault READ
    /// must not fall through to minting a replacement bearer. `get_secret` collapsed
    /// a read error into `None`, which looks exactly like "this client has no bearer
    /// yet", so the mint path overwrote the vaulted copy and `retain`ed the client's
    /// `http_clients` row away for a new one. The bearer the client was already
    /// configured with then hashed to no row, so every request it made 401'd until
    /// the user reconnected that client by hand.
    #[test]
    fn ensure_client_http_token_propagates_a_failed_vault_read_instead_of_minting() {
        let fixture = RevokeFixture::new("sbs-840-ensure-read");

        let err = ensure_client_http_token_with(
            &fixture.state,
            &fixture.client_id,
            None,
            |_server, _client| Err("the keychain is locked".into()),
        )
        .expect_err("a failed vault read must not mint a replacement bearer");

        assert!(
            err.starts_with("Could not read the saved token"),
            "the vault read failure must reach the caller as a complete sentence \
             (the frontend renders it verbatim), got: {err}"
        );
        assert!(
            err.contains("the keychain is locked"),
            "the underlying cause must survive, got: {err}"
        );
        // The `expect_err` above is what pins the reported bug; today's mint path
        // returns `Ok`, so execution never reaches here under it.
        //
        // This guards the ORDERING invariant instead: nothing may error out after
        // already replacing the row. Assert the HASH rather than the id, because the
        // mint path `retain`s the old row away and pushes a new one under the SAME
        // id, so an id-only check cannot tell a surviving bearer from a replaced one.
        // `http_client_for_token` matches on `token_sha256`, so that is what decides
        // whether the client's configured token still authenticates.
        let on_disk = registry::load().expect("scratch registry loads");
        let row = on_disk
            .http_clients
            .iter()
            .find(|c| c.id == fixture.http_id)
            .expect("a failed vault read must not drop the client's http_clients row");
        assert_eq!(
            row.token_sha256,
            registry::sha256_hex("leftover-bearer"),
            "the row must still carry the ORIGINAL bearer's hash; a new hash means a \
             replacement was minted and the client's configured token is now dead"
        );
    }

    /// The reuse path is unchanged by the fix: a confirmed vaulted token that still
    /// matches a registered row is handed back as-is, with nothing minted. Guards
    /// against over-correcting the above into "never reuse".
    #[test]
    fn ensure_client_http_token_reuses_a_vaulted_token_that_matches_its_row() {
        let fixture = RevokeFixture::new("sbs-840-ensure-reuse");

        let token = ensure_client_http_token_with(
            &fixture.state,
            &fixture.client_id,
            None,
            // The fixture's row is registered against this exact token.
            |_server, _client| Ok(Some("leftover-bearer".to_string())),
        )
        .expect("a matching vaulted token must be reused");

        assert_eq!(token, "leftover-bearer");
        assert!(fixture.http_row_present());
    }

    #[test]
    fn revoke_client_http_token_fails_when_vault_delete_fails() {
        let fixture = RevokeFixture::new("sbs-845-vault-delete");
        let err =
            revoke_client_http_token_with(&fixture.state, &fixture.client_id, |_server, _client| {
                Err("the keychain is locked".into())
            })
            .expect_err("a failed vault delete must not look like a successful disconnect");
        assert!(
            err.contains("the keychain is locked"),
            "the vault failure must reach the caller verbatim, got: {err}"
        );
        // The point of the ordering: the row is dropped before the vault is
        // touched, so a keychain failure still revokes the bearer. The caller
        // is told the disconnect was not clean (an orphaned vault entry is
        // left), but the token can no longer authenticate.
        assert!(
            !fixture.http_row_present(),
            "the bearer must be revoked even when the vault delete fails"
        );
    }

    #[test]
    fn revoke_client_http_token_fails_when_registry_write_fails() {
        let fixture = RevokeFixture::new("sbs-845-registry-write");
        let vault_called = std::cell::Cell::new(false);
        fail_next_registry_write();
        let err =
            revoke_client_http_token_with(&fixture.state, &fixture.client_id, |_server, _client| {
                vault_called.set(true);
                Ok(())
            })
            .expect_err("a failed http_clients persist must not look like a successful disconnect");
        assert!(
            err.contains("injected registry write"),
            "expected the injected registry failure, got: {err}"
        );
        assert!(
            fixture.http_row_present(),
            "a failed persist must leave the http_clients row registered"
        );
        // Nothing was revoked, so the vault copy must survive: it still belongs
        // to a bearer that is registered and can authenticate. Deleting it here
        // would strip Toolport's own record of a token that still works.
        assert!(
            !vault_called.get(),
            "a failed persist must not delete the vaulted bearer"
        );
    }

    #[test]
    fn revoke_client_http_token_drops_the_http_clients_row() {
        let fixture = RevokeFixture::new("sbs-845-success");
        let deleted = std::cell::RefCell::new(Vec::new());
        revoke_client_http_token_with(&fixture.state, &fixture.client_id, |server, client| {
            deleted.borrow_mut().push((server.to_string(), client.to_string()));
            Ok(())
        })
        .expect("a successful vault delete must not block Disconnect");
        assert_eq!(
            deleted.into_inner(),
            vec![(
                "__toolport_http_clients__".to_string(),
                fixture.client_id.clone()
            )],
            "the vaulted bearer must be deleted under the shared-HTTP namespace"
        );
        assert!(
            !fixture.http_row_present(),
            "success means the bearer is gone from the registry"
        );
    }
}
