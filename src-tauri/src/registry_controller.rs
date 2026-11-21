//! Shell-neutral mutations for desktop adapters.
//!
//! Keep policy checks here so the Tauri and native GTK shells cannot drift on
//! security-sensitive registry behavior.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::clients::{self, GatewayEntryState, WriteOutcome};
use crate::registry::{self, ManagedEntry, Registry, ServerEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientImportCandidate {
    pub client_id: String,
    pub key: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub credentials: Vec<CredentialReview>,
    pub unsupported: Option<String>,
    /// The client's own name for the server when `name`, the name it is saved under,
    /// differs from it (`github` saved as `github (2)`).
    pub source_name: Option<String>,
    /// [`import_connection_key`]: which detected server this is, since two servers
    /// whose secrets differ can show the same redacted command or URL.
    pub connection: String,
}

const AUTH_LOCK_LEASE_SECS: u64 = 180;
const AUTH_LOCK_WAIT_SECS: u64 = 30;
const AUTH_LOCK_POLL_MS: u64 = 250;

pub(crate) struct AuthMutationLock {
    path: std::path::PathBuf,
    _personal_owner: Option<crate::local_auth::OwnerPin>,
}

impl Drop for AuthMutationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct AuthLockSnapshot {
    modified: std::time::SystemTime,
    contents: String,
}

impl AuthLockSnapshot {
    fn instance_key(&self) -> String {
        let modified = self
            .modified
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{modified}:{}", self.contents)
    }
}

fn auth_lock_path(server_id: &str) -> Result<std::path::PathBuf, String> {
    use sha2::{Digest, Sha256};

    let dir = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    let locks = dir.join("oauth-locks");
    std::fs::create_dir_all(&locks)
        .map_err(|error| format!("could not create oauth lock directory: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    Ok(locks.join(format!("auth-write-{:x}.lock", hasher.finalize())))
}

fn read_auth_lock(path: &Path) -> Result<Option<AuthLockSnapshot>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not stat auth mutation lock file: {error}")),
    };
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read auth mutation lock file: {error}"))?;
    Ok(Some(AuthLockSnapshot {
        modified: metadata
            .modified()
            .map_err(|error| format!("could not read auth mutation lock timestamp: {error}"))?,
        contents,
    }))
}

fn try_acquire_auth_lock(path: &Path) -> Result<Option<AuthMutationLock>, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mutation_id = format!("{}-{}", std::process::id(), now.as_nanos());
    let contents = format!(
        "mutation_id={mutation_id}\npid={}\nstarted={}\nlease_secs={}\n",
        std::process::id(),
        now.as_secs(),
        AUTH_LOCK_LEASE_SECS
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(contents.as_bytes())
                .map_err(|error| format!("could not write auth mutation lock file: {error}"))?;
            Ok(Some(AuthMutationLock {
                path: path.to_path_buf(),
                _personal_owner: None,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let Some(observed) = read_auth_lock(path)? else {
                return Ok(None);
            };
            let expired = observed
                .modified
                .elapsed()
                .is_ok_and(|elapsed| elapsed.as_secs() >= AUTH_LOCK_LEASE_SECS);
            if !expired {
                return Ok(None);
            }
            let Some(current) = read_auth_lock(path)? else {
                return Ok(None);
            };
            if current.instance_key() != observed.instance_key() {
                return Ok(None);
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .map_err(|error| format!("could not rewrite auth mutation lock file: {error}"))?;
            use std::io::Write as _;
            file.write_all(contents.as_bytes())
                .map_err(|error| format!("could not write auth mutation lock file: {error}"))?;
            file.flush()
                .map_err(|error| format!("could not flush auth mutation lock file: {error}"))?;
            Ok(Some(AuthMutationLock {
                path: path.to_path_buf(),
                _personal_owner: None,
            }))
        }
        Err(error) => Err(format!("could not create auth mutation lock file: {error}")),
    }
}

pub(crate) fn acquire_auth_lock(server_id: &str) -> Result<AuthMutationLock, String> {
    let pin = crate::local_auth::pin_personal_owner(server_id)?;
    let mut lock = acquire_auth_owner_lock(&crate::local_auth::owner(server_id)?)?;
    lock._personal_owner = pin;
    Ok(lock)
}

/// Handoffs lock both raw namespaces before changing ownership, outside the registry lock.
pub(crate) fn acquire_auth_owner_lock(owner: &str) -> Result<AuthMutationLock, String> {
    let path = auth_lock_path(owner)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(AUTH_LOCK_WAIT_SECS);
    loop {
        if let Some(lock) = try_acquire_auth_lock(&path)? {
            return Ok(lock);
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "another Toolport process is updating this configuration; timed out waiting for it to finish"
                    .into(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(AUTH_LOCK_POLL_MS));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFields {
    pub sync_local_only: bool,
    /// None preserves the current value; Some(None) restores the default.
    pub initialize_timeout_ms: Option<Option<u64>>,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub cwd: Option<String>,
    /// "Use my shell environment" for a local server. `None` keeps the saved choice.
    pub inherit_env: Option<bool>,
    /// "Hide prompts" for any transport. `None` keeps the saved choice.
    pub hide_prompts: Option<bool>,
}

#[derive(Debug)]
pub struct ClientMutationResult {
    pub registry: Registry,
    pub outcome: WriteOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedPrerequisite {
    pub server_id: String,
    pub server: String,
    pub tool: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EssentialSettings {
    pub safety_level: registry::SafetyLevel,
    pub team_min_safety_level: registry::SafetyLevel,
    pub lazy_discovery: bool,
    pub code_mode: bool,
    pub live_inspect: bool,
    pub deny_destructive: bool,
    pub deny_destructive_forced: bool,
    pub confirm_destructive: bool,
    pub human_approval: bool,
    pub human_approval_forced: bool,
    pub content_defense: bool,
    pub content_defense_forced: bool,
    pub quarantine_on_drift: bool,
    pub quarantine_on_drift_forced: bool,
    pub block_on_injection: bool,
    pub block_on_injection_forced: bool,
    pub pii_redaction: bool,
    pub pii_redaction_forced: bool,
    pub kept_v1_safety: registry::KeptV1Safety,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRoutingSettings {
    pub profiles: Vec<(String, String)>,
    pub mappings: Vec<crate::registry::FolderProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpClientSettings {
    pub clients: Vec<crate::registry::HttpClient>,
    pub profiles: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedHttpClient {
    pub settings: HttpClientSettings,
    pub token: String,
}

impl EssentialSettings {
    fn from_registry(registry: &Registry) -> Self {
        Self {
            safety_level: registry.safety_level_effective(),
            team_min_safety_level: registry.safety_level_team_floor(),
            lazy_discovery: registry.lazy_discovery,
            code_mode: registry.code_mode,
            live_inspect: registry.live_inspect,
            deny_destructive: registry.deny_destructive_effective(),
            deny_destructive_forced: registry.team_forced_deny_destructive,
            confirm_destructive: registry.confirm_destructive,
            human_approval: registry.human_approval_effective(),
            human_approval_forced: registry.team_forced_human_approval,
            content_defense: registry.content_defense_effective(),
            content_defense_forced: registry.team_forced_content_defense,
            quarantine_on_drift: registry.quarantine_on_drift_effective(),
            quarantine_on_drift_forced: registry.team_forced_quarantine_on_drift,
            block_on_injection: registry.block_on_injection_effective(),
            block_on_injection_forced: registry.team_forced_block_on_injection,
            pii_redaction: registry.pii_redaction_effective(),
            pii_redaction_forced: registry.team_forced_pii_redaction,
            kept_v1_safety: registry.kept(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EssentialSetting {
    LazyDiscovery,
    CodeMode,
    LiveInspect,
    PiiRedaction,
}

struct ClientConfigReceipt {
    target: PathBuf,
    backup: Option<PathBuf>,
    existed_before: bool,
    written: Option<Vec<u8>>,
    recovery_path: Option<PathBuf>,
    exact_rollback: bool,
}

impl ClientConfigReceipt {
    fn capture(outcome: &WriteOutcome) -> Result<Self, String> {
        let target = PathBuf::from(&outcome.path);
        let written = match std::fs::read(&target) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("could not verify the updated client config: {e}")),
        };
        let exact_rollback = outcome.recovery_path.is_none()
            || written
                .as_deref()
                .map(|bytes| registry::sha256_hex(std::str::from_utf8(bytes).unwrap_or("")))
                == outcome.revision;
        Ok(Self {
            target,
            backup: outcome.backup.as_deref().map(PathBuf::from),
            existed_before: outcome.existed_before,
            written,
            recovery_path: outcome.recovery_path.clone(),
            exact_rollback,
        })
    }

    fn rollback(&self) -> Result<(), String> {
        if !self.exact_rollback {
            return Err("exact rollback is unavailable because the client saved after this operation; newer edits were left untouched".into());
        }
        if self.backup.is_none() && self.existed_before {
            // Without a backup, the only rollback is removing the file, which is
            // right only for a config this operation created.
            return Err(format!(
                "there is no backup of {}, so Toolport left it as it wrote it",
                self.target.display()
            ));
        }
        let dir = registry::conduit_dir().ok_or("Could not resolve mutation lock dir")?;
        let _lock = registry::lock_at(&dir.join("client-config-mutation"))?;
        let revision = registry::client_file::read(&self.target)?;
        if revision.text.as_deref().map(str::as_bytes) != self.written.as_deref() {
            return Err(
                "the client config changed again, so Toolport left the newer file untouched".into(),
            );
        }
        let original = self
            .backup
            .as_ref()
            .map(|path| clients::read_client_backup(path, &self.target))
            .transpose()?;
        // A client whose folder Toolport never creates keeps that rule in rollback too:
        // with the folder gone, the backup stays and the rollback reports it.
        match clients::folder_gone_message_for_path(&self.target) {
            Some(missing) => registry::client_file::commit_in_existing_folder(
                &self.target,
                &revision,
                original.as_deref(),
                missing,
            )?,
            None => registry::client_file::commit(&self.target, &revision, original.as_deref())?,
        }
        if let Some(file) = &self.recovery_path {
            clients::record_config_rollback(file, &self.target, original.as_deref())?;
        }
        Ok(())
    }
}

impl ServerFields {
    fn normalized(mut self) -> Result<Self, String> {
        if self
            .initialize_timeout_ms
            .flatten()
            .is_some_and(|ms| ms == 0 || ms > 86_400_000)
        {
            return Err(
                "Startup timeout must be greater than 0 and at most 86,400 seconds.".into(),
            );
        }
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            return Err("give the server a name".into());
        }
        match self.transport.as_str() {
            "stdio" => {
                self.command = nonempty(self.command);
                if self.command.is_none() {
                    return Err("enter the command to run".into());
                }
                self.url = None;
                self.cwd = nonempty(self.cwd);
            }
            "http" | "sse" => {
                self.url = nonempty(self.url);
                if !self
                    .url
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
                {
                    return Err("enter an http:// or https:// server URL".into());
                }
                self.command = None;
                self.args.clear();
                self.cwd = None;
                self.inherit_env = self.inherit_env.map(|_| false);
            }
            _ => return Err("choose stdio, HTTP, or SSE transport".into()),
        }
        Ok(self)
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

pub fn apply_add_server(registry: &mut Registry, fields: ServerFields) -> Result<String, String> {
    Ok(apply_add_entry(registry, entry_from_fields(fields)?))
}

fn entry_from_fields(fields: ServerFields) -> Result<ServerEntry, String> {
    let fields = fields.normalized()?;
    Ok(ServerEntry {
        enabled: false,
        inherit_env: fields.inherit_env.unwrap_or(false),
        id: String::new(),
        name: fields.name,
        transport: fields.transport,
        command: fields.command,
        args: fields.args,
        env: Vec::new(),
        url: fields.url,
        cwd: fields.cwd,
        source: Some("manual".into()),
        disabled_tools: Vec::new(),
        hide_prompts: fields.hide_prompts.unwrap_or(false),
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: fields.initialize_timeout_ms.flatten(),
        launch: None,
        unknown_fields: if fields.sync_local_only {
            serde_json::Map::from_iter([("syncLocalOnly".into(), serde_json::json!(true))])
        } else {
            serde_json::Map::new()
        },
    })
}

pub fn apply_add_entry(registry: &mut Registry, mut entry: ServerEntry) -> String {
    entry.enabled = false;
    apply_import_entry(registry, entry)
}

pub(crate) fn server_from_detected(server: &clients::McpServer, client_id: &str) -> ServerEntry {
    ServerEntry {
        enabled: false,
        inherit_env: false,
        id: String::new(),
        name: server.name.clone(),
        transport: server.transport.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        env: server
            .env_keys
            .iter()
            .map(|key| registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            })
            .collect(),
        url: server.url.clone(),
        source: Some(format!("imported:{client_id}")),
        disabled_tools: Vec::new(),
        hide_prompts: false,
        cwd: None,
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: None,
        launch: None,
        unknown_fields: serde_json::Map::new(),
    }
}

/// An opaque id for a connection (and, for an entry with env or header keys, the
/// account name it is listed under), stable for this process. The connection can hold a
/// secret (a token in a URL or argument), so the hash is salted per process and the
/// id reveals nothing that could be checked against a guess.
pub(crate) fn import_connection_key(server: &ServerEntry) -> String {
    static SALT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let salt = SALT.get_or_init(|| {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).expect("CSPRNG unavailable");
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    });
    // Credentials in env or headers are not part of the connection, so two accounts
    // of one server (a personal and a work token) under different names are two
    // servers, as `existing_connection` treats them. Without such keys the names
    // cannot differ in anything that matters, so one connection is one server.
    let account = if server.env.is_empty() {
        String::new()
    } else {
        server.name.to_ascii_lowercase()
    };
    format!(
        "connection:{}",
        registry::sha256_hex(&format!(
            "{salt}\n{}\n{account}",
            clients::connection_key(
                &server.transport,
                server.command.as_deref(),
                &server.args,
                server.url.as_deref()
            )
        ))
    )
}

/// What the reviewed import key binds: the connection and the name it is saved under,
/// so a name that shifts between review and import is refused instead of applied to
/// another server.
pub(crate) fn import_review_key(server: &ServerEntry, name: &str) -> String {
    format!(
        "{}:{}",
        import_connection_key(server),
        registry::sha256_hex(name)
    )
}

/// Whether identity may be confirmed through the vault. Detection (client lists and
/// counts) never reads it; only a review the user opened, or its import, may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirm {
    VaultFree,
    WithVault,
}

/// A client server Import may offer: its entry, why its identity is unconfirmed (if
/// so), and the name it is saved under.
pub(crate) struct ImportCandidate {
    pub(crate) entry: ServerEntry,
    pub(crate) unknown: Option<String>,
    pub(crate) name: String,
}

/// Client servers Import may offer, deduped by connection (and by name for an entry
/// with env or header keys, one per account), minus those already in
/// Toolport under the one identity rule (`existing_connection`). An entry whose
/// identity cannot be confirmed is kept with its reason, so review shows it as
/// unsupported instead of hiding it or counting it as new. New entries get their
/// own name, or `name (2)` and so on when an existing server or an earlier pick
/// uses it.
pub(crate) fn import_candidates(
    detected: &[clients::DetectedClient],
    existing: &Registry,
    confirm: Confirm,
) -> Vec<ImportCandidate> {
    let mut picked = Vec::new();
    let mut import_keys = std::collections::HashSet::new();
    let mut names = existing
        .servers
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>();
    for client in detected {
        for server in client.servers.iter().chain(client.plugin_servers.iter()) {
            let entry = server_from_detected(server, &client.id);
            if clients::is_gateway_server(&entry)
                || !import_keys.insert(import_connection_key(&entry))
            {
                continue;
            }
            let unknown = match existing_connection(existing, client, server, confirm) {
                ConnectionState::Existing(_) => continue,
                ConnectionState::New => None,
                ConnectionState::Unknown(reason) => Some(reason),
            };
            let name = if unknown.is_none() {
                let name = clients::distinct_import_name(&entry.name, &names);
                names.push(name.clone());
                name
            } else {
                entry.name.clone()
            };
            picked.push(ImportCandidate {
                entry,
                unknown,
                name,
            });
        }
    }
    picked
}

#[cfg(test)]
pub(crate) fn servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
) -> Vec<ServerEntry> {
    import_candidates(detected, existing, Confirm::WithVault)
        .into_iter()
        .filter(|c| c.unknown.is_none())
        .map(|c| c.entry)
        .collect()
}

/// The reviewed servers to import, each with the name it is saved under. A key that
/// no longer matches a server under its reviewed name is left out; callers refuse.
pub(crate) fn selected_servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
    selected: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<(ServerEntry, String)>, String> {
    let candidates = crate::secrets::with_backup_batch(|| {
        import_candidates(detected, existing, Confirm::WithVault)
    });
    // A reviewed server whose saved name shifted since review (another server took
    // it) is refused, so no reviewed choice or value lands on a different server.
    if let Some(keys) = selected {
        let reviewed = keys
            .iter()
            .filter_map(|key| key.rsplit_once(':'))
            .collect::<std::collections::HashMap<_, _>>();
        if candidates.iter().any(|c| {
            c.unknown.is_none()
                && reviewed
                    .get(import_connection_key(&c.entry).as_str())
                    .is_some_and(|name| *name != registry::sha256_hex(&c.name))
        }) {
            return Err("Server changed. Refresh the client and review again.".into());
        }
    }
    let picked = candidates
        .into_iter()
        .filter(|c| {
            c.unknown.is_none()
                && selected.is_none_or(|keys| keys.contains(&import_review_key(&c.entry, &c.name)))
        })
        .map(|c| (c.entry, c.name))
        .collect::<Vec<_>>();
    for client in detected {
        let source = format!("imported:{}", client.id);
        let names = picked
            .iter()
            .filter(|(server, _)| server.source.as_deref() == Some(source.as_str()))
            .map(|(server, _)| server.name.clone())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            clients::validate_client_import(client, &names, false)?;
        }
    }
    Ok(picked)
}

// Scoped keys bind the selected origin and the complete reviewed definition,
// including credential values that are intentionally absent from the UI.
fn scoped_import_key(
    client_id: &str,
    server: &ServerEntry,
    name: &str,
    definition: Option<&serde_json::Value>,
) -> Result<String, String> {
    let reviewed = serde_json::to_string(&(server, definition)).map_err(|e| e.to_string())?;
    Ok(format!(
        "client-import:{}",
        serde_json::to_string(&(
            client_id,
            import_review_key(server, name),
            registry::sha256_hex(&reviewed)
        ))
        .map_err(|e| e.to_string())?
    ))
}

pub fn preview_client_imports_for(client_id: &str) -> Result<Vec<ClientImportCandidate>, String> {
    preview_client_imports_scoped(Some(client_id))
}

pub fn preview_client_imports() -> Result<Vec<ClientImportCandidate>, String> {
    preview_client_imports_scoped(None)
}

fn preview_client_imports_scoped(
    client_id: Option<&str>,
) -> Result<Vec<ClientImportCandidate>, String> {
    let registry = read_registry_exact_or_default()?;
    let mut detected = clients::detect_clients();
    if let Some(id) = client_id {
        detected.retain(|client| client.id == id);
        if detected.is_empty() {
            return Err("Missing import client".into());
        }
    }
    // A review may read the keychain to confirm identity, but never ask to unlock it:
    // a locked keyring leaves the row unconfirmed with its reason. The import may ask.
    // One keychain session for the whole review, so a hung daemon costs one wait.
    let candidates = crate::secrets::with_backup_batch(|| {
        crate::secrets::without_prompts(|| {
            import_candidates(&detected, &registry, Confirm::WithVault)
        })
    });
    candidates
        .into_iter()
        .map(
            |ImportCandidate {
                 entry: server,
                 unknown,
                 name,
             }| {
                let client = detected
                    .iter()
                    .find(|client| {
                        server.source.as_deref() == Some(format!("imported:{}", client.id).as_str())
                    })
                    .ok_or("Missing import client")?;
                let definition = clients::import_definition(client, &server.name)?;
                let key = if client_id.is_some() {
                    scoped_import_key(&client.id, &server, &name, definition.as_ref())?
                } else {
                    import_review_key(&server, &name)
                };
                let prepared = match unknown {
                    Some(reason) => Err(reason),
                    None => crate::import_credentials::Import::prepare(
                        server.clone(),
                        definition.as_ref(),
                    ),
                };
                let (credentials, unsupported) = match prepared {
                    Ok(import) => (
                        import
                            .review_credentials()
                            .into_iter()
                            .map(|(key, secret, present, required)| CredentialReview {
                                key,
                                secret,
                                present,
                                required,
                            })
                            .collect(),
                        None,
                    ),
                    Err(error) => (Vec::new(), Some(error)),
                };
                Ok(ClientImportCandidate {
                    client_id: client.id.clone(),
                    credentials,
                    unsupported,
                    key,
                    source_name: (name != server.name).then(|| server.name.clone()),
                    connection: import_connection_key(&server),
                    name,
                    transport: server.transport,
                    command: server.command.map(|c| {
                        if registry::arg_looks_secret(&c) {
                            "<command>".into()
                        } else {
                            c
                        }
                    }),
                    args: crate::import_credentials::shown_args(&server.args),
                    url: server
                        .url
                        .as_deref()
                        .map(crate::import_credentials::shown_url),
                })
            },
        )
        .collect::<Result<Vec<_>, String>>()
}

fn unresolved_connection(source_name: &str, existing: &str) -> String {
    format!(
        "Could not compare {source_name} with {existing}, which is missing a required value. Finish {existing} under Servers, then review again. Client config unchanged."
    )
}

/// Existing servers that can be the same connection as a detected client server, found
/// without the vault: the same raw connection key, or, for a client entry carrying a
/// secret in its URL or args, the redacted key an import stores (`shown_url`,
/// `<launch-input>` args). Only these are confirmed with `Import::matches`, so listing
/// clients reads the vault only when a stored secret-bearing server could match.
fn connection_candidates<'a>(
    registry: &'a Registry,
    detected: &clients::McpServer,
) -> Vec<&'a ServerEntry> {
    let raw = detected_connection(detected);
    let shown_url = detected
        .url
        .as_deref()
        .map(crate::import_credentials::shown_url);
    let redacted = clients::connection_key(
        &detected.transport,
        detected.command.as_deref(),
        &crate::import_credentials::shown_args(&detected.args),
        shown_url.as_deref(),
    );
    registry
        .servers
        .iter()
        .filter(|existing| {
            // Launch bindings say exactly which parts of a stored stdio argv are known;
            // a redacted or raw key must not override a known fragment that differs.
            if existing.transport == "stdio"
                && existing
                    .launch
                    .as_ref()
                    .is_some_and(|launch| !launch.bindings.is_empty())
            {
                return launch_bound_candidate(existing, detected);
            }
            let key = clients::connection_key(
                &existing.transport,
                existing.command.as_deref(),
                &existing.args,
                existing.url.as_deref(),
            );
            key == raw || (redacted != raw && key == redacted)
        })
        .collect()
}

/// A stored stdio server whose argv holds launch-bound placeholders (a catalog
/// Filesystem entry stores `<launch-input>` for its allowed directory) can be the same
/// connection as a client's literal argv. Each bound argument becomes a pattern of
/// known fragments (literals and non-secret inputs, resolved exactly as
/// `launch_inputs::resolve_args_with` does, without the vault) and unknown parts
/// (secret or referenced inputs, or a required value that is missing). The server is
/// kept for confirmation by `Import::matches` only when every argument fits its
/// pattern, so a known fragment that differs rules it out without reading the vault.
fn launch_bound_candidate(existing: &ServerEntry, detected: &clients::McpServer) -> bool {
    launch_bound_match(existing, detected).is_some()
}

/// `None` when `existing` cannot be the connection; `Some(true)` when every argument is
/// known without the vault and matches exactly, so no vault read can change the answer.
fn launch_bound_match(existing: &ServerEntry, detected: &clients::McpServer) -> Option<bool> {
    let launch = existing
        .launch
        .as_ref()
        .filter(|launch| !launch.bindings.is_empty())?;
    if existing.transport != "stdio"
        || detected.transport != "stdio"
        || existing.command != detected.command
        || existing.args.len() != detected.args.len()
    {
        return None;
    }
    // `None` is an unknown part.
    let mut patterns = existing
        .args
        .iter()
        .map(|arg| vec![Some(arg.clone())])
        .collect::<Vec<_>>();
    for binding in &launch.bindings {
        let slot = patterns.get_mut(binding.index)?;
        let mut pattern = Vec::new();
        for part in &binding.parts {
            match part {
                registry::ArgPart::Literal { value, .. } => pattern.push(Some(value.clone())),
                registry::ArgPart::Input { key, .. } => {
                    let input = launch.inputs.iter().find(|input| &input.key == key)?;
                    let referenced = crate::secret_refs::source(&input.unknown_fields)
                        .ok()
                        .flatten()
                        .is_some();
                    if input.secret || referenced {
                        pattern.push(None);
                        continue;
                    }
                    // Mirrors the resolver: a blank value is missing; a missing optional
                    // input is empty; a missing required one cannot be resolved.
                    match input.value.as_deref().filter(|v| !v.trim().is_empty()) {
                        Some(value) => pattern.push(Some(value.to_string())),
                        None if input.required => pattern.push(None),
                        None => pattern.push(Some(String::new())),
                    }
                }
            }
        }
        *slot = pattern;
    }
    patterns
        .iter()
        .zip(&detected.args)
        .all(|(pattern, actual)| fragments_match(pattern, actual))
        .then(|| patterns.iter().flatten().all(Option::is_some))
}

/// Whether `actual` is the known fragments in order, with each unknown part matching
/// any text (including none). Adjacent known fragments join into one exact chunk; the
/// first chunk anchors the start, the last the end, and the chunks between are found
/// leftmost in order, which is exact for patterns whose only wildcard is "any text".
fn fragments_match(pattern: &[Option<String>], actual: &str) -> bool {
    let mut chunks = vec![String::new()];
    for part in pattern {
        match part {
            Some(known) => chunks.last_mut().unwrap().push_str(known),
            None => chunks.push(String::new()),
        }
    }
    if chunks.len() == 1 {
        return chunks[0] == actual;
    }
    let (first, rest) = chunks.split_first().unwrap();
    let (last, middle) = rest.split_last().unwrap();
    let Some(mut remaining) = actual.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some(trimmed) = remaining.strip_suffix(last.as_str()) else {
        return false;
    };
    remaining = trimmed;
    for chunk in middle {
        let Some(at) = remaining.find(chunk.as_str()) else {
            return false;
        };
        remaining = &remaining[at + chunk.len()..];
    }
    true
}

/// How import candidates and client counts see a detected client server under the one
/// identity rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionState {
    /// No existing server has this connection: importable.
    New,
    /// This existing server id has the same connection: already in Toolport.
    Existing(String),
    /// Identity could not be confirmed (for example a vaulted value is missing). Neither
    /// importable nor managed; the reason is shown in review.
    Unknown(String),
}

pub(crate) fn existing_connection(
    registry: &Registry,
    client: &clients::DetectedClient,
    detected: &clients::McpServer,
    confirm: Confirm,
) -> ConnectionState {
    let mut candidates = connection_candidates(registry, detected);
    // Credentials in env are not part of the connection, so two accounts of one server
    // (one token each) look alike. For such an entry only a server saved under its
    // name, or that name with an import suffix (`slack (2)`), is the same one.
    if !detected.env_keys.is_empty() {
        candidates.retain(|existing| saved_under_name(&existing.name, &detected.name));
    }
    if candidates.is_empty() {
        return ConnectionState::New;
    }
    // A raw-equal server without vault references resolves to exactly its stored args
    // and URL, which is what `Import::matches` would conclude; skip reading the client
    // config and the vault for the common already-imported case.
    let raw = detected_connection(detected);
    if let Some(existing) = candidates.iter().find(|existing| {
        !crate::import_credentials::has_imported_url(existing)
            && existing
                .launch
                .as_ref()
                .is_none_or(|launch| launch.bindings.is_empty())
            && clients::connection_key(
                &existing.transport,
                existing.command.as_deref(),
                &existing.args,
                existing.url.as_deref(),
            ) == raw
    }) {
        return ConnectionState::Existing(existing.id.clone());
    }
    if confirm == Confirm::VaultFree {
        // A launch-bound server whose arguments are all known matches without the vault.
        if let Some(existing) = candidates
            .iter()
            .find(|existing| launch_bound_match(existing, detected) == Some(true))
        {
            return ConnectionState::Existing(existing.id.clone());
        }
        // Only the vault could tell; detection never reads it. Review confirms.
        return ConnectionState::Unknown(format!(
            "Review {} to check whether it is already in Toolport as {}.",
            detected.name, candidates[0].name
        ));
    }
    let import = clients::import_definition(client, &detected.name).and_then(|definition| {
        crate::import_credentials::Import::prepare(
            server_from_detected(detected, &client.id),
            definition.as_ref(),
        )
    });
    let import = match import {
        Ok(import) => import,
        // The plain raw match was handled above. Every remaining candidate needs vault
        // or launch resolution, which needs a prepared import: unknown, never assumed.
        Err(error) => return ConnectionState::Unknown(error),
    };
    let (mut unresolved, mut locked) = (None, false);
    for existing in candidates {
        match import.matches(existing) {
            Ok(true) => return ConnectionState::Existing(existing.id.clone()),
            Ok(false) => {}
            // A keychain that could not be read is not a missing value.
            Err(error) if crate::import_credentials::is_vault_failure(&error) => locked = true,
            Err(_) => {
                unresolved.get_or_insert(existing.name.clone());
            }
        }
    }
    match (locked, unresolved) {
        (true, _) => ConnectionState::Unknown(format!(
            "Keychain unavailable, so {} could not be compared with a server in Toolport. Unlock it and review again. Client config unchanged.",
            detected.name
        )),
        (false, Some(name)) => {
            ConnectionState::Unknown(unresolved_connection(&detected.name, &name))
        }
        (false, None) => ConnectionState::New,
    }
}

/// Whether `saved` is `name`, or `name (N)` as [`clients::distinct_import_name`] saves
/// it beside a taken name (ignoring ASCII case).
fn saved_under_name(saved: &str, name: &str) -> bool {
    let (saved, name) = (saved.to_ascii_lowercase(), name.to_ascii_lowercase());
    saved == name
        || saved
            .strip_prefix(&format!("{name} ("))
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The raw native connection of a detected client server. Two reviewed entries with the
/// same connection would land on one server and overwrite each other's settings.
fn detected_connection(server: &clients::McpServer) -> String {
    clients::connection_key(
        &server.transport,
        server.command.as_deref(),
        &server.args,
        server.url.as_deref(),
    )
}

fn registry_matches(left: &Registry, right: &Registry) -> Result<bool, String> {
    let mut left = left.clone();
    let mut right = right.clone();
    left.normalize_profile_references();
    right.normalize_profile_references();
    left.sync_legacy_safety_mirror();
    right.sync_legacy_safety_mirror();
    Ok(serde_json::to_value(&left).map_err(|e| e.to_string())?
        == serde_json::to_value(&right).map_err(|e| e.to_string())?)
}
/// The check before a connect rewrites its client: the servers and all shared settings
/// are as staged. Another client's own records (its managed entry and scope) may change
/// meanwhile, for example when a Clients scan finishes a repair it deferred while the
/// keyring was locked, and the connect's own registry write keeps them as they are now.
fn setup_registry_matches(
    latest: &Registry,
    staged: &Registry,
    client_id: &str,
) -> Result<bool, String> {
    let own = |registry: &Registry| {
        let mut registry = registry.clone();
        registry
            .client_managed_entries
            .retain(|id, _| id == client_id);
        registry.client_scopes.retain(|id, _| id == client_id);
        registry
    };
    registry_matches(&own(latest), &own(staged))
}

fn commit_imports(previous: &Registry, prepared: Registry) -> Result<Registry, String> {
    registry::update(|latest| {
        if !registry_matches(latest, previous)? {
            return Err("Servers changed during import. Review again before retrying.".into());
        }
        *latest = prepared;
        Ok(())
    })
    .map(|(registry, ())| registry)
}

pub fn import_client_servers(selected: Vec<String>) -> Result<(Registry, usize), String> {
    import_client_servers_inputs(selected, &Default::default(), &Default::default())
}

pub fn import_client_servers_inputs(
    selected: Vec<String>,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> Result<(Registry, usize), String> {
    let mut detected = clients::detect_clients();
    let scoped = selected
        .iter()
        .filter_map(|key| {
            key.strip_prefix("client-import:").map(|json| {
                serde_json::from_str::<(String, String, String)>(json)
                    .map_err(|_| "Invalid reviewed import key".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !scoped.is_empty()
        && (scoped.len() != selected.len() || scoped.iter().any(|item| item.0 != scoped[0].0))
    {
        return Err("Review imports from one client at a time.".into());
    }
    if let Some((client_id, _, _)) = scoped.first() {
        detected.retain(|client| client.id == *client_id);
    }
    let reviewed = selected
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let selected = if scoped.is_empty() {
        reviewed.clone()
    } else {
        scoped.iter().map(|item| item.1.clone()).collect()
    };
    let current = read_registry_exact_or_default()?;
    let servers = selected_servers_to_import(&detected, &current, Some(&selected))?;
    if !scoped.is_empty() && servers.len() != selected.len() {
        return Err("Server changed. Refresh the client and review again.".into());
    }
    let mut prepared = Vec::new();
    for (entry, name) in servers {
        let client_id = entry
            .source
            .as_deref()
            .and_then(|s| s.strip_prefix("imported:"))
            .ok_or("Missing import source")?;
        let client = detected
            .iter()
            .find(|c| c.id == client_id)
            .ok_or("Missing import client")?;
        let definition = clients::import_definition(client, &entry.name)?;
        if !scoped.is_empty()
            && !reviewed.contains(&scoped_import_key(
                client_id,
                &entry,
                &name,
                definition.as_ref(),
            )?)
        {
            return Err("Server changed. Refresh the client and review again.".into());
        }
        // Reviews show and key each server by the name it is saved under, which its
        // review key binds.
        match crate::import_credentials::Import::prepare_with_choices(
            entry,
            definition.as_ref(),
            choices.get(&name),
        ) {
            Ok(mut import) => {
                import.entry.name = name.clone();
                if let Some(values) = inputs.get(&name) {
                    import.supply(values)?;
                }
                prepared.push(import);
            }
            Err(error)
                if error.contains("custom HTTP headers")
                    || error.contains("unsupported HTTP")
                    || error.contains("HTTP headers from environment") => {}
            Err(error) => return Err(error),
        }
    }
    crate::import_credentials::transaction(|writes| {
        let mut registry = current.clone();
        let mut added = 0;
        for import in prepared {
            let id = registry.add_server(import.entry.clone());
            let missing =
                import.transfer_into(&id, writes, current.servers.iter().any(|s| s.id == id))?;
            if missing.is_empty() {
                let profile = registry.default_access_id();
                apply_server_enabled(&mut registry, &profile, &id, true, false)?;
                let _ = registry.set_access_server(&profile, &id, true);
            }
            added += 1;
        }
        if added == 0 {
            return Ok((current, 0));
        }
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok((commit_imports(&current, registry)?, added))
    })
}

pub fn apply_update_entry(registry: &mut Registry, entry: ServerEntry) -> Result<(), String> {
    crate::secret_refs::validate_server(&entry).map_err(|e| e.to_string())?;
    if let Some(launch) = &entry.launch {
        launch.validate(&entry.args, true)?;
    }
    registry.update_server(entry)
}

pub fn apply_update_server_fields(
    registry: &mut Registry,
    server_id: &str,
    fields: ServerFields,
) -> Result<(), String> {
    let fields = fields.normalized()?;
    let server = registry
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    if server.command != fields.command || server.args != fields.args || fields.transport != "stdio"
    {
        if server.launch.is_some() {
            if fields.args.iter().any(|arg| arg == "<launch-input>") {
                return Err("Replace <launch-input> with a literal argument before saving the edited command or arguments".into());
            }
            server.source = Some("manual".into());
        }
        server.launch = None;
    }
    if let Some(timeout) = fields.initialize_timeout_ms {
        server.initialize_timeout_ms = timeout;
    }
    server.name = fields.name;
    server.transport = fields.transport;
    server.command = fields.command;
    server.args = fields.args;
    server.url = fields.url;
    server.cwd = fields.cwd;
    if let Some(inherit_env) = fields.inherit_env {
        server.inherit_env = inherit_env;
    }
    if let Some(hide_prompts) = fields.hide_prompts {
        server.hide_prompts = hide_prompts;
    }
    Ok(())
}

pub fn apply_remove_server(registry: &mut Registry, server_id: &str) -> Result<(), String> {
    registry.remove_server(server_id)
}

pub fn add_reviewed_entry(entry: ServerEntry) -> Result<(Registry, String), String> {
    let env = entry
        .env
        .iter()
        .map(|e| {
            (
                e.key.clone(),
                e.value
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let definition = serde_json::json!({"env":env});
    let choices = entry
        .env
        .iter()
        .map(|env| (env.key.clone(), env.secret))
        .collect();
    let import = crate::import_credentials::Import::prepare_with_choices(
        entry,
        Some(&definition),
        Some(&choices),
    )?;
    crate::import_credentials::transaction(|writes| {
        let current = read_registry_exact_or_default()?;
        let mut registry = current.clone();
        let mut entry = import.entry.clone();
        entry.enabled = false;
        let id = registry.add_server(entry);
        let missing =
            import.transfer_into(&id, writes, current.servers.iter().any(|s| s.id == id))?;
        if missing.is_empty() {
            let profile = registry.default_access_id();
            if apply_server_enabled(&mut registry, &profile, &id, true, false).is_ok() {
                let _ = registry.set_access_server(&profile, &id, true);
            }
        }
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok((commit_imports(&current, registry)?, id))
    })
}

pub fn add_server(fields: ServerFields) -> Result<Registry, String> {
    Ok(add_snippet_server(fields, Vec::new())?.registry)
}

pub fn add_server_with_launch(
    fields: ServerFields,
    launch: crate::registry::LaunchConfig,
) -> Result<Registry, String> {
    let (registry, _) = registry::update(|registry| {
        let id = apply_add_server(registry, fields)?;
        let server = registry
            .servers
            .iter_mut()
            .find(|server| server.id == id)
            .expect("just added");
        launch.validate(&server.args, true)?;
        server.launch = Some(launch);
        server.enabled = false;
        let profile = registry.default_access_id();
        if apply_server_enabled(registry, &profile, &id, true, false).is_ok() {
            let _ = registry.set_access_server(&profile, &id, true);
        }
        Ok(id)
    })?;
    Ok(registry)
}

/// The result of adding a server from a pasted config snippet.
#[derive(Debug)]
pub struct SnippetAddOutcome {
    pub registry: Registry,
    /// Env keys declared on the entry without a pasted value; the user still has
    /// to store these through the credentials flow.
    pub declared_without_value: Vec<String>,
    /// Legacy field kept for existing single-paste consumers. Failed writes now
    /// return an error and roll back the whole batch.
    pub failed: Vec<String>,
    pub servers: Vec<SnippetServerOutcome>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetServerOutcome {
    pub name: String,
    pub status: String,
    pub missing: Vec<String>,
}

fn add_snippet_imports(
    imports: Vec<crate::import_credentials::Import>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> Result<SnippetAddOutcome, String> {
    crate::import_credentials::transaction(|writes| {
        let current = read_registry_exact_or_default()?;
        let mut registry = current.clone();
        let mut servers = Vec::new();
        let mut missing_all = Vec::new();
        for mut import in imports {
            if let Some(values) = inputs.get(&import.entry.name) {
                import.supply(values)?;
            }
            let name = import.entry.name.clone();
            if registry
                .servers
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&name))
            {
                servers.push(SnippetServerOutcome {
                    name,
                    status: "already present".into(),
                    missing: Vec::new(),
                });
                continue;
            }
            let id = registry.add_server(import.entry.clone());
            let missing =
                import.transfer_into(&id, writes, current.servers.iter().any(|s| s.id == id))?;
            let profile = registry.default_access_id();
            if missing.is_empty() {
                apply_server_enabled(&mut registry, &profile, &id, true, false)?;
                let _ = registry.set_access_server(&profile, &id, true);
            }
            missing_all.extend(missing.clone());
            servers.push(SnippetServerOutcome {
                name,
                status: if missing.is_empty() {
                    "added"
                } else {
                    "needs input"
                }
                .into(),
                missing,
            });
        }
        if servers.iter().any(|s| s.status != "already present") {
            registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
            registry = commit_imports(&current, registry)?;
        }
        Ok(SnippetAddOutcome {
            registry,
            declared_without_value: missing_all,
            failed: Vec::new(),
            servers,
        })
    })
}

/// Add a pasted server atomically. Secret values go to the vault; ordinary
/// environment settings remain in the registry. A failed write restores both.
pub fn add_snippet_server(
    fields: ServerFields,
    env: Vec<(String, Option<String>)>,
) -> Result<SnippetAddOutcome, String> {
    add_snippet_server_choices(fields, env, &Default::default())
}

/// One value row of Add server, as both shells collect it.
#[derive(Debug, Clone, Default)]
pub struct ValueRow {
    pub key: String,
    pub value: String,
    /// Kept in the keychain (or read from a password manager).
    pub secret: bool,
    /// A password manager reference, when the row reads its value from one.
    pub reference: Option<String>,
    /// "Same on every machine", only once chosen: until then sync's default applies.
    pub portable: Option<bool>,
}

/// What [`add_server_with_rows`] saved.
pub struct RowsAddOutcome {
    pub registry: Registry,
    /// The added server's id.
    pub id: String,
    /// Keys saved without a usable value (blank or a placeholder); the user adds
    /// them in Credentials.
    pub declared_without_value: Vec<String>,
}

/// Add a server from Add server's rows in one step, as a pasted server is added:
/// the entry, its plain values and references, and every typed secret are saved
/// together or not at all, with one keychain unlock. The server is switched on
/// (and added to the default access) once every row has a value or a password
/// manager reference, as the React dialog does.
pub fn add_server_with_rows(
    fields: ServerFields,
    rows: Vec<ValueRow>,
) -> Result<RowsAddOutcome, String> {
    add_server_with_values(fields, rows, EntryExtras::default())
}

/// What React's dialog sends beyond [`ServerFields`] and the rows. GTK passes none.
#[derive(Debug, Default)]
pub struct EntryExtras {
    /// A catalog preset's or a Duplicate's launch inputs.
    pub launch: Option<registry::LaunchConfig>,
    /// Where the entry came from (`catalog:curated`, a Duplicate's original);
    /// `manual` when absent.
    pub source: Option<String>,
    /// A remote Duplicate's request timeout.
    pub request_timeout_ms: Option<u64>,
    /// A remote Duplicate's header credentials (`headerKeys`) and its allowed
    /// password manager prefixes (`secretSources`), kept as the entry had them.
    pub kept_fields: serde_json::Map<String, serde_json::Value>,
}

/// Add the server React's Add server dialog sends, in one step: what the
/// `add_server_with_rows` command runs.
pub fn add_server_from_dialog(entry: ServerEntry) -> Result<RowsAddOutcome, String> {
    let (fields, rows, extras) = values_from_entry(entry)?;
    add_server_with_values(fields, rows, extras)
}

/// React's Add server dialog sends its whole entry: split it into what
/// [`add_server_with_values`] takes. Every env row is a value row; a row with a
/// `source` is a password manager reference.
/// The entry fields React's dialog sends that the values path keeps as they are.
const KEPT_ENTRY_FIELDS: [&str; 2] = ["headerKeys", "secretSources"];

pub fn values_from_entry(
    entry: ServerEntry,
) -> Result<(ServerFields, Vec<ValueRow>, EntryExtras), String> {
    let rows = entry
        .env
        .iter()
        .map(|env| {
            let reference = crate::secret_refs::source(&env.unknown_fields)
                .map_err(|error| error.to_string())?
                .map(str::to_string);
            Ok(ValueRow {
                key: env.key.clone(),
                value: env.value.clone().unwrap_or_default(),
                secret: env.secret,
                portable: env
                    .unknown_fields
                    .get("portable")
                    .and_then(serde_json::Value::as_bool),
                reference,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let fields = ServerFields {
        sync_local_only: entry.unknown_fields.get("syncLocalOnly")
            == Some(&serde_json::Value::Bool(true)),
        initialize_timeout_ms: Some(entry.initialize_timeout_ms),
        name: entry.name,
        transport: entry.transport,
        command: entry.command,
        args: entry.args,
        url: entry.url,
        cwd: entry.cwd,
        inherit_env: Some(entry.inherit_env),
        hide_prompts: Some(entry.hide_prompts),
    };
    let kept_fields = KEPT_ENTRY_FIELDS
        .iter()
        .filter_map(|key| {
            entry
                .unknown_fields
                .get(*key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect();
    let extras = EntryExtras {
        launch: entry.launch,
        source: entry.source,
        request_timeout_ms: entry.request_timeout_ms,
        kept_fields,
    };
    Ok((fields, rows, extras))
}

/// [`add_server_with_rows`] with what React's dialog adds: a catalog preset's or
/// a Duplicate's launch inputs, whose secret values are saved in the same step
/// as the rows' values, and the entry's source and request timeout.
pub fn add_server_with_values(
    fields: ServerFields,
    rows: Vec<ValueRow>,
    extras: EntryExtras,
) -> Result<RowsAddOutcome, String> {
    let rows: Vec<ValueRow> = rows
        .into_iter()
        .filter(|row| !row.key.trim().is_empty())
        .map(|row| ValueRow {
            key: row.key.trim().to_string(),
            ..row
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(row) = rows.iter().find(|row| !seen.insert(row.key.as_str())) {
        return Err(crate::i18n::trf(
            "Use each variable name once. Rename or remove the second {value0}.",
            &[("value0", &row.key)],
        ));
    }
    let mut entry = entry_from_fields(fields)?;
    if let Some(source) = extras.source {
        entry.source = Some(source);
    }
    entry.request_timeout_ms = extras.request_timeout_ms;
    entry.unknown_fields.extend(extras.kept_fields);
    if let Some(mut launch) = extras.launch {
        // A row named like a launch input would be stored under the input's
        // name and vanish from the entry.
        if let Some(row) = rows
            .iter()
            .find(|row| launch.inputs.iter().any(|input| input.key == row.key))
        {
            return Err(crate::i18n::trf(
                "Use each variable name once. Rename or remove the second {value0}.",
                &[("value0", &row.key)],
            ));
        }
        // Secret inputs still carry their values here; the import moves them
        // to the keychain, and the registry checks the stored form on save.
        launch.validate(&entry.args, false)?;
        // Every row still needs a value before the server is switched on, as
        // without launch inputs (the import lists them all then). A catalog
        // preset's own list stays: it leaves its optional keys out.
        if launch.template.is_none() {
            for row in &rows {
                if !launch.required_env.contains(&row.key) {
                    launch.required_env.push(row.key.clone());
                }
            }
        }
        entry.launch = Some(launch);
    }
    // A launch input read from a password manager has its value there.
    let referenced_inputs: Vec<String> = entry
        .launch
        .iter()
        .flat_map(|launch| &launch.inputs)
        .filter(|input| {
            matches!(
                crate::secret_refs::source(&input.unknown_fields),
                Ok(Some(_))
            )
        })
        .map(|input| input.key.clone())
        .collect();
    let remote = entry.transport != "stdio";
    entry.env = rows
        .iter()
        .map(|row| {
            let mut unknown_fields = serde_json::Map::new();
            if let Some(reference) = &row.reference {
                unknown_fields.insert("source".into(), serde_json::json!({"ref": reference}));
            } else if !row.secret {
                if let Some(portable) = row.portable {
                    unknown_fields.insert(
                        "portable".into(),
                        serde_json::json!(
                            portable && !crate::personal_sync::risky_sync_env(&row.key)
                        ),
                    );
                }
            }
            registry::EnvVar {
                key: row.key.clone(),
                value: None,
                secret: row.reference.is_some() || row.secret,
                unknown_fields,
            }
        })
        .collect();
    // The rows' values, for the import to sort into the vault and the entry.
    let definition = serde_json::json!({"env": rows
        .iter()
        .filter(|row| row.reference.is_none())
        .map(|row| {
            // A blank value is no value: the key is reported as needing one.
            if row.value.trim().is_empty() {
                return Ok((row.key.clone(), serde_json::Value::Null));
            }
            // A remote server's Authorization is a bearer token, written as
            // the import reads one.
            let value = if remote && row.key.eq_ignore_ascii_case("authorization") {
                match bearer_token(&row.value) {
                    Ok(token) => format!("Bearer {token}"),
                    Err(AuthValueError::NoToken) => {
                        return Err("This server needs a bearer token. Review its native authentication settings. Client config unchanged.".to_string());
                    }
                    Err(AuthValueError::OtherScheme) => {
                        return Err("This server uses unsupported HTTP authentication. Keep its native entry.".to_string());
                    }
                }
            } else {
                row.value.clone()
            };
            Ok((row.key.clone(), serde_json::Value::String(value)))
        })
        .collect::<Result<serde_json::Map<_, _>, String>>()?});
    let choices = rows
        .iter()
        .filter(|row| row.reference.is_none())
        .map(|row| (row.key.clone(), row.secret))
        .collect();
    let import = crate::import_credentials::Import::prepare_with_choices(
        entry,
        Some(&definition),
        Some(&choices),
    )?;
    let (registry, id, missing) = crate::import_credentials::transaction(|writes| {
        let current = read_registry_exact_or_default()?;
        let mut registry = current.clone();
        let mut entry = import.entry.clone();
        entry.enabled = false;
        let id = registry.add_server(entry);
        let mut missing =
            import.transfer_into(&id, writes, current.servers.iter().any(|s| s.id == id))?;
        missing.retain(|key| !referenced_inputs.contains(key));
        if missing.is_empty() {
            let profile = registry.default_access_id();
            if apply_server_enabled(&mut registry, &profile, &id, true, false).is_ok() {
                let _ = registry.set_access_server(&profile, &id, true);
            }
        }
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok((commit_imports(&current, registry)?, id, missing))
    })?;
    // Name each key as its row did, not as it is stored (the bearer slot), and a
    // launch input by its label.
    let inputs: &[registry::LaunchInput] = import
        .entry
        .launch
        .as_ref()
        .map_or(&[], |launch| &launch.inputs);
    let mut declared_without_value: Vec<String> = missing
        .into_iter()
        .map(|key| {
            if key == crate::secrets::HTTP_AUTH_KEY {
                rows.iter()
                    .find(|row| row.key.eq_ignore_ascii_case("authorization"))
                    .map_or(key, |row| row.key.clone())
            } else if let Some(input) = inputs.iter().find(|input| input.key == key) {
                input.label.clone()
            } else {
                key
            }
        })
        .collect();
    // A required plain launch input left blank also keeps the server off.
    if !registry.is_enabled(&registry.default_access_id(), &id) {
        for input in inputs {
            let blank = input.value.as_deref().is_none_or(|v| v.trim().is_empty());
            if input.required
                && !input.secret
                && blank
                && !referenced_inputs.contains(&input.key)
                && !declared_without_value.contains(&input.label)
            {
                declared_without_value.push(input.label.clone());
            }
        }
    }
    Ok(RowsAddOutcome {
        registry,
        id,
        declared_without_value,
    })
}

/// [`add_snippet_server`] with the user's Keychain choice per key; a key absent
/// from `keychain` keeps the import's own secret detection.
pub fn add_snippet_server_choices(
    fields: ServerFields,
    env: Vec<(String, Option<String>)>,
    keychain: &std::collections::BTreeMap<String, bool>,
) -> Result<SnippetAddOutcome, String> {
    let mut seen = std::collections::BTreeSet::new();
    if let Some((key, _)) = env.iter().find(|(key, _)| !seen.insert(key.as_str())) {
        // A JSON map below would keep only one of them and drop the other value.
        return Err(crate::i18n::trf(
            "Use each variable name once. Rename or remove the second {value0}.",
            &[("value0", key)],
        ));
    }
    let mut entry = entry_from_fields(fields)?;
    entry.env = env
        .iter()
        .map(|(key, _)| registry::EnvVar {
            key: key.clone(),
            value: None,
            secret: true,
            unknown_fields: Default::default(),
        })
        .collect();
    let definition = serde_json::json!({"env": env.into_iter().map(|(key, value)| (key, value.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))).collect::<serde_json::Map<_, _>>()});
    let import = crate::import_credentials::Import::prepare_with_choices(
        entry,
        Some(&definition),
        Some(keychain),
    )?;
    add_snippet_imports(vec![import], &Default::default())
}

/// Prepare every selected pasted definition before any registry or vault mutation.
pub fn add_snippet_servers(text: &str, selected: &[String]) -> Result<SnippetAddOutcome, String> {
    add_snippet_servers_choices(text, selected, &Default::default())
}
pub fn add_snippet_servers_choices(
    text: &str,
    selected: &[String],
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
) -> Result<SnippetAddOutcome, String> {
    add_snippet_servers_inputs(text, selected, choices, &Default::default())
}

pub fn add_snippet_servers_inputs(
    text: &str,
    selected: &[String],
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> Result<SnippetAddOutcome, String> {
    let parsed = clients::parse_snippet(text).map_err(|_| "Could not parse the pasted config")?;
    let mut imports = Vec::new();
    for (i, server) in parsed.into_iter().enumerate() {
        if !selected.contains(&i.to_string()) {
            continue;
        }
        let entry = entry_from_fields(ServerFields {
            sync_local_only: false,
            initialize_timeout_ms: None,
            name: server.name,
            transport: server.transport,
            command: server.command,
            args: server.args,
            url: server.url,
            cwd: None,
            inherit_env: None,
            hide_prompts: None,
        })?;
        let definition = serde_json::json!({"env": server.env.into_iter().map(|e| (e.key,e.value.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))).collect::<serde_json::Map<_,_>>()});
        let choice = choices.get(&entry.name);
        imports.push(crate::import_credentials::Import::prepare_with_choices(
            entry,
            Some(&definition),
            choice,
        )?);
    }
    add_snippet_imports(imports, inputs)
}

fn catalog_server(entry: crate::catalog::CatalogEntry) -> ServerEntry {
    ServerEntry {
        enabled: false,
        inherit_env: false,
        id: String::new(),
        name: entry.name,
        transport: entry.transport,
        command: entry.command,
        args: entry.args,
        env: entry
            .env_keys
            .into_iter()
            .map(|key| crate::registry::EnvVar {
                key,
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            })
            .collect(),
        url: entry.url,
        cwd: None,
        source: Some(format!("catalog:{}", entry.source)),
        disabled_tools: Vec::new(),
        hide_prompts: false,
        client_credentials: None,
        request_timeout_ms: entry.request_timeout_ms,
        initialize_timeout_ms: None,
        launch: entry.launch,
        unknown_fields: serde_json::Map::new(),
    }
}

pub fn add_catalog_entry(entry: crate::catalog::CatalogEntry) -> Result<Registry, String> {
    // Self-hosted entries carry a url_hint instead of a url, because the
    // endpoint is the user's own instance. Committing one here would write a
    // server with no way to reach anything, so both shells send these through
    // the prefilled server editor and this stays a backstop.
    if entry.url.is_none() && entry.command.is_none() {
        return Err(format!(
            "{} needs its own endpoint URL. Open it from the catalog to enter one.",
            entry.name
        ));
    }
    let server = catalog_server(entry);
    let (registry, _) = registry::update(|registry| Ok(apply_add_entry(registry, server)))?;
    Ok(registry)
}

pub fn update_server_fields(server_id: &str, fields: ServerFields) -> Result<Registry, String> {
    let (registry, ()) =
        registry::update(|registry| apply_update_server_fields(registry, server_id, fields))?;
    Ok(registry)
}

/// A plain (nonsecret) environment value as edited in a server editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainEnvEdit {
    pub key: String,
    pub value: String,
    /// Set only when the user chose. Otherwise personal sync's own default
    /// decides, so an untouched old value keeps its current behavior.
    pub local_only: Option<bool>,
}

/// Replaces the server's plain environment values with `edits`. Secret and
/// password manager entries are left alone; they are edited in Credentials.
/// Existing plain entries keep their newer fields when their key stays.
pub fn apply_plain_env(server: &mut ServerEntry, edits: Vec<PlainEnvEdit>) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut plain = Vec::new();
    for edit in edits {
        let key = edit.key.trim().to_string();
        if key.is_empty() && edit.value.trim().is_empty() {
            continue;
        }
        if key.is_empty() {
            return Err("give each environment value a name".into());
        }
        if key.contains(|c: char| c == '=' || c == '\0' || c.is_whitespace()) {
            return Err(format!("{key} is not a valid environment variable name"));
        }
        if !seen.insert(key.clone()) {
            return Err(format!("{key} is listed more than once"));
        }
        if server.env.iter().any(|e| e.key == key && is_vaulted(e)) {
            return Err(format!(
                "{key} is a stored credential. Change it in Credentials."
            ));
        }
        let mut entry = server
            .env
            .iter()
            .find(|e| e.key == key && !is_vaulted(e))
            .cloned()
            .unwrap_or(crate::registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: false,
                unknown_fields: serde_json::Map::new(),
            });
        entry.value = (!edit.value.is_empty()).then_some(edit.value);
        if let Some(local_only) = edit.local_only {
            if !local_only && crate::personal_sync::risky_sync_env(&key) {
                return Err(format!(
                    "{key} changes how programs run, so it stays on this machine."
                ));
            }
            entry
                .unknown_fields
                .insert("portable".into(), serde_json::json!(!local_only));
        }
        plain.push(entry);
    }
    // Keep existing entries where they were so an unchanged list stays equal.
    let mut env = Vec::with_capacity(server.env.len() + plain.len());
    for existing in std::mem::take(&mut server.env) {
        if is_vaulted(&existing) {
            env.push(existing);
        } else if let Some(at) = plain.iter().position(|e| e.key == existing.key) {
            env.push(plain.remove(at));
        }
    }
    env.extend(plain);
    server.env = env;
    Ok(())
}

fn is_vaulted(entry: &crate::registry::EnvVar) -> bool {
    entry.secret || entry.unknown_fields.contains_key("source")
}

/// Saves the editor's fields and, when given, its plain environment values in
/// one registry write.
pub fn update_server_fields_and_env(
    server_id: &str,
    fields: ServerFields,
    env: Option<Vec<PlainEnvEdit>>,
) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        apply_update_server_fields(registry, server_id, fields)?;
        if let Some(env) = env {
            let server = registry
                .servers
                .iter_mut()
                .find(|server| server.id == server_id)
                .ok_or_else(|| format!("No server with id '{server_id}'"))?;
            apply_plain_env(server, env)?;
        }
        Ok(())
    })?;
    Ok(registry)
}

pub fn server_entry_for_probe(
    server_id: Option<&str>,
    fields: ServerFields,
) -> Result<ServerEntry, String> {
    match server_id {
        Some(server_id) => {
            let mut registry = read_registry_exact()?;
            apply_update_server_fields(&mut registry, server_id, fields)?;
            registry
                .servers
                .into_iter()
                .find(|server| server.id == server_id)
                .ok_or_else(|| format!("No server with id '{server_id}'"))
        }
        None => {
            let fields = fields.normalized()?;
            Ok(ServerEntry {
                enabled: false,
                inherit_env: fields.inherit_env.unwrap_or(false),
                id: "native-connection-test".into(),
                name: fields.name,
                transport: fields.transport,
                command: fields.command,
                args: fields.args,
                env: Vec::new(),
                url: fields.url,
                cwd: fields.cwd,
                source: Some("manual".into()),
                disabled_tools: Vec::new(),
                hide_prompts: false,
                client_credentials: None,
                request_timeout_ms: None,
                initialize_timeout_ms: fields.initialize_timeout_ms.flatten(),
                launch: None,
                unknown_fields: serde_json::Map::new(),
            })
        }
    }
}

pub fn remove_server(server_id: &str) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| apply_remove_server(registry, server_id))?;
    Ok(registry)
}

pub fn apply_create_profile(registry: &mut Registry, name: &str) -> Result<(), String> {
    registry::validate_access_set_name(name)?;
    registry.add_profile(name.trim());
    Ok(())
}

pub fn apply_delete_profile(registry: &mut Registry, profile_id: &str) -> Result<(), String> {
    registry.remove_profile(profile_id)
}

pub fn create_profile(name: &str) -> Result<Registry, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the profile a name".into());
    }
    let (registry, ()) = registry::update(|registry| apply_create_profile(registry, name))?;
    Ok(registry)
}

pub fn delete_profile(profile_id: &str) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| apply_delete_profile(registry, profile_id))?;
    cleanup_deleted_profile_reviews(profile_id, &registry);
    Ok(registry)
}

/// Both desktop shells call this after committing deletion. Cleanup failures
/// cannot undo the saved registry or turn a successful deletion into an error.
pub fn cleanup_deleted_profile_reviews(profile_id: &str, registry: &Registry) {
    if let Err(error) = crate::integrity::reconcile_definition_reviews(
        Some(profile_id),
        registry,
        &Default::default(),
    ) {
        eprintln!("toolport: profile deleted, but pending review cleanup failed: {error}");
    }
}

pub fn set_default_access(profile: Option<&str>) -> Result<Registry, String> {
    registry::update(|r| r.set_default_access(profile)).map(|(r, ())| r)
}

pub fn set_access_server(
    profile_id: &str,
    server_id: &str,
    included: bool,
) -> Result<Registry, String> {
    registry::update(|r| r.set_access_server(profile_id, server_id, included)).map(|(r, ())| r)
}

pub fn set_all_enabled(profile_id: &str, enabled: bool) -> Result<Registry, String> {
    let (registry, ()) =
        registry::update(|registry| registry.set_all_enabled(profile_id, enabled))?;
    Ok(registry)
}

pub fn set_profile_server_tools(
    profile_id: &str,
    server_id: &str,
    tools: Option<Vec<String>>,
) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        registry.set_profile_server_tools(profile_id, server_id, tools)
    })?;
    Ok(registry)
}

pub fn set_client_discovery(client_id: &str, mode: Option<&str>) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        registry.set_client_discovery(client_id, mode);
        Ok(())
    })?;
    Ok(registry)
}

fn client_gateway_state(
    managed: &HashMap<String, ManagedEntry>,
    client_id: &str,
) -> Option<GatewayEntryState> {
    let mut detected: Vec<_> = clients::detect_client(client_id).into_iter().collect();
    clients::apply_entry_states(&mut detected, managed);
    detected.pop().map(|client| client.entry_state)
}

fn refuse_customized_client(state: Option<GatewayEntryState>, force: bool) -> Result<(), String> {
    if !force && state == Some(GatewayEntryState::Customized) {
        return Err(
            "This client's Toolport entry has a custom configuration. Confirm the reset to replace it with the default gateway."
                .into(),
        );
    }
    Ok(())
}

fn finish_client_config_mutation(
    mut outcome: WriteOutcome,
    write_registry: impl FnOnce(Option<ManagedEntry>) -> Result<Registry, String>,
) -> Result<ClientMutationResult, String> {
    let receipt = ClientConfigReceipt::capture(&outcome)?;
    if !receipt.exact_rollback {
        outcome.warnings.push(
            "the client saved after this operation; exact rollback is unavailable for this write"
                .into(),
        );
        if let Some(file) = &outcome.recovery_path {
            if let Err(error) = clients::record_config_capture_conflict(
                file,
                &receipt.target,
                outcome.revision.as_deref(),
            ) {
                outcome.warnings.push(format!(
                    "could not record unavailable exact rollback: {error}"
                ));
            }
        }
    }
    match write_registry(outcome.managed.clone()) {
        Ok(registry) => Ok(ClientMutationResult { registry, outcome }),
        Err(registry_error) => match receipt.rollback() {
            Ok(()) => Err(format!(
                "could not update the registry, so the client configuration was rolled back: {registry_error}"
            )),
            Err(rollback_error) => Err(format!(
                "the client configuration changed, but the registry update and client rollback both failed: {registry_error}; rollback: {rollback_error}"
            )),
        },
    }
}

pub(crate) fn apply_client_stdio_update(
    registry: &mut Registry,
    client_id: &str,
    profile: Option<&str>,
    managed_entry: Option<ManagedEntry>,
) -> bool {
    match profile.map(str::trim).filter(|profile| !profile.is_empty()) {
        Some(profile) => registry.set_client_scope(client_id, Some(profile)),
        None => registry.set_client_unscoped(client_id),
    }
    if let Some(managed_entry) = managed_entry {
        registry.set_client_managed_entry(client_id, managed_entry);
    }
    let http_id = format!("client:{client_id}");
    let before = registry.http_clients.len();
    registry.http_clients.retain(|row| row.id != http_id);
    registry.http_clients.len() != before
}

fn finish_client_stdio_mutation(
    client_id: &str,
    outcome: WriteOutcome,
    write_registry: impl FnOnce(Option<ManagedEntry>) -> Result<(Registry, bool), String>,
) -> Result<ClientMutationResult, String> {
    let mut revoked = false;
    let mut result = finish_client_config_mutation(outcome, |managed_entry| {
        let (registry, removed_http_row) = write_registry(managed_entry)?;
        revoked = removed_http_row;
        Ok(registry)
    })?;
    // The config and the registry are both written by now, so this cleanup can't fail the
    // change: an error here would report a connect as failed after it took effect. The
    // token's row is gone, so a token left behind (say, with the keyring locked) is unused.
    // Reported as a warning, as Disconnect reports the same cleanup.
    if revoked {
        if let Err(error) = crate::secrets::delete_secret(CLIENT_HTTP_VAULT_SERVER, client_id) {
            result.outcome.warnings.push(format!(
                "connected; could not remove the old keychain token: {error}"
            ));
        }
    }
    Ok(result)
}

pub fn connect_client_stdio_with(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    managed: &HashMap<String, ManagedEntry>,
    write_registry: impl FnOnce(Option<ManagedEntry>) -> Result<(Registry, bool), String>,
) -> Result<ClientMutationResult, String> {
    refuse_customized_client(client_gateway_state(managed, client_id), force)?;
    let _lock = acquire_auth_lock(&format!("client-config:{client_id}"))?;
    // Only an explicit reset (force) writes every Codex default again; an access
    // change keeps the settings the user removed.
    let keep = (!force).then(|| {
        managed
            .get(client_id)
            .map_or(&[][..], |record| record.codex_settings.as_slice())
    });
    let outcome = clients::install_gateway_for(client_id, profile, keep)?;
    finish_client_stdio_mutation(client_id, outcome, write_registry)
}

pub fn disconnect_client_stdio_with(
    client_id: &str,
    has_shared_http_token: bool,
    write_registry: impl FnOnce(Option<ManagedEntry>) -> Result<Registry, String>,
) -> Result<ClientMutationResult, String> {
    if has_shared_http_token {
        return Err(
            "This client uses Toolport Shared HTTP. Disconnect it in the current app until native bearer revocation is available."
                .into(),
        );
    }
    let _lock = acquire_auth_lock(&format!("client-config:{client_id}"))?;
    let outcome = clients::uninstall_gateway(client_id)?;
    let mut result = finish_client_config_mutation(outcome, write_registry)?;
    let warnings = clients::finish_uninstall_with_warnings(client_id, &result.outcome)?;
    result.outcome.warnings.extend(warnings);
    Ok(result)
}

fn read_registry_exact() -> Result<Registry, String> {
    let path = registry::resolved_path().ok_or("could not resolve the registry path")?;
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read the registry: {error}"))?;
    registry::parse_registry_contents(&contents)
}

fn read_registry_exact_or_default() -> Result<Registry, String> {
    let path = registry::resolved_path().ok_or("could not resolve the registry path")?;
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(error) => return Err(format!("could not read the registry: {error}")),
    };
    registry::parse_registry_contents(&contents)
}

pub fn essential_settings() -> Result<EssentialSettings, String> {
    Ok(EssentialSettings::from_registry(
        &read_registry_exact_or_default()?,
    ))
}

pub fn folder_routing_settings() -> Result<FolderRoutingSettings, String> {
    let registry = read_registry_exact_or_default()?;
    Ok(FolderRoutingSettings {
        profiles: registry
            .profiles
            .into_iter()
            .map(|profile| (profile.id, profile.name))
            .collect(),
        mappings: registry.folder_profiles,
    })
}

pub fn upsert_folder_profile(path: &str, profile: &str) -> Result<FolderRoutingSettings, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("choose a project folder".into());
    }
    let profile = profile.trim();
    if profile.is_empty() {
        return Err("choose a profile".into());
    }
    registry::update(|registry| {
        if !registry
            .profiles
            .iter()
            .any(|candidate| candidate.id == profile || candidate.name == profile)
        {
            return Err("the selected profile no longer exists".into());
        }
        let mut mappings = registry.folder_profiles.clone();
        mappings.retain(|mapping| mapping.path.trim() != path);
        mappings.push(crate::registry::FolderProfile {
            path: path.to_string(),
            profile: profile.to_string(),
            unknown_fields: Default::default(),
        });
        registry.set_folder_profiles(mappings);
        Ok(())
    })?;
    folder_routing_settings()
}

pub fn remove_folder_profile(path: &str) -> Result<FolderRoutingSettings, String> {
    let path = path.to_string();
    registry::update(|registry| {
        let mappings = registry
            .folder_profiles
            .iter()
            .filter(|mapping| mapping.path != path)
            .cloned()
            .collect();
        registry.set_folder_profiles(mappings);
        Ok(())
    })?;
    folder_routing_settings()
}

pub fn http_client_settings() -> Result<HttpClientSettings, String> {
    let registry = read_registry_exact_or_default()?;
    Ok(HttpClientSettings {
        clients: registry.http_clients,
        profiles: registry
            .profiles
            .into_iter()
            .map(|profile| (profile.id, profile.name))
            .collect(),
    })
}

pub fn add_http_client(label: &str, profile: Option<&str>) -> Result<AddedHttpClient, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("give the HTTP client a name".into());
    }
    let token = random_token()?;
    let id = random_token()?;
    let profile = profile.unwrap_or_default().trim().to_string();
    registry::update(|registry| {
        apply_add_http_client(
            registry,
            id,
            label.to_string(),
            registry::sha256_hex(&token),
            profile,
        )
    })?;
    Ok(AddedHttpClient {
        settings: http_client_settings()?,
        token,
    })
}

pub fn remove_http_client(id: &str) -> Result<HttpClientSettings, String> {
    let id = id.to_string();
    registry::update(|registry| apply_remove_http_client(registry, &id))?;
    http_client_settings()
}

fn apply_add_http_client(
    registry: &mut Registry,
    id: String,
    label: String,
    token_sha256: String,
    profile: String,
) -> Result<(), String> {
    if !profile.is_empty()
        && profile != registry::ALL_ENABLED_ACCESS
        && !registry
            .profiles
            .iter()
            .any(|candidate| candidate.id == profile || candidate.name == profile)
    {
        return Err("the selected profile no longer exists".into());
    }
    registry.http_clients.push(registry::HttpClient {
        id,
        label,
        token_sha256,
        profile,
        unknown_fields: Default::default(),
    });
    Ok(())
}

fn apply_remove_http_client(registry: &mut Registry, id: &str) -> Result<(), String> {
    if id.starts_with("client:") {
        return Err("disconnect this managed client from the Clients page".into());
    }
    registry.http_clients.retain(|client| client.id != id);
    Ok(())
}

pub fn set_essential_setting(
    setting: EssentialSetting,
    enabled: bool,
) -> Result<EssentialSettings, String> {
    let (registry, ()) = registry::update(|registry| {
        match setting {
            EssentialSetting::LazyDiscovery => registry.set_lazy_discovery(enabled),
            EssentialSetting::CodeMode => registry.code_mode = enabled,
            EssentialSetting::LiveInspect => registry.set_live_inspect(enabled),
            EssentialSetting::PiiRedaction => registry.pii_redaction = enabled,
        }
        Ok(())
    })?;
    if setting == EssentialSetting::LiveInspect && !enabled {
        crate::inspect::try_clear()
            .map_err(|error| format!("could not clear the live inspection buffer: {error}"))?;
    }
    Ok(EssentialSettings::from_registry(&registry))
}

pub fn release_quarantine(profile: Option<&str>, tool: &str) -> Result<(), String> {
    if !crate::integrity::release(profile, tool)
        .map_err(|error| format!("Could not re-approve {tool}: {error}"))?
    {
        let still_blocked = crate::integrity::quarantined(profile)
            .map_err(|error| format!("Could not verify re-approval for {tool}: {error}"))?;
        if still_blocked.contains(tool) {
            return Err(format!(
                "Could not re-approve {tool}; its quarantine record or integrity pin could not be updated"
            ));
        }
    }
    Ok(())
}

/// The combined result of re-approving every quarantined tool across profile scopes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseAllSummary {
    pub released: usize,
    /// Tools whose captured definition could not be read; they stay blocked.
    pub skipped: Vec<String>,
    /// Records replaced or added since the displayed list was loaded.
    pub changed: Vec<String>,
    /// Profile scopes whose store could not be updated at all.
    pub failed: Vec<String>,
}

/// Re-approve every quarantined tool in one pass per profile scope.
///
/// A lost integrity baseline blocks the whole catalog, and clearing that one tool
/// at a time does not finish on a real install. One scope failing must not throw
/// away the outcome of the scopes that succeeded, so errors are collected instead
/// of returned early. An empty profile string means the global scope.
pub fn release_all_quarantine(profiles: &[String]) -> ReleaseAllSummary {
    release_all_quarantine_inner(profiles, None)
}

pub fn release_all_quarantine_records(
    profiles: &[String],
    expected: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> ReleaseAllSummary {
    release_all_quarantine_inner(profiles, Some(expected))
}

fn release_all_quarantine_inner(
    profiles: &[String],
    expected: Option<
        &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    >,
) -> ReleaseAllSummary {
    let mut summary = ReleaseAllSummary::default();
    let empty = std::collections::BTreeMap::new();
    let mut scopes = profiles.to_vec();
    if expected.is_some() {
        // New scopes must stay blocked too, and appear in the outcome even if
        // they were absent from the displayed list. They receive an empty bound set.
        match crate::integrity::all_quarantined() {
            Ok(records) => {
                for record in records {
                    let profile = record["profile"].as_str().unwrap_or_default().to_string();
                    if !scopes.contains(&profile) {
                        scopes.push(profile);
                    }
                }
            }
            Err(error) => summary.failed.push(error),
        }
    }
    for profile in &scopes {
        let scope = (!profile.is_empty()).then_some(profile.as_str());
        let result = match expected {
            Some(expected) => crate::integrity::release_all_records(
                scope,
                expected.get(profile).unwrap_or(&empty),
            ),
            None => crate::integrity::release_all(scope),
        };
        match result {
            Ok(outcome) => {
                summary.released += outcome.released;
                summary.skipped.extend(outcome.skipped);
                summary.changed.extend(outcome.changed);
            }
            Err(error) => summary.failed.push(error),
        }
    }
    summary
}

pub fn set_tool_enabled(server_id: &str, tool: &str, enabled: bool) -> Result<Registry, String> {
    let (registry, ()) =
        registry::update(|registry| registry.set_tool_enabled(server_id, tool, enabled))?;
    Ok(registry)
}

pub fn set_tool_pinned(server_id: &str, tool: &str, pinned: bool) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        registry.set_tool_pinned(server_id, tool, pinned);
        Ok(())
    })?;
    Ok(registry)
}

pub fn pinned_prerequisites() -> Result<Vec<PinnedPrerequisite>, String> {
    let registry = registry::load()?;
    Ok(pinned_prerequisites_from(&registry))
}

fn pinned_prerequisites_from(registry: &Registry) -> Vec<PinnedPrerequisite> {
    let names = registry
        .servers
        .iter()
        .map(|server| (server.id.as_str(), server.name.as_str()))
        .collect::<HashMap<_, _>>();
    let mut pins = registry
        .pinned_tools
        .iter()
        .flat_map(|(server_id, tools)| {
            let server = names.get(server_id.as_str()).copied().unwrap_or(server_id);
            tools.iter().map(move |tool| PinnedPrerequisite {
                server_id: server_id.clone(),
                server: server.to_string(),
                tool: tool.clone(),
            })
        })
        .collect::<Vec<_>>();
    pins.sort_by(|left, right| {
        left.server
            .to_lowercase()
            .cmp(&right.server.to_lowercase())
            .then(left.tool.to_lowercase().cmp(&right.tool.to_lowercase()))
    });
    pins
}

pub fn set_tool_override(
    server_id: &str,
    tool: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<Registry, String> {
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let (registry, ()) = registry::update(|registry| {
        registry.set_tool_override(
            server_id.to_string(),
            tool.to_string(),
            crate::registry::ToolOverride {
                name: clean(name),
                description: clean(description),
                unknown_fields: Default::default(),
            },
        );
        Ok(())
    })?;
    Ok(registry)
}

/// Remove a tool's exposure override so clients see the server's original name
/// and description again.
pub fn clear_tool_override(server_id: &str, tool: &str) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        registry.clear_tool_override(server_id, tool);
        Ok(())
    })?;
    Ok(registry)
}

/// What a one-shot client migration accomplished.
#[derive(Debug)]
pub struct MigrateOutcome {
    pub result: ClientMutationResult,
    /// How many of the client's servers were newly imported into Toolport.
    pub imported: usize,
    /// Names of the servers moved out of the client's config.
    pub moved: Vec<String>,
    pub tools: Vec<serde_json::Value>,
    pub servers: Vec<SetupServerResult>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupServerResult {
    pub name: String,
    pub tool_count: usize,
    pub credential_state: String,
}

#[derive(Debug, Default)]
struct SetupVerification {
    tools: Vec<serde_json::Value>,
    servers: Vec<SetupServerResult>,
}

impl From<Vec<serde_json::Value>> for SetupVerification {
    fn from(tools: Vec<serde_json::Value>) -> Self {
        Self {
            tools,
            servers: Vec::new(),
        }
    }
}

/// Import the servers a client directly manages before its config is replaced
/// with the Toolport gateway. Gateway identities must be skipped before they
/// reach `moved`, otherwise migration reports moving a server it never imported.
fn prepare_client_servers_for_migration(
    registry: &mut Registry,
    client: &clients::DetectedClient,
    writes: &mut crate::import_credentials::VaultWrites,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> Result<(usize, Vec<String>), String> {
    let names = client
        .servers
        .iter()
        .filter(|server| !clients::detected_is_gateway(server))
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    clients::validate_client_import(client, &names, true)?;
    let mut imported = 0;
    let mut moved = Vec::new();
    for server in &client.servers {
        if clients::detected_is_gateway(server) {
            continue;
        }
        moved.push(server.name.clone());
        let definition = clients::import_definition(client, &server.name)?;
        let mut import = crate::import_credentials::Import::prepare_with_choices(
            server_from_detected(server, &client.id),
            definition.as_ref(),
            choices.get(&server.name),
        )?;
        if let Some(values) = inputs.get(&server.name) {
            import.supply(values)?;
        }
        let was_registered = registry
            .servers
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(&server.name));
        let id = if let Some(existing) = registry
            .servers
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&server.name))
        {
            if !import.matches(existing)? {
                return Err(format!("{} already exists with a different definition. Resolve it under Servers before connecting. Client config unchanged.", server.name));
            }
            let id = existing.id.clone();
            let entry = registry.servers.iter_mut().find(|e| e.id == id).unwrap();
            entry.command = import.entry.command.clone();
            entry.args = import.entry.args.clone();
            entry.url = import.entry.url.clone();
            entry.env = import.entry.env.clone();
            entry.launch = import.entry.launch.clone();
            if let Some(key) = import.entry.unknown_fields.get("importedUrlKey") {
                entry
                    .unknown_fields
                    .insert("importedUrlKey".into(), key.clone());
            }
            id
        } else {
            imported += 1;
            registry.add_server(import.entry.clone())
        };
        let missing = import.transfer_into(&id, writes, was_registered)?;
        if !missing.is_empty() {
            return Err(format!("{} needs credentials. Enter the missing values in review or its native config, then review again. Client config unchanged.", server.name));
        }
    }
    if !moved.is_empty() {
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
    }
    Ok((imported, moved))
}

#[cfg(test)]
pub(crate) fn import_client_servers_for_migration(
    registry: &mut Registry,
    client: &clients::DetectedClient,
) -> Result<(usize, Vec<String>), String> {
    crate::import_credentials::transaction(|writes| {
        prepare_client_servers_for_migration(
            registry,
            client,
            writes,
            &Default::default(),
            &Default::default(),
        )
    })
}

/// Enable each moved server (matched by name, as the import above does) in the
/// profile the migrated client will use: the named scope, else the active profile.
fn enable_moved_servers(
    registry: &mut Registry,
    profile: Option<&str>,
    moved: &[String],
) -> Result<(), String> {
    let profile_id = registry.resolve_profile_id(profile.unwrap_or(""));
    let access_set = registry.access_profile(&profile_id).map(|p| p.id.clone());
    let all = profile_id == registry.all_access_id()
        || (profile_id == registry.default_access_id()
            && registry.default_access_profile_id.is_none());
    if access_set.is_none() && !all {
        return Err(
            "The access set no longer exists, so the client's config was left unchanged".into(),
        );
    }
    for name in moved {
        let Some(id) = registry
            .servers
            .iter()
            .find(|server| server.name.eq_ignore_ascii_case(name))
            .map(|server| server.id.clone())
        else {
            continue;
        };
        apply_server_enabled(registry, &profile_id, &id, true, false).map_err(|error| {
            format!("Could not turn on {name} in Toolport, so the client's config was left unchanged: {error}")
        })?;
        if let Some(profile) = &access_set {
            registry.set_access_server(profile, &id, true)?;
        }
    }
    Ok(())
}

/// Add an imported server and turn it on in the active profile, so an import
/// serves tools right away (UX-01). A server that cannot be enabled yet (a team
/// server awaiting review, an unresolved launch input) is still imported, off.
pub(crate) fn apply_import_entry(registry: &mut Registry, entry: ServerEntry) -> String {
    let id = registry.add_server(entry);
    let profile_id = registry
        .default_access_profile_id
        .clone()
        .unwrap_or_else(|| registry.active_profile_id());
    let ready = registry
        .servers
        .iter()
        .find(|s| s.id == id)
        .is_some_and(|s| s.env.is_empty() && (s.command.is_some() || s.url.is_some()));
    if ready && apply_server_enabled(registry, &profile_id, &id, true, false).is_ok() {
        let _ = registry.set_access_server(&profile_id, &id, true);
    }
    id
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialReview {
    pub key: String,
    pub secret: bool,
    pub present: bool,
    pub required: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupItem {
    pub key: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env_keys: Vec<String>,
    pub is_new: bool,
    pub credentials: Vec<CredentialReview>,
    pub unsupported: Option<String>,
    pub updates: Vec<String>,
    /// The client's own name when `name`, the name an import saves, differs from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetupReview {
    pub config_path: String,
    pub backup_dir: String,
    pub revision: String,
    pub items: Vec<SetupItem>,
    /// Client settings the connect adds to Toolport's own entry, as TOML lines, each
    /// only when it is not set. Empty for clients that get none.
    pub gateway_settings: Vec<String>,
    /// Reading this review asked to unlock the keyring and it is still locked: the ask
    /// was dismissed. A Connect all step then connects quietly, as later steps do.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keyring_asked: bool,
}

pub fn preview_client_setup(client_id: &str) -> Result<ClientSetupReview, String> {
    let revision = clients::setup_revision(client_id)?;
    let client = clients::detect_client(client_id).ok_or("Unknown client")?;
    if client.error.is_some() {
        return Err("Could not read this client config. Fix it before connecting.".into());
    }
    let registry = read_registry_exact_or_default()?;
    let items = client
        .servers
        .iter()
        .filter(|s| !clients::detected_is_gateway(s))
        .map(|s| {
            let definition = clients::import_definition(&client, &s.name)?;
            let prepared = crate::import_credentials::Import::prepare(
                server_from_detected(s, &client.id),
                definition.as_ref(),
            );
            let updates = prepared
                .as_ref()
                .ok()
                .and_then(|import| {
                    registry
                        .servers
                        .iter()
                        .find(|existing| existing.name.eq_ignore_ascii_case(&s.name))
                        .map(|existing| import.updates(existing))
                })
                .unwrap_or_default();
            let (credentials, unsupported) = match prepared {
                Ok(import) => (
                    import
                        .review_credentials_for(
                            registry
                                .servers
                                .iter()
                                .find(|existing| existing.name.eq_ignore_ascii_case(&s.name)),
                        )?
                        .into_iter()
                        .map(|(key, secret, present, required)| CredentialReview {
                            key,
                            secret,
                            present,
                            required,
                        })
                        .collect(),
                    None,
                ),
                Err(error) => (Vec::new(), Some(error)),
            };
            Ok(SetupItem {
                source_name: None,
                updates,
                credentials,
                unsupported,
                key: s.name.clone(),
                name: s.name.clone(),
                transport: s.transport.clone(),
                command: s.command.as_ref().map(|c| {
                    if registry::arg_looks_secret(c) {
                        "<command>".into()
                    } else {
                        c.clone()
                    }
                }),
                args: crate::import_credentials::shown_args(&s.args),
                url: s.url.as_deref().map(crate::import_credentials::shown_url),
                env_keys: s.env_keys.clone(),
                is_new: !registry
                    .servers
                    .iter()
                    .any(|e| e.name.eq_ignore_ascii_case(&s.name)),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if clients::setup_revision(client_id)? != revision {
        return Err("Client config changed. Review it again.".into());
    }
    Ok(ClientSetupReview {
        config_path: client.config_path,
        revision,
        items,
        gateway_settings: clients::codex_gateway_settings_for_connect(client_id),
        keyring_asked: false,
        backup_dir: clients::backup_dir(client_id)
            .ok_or("Could not resolve backup directory")?
            .display()
            .to_string(),
    })
}

pub fn migrate_client_reviewed(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
) -> Result<MigrateOutcome, String> {
    migrate_client_reviewed_with(
        client_id,
        profile,
        force,
        names,
        revision,
        verify_setup_gateway,
    )
}

// Undo only fields written by setup. A concurrent edit wins over its staged
// value; unrelated server/profile additions do not prevent rollback.
fn undo_staged_value(
    latest: &mut serde_json::Value,
    previous: &serde_json::Value,
    staged: &serde_json::Value,
) -> bool {
    use serde_json::Value;
    let same_row = |left: &Value, right: &Value| {
        for field in ["id", "key"] {
            let left_key = left.get(field).and_then(Value::as_str);
            let right_key = right.get(field).and_then(Value::as_str);
            if left_key.is_some() || right_key.is_some() {
                return left_key == right_key;
            }
        }
        left == right
    };
    if previous == staged {
        return true;
    }
    if latest == staged {
        *latest = previous.clone();
        return true;
    }
    match (latest, previous, staged) {
        (Value::Object(latest), Value::Object(previous), Value::Object(staged)) => {
            let mut complete = true;
            for key in previous
                .keys()
                .chain(staged.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let old = previous.get(key).unwrap_or(&Value::Null);
                let written = staged.get(key).unwrap_or(&Value::Null);
                if old == written {
                    continue;
                }
                let current = latest.entry(key.clone()).or_insert(Value::Null);
                complete &= undo_staged_value(current, old, written);
                if current.is_null() && !previous.contains_key(key) {
                    latest.remove(key);
                }
            }
            complete
        }
        (Value::Array(latest), Value::Array(previous), Value::Array(staged)) => {
            if previous
                .iter()
                .chain(staged)
                .chain(latest.iter())
                .any(|row| {
                    row.get("id").and_then(Value::as_str).is_none()
                        && row.get("key").and_then(Value::as_str).is_none()
                })
            {
                return latest == previous;
            }
            let mut complete = true;
            for written in staged {
                let old = previous.iter().find(|v| same_row(v, written));
                let current = latest.iter().position(|v| same_row(v, written));
                match (old, current) {
                    (Some(old), Some(index)) => {
                        complete &= undo_staged_value(&mut latest[index], old, written)
                    }
                    (None, Some(index)) if latest[index] == *written => {
                        latest.remove(index);
                    }
                    (None, Some(_)) => complete = false,
                    (Some(old), None) if old != written => complete = false,
                    _ => {}
                }
            }
            for old in previous
                .iter()
                .filter(|old| !staged.iter().any(|v| same_row(v, old)))
            {
                if let Some(current) = latest.iter().find(|v| same_row(v, old)) {
                    complete &= current == old;
                } else {
                    latest.push(old.clone());
                }
            }
            complete
        }
        (latest, _, _) => latest == previous,
    }
}

fn rollback_imports(
    previous: &Registry,
    staged: &Registry,
) -> Result<(bool, std::collections::BTreeSet<String>), String> {
    let (restored, complete) = registry::update(|latest| {
        let mut value = serde_json::to_value(&*latest).map_err(|e| e.to_string())?;
        let complete = undo_staged_value(
            &mut value,
            &serde_json::to_value(previous).map_err(|e| e.to_string())?,
            &serde_json::to_value(staged).map_err(|e| e.to_string())?,
        );
        *latest = serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok(complete)
    })?;
    let mut retained = std::collections::BTreeSet::new();
    for written in &staged.servers {
        if let Some(current) = restored.servers.iter().find(|row| row.id == written.id) {
            let old = previous.servers.iter().find(|row| row.id == written.id);
            let mut current = serde_json::to_value(current).map_err(|e| e.to_string())?;
            if !undo_staged_value(
                &mut current,
                &serde_json::to_value(old).map_err(|e| e.to_string())?,
                &serde_json::to_value(written).map_err(|e| e.to_string())?,
            ) {
                retained.insert(written.id.clone());
            }
        }
    }
    Ok((complete, retained))
}

fn migrate_client_reviewed_with(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
    verify: impl FnMut(&Registry, &[String], &str, Option<&str>) -> Result<SetupVerification, String>,
) -> Result<MigrateOutcome, String> {
    migrate_client_reviewed_choices_with(
        client_id,
        profile,
        force,
        names,
        revision,
        &Default::default(),
        verify,
    )
}

pub fn migrate_client_reviewed_choices(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
) -> Result<MigrateOutcome, String> {
    migrate_client_reviewed_choices_with(
        client_id,
        profile,
        force,
        names,
        revision,
        choices,
        verify_setup_gateway,
    )
}

/// What a reviewed connect's rollback says when it could not undo everything.
const ROLLBACK_KEPT_EDITS: &str = "Registry changed during setup. Concurrent edits were kept. Review Servers before retrying. Credentials for retained servers were kept.";
const ROLLBACK_KEPT_CREDENTIALS: &str = "Imported credential values were kept.";

/// Connect all's keyring rule, the one #1378 set for Disconnect: a step the user confirms
/// may ask to unlock the keyring, but a run of steps must not ask again and again. Once a
/// step has failed with the keyring still locked, the run's later steps are `quiet`: no
/// keyring call in them can ask, and one that needed the keyring fails at once and says to
/// unlock it and choose Retry. A Retry the user chooses is not quiet, so it may ask once.
/// Bulk paths nobody confirms step by step (Remove from all clients, purge) never ask.
pub fn connect_step<T>(quiet: bool, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if !quiet {
        return f();
    }
    clients::backup_vault::noninteractive(f).map_err(|error| {
        // Only a failure the locked keyring caused: a locked error, or a vault write that
        // failed while the store is locked. Any other error is shown as it is.
        let locked_now =
            || clients::backup_vault::store_locked() || crate::secrets::write_would_ask();
        if clients::backup_vault::is_locked_error(&error)
            || (crate::import_credentials::is_vault_failure(&error) && locked_now())
        {
            let unlock = crate::i18n::tr("Unlock your keyring, then choose Retry.");
            // A rollback that kept something says so; that stays with the instruction.
            if [ROLLBACK_KEPT_EDITS, ROLLBACK_KEPT_CREDENTIALS]
                .iter()
                .any(|note| error.contains(note))
            {
                format!("{unlock}\n\n{error}")
            } else {
                unlock
            }
        } else {
            error
        }
    })
}

/// A connect step's review, read under [`connect_step`]'s rule. A review that may ask asks
/// at most once: after one refused unlock, its other reads do not ask. A refused ask is
/// reported in `keyring_asked`: that ask was the run's one, so the step's Connect is quiet
/// and a Retry the user chooses may ask again. A dismissed ask does not come back in the
/// same step.
pub fn preview_step(client_id: &str, quiet: bool) -> Result<ClientSetupReview, String> {
    review_step(quiet, || preview_client_setup(client_id))
}

fn review_step(
    quiet: bool,
    read: impl FnOnce() -> Result<ClientSetupReview, String>,
) -> Result<ClientSetupReview, String> {
    connect_step(quiet, || {
        let (review, refused) = crate::secrets::asking_once(read);
        let mut review = review?;
        review.keyring_asked = !quiet && refused && keyring_locked_after("");
        Ok(review)
    })
}

/// Whether a failed connect step left the keyring locked, so the rest of its run is quiet.
/// Never asks to unlock.
pub fn keyring_locked_after(error: &str) -> bool {
    clients::backup_vault::is_locked_error(error)
        || clients::backup_vault::store_locked()
        || crate::secrets::write_would_ask()
}

pub fn migrate_client_reviewed_inputs(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
) -> Result<MigrateOutcome, String> {
    migrate_client_reviewed_inputs_with(
        client_id,
        profile,
        force,
        names,
        revision,
        choices,
        inputs,
        verify_setup_gateway,
    )
}

fn migrate_client_reviewed_choices_with(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    verify: impl FnMut(&Registry, &[String], &str, Option<&str>) -> Result<SetupVerification, String>,
) -> Result<MigrateOutcome, String> {
    migrate_client_reviewed_inputs_with(
        client_id,
        profile,
        force,
        names,
        revision,
        choices,
        &Default::default(),
        verify,
    )
}

// Preserve the complete reviewed setup request and verification callback together.
#[allow(clippy::too_many_arguments)]
fn migrate_client_reviewed_inputs_with(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
    choices: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    inputs: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    mut verify: impl FnMut(
        &Registry,
        &[String],
        &str,
        Option<&str>,
    ) -> Result<SetupVerification, String>,
) -> Result<MigrateOutcome, String> {
    let current = read_registry_exact_or_default()?;
    refuse_customized_client(
        client_gateway_state(&current.client_managed_entries, client_id),
        force,
    )?;
    let mut client = clients::detect_client(client_id).ok_or("Unknown client")?;
    if names.iter().any(|name| {
        !client
            .servers
            .iter()
            .any(|s| &s.name == name && !clients::detected_is_gateway(s))
    }) {
        return Err("Reviewed server no longer exists. Review the client config again.".into());
    }
    client.servers.retain(|s| names.contains(&s.name));
    let mut unsupported = Vec::new();
    let definitions_client = client.clone();
    client.servers.retain(|server| {
        let result = clients::import_definition(&definitions_client, &server.name);
        // Definitions are already read in review; errors are reported by preparation.
        match result.and_then(|definition| {
            crate::import_credentials::Import::prepare(
                server_from_detected(server, client_id),
                definition.as_ref(),
            )
        }) {
            Ok(_) => true,
            Err(error)
                if error.contains("custom HTTP headers")
                    || error.contains("unsupported HTTP")
                    || error.contains("HTTP headers from environment") =>
            {
                unsupported.push(server.name.clone());
                false
            }
            Err(_) => true,
        }
    });
    if clients::setup_revision(client_id)? != revision {
        return Err(
            "Client config changed. Review it again before connecting. Config unchanged.".into(),
        );
    }
    let (mut result, imported, moved, verification) =
        crate::import_credentials::transaction(|writes| {
            let mut prepared = current.clone();
            let (imported, moved) = prepare_client_servers_for_migration(
                &mut prepared,
                &client,
                writes,
                choices,
                inputs,
            )?;
            enable_moved_servers(&mut prepared, profile, &moved)?;
            // Only staged registry writes run under the client mutation lock. Vault and
            // transport work can prompt or block and must run without either lock.
            let staged = clients::stage_reviewed(client_id, revision, || {
                crate::clients::backup_storage::protect_existing()?;
                commit_imports(&current, prepared)
            })?;
            let attempt = (|| {
                let verification = verify(&staged, &moved, client_id, profile)?;
                let outcome =
                    clients::migrate_reviewed(client_id, profile, &moved, revision, || {
                        if !setup_registry_matches(&read_registry_exact()?, &staged, client_id)? {
                            return Err("Servers changed during setup. Review again.".into());
                        }
                        Ok(())
                    })?;
                let result = finish_client_stdio_mutation(client_id, outcome, |managed_entry| {
                    registry::update(|registry| {
                        Ok(apply_client_stdio_update(
                            registry,
                            client_id,
                            profile,
                            managed_entry,
                        ))
                    })
                })?;
                Ok((result, imported, moved, verification))
            })();
            match attempt {
                Ok(result) => Ok(result),
                Err(error) => match rollback_imports(&current, &staged) {
                    Ok((true, _)) => Err(error),
                    Ok((false, retained)) => {
                        writes.keep_servers(&retained);
                        Err(format!("{error} {ROLLBACK_KEPT_EDITS}"))
                    }
                    Err(rollback) => {
                        writes.keep();
                        Err(format!("{error} {rollback} {ROLLBACK_KEPT_CREDENTIALS}"))
                    }
                },
            }
        })?;
    if !unsupported.is_empty() {
        result.outcome.warnings.push(format!(
            "Unsupported servers stayed in the client: {}",
            unsupported.join(", ")
        ));
    }
    let context = result.registry.resolve_profile_id(profile.unwrap_or(""));
    let others = result
        .registry
        .servers
        .iter()
        .filter(|s| result.registry.is_enabled(&context, &s.id) && !moved.contains(&s.name))
        .map(|s| s.name.clone())
        .collect::<Vec<_>>();
    if !others.is_empty() {
        result.outcome.warnings.push(format!(
            "Other servers were not verified during this connection: {}",
            others.join(", ")
        ));
    }
    Ok(MigrateOutcome {
        result,
        imported,
        moved,
        tools: verification.tools,
        servers: verification.servers,
    })
}

/// Read the same gateway surface and discovery mode this client will use. Probe
/// selected entries first so an empty cold catalog cannot masquerade as success.
fn verify_setup_gateway(
    registry: &Registry,
    moved: &[String],
    client_id: &str,
    profile: Option<&str>,
) -> Result<SetupVerification, String> {
    let intended = moved;
    // Nothing moves, so there is nothing to verify. Spawning the setup gateway
    // here waited on every existing server's catalog before confirming.
    if intended.is_empty() {
        clients::resolve_gateway_path_readonly()
            .ok_or("Could not locate the Toolport gateway. Client config unchanged.")?;
        return Ok(SetupVerification {
            tools: Vec::new(),
            servers: Vec::new(),
        });
    }
    let mut servers = Vec::new();
    let mut tool_counts = std::collections::BTreeMap::new();
    for name in intended {
        let server = registry
            .servers
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or("Reviewed server missing")?;
        if !crate::import_credentials::ready(server)? {
            return Err(format!(
                "{} needs credentials. Enter the missing values in review or fix the native config, then retry. Client config unchanged.",
                server.name
            ));
        }
        let probe = crate::server_runtime::probe_one_bounded(server);
        if !probe.ok {
            // A quiet step's probe may not read a locked store, so a server that keeps
            // values there can't be checked yet: said as the lock, not as a broken server.
            if crate::secrets::prompts_blocked()
                && crate::server_runtime::keeps_values_in_store(server)
                && crate::secrets::store_would_prompt()
            {
                return Err(format!(
                    "{} can't be checked while the keyring is locked. Client config unchanged. ({})",
                    server.name,
                    clients::backup_vault::locked_error()
                ));
            }
            return Err(if probe.auth_required {
                format!(
                    "{} needs credentials. Enter the missing values in review or fix the native config, then retry. Client config unchanged.",
                    server.name
                )
            } else {
                format!("{} could not start. Check its command or URL and retry. Client config unchanged.", server.name)
            });
        }
        tool_counts.insert(server.id.clone(), probe.tool_count);
    }
    let gateway = clients::resolve_gateway_path_readonly()
        .ok_or("Could not locate the Toolport gateway. Client config unchanged.")?;
    let mode = clients::discovery_capabilities(client_id)
        .resolve_mode(registry.client_discovery.get(client_id).map(String::as_str));
    let mut env = vec![
        (
            "TOOLPORT_DATA_DIR".into(),
            registry::conduit_dir()
                .ok_or("Missing data directory")?
                .display()
                .to_string(),
        ),
        ("TOOLPORT_PROFILE".into(), profile.unwrap_or("").to_string()),
        ("TOOLPORT_DISCOVERY".into(), mode.to_string()),
        (crate::brand::CLIENT_ID.into(), client_id.to_string()),
    ];
    if let Some(key) =
        std::env::var_os("TOOLPORT_SECRET_KEY").or_else(|| std::env::var_os("CONDUIT_SECRET_KEY"))
    {
        env.push((
            "TOOLPORT_SECRET_KEY".into(),
            key.to_string_lossy().into_owned(),
        ));
    }
    let transport = crate::downstream::StdioTransport::spawn(
        &gateway.to_string_lossy(),
        &["--setup-review".into()],
        &env,
        None,
        false,
    )
    .map_err(|_| "Gateway could not start. Client config unchanged.")?;
    let mut gateway =
        crate::downstream::DownstreamServer::connect("setup-review".into(), Box::new(transport))
            .map_err(|_| "Gateway did not answer. Client config unchanged.")?;
    for name in intended {
        let server = registry
            .servers
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .unwrap();
        let response = gateway
            .call(
                "toolport_search_tools",
                serde_json::json!({"query":"", "server":server.id, "limit":200}),
            )
            .map_err(|_| "Gateway discovery failed. Client config unchanged.")?;
        let text = response["content"][0]["text"].as_str().unwrap_or("");
        let catalog = text
            .split_once("\n\n")
            .and_then(|(_, json)| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
            .unwrap_or_default();
        if catalog.is_empty() || response["isError"].as_bool() == Some(true) {
            return Err(format!("{} has no verified gateway tools. Retry after it is ready. Client config unchanged.", server.name));
        }
        servers.push(SetupServerResult {
            name: server.name.clone(),
            tool_count: tool_counts[&server.id],
            credential_state: if crate::import_credentials::has_secrets(server) {
                "stored"
            } else {
                "none"
            }
            .into(),
        });
    }
    Ok(SetupVerification {
        tools: gateway.tools.materialize_all(),
        servers,
    })
}

pub fn connect_client_stdio(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
) -> Result<ClientMutationResult, String> {
    let current = read_registry_exact()?;
    let managed = current.client_managed_entries.clone();
    connect_client_stdio_with(client_id, profile, force, &managed, |managed_entry| {
        registry::update(|registry| {
            Ok(apply_client_stdio_update(
                registry,
                client_id,
                profile,
                managed_entry,
            ))
        })
    })
}

pub(crate) const CLIENT_HTTP_VAULT_SERVER: &str = "__toolport_http_clients__";

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("could not generate randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn finish_http_disconnect(
    client_id: &str,
    result: &mut ClientMutationResult,
    revoke: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let outcome = &result.outcome;
    let finish = || clients::finish_uninstall_with_warnings(client_id, outcome);
    let mut warnings = Vec::new();
    let finished = finish_http_cleanup(&mut warnings, finish, revoke);
    result.outcome.warnings.extend(warnings);
    finished
}

/// Both cleanup steps run after the registry write, even when one fails: the
/// bearer row is already gone, so a retried Disconnect takes the stdio branch
/// and would never retry the keychain delete. Both results are reported.
fn finish_http_cleanup(
    warnings: &mut Vec<String>,
    finish: impl FnOnce() -> Result<Vec<String>, String>,
    revoke: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let finished = finish();
    let revoked = revoke().map_err(|error| {
        format!("config restored; could not remove the revoked keychain token: {error}")
    });
    match (finished, revoked) {
        (Ok(more), Ok(())) => {
            warnings.extend(more);
            Ok(())
        }
        (Ok(more), Err(keychain)) => {
            warnings.extend(more);
            warnings.push(keychain);
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(keychain)) => Err(format!("{error}\n\n{keychain}")),
    }
}

pub(crate) fn registry_for_disconnect() -> Result<Registry, String> {
    read_registry_exact_or_default()
}

pub fn disconnect_client(client_id: &str) -> Result<ClientMutationResult, String> {
    disconnect_client_with_revocation(client_id, || {
        crate::secrets::delete_secret(CLIENT_HTTP_VAULT_SERVER, client_id)
    })
}

/// Disconnect one client the user chose in the app. If the keyring is locked
/// and the recovery files need it, this asks to unlock once (call it off the
/// UI thread); Remove from all clients, purge and the CLI use
/// [`disconnect_client`], which never asks.
pub fn disconnect_client_asking_to_unlock(client_id: &str) -> Result<ClientMutationResult, String> {
    disconnect_client_with_unlock(client_id, update_registry_on_disk, || {
        crate::secrets::delete_secret(CLIENT_HTTP_VAULT_SERVER, client_id)
    })
}

/// [`disconnect_client_with`] for a Disconnect the user chose: the first try
/// never prompts. Refused only because the keyring is locked, before the
/// client config or the registry changed, it removes exactly the backups that
/// try wrote, asks to unlock once and tries again. If the keyring stays locked
/// it refuses and leaves everything as it was.
pub(crate) fn disconnect_client_with_unlock(
    client_id: &str,
    update_registry: impl FnOnce(&dyn Fn(&mut Registry)) -> Result<Registry, String>,
    revoke: impl FnOnce() -> Result<(), String>,
) -> Result<ClientMutationResult, String> {
    let mut update_registry = Some(update_registry);
    let mut revoke = Some(revoke);
    let (first, attempt) = clients::attempt::recorded(|| {
        clients::backup_vault::noninteractive(|| {
            disconnect_try(client_id, &mut update_registry, &mut revoke)
        })
    });
    match first {
        Err(error)
            if clients::backup_vault::is_locked_error(&error)
                && update_registry.is_some()
                && revoke.is_some() =>
        {
            attempt.undo()?;
            match clients::backup_vault::request_unlock() {
                clients::backup_vault::Unlock::Open => {
                    disconnect_try(client_id, &mut update_registry, &mut revoke)
                }
                clients::backup_vault::Unlock::StillLocked => Err(unlock_to_disconnect(client_id)),
            }
        }
        other => {
            attempt.keep();
            other
        }
    }
}

/// One Disconnect try. The callbacks are taken only when used, so a try
/// refused before the registry write can run again with them.
fn disconnect_try<U, R>(
    client_id: &str,
    update_registry: &mut Option<U>,
    revoke: &mut Option<R>,
) -> Result<ClientMutationResult, String>
where
    U: FnOnce(&dyn Fn(&mut Registry)) -> Result<Registry, String>,
    R: FnOnce() -> Result<(), String>,
{
    disconnect_client_with(
        client_id,
        |apply: &dyn Fn(&mut Registry)| match update_registry.take() {
            Some(update) => update(apply),
            None => Err("Disconnect already wrote the registry".into()),
        },
        // The user asked for this delete; it may prompt like any other.
        || match revoke.take() {
            Some(revoke) => clients::backup_vault::interactive(revoke),
            None => Ok(()),
        },
    )
}

/// Disconnect's answer when the keyring stayed locked.
pub(crate) fn unlock_to_disconnect(client_id: &str) -> String {
    crate::i18n::trf(
        "Unlock your keyring to disconnect {client}.",
        &[("client", &clients::client_display_name(client_id))],
    )
}

fn disconnect_client_with_revocation(
    client_id: &str,
    revoke: impl FnOnce() -> Result<(), String>,
) -> Result<ClientMutationResult, String> {
    disconnect_client_with(client_id, update_registry_on_disk, revoke)
}

fn update_registry_on_disk(apply: &dyn Fn(&mut Registry)) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        apply(registry);
        Ok(())
    })?;
    Ok(registry)
}

/// The one Disconnect path for both shells. `update_registry` persists the
/// registry change it is handed (GTK writes the file; the Tauri shell also
/// refreshes its in-memory registry), and a failure there rolls the client
/// config back. `revoke` deletes the vaulted Shared HTTP bearer after its
/// registry row is gone; a failure there is reported as a warning because the
/// bearer no longer authenticates.
pub(crate) fn disconnect_client_with(
    client_id: &str,
    update_registry: impl FnOnce(&dyn Fn(&mut Registry)) -> Result<Registry, String>,
    revoke: impl FnOnce() -> Result<(), String>,
) -> Result<ClientMutationResult, String> {
    let current = read_registry_exact_or_default()?;
    let http_id = format!("client:{client_id}");
    let has_shared_http_token = current
        .http_clients
        .iter()
        .any(|client| client.id == http_id);
    if !has_shared_http_token {
        return disconnect_client_stdio_with(client_id, false, |_| {
            update_registry(&|registry| {
                registry.set_client_scope(client_id, None);
                registry.clear_client_managed_entry(client_id);
            })
        });
    }

    let _lock = acquire_auth_lock(&format!("client-config:{client_id}"))?;
    let outcome = clients::uninstall_gateway(client_id)?;
    let mut result = finish_client_config_mutation(outcome, |_| {
        update_registry(&|registry| {
            registry.set_client_scope(client_id, None);
            registry.clear_client_managed_entry(client_id);
            // The row is what authenticates the bearer, so dropping it in the
            // same write as the scope revokes the token atomically.
            registry.http_clients.retain(|row| row.id != http_id);
        })
    })?;
    finish_http_disconnect(client_id, &mut result, revoke)?;
    Ok(result)
}

pub fn disconnect_client_stdio(client_id: &str) -> Result<ClientMutationResult, String> {
    let current = read_registry_exact()?;
    let http_id = format!("client:{client_id}");
    let has_shared_http_token = current
        .http_clients
        .iter()
        .any(|client| client.id == http_id);
    disconnect_client_stdio_with(client_id, has_shared_http_token, |_| {
        let (registry, ()) = registry::update(|registry| {
            registry.set_client_scope(client_id, None);
            registry.clear_client_managed_entry(client_id);
            Ok(())
        })?;
        Ok(registry)
    })
}

pub fn set_auth_token_with(
    server_id: &str,
    token: &str,
    bump_generation: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _mutation = acquire_auth_lock(server_id)?;
    crate::remote::clear_oauth_state(server_id)
        .map_err(|error| could_not_clear_sign_in_state(&error))?;
    crate::secrets::set_secret(server_id, crate::secrets::HTTP_AUTH_KEY, token)
        .map_err(|error| could_not_store_token(&error))?;
    bump_generation().map_err(|error| stored_token_but_reload_failed(&error))
}

pub fn clear_auth_token_with(
    server_id: &str,
    bump_generation: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _mutation = acquire_auth_lock(server_id)?;
    crate::remote::clear_oauth_state(server_id)
        .map_err(|error| could_not_clear_sign_in_state(&error))?;
    crate::secrets::delete_secret(server_id, crate::secrets::HTTP_AUTH_KEY)
        .map_err(|error| could_not_remove_token(&error))?;
    bump_generation().map_err(|error| removed_token_but_reload_failed(&error))
}

pub(crate) fn stored_token_but_reload_failed(error: &str) -> String {
    format!("The token was stored in the keychain, but {error}")
}

pub(crate) fn removed_token_but_reload_failed(error: &str) -> String {
    format!(
        "The token was removed from the keychain, but {error}; the running gateway may still serve it"
    )
}

pub(crate) fn could_not_clear_sign_in_state(error: &str) -> String {
    format!("Could not clear the previous sign-in state: {error}")
}

pub(crate) fn could_not_remove_token(error: &str) -> String {
    format!("Could not remove the token: {error}")
}

pub(crate) fn could_not_store_token(error: &str) -> String {
    format!("Could not store the token: {error}")
}

pub fn has_auth_token(server_id: &str) -> Result<bool, String> {
    Ok(crate::secrets::get_secret_result(server_id, crate::secrets::HTTP_AUTH_KEY)?.is_some())
}

fn bump_secrets_generation_on_disk() -> Result<(), String> {
    registry::update(|registry| {
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok(())
    })
    .map(|_| ())
    .map_err(|error| {
        format!("could not reload the running gateway after the secret change: {error}")
    })
}

pub fn set_auth_token(server_id: &str, token: &str) -> Result<(), String> {
    set_auth_token_with(server_id, token, bump_secrets_generation_on_disk)
}

pub fn clear_auth_token(server_id: &str) -> Result<(), String> {
    clear_auth_token_with(server_id, bump_secrets_generation_on_disk)
}

pub fn has_client_secret(server_id: &str) -> Result<bool, String> {
    Ok(crate::secrets::get_secret_result(server_id, crate::secrets::CLIENT_SECRET_KEY)?.is_some())
}

fn client_credentials_edit_owner(reg: &Registry, id: &str) -> Result<String, String> {
    let owner = crate::local_auth::owner_in(reg, id)?;
    // A managed alias still edits its personal original. A random personal-sync
    // vault owner or URL-specific namespace is the server's own authentication.
    if reg
        .unknown_fields
        .get("localTeamAuthentication")
        .and_then(|v| v.get(id))
        .is_some()
        && owner != id
    {
        return Err(
            "Edit the personal original to change the shared local sign-in configuration.".into(),
        );
    }
    Ok(owner)
}

pub fn set_client_credentials(
    server_id: &str,
    client_id: &str,
    client_secret: Option<String>,
    token_endpoint_auth_method: Option<&str>,
    scope: Option<&str>,
) -> Result<Registry, String> {
    let _mutation = acquire_auth_lock(server_id)?;
    let current = read_registry_exact()?;
    let owner = client_credentials_edit_owner(&current, server_id)?;
    let _owner_pin = crate::local_auth::pin_credential_owner(server_id, &owner);
    let client_id = client_id.trim().to_string();
    if client_id.is_empty() {
        return Err("a client id is required for client-credentials auth".into());
    }
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let method = clean(token_endpoint_auth_method);
    let scope = clean(scope);
    if let Some(method) = method.as_deref() {
        if !crate::oauth::ClientAuthMethod::parse(method)
            .is_some_and(|method| method.is_implemented())
        {
            return Err(format!(
                "unsupported token endpoint auth method {method:?}; expected client_secret_basic or client_secret_post"
            ));
        }
    }
    if !current.servers.iter().any(|server| server.id == server_id) {
        return Err(format!("no server with id {server_id:?}"));
    }
    let secret_to_store = client_secret.filter(|secret| !secret.trim().is_empty());
    if secret_to_store.is_none() && !has_client_secret(server_id)? {
        return Err("no client secret is stored for this server yet; enter one".into());
    }
    crate::remote::reset_client_credentials(server_id)?;
    let (registry, ()) = registry::update(|registry| {
        if client_credentials_edit_owner(registry, server_id)? != owner {
            return Err("The destination changed. Review authentication again.".into());
        }
        let Some(server) = registry
            .servers
            .iter_mut()
            .find(|server| server.id == server_id)
        else {
            return Err(format!("no server with id {server_id:?}"));
        };
        let mut existing = server.client_credentials.take().unwrap_or_default();
        existing.strip_secret_fields();
        server.client_credentials = Some(crate::registry::ClientCredentials {
            client_id,
            token_endpoint_auth_method: method,
            scope,
            unknown_fields: existing.unknown_fields,
        });
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok(())
    })?;
    if let Some(secret) = secret_to_store {
        crate::secrets::set_secret(server_id, crate::secrets::CLIENT_SECRET_KEY, &secret)?;
    }
    Ok(registry)
}

pub fn clear_client_credentials(server_id: &str) -> Result<Registry, String> {
    let _mutation = acquire_auth_lock(server_id)?;
    let current = read_registry_exact()?;
    let owner = client_credentials_edit_owner(&current, server_id)?;
    let _owner_pin = crate::local_auth::pin_credential_owner(server_id, &owner);
    crate::remote::reset_client_credentials(server_id)?;
    let (registry, ()) = registry::update(|registry| {
        if client_credentials_edit_owner(registry, server_id)? != owner {
            return Err("The destination changed. Review authentication again.".into());
        }
        let Some(server) = registry
            .servers
            .iter_mut()
            .find(|server| server.id == server_id)
        else {
            return Err(format!("no server with id {server_id:?}"));
        };
        server.client_credentials = None;
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok(())
    })?;
    crate::secrets::delete_secret(server_id, crate::secrets::CLIENT_SECRET_KEY)?;
    Ok(registry)
}

fn normalize_secret_key(key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("give the environment variable a name".into());
    }
    if key.contains('=') || key.contains('\0') {
        return Err("environment variable names cannot contain '=' or NUL".into());
    }
    Ok(key.to_string())
}

pub fn apply_secret_declaration(
    registry: &mut Registry,
    server_id: &str,
    key: &str,
) -> Result<(), String> {
    let server = registry
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    if let Some(overrides) = server
        .unknown_fields
        .get_mut("memberSecretRefs")
        .and_then(serde_json::Value::as_object_mut)
    {
        overrides.remove(&format!("env:{key}"));
        overrides.remove(&format!("header:{key}"));
    }
    if let Some(header) = server
        .unknown_fields
        .get_mut("headerKeys")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|hs| hs.iter_mut().find(|h| h["key"] == key))
    {
        if let Some(object) = header.as_object_mut() {
            object.remove("source");
        }
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        return Ok(());
    }
    // A typed bearer token replaces a password manager Authorization row, which
    // would otherwise still be sent in its place.
    if key == crate::secrets::HTTP_AUTH_KEY {
        let referenced = |entry: &crate::registry::EnvVar| {
            entry.key.trim().eq_ignore_ascii_case("authorization")
                && entry.unknown_fields.contains_key("source")
        };
        let dropped: Vec<String> = server
            .env
            .iter()
            .filter(|entry| referenced(entry))
            .map(|entry| entry.key.clone())
            .collect();
        server.env.retain(|entry| !referenced(entry));
        if let Some(launch) = server.launch.as_mut() {
            launch.required_env.retain(|k| !dropped.contains(k));
        }
    }
    match server.env.iter_mut().find(|entry| entry.key == key) {
        Some(entry) => {
            entry.secret = true;
            entry.value = None;
            entry.unknown_fields.remove("source");
        }
        None => server.env.push(crate::registry::EnvVar {
            key: key.to_string(),
            value: None,
            secret: true,
            unknown_fields: Default::default(),
        }),
    }
    registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
    Ok(())
}

pub fn apply_secret_removal(
    registry: &mut Registry,
    server_id: &str,
    key: &str,
) -> Result<(), String> {
    let server = registry
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    if let Some(overrides) = server
        .unknown_fields
        .get_mut("memberSecretRefs")
        .and_then(serde_json::Value::as_object_mut)
    {
        overrides.remove(&format!("env:{key}"));
        overrides.remove(&format!("header:{key}"));
    }
    server.env.retain(|entry| entry.key != key);
    registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
    Ok(())
}

fn restore_secret<S, D>(
    previous: Option<String>,
    server_id: &str,
    key: &str,
    set: &mut S,
    delete: &mut D,
) -> Result<(), String>
where
    S: FnMut(&str, &str, &str) -> Result<(), String>,
    D: FnMut(&str, &str) -> Result<(), String>,
{
    match previous {
        Some(previous) => set(server_id, key, &previous),
        None => delete(server_id, key),
    }
}

fn set_server_secret_using<G, S, D, W>(
    server_id: &str,
    key: &str,
    value: &str,
    mut get: G,
    mut set: S,
    mut delete: D,
    write_registry: W,
) -> Result<Registry, String>
where
    G: FnMut(&str, &str) -> Result<Option<String>, String>,
    S: FnMut(&str, &str, &str) -> Result<(), String>,
    D: FnMut(&str, &str) -> Result<(), String>,
    W: FnOnce(&str, &str) -> Result<Registry, String>,
{
    let previous = get(server_id, key)?;
    set(server_id, key, value)?;
    match write_registry(server_id, key) {
        Ok(registry) => Ok(registry),
        Err(registry_error) => match restore_secret(
            previous,
            server_id,
            key,
            &mut set,
            &mut delete,
        ) {
            Ok(()) => Err(format!(
                "could not update the registry, so the keychain change was rolled back: {registry_error}"
            )),
            Err(rollback_error) => Err(format!(
                "the value was stored in the keychain, but the registry update and keychain rollback both failed: {registry_error}; rollback: {rollback_error}"
            )),
        },
    }
}

fn delete_server_secret_using<G, S, D, W>(
    server_id: &str,
    key: &str,
    mut get: G,
    mut set: S,
    mut delete: D,
    write_registry: W,
) -> Result<Registry, String>
where
    G: FnMut(&str, &str) -> Result<Option<String>, String>,
    S: FnMut(&str, &str, &str) -> Result<(), String>,
    D: FnMut(&str, &str) -> Result<(), String>,
    W: FnOnce(&str, &str) -> Result<Registry, String>,
{
    let previous = get(server_id, key)?;
    delete(server_id, key)?;
    match write_registry(server_id, key) {
        Ok(registry) => Ok(registry),
        Err(registry_error) => match previous {
            Some(previous) => match set(server_id, key, &previous) {
                Ok(()) => Err(format!(
                    "could not update the registry, so the keychain change was rolled back: {registry_error}"
                )),
                Err(rollback_error) => Err(format!(
                    "the value was removed from the keychain, but the registry update and keychain rollback both failed: {registry_error}; rollback: {rollback_error}"
                )),
            },
            None => Err(format!("could not update the registry: {registry_error}")),
        },
    }
}

pub fn set_server_secret_with(
    server_id: &str,
    key: &str,
    value: &str,
    write_registry: impl FnOnce(&str, &str) -> Result<Registry, String>,
) -> Result<Registry, String> {
    // Both shells store through here, so both get the same rules.
    let (key, value, bearer) = credential_slot(server_id, key, value)?;
    let key = normalize_secret_key(&key)?;
    let _lock = acquire_auth_lock(server_id)?;
    // Same as a token set from the server's sign-in settings: a stale sign-in
    // must not outlive the token typed over it. Taken first, so a failed save
    // puts the sign-in back.
    let sign_in = if bearer {
        take_sign_in_using(
            server_id,
            crate::secrets::get_secret_result,
            crate::secrets::delete_secret,
            crate::secrets::set_secret,
        )?
    } else {
        Vec::new()
    };
    set_server_secret_using(
        server_id,
        &key,
        value,
        crate::secrets::get_secret_result,
        crate::secrets::set_secret,
        crate::secrets::delete_secret,
        write_registry,
    )
    .map_err(|error| {
        with_sign_in_put_back(
            error,
            put_back_sign_in(server_id, &sign_in, crate::secrets::set_secret),
        )
    })
}

/// A sign-in's saved state, by key, as it was before a typed token replaced it.
type SignIn = Vec<(&'static str, Option<String>)>;

/// Clear a sign-in's state and return what was there. When a delete fails,
/// whatever was deleted is put back and the error says so.
fn take_sign_in_using(
    server_id: &str,
    mut get: impl FnMut(&str, &str) -> Result<Option<String>, String>,
    mut delete: impl FnMut(&str, &str) -> Result<(), String>,
    set: impl FnMut(&str, &str, &str) -> Result<(), String>,
) -> Result<SignIn, String> {
    let saved = crate::remote::OAUTH_STATE_KEYS
        .iter()
        .map(|key| Ok((*key, get(server_id, key)?)))
        .collect::<Result<SignIn, String>>()
        .map_err(|error| could_not_clear_sign_in_state(&error))?;
    // Attempt every delete, as `clear_oauth_state` does, then report the first failure.
    let mut failed = None;
    for (key, _) in &saved {
        if let Err(error) = delete(server_id, key) {
            failed.get_or_insert(error);
        }
    }
    match failed {
        None => Ok(saved),
        Some(error) => Err(with_sign_in_put_back(
            could_not_clear_sign_in_state(&error),
            put_back_sign_in(server_id, &saved, set),
        )),
    }
}

/// Put a taken sign-in back, every key tried; the first failure is returned.
fn put_back_sign_in(
    server_id: &str,
    sign_in: &SignIn,
    mut set: impl FnMut(&str, &str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut failed = None;
    for (key, value) in sign_in {
        if let Some(value) = value {
            if let Err(error) = set(server_id, key, value) {
                failed.get_or_insert(error);
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

fn with_sign_in_put_back(error: String, put_back: Result<(), String>) -> String {
    match put_back {
        Ok(()) => error,
        Err(put_back) => {
            format!("{error}; the previous sign-in could not be restored: {put_back}")
        }
    }
}

pub fn delete_server_secret_with(
    server_id: &str,
    key: &str,
    write_registry: impl FnOnce(&str, &str) -> Result<Registry, String>,
) -> Result<Registry, String> {
    let key = normalize_secret_key(key)?;
    let _lock = acquire_auth_lock(server_id)?;
    delete_server_secret_using(
        server_id,
        &key,
        crate::secrets::get_secret_result,
        crate::secrets::set_secret,
        crate::secrets::delete_secret,
        write_registry,
    )
}

pub fn set_server_secret(server_id: &str, key: &str, value: &str) -> Result<Registry, String> {
    set_server_secret_with(server_id, key, value, |server_id, key| {
        let (registry, ()) =
            registry::update(|registry| apply_secret_declaration(registry, server_id, key))?;
        Ok(registry)
    })
}

/// The key and value a typed credential is stored under. A remote server's
/// Authorization header is its bearer token, kept where sign-in keeps it.
fn credential_slot<'a>(
    server_id: &str,
    key: &str,
    value: &'a str,
) -> Result<(String, &'a str, bool), String> {
    if !crate::import_credentials::provided(value) {
        return Err("Enter a credential value. Placeholder values cannot be saved.".into());
    }
    if !key.trim().eq_ignore_ascii_case("authorization") {
        return Ok((key.to_string(), value, false));
    }
    let registry = read_registry_exact()?;
    let Some(server) = registry.servers.iter().find(|s| s.id == server_id) else {
        return Ok((key.to_string(), value, false));
    };
    // A shared Authorization header keeps its own row, so a member's value
    // replaces the team's source there, under the header's own name. Only the
    // bearer slot is mapped here.
    let header = server
        .unknown_fields
        .get("headerKeys")
        .and_then(serde_json::Value::as_array)
        .and_then(|headers| {
            headers.iter().find_map(|h| {
                h["key"]
                    .as_str()
                    .filter(|name| name.trim().eq_ignore_ascii_case(key.trim()))
            })
        });
    if let Some(header) = header {
        return Ok((header.to_string(), value, false));
    }
    if server.transport == "stdio" {
        return Ok((key.to_string(), value, false));
    }
    match bearer_token(value) {
        Ok(token) if !crate::import_credentials::provided(token) => {
            Err("Enter a credential value. Placeholder values cannot be saved.".into())
        }
        Ok(token) => Ok((crate::secrets::HTTP_AUTH_KEY.to_string(), token, true)),
        Err(AuthValueError::NoToken) => {
            Err("Enter the token after Bearer in the Authorization value".into())
        }
        Err(AuthValueError::OtherScheme) => {
            Err("Only Bearer authentication is supported for Authorization headers".into())
        }
    }
}

/// Why an Authorization value holds no bearer token.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthValueError {
    /// `Bearer` (or `Basic`) alone.
    NoToken,
    /// A scheme other than Bearer, or more than a scheme and a token.
    OtherScheme,
}

/// The bearer token in a typed or pasted Authorization value: a bare token, or
/// `Bearer` and a token, split on any whitespace. Both shells read it here.
pub(crate) fn bearer_token(value: &str) -> Result<&str, AuthValueError> {
    let mut words = value.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(word), None, _)
            if word.eq_ignore_ascii_case("bearer") || word.eq_ignore_ascii_case("basic") =>
        {
            Err(AuthValueError::NoToken)
        }
        (Some(token), None, _) => Ok(token),
        (Some(scheme), Some(token), None) if scheme.eq_ignore_ascii_case("bearer") => Ok(token),
        _ => Err(AuthValueError::OtherScheme),
    }
}

/// Vault a launch argument input without declaring an environment variable.
pub fn set_launch_secret_with(
    server_id: &str,
    key: &str,
    value: &str,
    write_registry: impl FnOnce(&str, &str) -> Result<Registry, String>,
) -> Result<Registry, String> {
    let key = normalize_secret_key(key)?;
    let _lock = acquire_auth_lock(server_id)?;
    set_server_secret_using(
        server_id,
        &key,
        value,
        crate::secrets::get_vault_secret_result,
        crate::secrets::set_secret,
        crate::secrets::delete_secret,
        write_registry,
    )
}

pub fn set_launch_secret(server_id: &str, key: &str, value: &str) -> Result<Registry, String> {
    set_launch_secret_with(server_id, key, value, |server_id, key| {
        let (registry, ()) =
            registry::update(|registry| apply_launch_secret_generation(registry, server_id, key))?;
        Ok(registry)
    })
}

pub fn set_launch_input_value(
    server_id: &str,
    key: &str,
    value: Option<String>,
) -> Result<Registry, String> {
    let (registry, ()) =
        registry::update(|registry| apply_launch_input_value(registry, server_id, key, value))?;
    Ok(registry)
}

pub fn apply_launch_input_value(
    registry: &mut Registry,
    server_id: &str,
    key: &str,
    value: Option<String>,
) -> Result<(), String> {
    let server = registry
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    let input = server
        .launch
        .as_mut()
        .and_then(|launch| launch.inputs.iter_mut().find(|input| input.key == key))
        .ok_or("launch input no longer exists")?;
    if input.secret {
        return Err("secret launch inputs must be vaulted".into());
    }
    input.value = value;
    Ok(())
}

pub fn apply_launch_secret_generation(
    registry: &mut Registry,
    server_id: &str,
    key: &str,
) -> Result<(), String> {
    let server = registry
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    if let Some(overrides) = server
        .unknown_fields
        .get_mut("memberSecretRefs")
        .and_then(serde_json::Value::as_object_mut)
    {
        overrides.remove(&format!("input:{key}"));
    }
    if !server.launch.as_ref().is_some_and(|launch| {
        launch
            .inputs
            .iter()
            .any(|input| input.key == key && input.secret)
    }) {
        return Err("not a declared secret launch input".into());
    }
    if let Some(input) = server
        .launch
        .as_mut()
        .and_then(|l| l.inputs.iter_mut().find(|i| i.key == key))
    {
        input.unknown_fields.remove("source");
    }
    registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
    Ok(())
}

pub fn delete_server_secret(server_id: &str, key: &str) -> Result<Registry, String> {
    delete_server_secret_with(server_id, key, |server_id, key| {
        let (registry, ()) =
            registry::update(|registry| apply_secret_removal(registry, server_id, key))?;
        Ok(registry)
    })
}

pub fn apply_server_enabled(
    registry: &mut Registry,
    profile_id: &str,
    server_id: &str,
    enabled: bool,
    reviewed: bool,
) -> Result<(), String> {
    if enabled && crate::teams::server_change_held(registry, server_id) {
        return Err("Review this change in Teams before enabling it.".into());
    }
    if enabled
        && registry
            .servers
            .iter()
            .any(|s| s.id == server_id && !crate::personal_sync::missing_secret_args(s).is_empty())
    {
        return Err(
            "Edit this server and enter its secret argument on this machine before enabling it."
                .into(),
        );
    }
    if !enabled {
        crate::teams::remember_held_disable(registry, profile_id, server_id)?;
    }
    if reviewed {
        crate::local_auth::detach_changed(registry, server_id)?;
    }
    if enabled {
        if let Some(server) = registry
            .servers
            .iter()
            .find(|server| server.id == server_id)
        {
            if server.launch.is_some() {
                crate::launch_inputs::check_ready_for_enable(server)?;
            }
            server.check_enable_allowed(reviewed)?;
            if reviewed {
                crate::secret_refs::approve_server(server).map_err(|e| e.to_string())?;
            }
        }
    }
    if crate::personal_sync::is_personal(registry) {
        if let Some(server) = registry.servers.iter_mut().find(|s| s.id == server_id) {
            server.unknown_fields.insert(
                "personalSyncDesiredEnabled".into(),
                serde_json::json!(enabled),
            );
        }
    }
    if enabled && reviewed && crate::personal_sync::is_personal(registry) {
        if let Some(server) = registry.servers.iter_mut().find(|s| s.id == server_id) {
            server.unknown_fields.remove("teamEnableReview");
            server.unknown_fields.remove("personalSyncArgsReview");
            // Approving kept local arguments covers the synced layout that could
            // not be matched, so the next poll does not hold the server again.
            if let Some(incoming) = server.unknown_fields.remove("personalSyncArgsIncoming") {
                server
                    .unknown_fields
                    .insert("personalSyncArgsApproved".into(), incoming);
            }
            let identity =
                crate::personal_sync::command_identity(&crate::personal_sync::export(server));
            let fields = crate::personal_sync::execution_review_fields(server);
            server
                .unknown_fields
                .insert("syncExecutionReview".into(), serde_json::json!(fields));
            server
                .unknown_fields
                .insert("syncCommandConsent".into(), identity);
        }
    }
    if registry.version >= 3 {
        // Teams restores consent by access-set membership during definition sync.
        // Record a reviewed enable in its existing local context without changing
        // any other access set; a global off keeps that consent but stays off.
        if enabled
            && registry.servers.iter().any(|s| {
                s.id == server_id
                    && s.source
                        .as_deref()
                        .is_some_and(|source| source.starts_with("team:"))
            })
        {
            let context = registry.active_profile_id();
            if let Some(profile) = registry.profiles.iter_mut().find(|p| p.id == context) {
                if !profile.enabled_server_ids.iter().any(|id| id == server_id) {
                    profile.enabled_server_ids.push(server_id.into());
                }
            }
        }
        registry.set_global_server_enabled(server_id, enabled)
    } else {
        registry.set_server_enabled(profile_id, server_id, enabled)
    }
}

pub fn set_server_enabled_after_reference_review(
    profile_id: &str,
    reviewed: &ServerEntry,
) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        let current = registry
            .servers
            .iter()
            .find(|s| s.id == reviewed.id)
            .ok_or("Server no longer exists")?;
        crate::personal_sync::check_review(registry, current, Some(reviewed))?;
        apply_server_enabled(registry, profile_id, &reviewed.id, true, true)
    })?;
    Ok(registry)
}

pub fn set_server_enabled(
    profile_id: &str,
    server_id: &str,
    enabled: bool,
    reviewed: bool,
) -> Result<Registry, String> {
    let (registry, ()) = registry::update(|registry| {
        apply_server_enabled(registry, profile_id, server_id, enabled, reviewed)
    })?;
    Ok(registry)
}

#[cfg(test)]
fn set_server_enabled_at(
    path: &Path,
    profile_id: &str,
    server_id: &str,
    enabled: bool,
    reviewed: bool,
) -> Result<Registry, String> {
    let (registry, ()) = registry::update_at(path, |registry| {
        apply_server_enabled(registry, profile_id, server_id, enabled, reviewed)
    })?;
    Ok(registry)
}

/// Update the member's safety choice without changing releasable team policy.
pub fn set_safety_level(level: registry::SafetyLevel) -> Result<Registry, String> {
    let (registry, _) = registry::update(|registry| {
        registry.validate_safety_level(level)?;
        registry.set_safety_level(level);
        Ok(())
    })?;
    Ok(registry)
}

/// Store a reference, never its resolved value. Existing keychain values are ignored
/// while the reference is selected; switching back uses the normal keychain path.
pub fn apply_secret_reference(
    reg: &mut Registry,
    server_id: &str,
    key: &str,
    reference: &str,
) -> Result<(), String> {
    let key = normalize_secret_key(key)?;
    let personal = crate::personal_sync::is_personal(reg);
    let server = reg
        .servers
        .iter_mut()
        .find(|s| s.id == server_id)
        .ok_or("Server not found")?;
    crate::secret_refs::check_policy(server, reference).map_err(|e| e.to_string())?;
    let location = if server
        .launch
        .iter()
        .flat_map(|l| &l.inputs)
        .any(|i| i.key == key)
    {
        "input"
    } else if crate::secret_refs::headers(server)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|h| h.key == key)
    {
        "header"
    } else {
        "env"
    };
    if let Some(input) = server
        .launch
        .as_mut()
        .and_then(|l| l.inputs.iter_mut().find(|i| i.key == key))
    {
        input.secret = true;
        input.value = None;
        input
            .unknown_fields
            .insert("source".into(), serde_json::json!({"ref": reference}));
    } else if let Some(header) = server
        .unknown_fields
        .get_mut("headerKeys")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|hs| hs.iter_mut().find(|h| h["key"] == key))
    {
        header["source"] = serde_json::json!({"ref": reference});
    } else {
        if !server.env.iter().any(|e| e.key == key) {
            server.env.push(crate::registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            });
        }
        let entry = server.env.iter_mut().find(|e| e.key == key).unwrap();
        entry.value = None;
        entry.secret = true;
        entry
            .unknown_fields
            .insert("source".into(), serde_json::json!({"ref": reference}));
    }
    crate::secret_refs::validate_server(server).map_err(|e| e.to_string())?;
    if personal {
        // Every machine on a personal account is the owner's, so the reference
        // is the synced definition, not a local override that export holds back.
        if let Some(overrides) = server
            .unknown_fields
            .get_mut("memberSecretRefs")
            .and_then(serde_json::Value::as_object_mut)
        {
            overrides.remove(&format!("{location}:{key}"));
        }
    } else if crate::secret_refs::is_shared(server) {
        let overrides = server
            .unknown_fields
            .entry("memberSecretRefs")
            .or_insert_with(|| serde_json::json!({}));
        overrides[format!("{location}:{key}")] = serde_json::json!(reference);
    }
    reg.secrets_generation = reg.secrets_generation.wrapping_add(1);
    Ok(())
}
pub fn set_secret_reference(
    server_id: &str,
    key: &str,
    reference: &str,
) -> Result<Registry, String> {
    let (reg, ()) = registry::update(|reg| apply_secret_reference(reg, server_id, key, reference))?;
    approve_own_reference(&reg, server_id)?;
    Ok(reg)
}
/// On a personal account the reference is now part of the synced definition,
/// which other machines must approve. Saving it here is this machine's approval.
pub fn approve_own_reference(reg: &Registry, server_id: &str) -> Result<(), String> {
    if !crate::personal_sync::is_personal(reg) {
        return Ok(());
    }
    match reg.servers.iter().find(|s| s.id == server_id) {
        Some(server) => crate::secret_refs::approve_server(server).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}
pub fn test_secret_reference(
    server_id: &str,
    reference: &str,
) -> Result<(), crate::secret_refs::ResolveError> {
    if !server_id.is_empty() {
        let reg = registry::load().map_err(|_| crate::secret_refs::ResolveError {
            state: crate::secret_refs::ErrorState::Failed,
            message: "Could not read server configuration.".into(),
        })?;
        let server = reg
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or_else(|| crate::secret_refs::ResolveError {
                state: crate::secret_refs::ErrorState::Failed,
                message: "Server not found.".into(),
            })?;
        crate::secret_refs::check_policy(server, reference)?;
        crate::secret_refs::check_approval(server)?;
    }
    crate::secret_refs::resolve(reference).map(|_| ())
}

#[cfg(test)]
mod tests {
    /// A failed recovery cleanup still lets the keychain delete run, and both
    /// failures are reported (one shared helper for GTK and React).
    #[test]
    fn http_disconnect_cleanup_runs_both_steps_and_reports_both() {
        let revoked = std::cell::Cell::new(false);
        let mut warnings = Vec::new();
        let err = finish_http_cleanup(
            &mut warnings,
            || Err("recovery cleanup failed".into()),
            || {
                revoked.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            revoked.get(),
            "the keychain delete must run after a cleanup failure"
        );
        assert_eq!(err, "recovery cleanup failed");

        let err = finish_http_cleanup(
            &mut warnings,
            || Err("recovery cleanup failed".into()),
            || Err("keychain locked".into()),
        )
        .unwrap_err();
        assert!(
            err.contains("recovery cleanup failed") && err.contains("keychain locked"),
            "{err}"
        );

        let mut warnings = Vec::new();
        finish_http_cleanup(
            &mut warnings,
            || Ok(vec!["recovery warning".into()]),
            || Err("keychain locked".into()),
        )
        .unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[1].contains("keychain locked"));
    }

    /// A filesystem-safe label for a scratch path. `thread::current().name()` is
    /// the test's full path (`registry_controller::tests::foo`), and `:` is not a
    /// legal filename character on Windows, so embedding it whole made every
    /// fixture here fail there with `InvalidFilename`.
    fn scratch_label() -> String {
        std::thread::current()
            .name()
            .unwrap_or("test")
            .rsplit("::")
            .next()
            .unwrap_or("test")
            .to_string()
    }
    use super::*;
    use crate::registry::ServerEntry;

    #[test]
    fn bound_bulk_release_reports_a_new_scope_without_releasing_it() {
        let _lock = registry::data_dir_test_lock();
        let dir = test_path("bound-bulk-new-scope");
        std::fs::create_dir_all(&dir).unwrap();
        let data = registry::DataDirOverride::set(&dir);
        let mut registry = Registry::default();
        registry::save(&registry).unwrap();
        // Existing scopes already have stores on a live install. Avoid making
        // this fixture trigger legacy global-store migration while scanning.
        for profile in &registry.profiles {
            let path = dir.join(format!(
                "quarantine-v2-{}.json",
                registry::profile_store_key(&profile.id)
            ));
            std::fs::write(path, "{}").unwrap();
        }
        std::fs::write(
            dir.join("quarantine.json"),
            serde_json::json!({
                "srv__read": {"tool":"srv__read", "change":"changed", "ts":1}
            })
            .to_string(),
        )
        .unwrap();
        let displayed = crate::integrity::all_quarantined().unwrap();
        let expected = std::collections::BTreeMap::from([(
            "".to_string(),
            std::collections::BTreeMap::from([(
                "srv__read".to_string(),
                displayed[0]["record_id"].as_str().unwrap().to_string(),
            )]),
        )]);
        let new_scope = registry.add_profile("New scope");
        let path = dir.join(format!(
            "quarantine-v2-{}.json",
            registry::profile_store_key(&new_scope)
        ));
        std::fs::write(
            path,
            serde_json::json!({
                "other__read": {"tool":"other__read", "change":"changed", "ts":2}
            })
            .to_string(),
        )
        .unwrap();
        registry::save(&registry).unwrap();
        let summary = release_all_quarantine_records(&["".into()], &expected);
        assert_eq!(summary.released, 1);
        assert_eq!(summary.changed, vec!["other__read"]);
        assert!(summary.skipped.is_empty());
        assert!(summary.failed.is_empty());
        assert!(crate::integrity::quarantined(Some(&new_scope))
            .unwrap()
            .contains("other__read"));
        assert!(crate::integrity::quarantined(None).unwrap().is_empty());
        drop(data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct ZCodeImportFixture {
        root: PathBuf,
        client: clients::DetectedClient,
    }

    impl ZCodeImportFixture {
        fn new(servers: serde_json::Value) -> Self {
            let root = test_path("zcode-import");
            let path = root.join(".zcode/cli/config.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                serde_json::json!({"mcp":{"servers":servers}}).to_string(),
            )
            .unwrap();
            let inventory = servers
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, definition)| clients::McpServer {
                    name: name.clone(),
                    transport: definition
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("stdio")
                        .into(),
                    command: definition
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    args: Vec::new(),
                    env_keys: Vec::new(),
                    url: definition
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            Self {
                root,
                client: clients::DetectedClient {
                    discovery: clients::discovery_capabilities("fixture"),
                    id: "zcode".into(),
                    name: "ZCode".into(),
                    uses_connectors: false,
                    location_removable: false,
                    location_kept_by_t3_code: false,
                    config_path: path.display().to_string(),
                    config_exists: true,
                    app_present: true,
                    servers: inventory,
                    plugin_servers: Vec::new(),
                    gateway_installed: false,
                    entry_state: GatewayEntryState::Absent,
                    error: None,
                    claude_profile: None,
                    t3_code: Vec::new(),
                },
            }
        }
    }

    impl Drop for ZCodeImportFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn connect_fixture(
        client_id: &str,
        profile: Option<&str>,
        force: bool,
    ) -> Result<MigrateOutcome, String> {
        let review = preview_client_setup(client_id)?;
        let names = review
            .items
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>();
        migrate_client_reviewed_with(
            client_id,
            profile,
            force,
            &names,
            &review.revision,
            |_, _, _, _| Ok(Vec::new().into()),
        )
    }

    #[test]
    fn personal_sync_client_credentials_work_for_received_and_changed_urls() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut reg = Registry { team: Some(serde_json::from_value(serde_json::json!({"serverUrl":"https://example.com","teamId":"solo","role":"admin","accountStatus":{"personalSync":true}})).unwrap()), ..Default::default() };
            crate::personal_sync::apply(&mut reg, &serde_json::json!({"servers":[{"id":"received","name":"Received","transport":"http","url":"https://first.example/mcp"}]}), 1).unwrap();
            let id = reg.servers[0].id.clone();
            registry::save(&reg).unwrap();
            assert_ne!(crate::local_auth::owner(&id).unwrap(), id);
            set_client_credentials(&id, "client", Some("first-secret".into()), None, None).unwrap();
            assert!(has_client_secret(&id).unwrap());
            clear_client_credentials(&id).unwrap();
            assert!(!has_client_secret(&id).unwrap());
            set_client_credentials(&id, "client", Some("first-secret".into()), None, None).unwrap();
            crate::personal_sync::remote_update(|| {
                registry::update(|r| {
                    r.servers[0].url = Some("https://second.example/mcp".into());
                    Ok(())
                })
            })
            .unwrap();
            assert!(!has_client_secret(&id).unwrap());
            set_client_credentials(&id, "other", Some("second-secret".into()), None, None).unwrap();
            assert!(has_client_secret(&id).unwrap());
            clear_client_credentials(&id).unwrap();
            assert!(!has_client_secret(&id).unwrap());
            crate::personal_sync::remote_update(|| {
                registry::update(|r| {
                    r.servers[0].url = Some("https://first.example/mcp".into());
                    Ok(())
                })
            })
            .unwrap();
            assert!(has_client_secret(&id).unwrap());
        });
    }
    #[test]
    fn scoped_import_commits_the_second_clients_reviewed_definition_and_credentials() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(), r#"{"mcpServers":{"shared":{"command":"shared-mcp","env":{"API_KEY":"synthetic-second-origin"}}}}"#).unwrap();
        std::fs::write(
            fixture.codex(),
            r#"[mcp_servers.shared]
command = "shared-mcp"
[mcp_servers.shared.env]
API_KEY = "synthetic-first-origin"
"#,
        )
        .unwrap();
        let global = preview_client_imports().unwrap();
        assert_eq!(
            global.iter().filter(|item| item.name == "shared").count(),
            1
        );
        assert_eq!(
            global
                .iter()
                .find(|item| item.name == "shared")
                .unwrap()
                .client_id,
            "codex"
        );
        let second = preview_client_imports_for("claude-code").unwrap();
        let item = second.iter().find(|item| item.name == "shared").unwrap();
        assert_eq!(item.client_id, "claude-code");
        assert!(item
            .credentials
            .iter()
            .any(|c| c.key == "API_KEY" && c.secret && c.present));
        assert!(!item.key.contains("synthetic-second-origin"));
        let (registry, added) = import_client_servers_inputs(
            vec![item.key.clone()],
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(added, 1);
        let imported = registry
            .servers
            .iter()
            .find(|server| server.name == "shared")
            .unwrap();
        assert_eq!(imported.source.as_deref(), Some("imported:claude-code"));
        assert_eq!(
            crate::secrets::get_vault_secret_result(&imported.id, "API_KEY")
                .unwrap()
                .as_deref(),
            Some("synthetic-second-origin")
        );
        assert!(
            preview_client_imports_for("codex")
                .unwrap()
                .iter()
                .all(|item| item.name != "shared"),
            "registry dedupe still applies across clients"
        );
        assert!(std::fs::read_to_string(fixture.claude())
            .unwrap()
            .contains("synthetic-second-origin"));
    }

    #[test]
    fn scoped_import_rejects_changed_hidden_credentials_and_mixed_client_reviews() {
        let fixture = MoveFixture::new(&Registry::default());
        let config = |value: &str| {
            serde_json::json!({"mcpServers":{"shared":{"command":"shared-mcp","env":{"API_KEY":value}}}}).to_string()
        };
        std::fs::write(fixture.claude(), config("synthetic-reviewed")).unwrap();
        std::fs::write(
            fixture.codex(),
            "[mcp_servers.shared]\ncommand = \"shared-mcp\"\n",
        )
        .unwrap();
        let first = preview_client_imports_for("claude-code")
            .unwrap()
            .into_iter()
            .find(|i| i.name == "shared")
            .unwrap();
        let second = preview_client_imports_for("codex")
            .unwrap()
            .into_iter()
            .find(|i| i.name == "shared")
            .unwrap();
        assert!(import_client_servers(vec![first.key.clone(), second.key])
            .unwrap_err()
            .contains("one client"));
        std::fs::write(fixture.claude(), config("synthetic-changed")).unwrap();
        assert!(import_client_servers(vec![first.key])
            .unwrap_err()
            .contains("Server changed"));
        assert!(read_registry_exact().unwrap().servers.is_empty());
    }

    #[test]
    fn reviewed_bulk_import_shows_unsupported_and_uses_storage_choices() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"one":{"command":"one","env":{"PORT":"3000","VALUE":"synthetic-custom-secret"}},"unsupported":{"url":"https://example.invalid/mcp","headers":{"X-Custom":"synthetic-header"}}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_imports().unwrap();
        let one = review.iter().find(|item| item.name == "one").unwrap();
        assert!(one
            .credentials
            .iter()
            .any(|env| env.key == "PORT" && env.present && !env.secret));
        let unsupported = review
            .iter()
            .find(|item| item.name == "unsupported")
            .unwrap();
        assert!(unsupported
            .unsupported
            .as_ref()
            .unwrap()
            .contains("custom HTTP headers"));
        let choices = std::collections::BTreeMap::from([(
            "one".into(),
            std::collections::BTreeMap::from([("VALUE".into(), true)]),
        )]);
        let (registry, added) = import_client_servers_inputs(
            review.into_iter().map(|item| item.key).collect(),
            &choices,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(added, 1);
        let one = registry
            .servers
            .iter()
            .find(|server| server.name == "one")
            .unwrap();
        assert!(one
            .env
            .iter()
            .any(|env| env.key == "PORT" && !env.secret && env.value.as_deref() == Some("3000")));
        assert!(one
            .env
            .iter()
            .any(|env| env.key == "VALUE" && env.secret && env.value.is_none()));
        assert_eq!(
            crate::secrets::get_vault_secret_result(&one.id, "VALUE")
                .unwrap()
                .as_deref(),
            Some("synthetic-custom-secret")
        );
        assert!(!registry
            .servers
            .iter()
            .any(|server| server.name == "unsupported"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
    }

    /// The Import all key of a stdio server with no args.
    fn stdio_key(command: &str, name: &str) -> String {
        let mut entry = server(name);
        entry.command = Some(command.into());
        import_review_key(&entry, name)
    }

    /// The review key of a client entry that has env keys: those keys put the
    /// account name in its identity.
    fn stdio_key_with_env(command: &str, name: &str, env: &[&str]) -> String {
        let mut entry = server(name);
        entry.command = Some(command.into());
        entry.env = env
            .iter()
            .map(|key| registry::EnvVar {
                key: (*key).into(),
                value: None,
                secret: false,
                unknown_fields: Default::default(),
            })
            .collect();
        import_review_key(&entry, name)
    }

    fn detected_client(id: &str) -> clients::DetectedClient {
        clients::detect_clients()
            .into_iter()
            .find(|client| client.id == id)
            .unwrap()
    }

    fn names(servers: Vec<clients::McpServer>) -> Vec<String> {
        servers.into_iter().map(|server| server.name).collect()
    }

    #[test]
    fn same_name_different_connections_import_under_distinct_names() {
        let mut previous = Registry::default();
        let mut github = server("github");
        github.command = Some("npx".into());
        github.args = vec!["-y".into(), "@modelcontextprotocol/server-github".into()];
        previous.add_server(github);
        let fixture = MoveFixture::new(&previous);
        let before = read_registry_exact().unwrap().servers;
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"github":{"type":"http","url":"https://api.githubcopilot.com/mcp/"}}}"#,
        )
        .unwrap();
        std::fs::write(
            fixture.codex(),
            "[mcp_servers.github]\ncommand = \"npx\"\nargs = [\"-y\", \"@acme/github-mcp\"]\n",
        )
        .unwrap();
        let registry = read_registry_exact().unwrap();
        let (claude, codex) = (detected_client("claude-code"), detected_client("codex"));
        let remote = clients::importable_servers(&claude, &registry);
        assert_eq!(names(remote.clone()), ["github (2)"]);
        assert_eq!(
            remote[0].url.as_deref(),
            Some("https://api.githubcopilot.com/mcp/")
        );
        assert_eq!(
            names(clients::importable_servers(&codex, &registry)),
            ["github (2)"]
        );
        assert!(clients::managed_server_names(&claude, &registry).is_empty());

        // Import all offers both, each under its own saved name.
        let review = preview_client_imports().unwrap();
        let mut shown = review
            .iter()
            .map(|item| (item.name.as_str(), item.source_name.as_deref()))
            .collect::<Vec<_>>();
        shown.sort();
        assert_eq!(
            shown,
            [
                ("github (2)", Some("github")),
                ("github (3)", Some("github"))
            ]
        );
        let (_, added) =
            import_client_servers(review.iter().map(|item| item.key.clone()).collect()).unwrap();
        assert_eq!(added, 2);
        let saved = read_registry_exact().unwrap();
        let mut saved_names = saved
            .servers
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>();
        saved_names.sort();
        assert_eq!(saved_names, ["github", "github (2)", "github (3)"]);
        assert_eq!(saved.servers[0], before[0]);
        for client in [&claude, &codex] {
            assert!(clients::importable_servers(client, &saved).is_empty());
            assert_eq!(clients::managed_server_names(client, &saved), ["github"]);
        }
        assert!(preview_client_imports().unwrap().is_empty());
    }

    #[test]
    fn servers_whose_secrets_redact_alike_keep_their_own_ids() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{
                "personal":{"type":"http","url":"https://example.test/mcp?token=synthetic-a"},
                "work":{"type":"http","url":"https://example.test/mcp?token=synthetic-b"},
                "arg-a":{"command":"arg-cmd","args":["--api-key=synthetic-a"]},
                "arg-b":{"command":"arg-cmd","args":["--api-key=synthetic-b"]}}}"#,
        )
        .unwrap();
        let client = detected_client("claude-code");
        let registry = read_registry_exact().unwrap();
        let detected = clients::importable_server_slots(&client, &registry)
            .into_iter()
            .map(|(_, server, id)| (server.name, id))
            .collect::<std::collections::BTreeMap<_, _>>();
        let review = preview_client_imports_for("claude-code").unwrap();
        assert_eq!(review.len(), 4);
        for item in &review {
            // The id never holds the secret, and each row has its own.
            assert!(!item.connection.contains("synthetic"));
            assert_eq!(detected[&item.name], item.connection);
        }
        // Salted: not a plain hash someone could check a guessed token against.
        let personal = review.iter().find(|i| i.name == "personal").unwrap();
        let plain = clients::connection_key(
            "http",
            None,
            &[],
            Some("https://example.test/mcp?token=synthetic-a"),
        );
        assert_ne!(
            personal.connection,
            format!("connection:{}", registry::sha256_hex(&plain))
        );
        let ids = review
            .iter()
            .map(|item| &item.connection)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 4);
        let (_, added) =
            import_client_servers(review.iter().map(|item| item.key.clone()).collect()).unwrap();
        assert_eq!(added, 4);
    }

    #[test]
    fn a_saved_name_that_shifts_after_review_is_refused() {
        let mut previous = Registry::default();
        previous.add_server(server("github"));
        let fixture = MoveFixture::new(&previous);
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"github":{"type":"http","url":"https://api.githubcopilot.com/mcp/"}}}"#,
        )
        .unwrap();
        std::fs::write(
            fixture.codex(),
            "[mcp_servers.github]\ncommand = \"acme-github\"\n[mcp_servers.github.env]\nGITHUB_TOKEN = \"synthetic-token\"\n",
        )
        .unwrap();
        let token_choice = |name: &str| {
            std::collections::BTreeMap::from([(
                name.to_string(),
                std::collections::BTreeMap::from([("GITHUB_TOKEN".to_string(), false)]),
            )])
        };
        let codex_name = |items: &[ClientImportCandidate]| {
            items
                .iter()
                .find(|item| item.client_id == "codex")
                .unwrap()
                .name
                .clone()
        };
        let bulk = preview_client_imports().unwrap();
        let scoped = preview_client_imports_for("codex").unwrap();
        // An unrelated server takes a reviewed name before the import.
        registry::update(|r| {
            let mut taken = server("taken");
            taken.name = "github (2)".into();
            r.add_server(taken);
            Ok(())
        })
        .unwrap();
        let before = read_registry_exact().unwrap();
        let vault = fixture.root.join("data");
        let files = || {
            let mut all = std::fs::read_dir(&vault)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.is_file())
                .map(|p| (p.clone(), std::fs::read(p).unwrap()))
                .collect::<Vec<_>>();
            all.sort();
            all
        };
        let vault_before = files();
        for (items, keys) in [
            (
                &bulk,
                bulk.iter().map(|i| i.key.clone()).collect::<Vec<_>>(),
            ),
            (&scoped, scoped.iter().map(|i| i.key.clone()).collect()),
        ] {
            let error = import_client_servers_inputs(
                keys,
                &token_choice(&codex_name(items)),
                &Default::default(),
            )
            .unwrap_err();
            assert!(error.contains("Server changed"), "{error}");
            assert_eq!(read_registry_exact().unwrap().servers, before.servers);
            assert_eq!(files(), vault_before);
        }
        // Reviewed again, the choice lands on the server it was made for.
        let bulk = preview_client_imports().unwrap();
        let name = codex_name(&bulk);
        let (registry, added) = import_client_servers_inputs(
            bulk.iter().map(|i| i.key.clone()).collect(),
            &token_choice(&name),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(added, 2);
        let codex = registry.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(codex.command.as_deref(), Some("acme-github"));
        assert!(codex
            .env
            .iter()
            .any(|e| e.key == "GITHUB_TOKEN" && !e.secret && e.value.is_some()));
    }

    #[test]
    fn client_import_saves_and_keys_choices_by_the_shown_name() {
        let mut previous = Registry::default();
        previous.add_server(server("github"));
        let fixture = MoveFixture::new(&previous);
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"github":{"command":"npx","args":["-y","@acme/github-mcp"],"env":{"GITHUB_TOKEN":"synthetic-token"}}}}"#,
        )
        .unwrap();
        let review = preview_client_imports_for("claude-code").unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(review[0].name, "github (2)");
        assert_eq!(review[0].source_name.as_deref(), Some("github"));
        let choices = std::collections::BTreeMap::from([(
            "github (2)".into(),
            std::collections::BTreeMap::from([("GITHUB_TOKEN".into(), false)]),
        )]);
        let (registry, added) = import_client_servers_inputs(
            vec![review[0].key.clone()],
            &choices,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(added, 1);
        let imported = registry
            .servers
            .iter()
            .find(|s| s.source.as_deref() == Some("imported:claude-code"))
            .unwrap();
        assert_eq!(imported.name, "github (2)");
        assert!(imported.env.iter().any(|env| env.key == "GITHUB_TOKEN"
            && !env.secret
            && env.value.as_deref() == Some("synthetic-token")));
    }

    #[test]
    fn imported_secret_bearing_servers_are_not_offered_again() {
        let fixture = MoveFixture::new(&Registry::default());
        // Import the secret-bearing entries first, so the registry holds only their
        // redacted URL or `<launch-input>` arg plus vault values.
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{
                "url-same":{"type":"http","url":"https://same.invalid/mcp?token=synthetic-a"},
                "url-unknown":{"type":"http","url":"https://unknown.invalid/mcp?token=synthetic-u"},
                "arg-same":{"command":"arg-cmd","args":["--api-key=synthetic-arg-a"]}}}"#,
        )
        .unwrap();
        let keys = preview_client_imports_for("claude-code")
            .unwrap()
            .into_iter()
            .map(|item| item.key)
            .collect::<Vec<_>>();
        assert_eq!(import_client_servers(keys).unwrap().1, 3);
        let imported = read_registry_exact().unwrap();
        let find = |name: &str| imported.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(find("arg-same").args, ["<launch-input>"]);
        let unknown = find("url-unknown");
        assert_eq!(unknown.url.as_deref(), Some("https://unknown.invalid/mcp"));
        crate::secrets::delete_secret(&unknown.id, crate::secrets::IMPORTED_URL_KEY).unwrap();

        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{
                "new-one":{"command":"new-cmd"},
                "url-same":{"type":"http","url":"https://same.invalid/mcp?token=synthetic-a"},
                "url-changed":{"type":"http","url":"https://same.invalid/mcp?token=synthetic-b"},
                "url-unknown":{"type":"http","url":"https://unknown.invalid/mcp?token=synthetic-u"},
                "arg-same":{"command":"arg-cmd","args":["--api-key=synthetic-arg-a"]}}}"#,
        )
        .unwrap();
        let registry = read_registry_exact().unwrap();
        let client = detected_client("claude-code");
        let unknown = client
            .servers
            .iter()
            .find(|s| s.name == "url-unknown")
            .unwrap();
        assert!(matches!(
            existing_connection(&registry, &client, unknown, Confirm::WithVault),
            ConnectionState::Unknown(_)
        ));
        // Detection never reads the vault: what only the vault could confirm is neither
        // offered nor claimed as in Toolport.
        let ((importable, managed, unconfirmed), reads) =
            crate::secrets::tests::vault_reads_during(|| {
                (
                    names(clients::importable_servers(&client, &registry)),
                    clients::managed_server_names(&client, &registry),
                    clients::unconfirmed_server_slots(&client, &registry),
                )
            });
        assert_eq!(reads, Vec::<String>::new());
        assert_eq!(importable, ["new-one"]);
        assert!(managed.is_empty());
        // They are listed for review under their own names, not dropped.
        let mut listed = unconfirmed
            .iter()
            .map(|(_, server, _)| server.name.as_str())
            .collect::<Vec<_>>();
        listed.sort_unstable();
        assert_eq!(
            listed,
            ["arg-same", "url-changed", "url-same", "url-unknown"]
        );
        let review = preview_client_imports_for("claude-code").unwrap();
        for (_, server, connection) in &unconfirmed {
            let item = review.iter().find(|item| &item.connection == connection);
            // Review lists what still differs and leaves out what is already there.
            assert_eq!(
                item.is_some(),
                server.name == "url-changed" || server.name == "url-unknown",
                "{}",
                server.name
            );
        }
        for name in ["url-same", "arg-same", "url-changed"] {
            let server = client.servers.iter().find(|s| s.name == name).unwrap();
            assert!(matches!(
                existing_connection(&registry, &client, server, Confirm::VaultFree),
                ConnectionState::Unknown(_)
            ));
        }
        // The review the user opens confirms them through the vault.
        for name in ["url-same", "arg-same"] {
            let server = client.servers.iter().find(|s| s.name == name).unwrap();
            assert!(matches!(
                existing_connection(&registry, &client, server, Confirm::WithVault),
                ConnectionState::Existing(_)
            ));
        }
        // Review lists what is new, and shows the unconfirmed one as unsupported.
        let review = preview_client_imports_for("claude-code").unwrap();
        let mut new = review
            .iter()
            .filter(|item| item.unsupported.is_none())
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>();
        new.sort();
        assert_eq!(new, ["new-one", "url-changed"]);
        let unsupported = review
            .iter()
            .filter(|item| item.unsupported.is_some())
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(unsupported, ["url-unknown"]);
    }

    fn add_catalog_server(name: &str, edit: impl FnOnce(&mut ServerEntry)) -> ServerEntry {
        let entry = crate::catalog::search_curated(&name.to_lowercase())
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap();
        add_catalog_entry(entry).unwrap();
        registry::update(|r| {
            let server = r.servers.iter_mut().find(|s| s.name == name).unwrap();
            edit(server);
            Ok(server.clone())
        })
        .unwrap()
        .1
    }

    fn launch_input<'a>(
        server: &'a mut ServerEntry,
        key: &str,
    ) -> &'a mut crate::registry::LaunchInput {
        server
            .launch
            .as_mut()
            .unwrap()
            .inputs
            .iter_mut()
            .find(|input| input.key == key)
            .unwrap()
    }

    #[test]
    fn a_catalog_launch_bound_server_is_already_in_toolport() {
        let fixture = MoveFixture::new(&Registry::default());
        let stored = add_catalog_server("Filesystem", |server| {
            launch_input(server, "ALLOWED_DIRECTORY").value = Some("/tmp".into());
        });
        assert_eq!(stored.args[2], "<launch-input>");
        let mut argv = stored.args.clone();
        argv[2] = "/tmp".into();
        // A different name on purpose: identity is the connection, not the name.
        std::fs::write(
            fixture.claude(),
            serde_json::json!({"mcpServers":{"files":{"command": stored.command, "args": argv}}})
                .to_string(),
        )
        .unwrap();
        let registry = read_registry_exact().unwrap();
        let client = detected_client("claude-code");
        assert_eq!(
            existing_connection(&registry, &client, &client.servers[0], Confirm::VaultFree),
            ConnectionState::Existing(stored.id.clone())
        );
        assert!(clients::importable_servers(&client, &registry).is_empty());
        assert_eq!(clients::managed_server_names(&client, &registry), ["files"]);
        assert!(preview_client_imports_for("claude-code")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_different_known_fragment_rules_out_a_secret_bound_server_without_the_vault() {
        let fixture = MoveFixture::new(&Registry::default());
        // Twilio binds one argument as ACCOUNT_SID/API_KEY:API_SECRET. Only the SID is
        // stored; both secrets are missing from the vault.
        let stored = add_catalog_server("Twilio", |server| {
            launch_input(server, "TWILIO_ACCOUNT_SID").value = Some("AC111".into());
        });
        let bound = |sid: &str| {
            let mut argv = stored.args.clone();
            argv[2] = format!("{sid}/SKsynthetic:synthetic-secret");
            argv
        };
        std::fs::write(
            fixture.claude(),
            serde_json::json!({"mcpServers":{"twilio":{
                "command": stored.command,
                "args": bound("AC222"),
            }}})
            .to_string(),
        )
        .unwrap();
        let registry = read_registry_exact().unwrap();
        let client = detected_client("claude-code");
        // A different SID is a different connection: never a candidate, so the missing
        // secrets are never read and nothing is refused.
        assert!(connection_candidates(&registry, &client.servers[0]).is_empty());
        assert_eq!(
            existing_connection(&registry, &client, &client.servers[0], Confirm::WithVault),
            ConnectionState::New
        );
        assert_eq!(clients::importable_servers(&client, &registry).len(), 1);
        // The same SID keeps it as a candidate; its secrets decide, and they are missing.
        let mut same = client.servers[0].clone();
        same.args = bound("AC111");
        assert_eq!(connection_candidates(&registry, &same).len(), 1);
        assert!(matches!(
            existing_connection(&registry, &client, &same, Confirm::WithVault),
            ConnectionState::Unknown(_)
        ));
    }

    #[test]
    fn fragments_match_known_chunks_in_order_with_any_text_between() {
        let known = |s: &str| Some(s.to_string());
        let twilio = [known("AC111"), known("/"), None, known(":"), None];
        assert!(fragments_match(&twilio, "AC111/SK1:secret"));
        assert!(fragments_match(&twilio, "AC111/:"));
        assert!(!fragments_match(&twilio, "AC222/SK1:secret"));
        assert!(!fragments_match(&twilio, "AC111/SK1"));
        let ends = [known("ab"), None, known("ba")];
        assert!(fragments_match(&ends, "ab-ba"));
        assert!(!fragments_match(&ends, "aba"));
        let middle = [None, known("a"), None, known("b"), None];
        assert!(fragments_match(&middle, "xaxbx"));
        assert!(!fragments_match(&middle, "ba"));
        assert!(fragments_match(&[known("/tmp")], "/tmp"));
        assert!(!fragments_match(&[known("/tmp")], "/tmp/x"));
        assert!(fragments_match(&[None], ""));
    }

    #[test]
    fn distinct_import_names_skip_every_taken_name() {
        let taken = ["GitHub".to_string(), "github (2)".to_string()];
        assert_eq!(
            clients::distinct_import_name("github", &taken),
            "github (3)"
        );
        assert_eq!(clients::distinct_import_name("linear", &taken), "linear");
    }

    #[test]
    fn reviewed_new_import_never_uses_an_orphan_credential() {
        let fixture = MoveFixture::new(&Registry::default());
        crate::secrets::set_secret("one", "PAT", "stale-orphan").unwrap();
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"${PAT}"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(review.items[0].is_new);
        assert!(!review.items[0].credentials[0].present);
        let result = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| panic!("orphan must not satisfy a missing credential"),
        );
        assert!(result.is_err());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PAT")
                .unwrap()
                .as_deref(),
            Some("stale-orphan")
        );
    }

    #[test]
    fn reviewed_failed_import_restores_orphan_and_concurrent_edit() {
        let fixture = MoveFixture::new(&Registry::default());
        crate::secrets::set_secret("one", "PAT", "stale-orphan").unwrap();
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"fresh-value"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| {
                registry::update(|r| {
                    r.add_server(server("concurrent"));
                    Ok(())
                })
                .unwrap();
                Err("Launch failed".into())
            }
        )
        .is_err());
        let saved = read_registry_exact().unwrap();
        assert!(!saved.servers.iter().any(|s| s.id == "one"));
        assert!(saved.servers.iter().any(|s| s.id == "concurrent"));
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PAT")
                .unwrap()
                .as_deref(),
            Some("stale-orphan")
        );
    }

    /// F29: a Clients scan that writes another client's records during a connect (a
    /// deferred repair finishing after an unlock) no longer fails it, and keeps them.
    #[test]
    fn reviewed_connect_keeps_another_clients_records_written_meanwhile() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let other: crate::registry::ManagedEntry =
            serde_json::from_value(serde_json::json!({"command": "toolport-gateway"})).unwrap();
        let written = other.clone();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            move |_, _, _, _| {
                registry::update(|r| {
                    r.set_client_managed_entry("codex@t3-home", written.clone());
                    r.set_client_unscoped("codex@t3-home");
                    Ok(())
                })
                .unwrap();
                Ok(Vec::new().into())
            },
        )
        .unwrap();
        let saved = read_registry_exact().unwrap();
        assert!(saved.servers.iter().any(|s| s.name == "one"));
        assert_eq!(
            saved.client_managed_entries.get("codex@t3-home"),
            Some(&other)
        );
        assert_eq!(
            saved.client_scopes.get("codex@t3-home").map(String::as_str),
            Some("")
        );
    }

    /// A server changed during the connect still stops it before the client is rewritten.
    #[test]
    fn reviewed_connect_still_stops_when_servers_change_meanwhile() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| {
                registry::update(|r| {
                    r.servers
                        .iter_mut()
                        .find(|s| s.name == "one")
                        .unwrap()
                        .command = Some("changed".into());
                    Ok(())
                })
                .unwrap();
                Ok(Vec::new().into())
            },
        )
        .unwrap_err();
        assert!(error.contains("Servers changed during setup"), "{error}");
        assert_eq!(
            std::fs::read_to_string(fixture.claude()).unwrap(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#
        );
    }

    /// A change to the connecting client's own records during verification still stops
    /// the connect and leaves its config alone.
    fn assert_own_record_change_stops_the_connect(change: fn(&mut Registry)) {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            move |_, _, _, _| {
                registry::update(|r| {
                    change(r);
                    Ok(())
                })
                .unwrap();
                Ok(Vec::new().into())
            },
        )
        .unwrap_err();
        assert!(error.contains("Servers changed during setup"), "{error}");
        assert_eq!(
            std::fs::read_to_string(fixture.claude()).unwrap(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#
        );
    }

    #[test]
    fn reviewed_connect_still_stops_when_its_own_managed_entry_changes_meanwhile() {
        assert_own_record_change_stops_the_connect(|r| {
            let entry: crate::registry::ManagedEntry =
                serde_json::from_value(serde_json::json!({"command": "toolport-gateway"})).unwrap();
            r.set_client_managed_entry("claude-code", entry);
        });
    }

    #[test]
    fn reviewed_connect_still_stops_when_its_own_scope_changes_meanwhile() {
        assert_own_record_change_stops_the_connect(|r| r.set_client_unscoped("claude-code"));
    }

    #[test]
    fn reviewed_conflict_restores_other_existing_server_secret() {
        let mut previous = Registry::default();
        let mut a = server("a");
        a.command = Some("a".into());
        a.env.push(crate::registry::EnvVar {
            key: "PAT".into(),
            value: None,
            secret: true,
            unknown_fields: Default::default(),
        });
        previous.add_server(a);
        let fixture = MoveFixture::new(&previous);
        crate::secrets::set_secret("a", "PAT", "old").unwrap();
        std::fs::write(fixture.claude(), r#"{"mcpServers":{"a":{"command":"a","env":{"PAT":"new"}},"b":{"command":"b","env":{"PAT":"new-b"}}}}"#).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["a".into(), "b".into()],
            &review.revision,
            |_, _, _, _| {
                registry::update(|r| {
                    r.servers.iter_mut().find(|s| s.id == "b").unwrap().name =
                        "Concurrent B".into();
                    Ok(())
                })
                .unwrap();
                Err("Launch failed".into())
            },
        )
        .unwrap_err();
        assert!(error.contains("Concurrent edits"));
        assert_eq!(
            read_registry_exact()
                .unwrap()
                .servers
                .iter()
                .find(|s| s.id == "a")
                .unwrap(),
            &previous.servers[0]
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("a", "PAT")
                .unwrap()
                .as_deref(),
            Some("old")
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("b", "PAT")
                .unwrap()
                .as_deref(),
            Some("new-b")
        );
    }

    #[test]
    fn reviewed_existing_vault_value_is_present_and_required() {
        let mut previous = Registry::default();
        previous.add_server(server("one"));
        let fixture = MoveFixture::new(&previous);
        crate::secrets::set_secret("one", "PAT", "old").unwrap();
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"${PAT}"}}}}"#,
        )
        .unwrap();
        let review = serde_json::to_value(preview_client_setup("claude-code").unwrap()).unwrap();
        assert_eq!(review["items"][0]["credentials"][0]["present"], true);
        assert_eq!(review["items"][0]["credentials"][0]["required"], true);
    }

    #[test]
    fn reviewed_conflicting_registry_edit_keeps_staged_vault_values() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"fresh-value"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| {
                registry::update(|r| {
                    r.servers[0].name = "Edited during verification".into();
                    Ok(())
                })
                .unwrap();
                Err("Launch failed".into())
            }
        )
        .is_err());
        assert_eq!(
            read_registry_exact().unwrap().servers[0].name,
            "Edited during verification"
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PAT")
                .unwrap()
                .as_deref(),
            Some("fresh-value")
        );
    }

    #[test]
    fn reviewed_existing_remote_keeps_vaulted_url_reference() {
        let mut reg = Registry::default();
        let mut one = server("one");
        one.command = None;
        one.transport = "http".into();
        one.url = Some("https://example.invalid/mcp?token=synthetic-url-secret".into());
        one.unknown_fields
            .insert("custom".into(), serde_json::json!(true));
        reg.add_server(one);
        let fixture = MoveFixture::new(&reg);
        std::fs::write(fixture.claude(), serde_json::json!({"mcpServers":{"one":{"url":"https://example.invalid/mcp?token=synthetic-url-secret"}}}).to_string()).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |registry, _, _, _| {
                let entry = &registry.servers[0];
                assert!(
                    crate::import_credentials::has_imported_url(entry),
                    "updated URL lost its vault reference"
                );
                assert_eq!(entry.unknown_fields["custom"], true);
                assert_eq!(entry.url.as_deref(), Some("https://example.invalid/mcp"));
                assert_eq!(
                    crate::secrets::get_vault_secret_result(
                        &entry.id,
                        crate::secrets::IMPORTED_URL_KEY
                    )
                    .unwrap()
                    .as_deref(),
                    Some("https://example.invalid/mcp?token=synthetic-url-secret")
                );
                Ok(Vec::new().into())
            },
        )
        .unwrap();
    }

    #[test]
    fn reviewed_same_definition_discloses_changed_settings() {
        let mut reg = Registry::default();
        let mut one = server("one");
        one.command = Some("one".into());
        one.env.push(crate::registry::EnvVar {
            key: "PORT".into(),
            value: Some("3000".into()),
            secret: false,
            unknown_fields: Default::default(),
        });
        reg.add_server(one);
        let fixture = MoveFixture::new(&reg);
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PORT":"4000"}}}}"#,
        )
        .unwrap();
        let review = serde_json::to_value(preview_client_setup("claude-code").unwrap()).unwrap();
        assert!(
            review["items"][0]["updates"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|f| f == "Environment")),
            "changed environment must be disclosed: {review}"
        );
    }

    #[test]
    fn reviewed_vault_rolls_back_prior_values_and_late_batch_failure() {
        let _fixture = MoveFixture::new(&Registry::default());
        crate::secrets::set_secret("one", "PAT", "synthetic-prior").unwrap();
        let first = crate::import_credentials::Import::prepare(
            server("one"),
            Some(&serde_json::json!({"env":{"PAT":"synthetic-new"}})),
        )
        .unwrap();
        let second = crate::import_credentials::Import::prepare(
            server("two"),
            Some(&serde_json::json!({"env":{"TOKEN":"synthetic-second"}})),
        )
        .unwrap();
        let result = crate::secrets::tests::with_failed_write("TOKEN", || {
            crate::import_credentials::transaction(|writes| {
                first.transfer_into("one", writes, false)?;
                second.transfer_into("two", writes, false)?;
                Ok(())
            })
        });
        assert!(result.is_err());
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PAT")
                .unwrap()
                .as_deref(),
            Some("synthetic-prior")
        );
        assert!(crate::secrets::get_vault_secret_result("two", "TOKEN")
            .unwrap()
            .is_none());
    }

    #[test]
    fn reviewed_verification_releases_both_mutation_locks() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| {
                use fs2::FileExt;
                for path in [
                    fixture.root.join("data/client-config-mutation.lock"),
                    fixture.root.join("data/registry.json.lock"),
                ] {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(path)
                        .unwrap();
                    file.try_lock_exclusive()
                        .expect("vault/transport work must run outside mutation locks");
                }
                Ok(Vec::new().into())
            },
        )
        .unwrap();
    }

    #[test]
    fn reviewed_legacy_secret_definitions_match_without_masked_comparison() {
        let mut reg = Registry::default();
        let mut raw = server("one");
        raw.transport = "http".into();
        raw.command = None;
        raw.url = Some("https://example.invalid/mcp?token=synthetic-value".into());
        reg.add_server(raw);
        let fixture = MoveFixture::new(&reg);
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"url":"https://example.invalid/mcp?token=synthetic-value"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| Ok(Vec::new().into()),
        )
        .unwrap();
        let saved = read_registry_exact().unwrap();
        assert_eq!(saved.servers.len(), 1);
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("synthetic-value"));
    }

    #[test]
    fn reviewed_legacy_secret_arguments_match_without_masked_comparison() {
        let mut registry = Registry::default();
        let mut raw = server("one");
        raw.command = Some("one".into());
        raw.args = vec!["--api-key".into(), "synthetic-private-argument".into()];
        registry.add_server(raw);
        let fixture = MoveFixture::new(&registry);
        std::fs::write(fixture.claude(), r#"{"mcpServers":{"one":{"command":"one","args":["--api-key","synthetic-private-argument"]}}}"#).unwrap();
        let preview = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &preview.revision,
            |_, _, _, _| Ok(Vec::new().into()),
        )
        .unwrap();
        let saved = read_registry_exact().unwrap();
        assert_eq!(saved.servers.len(), 1);
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("synthetic-private-argument"));
    }

    #[test]
    fn reviewed_missing_inputs_are_transactional_and_retryable() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"TOKEN":"${TOKEN}"}}}}"#,
        )
        .unwrap();
        let preview = preview_client_setup("claude-code").unwrap();
        let inputs = std::collections::BTreeMap::from([(
            "one".into(),
            std::collections::BTreeMap::from([("TOKEN".into(), "synthetic-input-secret".into())]),
        )]);
        let attempt = |succeed| {
            migrate_client_reviewed_inputs_with(
                "claude-code",
                None,
                false,
                &["one".into()],
                &preview.revision,
                &Default::default(),
                &inputs,
                |_, _, _, _| {
                    if succeed {
                        Ok(Vec::new().into())
                    } else {
                        Err("one could not start".into())
                    }
                },
            )
        };
        assert!(attempt(false).is_err());
        assert!(crate::secrets::get_vault_secret_result("one", "TOKEN")
            .unwrap()
            .is_none());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert!(attempt(true).is_ok());
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "TOKEN")
                .unwrap()
                .as_deref(),
            Some("synthetic-input-secret")
        );
        assert!(!serde_json::to_string(&read_registry_exact().unwrap())
            .unwrap()
            .contains("synthetic-input-secret"));
    }

    #[test]
    fn reviewed_unsupported_server_stays_native_while_supported_selection_connects() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(),r#"{"mcpServers":{"one":{"command":"one"},"unsupported":{"url":"https://example.invalid/mcp","headers":{"X-API-Key":"synthetic-private"}}}}"#).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(review
            .items
            .iter()
            .find(|s| s.name == "unsupported")
            .unwrap()
            .unsupported
            .is_some());
        let result = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into(), "unsupported".into()],
            &review.revision,
            |_, moved, _, _| {
                assert_eq!(moved, ["one"]);
                Ok(Vec::new().into())
            },
        )
        .unwrap();
        assert_eq!(result.moved, ["one"]);
        assert!(json_file(&fixture.claude())["mcpServers"]
            .get("unsupported")
            .is_some());
        assert!(!serde_json::to_string(&review)
            .unwrap()
            .contains("synthetic-private"));
    }

    #[test]
    fn reviewed_plain_env_can_be_explicitly_vaulted() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PORT":"3000"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let choices = std::collections::BTreeMap::from([(
            "one".into(),
            std::collections::BTreeMap::from([("PORT".into(), true)]),
        )]);
        migrate_client_reviewed_choices_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            &choices,
            |_, _, _, _| Ok(Vec::new().into()),
        )
        .unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PORT")
                .unwrap()
                .as_deref(),
            Some("3000")
        );
        assert!(read_registry_exact().unwrap().servers[0].env[0].secret);
    }

    #[test]
    fn reviewed_failed_setup_rolls_back_vault_and_allows_changed_retry() {
        let fixture = MoveFixture::new(&Registry::default());
        let original =
            r#"{"mcpServers":{"one":{"command":"fixture","env":{"PAT":"synthetic-first"}}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| Err("Launch failed".into())
        )
        .is_err());
        assert!(crate::secrets::get_vault_secret_result("one", "PAT")
            .unwrap()
            .is_none());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        std::fs::write(
            fixture.claude(),
            original.replace("synthetic-first", "synthetic-retry"),
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| Ok(Vec::new().into()),
        )
        .unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result("one", "PAT")
                .unwrap()
                .as_deref(),
            Some("synthetic-retry")
        );
    }

    #[test]
    fn reviewed_multi_paste_is_atomic_on_invalid_later_server() {
        let _fixture = MoveFixture::new(&Registry::default());
        let text = r#"{"mcpServers":{"first":{"command":"fixture","env":{"PAT":"synthetic-first"}},"second":{"command":"fixture","env":{"BAD=NAME":"synthetic-second"}}}}"#;
        assert!(add_snippet_servers(text, &["0".into(), "1".into()]).is_err());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert!(crate::secrets::get_vault_secret_result("first", "PAT")
            .unwrap()
            .is_none());
    }

    #[test]
    fn startup_timeout_fields_validate_save_clear_and_preserve() {
        let mut registry = Registry::default();
        let mut input = fields("slow", "stdio");
        input.initialize_timeout_ms = Some(Some(125_000));
        let id = apply_add_server(&mut registry, input.clone()).unwrap();
        assert_eq!(registry.servers[0].initialize_timeout_ms, Some(125_000));
        input.initialize_timeout_ms = None;
        apply_update_server_fields(&mut registry, &id, input.clone()).unwrap();
        assert_eq!(registry.servers[0].initialize_timeout_ms, Some(125_000));
        input.initialize_timeout_ms = Some(None);
        apply_update_server_fields(&mut registry, &id, input.clone()).unwrap();
        assert_eq!(registry.servers[0].initialize_timeout_ms, None);
        for timeout in [0, 86_400_001] {
            input.initialize_timeout_ms = Some(Some(timeout));
            assert!(apply_update_server_fields(&mut registry, &id, input.clone()).is_err());
        }
    }

    #[test]
    fn reviewed_import_vaults_values_and_failed_vault_keeps_native_config() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"one":{"command":"fixture","env":{"PAT":"synthetic-native-pat","PORT":3000}}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let (registry, count) =
            import_client_servers(vec![stdio_key_with_env("fixture", "one", &["PAT", "PORT"])])
                .unwrap();
        assert_eq!(count, 1);
        let entry = &registry.servers[0];
        assert!(entry.enabled);
        assert_eq!(
            crate::secrets::get_vault_secret_result(&entry.id, "PAT")
                .unwrap()
                .as_deref(),
            Some("synthetic-native-pat")
        );
        assert!(crate::secrets::get_vault_secret_result(&entry.id, "PORT")
            .unwrap()
            .is_none());
        assert_eq!(
            entry
                .env
                .iter()
                .find(|e| e.key == "PORT")
                .unwrap()
                .value
                .as_deref(),
            Some("3000")
        );
        let saved = serde_json::to_string(&registry).unwrap();
        assert!(!saved.contains("synthetic-native-pat"));
        assert!(
            !crate::sharing_controller::build_export(&registry, None, None, None)
                .to_string()
                .contains("synthetic-native-pat")
        );
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        assert_eq!(
            import_client_servers(vec![stdio_key_with_env("fixture", "one", &["PAT", "PORT"])])
                .unwrap()
                .1,
            0
        );
        let before = preview_client_setup("claude-code").unwrap();
        std::fs::write(fixture.root.join("data/secrets.enc"), "unreadable-vault").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &before.revision,
            |_, _, _, _| panic!("vault failure must stop verification"),
        )
        .unwrap_err();
        assert!(error.contains("Keychain unavailable"));
        assert!(!error.contains("synthetic-native-pat"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        assert!(!fixture.move_record("claude-code").exists());
    }

    #[test]
    fn reviewed_missing_input_keeps_native_config_and_does_not_claim_success() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"one":{"command":"fixture","env":{"PAT":"${PAT}"}}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| panic!("missing input must stop verification"),
        )
        .unwrap_err();
        assert!(error.contains("needs credentials"));
        assert!(!error.contains("Keychain unavailable"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        let (registry, _) =
            import_client_servers(vec![stdio_key_with_env("fixture", "one", &["PAT"])]).unwrap();
        assert!(!registry.servers[0].enabled);
        assert!(registry.servers[0].env[0].value.is_none());
    }

    #[test]
    fn reviewed_setup_moves_only_selected_and_returns_gateway_tools() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(), r#"{"mcpServers":{"chosen":{"command":"chosen"},"kept":{"command":"kept","custom":true,"env":{"PAT":"native-only"}}}}"#).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let outcome = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["chosen".into()],
            &review.revision,
            |_, moved, _, _| {
                assert_eq!(moved, &["chosen"]);
                Ok(vec![serde_json::json!({"name":"chosen__read"})].into())
            },
        )
        .unwrap();
        let after = json_file(&fixture.claude());
        assert!(after["mcpServers"].get("chosen").is_none());
        assert_eq!(after["mcpServers"]["kept"]["env"]["PAT"], "native-only");
        assert_eq!(after["mcpServers"]["kept"]["custom"], true);
        assert_eq!(outcome.tools[0]["name"], "chosen__read");
        assert!(outcome.result.outcome.backup.is_some());
    }

    #[test]
    fn reviewed_setup_tolerates_unrelated_client_state_and_records_only_moved() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(), r#"{"numStartups":1,"mcpServers":{"chosen":{"command":"chosen"},"kept":{"command":"kept"}}}"#).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        std::fs::write(fixture.claude(), r#"{"numStartups":2,"mcpServers":{"chosen":{"command":"chosen"},"kept":{"command":"kept"}}}"#).unwrap();
        migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["chosen".into()],
            &review.revision,
            |_, _, _, _| Ok(Vec::new().into()),
        )
        .unwrap();
        let record = json_file(&fixture.move_record("claude-code"));
        assert_eq!(record["entries"].as_array().unwrap().len(), 1);
        assert_eq!(record["entries"][0]["name"], "chosen");
        let mut config = json_file(&fixture.claude());
        config["mcpServers"].as_object_mut().unwrap().remove("kept");
        std::fs::write(fixture.claude(), config.to_string()).unwrap();
        disconnect_client("claude-code").unwrap();
        let restored = json_file(&fixture.claude());
        assert!(restored["mcpServers"].get("kept").is_none());
        assert_eq!(restored["numStartups"], 2);
    }

    #[test]
    fn reviewed_setup_failed_gateway_leaves_native_bytes_intact() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{ "mcpServers": {"broken":{"command":"missing-command"}} }"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["broken".into()],
            &review.revision,
            |_, _, _, _| Err("Launch failed".into()),
        )
        .unwrap_err();
        assert_eq!(error, "Launch failed");
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        assert!(!fixture.move_record("claude-code").exists());
    }

    /// Connect all, one client failing part way: the client connected before it stays
    /// connected with its own backup, and the failing client's config is left byte for
    /// byte as it was, with no move record and nothing of it in the registry.
    #[test]
    fn a_connect_all_step_that_fails_leaves_earlier_clients_and_its_own_config_whole() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"first":{"command":"first"}}}"#,
        )
        .unwrap();
        let codex = "[mcp_servers.second]\ncommand = \"second\"\n";
        std::fs::write(fixture.codex(), codex).unwrap();
        let first = preview_client_setup("claude-code").unwrap();
        let second = preview_client_setup("codex").unwrap();
        let connected = connect_step(false, || {
            migrate_client_reviewed_with(
                "claude-code",
                None,
                false,
                &["first".into()],
                &first.revision,
                |_, _, _, _| Ok(Vec::new().into()),
            )
        })
        .unwrap();
        let after_first = std::fs::read_to_string(fixture.claude()).unwrap();
        let error = connect_step(false, || {
            migrate_client_reviewed_with(
                "codex",
                None,
                false,
                &["second".into()],
                &second.revision,
                |_, _, _, _| Err("Launch failed".into()),
            )
        })
        .unwrap_err();
        assert_eq!(error, "Launch failed");
        // The failing client: untouched, with nothing recorded for it.
        assert_eq!(std::fs::read_to_string(fixture.codex()).unwrap(), codex);
        assert!(!fixture.move_record("codex").exists());
        let registry = read_registry_exact().unwrap();
        assert!(registry
            .servers
            .iter()
            .all(|server| server.name != "second"));
        // The client before it: still connected, with its own backup.
        assert_eq!(
            std::fs::read_to_string(fixture.claude()).unwrap(),
            after_first
        );
        assert!(json_file(&fixture.claude())["mcpServers"]
            .get("first")
            .is_none());
        assert!(registry.servers.iter().any(|server| server.name == "first"));
        let backup = connected.result.outcome.backup.expect("its own backup");
        assert!(std::path::Path::new(&backup)
            .starts_with(fixture.root.join("data/backups/claude-code")));
        assert!(fixture.move_record("claude-code").exists());
    }

    /// Connect all with the keyring locked asks at most once: the step that may ask
    /// raises one prompt and, refused, fails; the steps after it are quiet, raise none,
    /// change nothing, and say to unlock the keyring and choose Retry.
    #[test]
    fn a_locked_keyring_is_asked_about_once_in_a_connect_all_run() {
        let fixture = MoveFixture::new(&Registry::default());
        // Each server has a value Toolport moves into the keyring when it connects.
        let claude =
            r#"{"mcpServers":{"first":{"command":"first","env":{"PAT":"synthetic-first"}}}}"#;
        let codex = "[mcp_servers.second]\ncommand = \"second\"\n\n[mcp_servers.second.env]\nPAT = \"synthetic-second\"\n";
        std::fs::write(fixture.claude(), claude).unwrap();
        std::fs::write(fixture.codex(), codex).unwrap();
        let first = preview_client_setup("claude-code").unwrap();
        let second = preview_client_setup("codex").unwrap();
        let attempt = |quiet: bool, client: &str, name: &str, revision: &str| {
            connect_step(quiet, || {
                migrate_client_reviewed_with(
                    client,
                    None,
                    false,
                    &[name.into()],
                    revision,
                    |_, _, _, _| Ok(Vec::new().into()),
                )
            })
            .err()
        };
        let mut errors = Vec::new();
        let prompts = crate::secrets::locked_store::write_prompts_during(|| {
            // The first step is the user's own and may ask; the prompt is dismissed.
            let asked = attempt(false, "claude-code", "first", &first.revision).unwrap();
            assert!(keyring_locked_after(&asked), "{asked}");
            // The rest of the run is quiet, including another try of the first step.
            errors.push(attempt(true, "codex", "second", &second.revision));
            errors.push(attempt(true, "claude-code", "first", &first.revision));
        });
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        for error in errors {
            assert_eq!(
                error.as_deref(),
                Some("Unlock your keyring, then choose Retry.")
            );
        }
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), claude);
        assert_eq!(std::fs::read_to_string(fixture.codex()).unwrap(), codex);
        assert!(!fixture.move_record("claude-code").exists());
        assert!(!fixture.move_record("codex").exists());
    }

    /// A quiet step says to unlock the keyring only when the lock caused its failure; any
    /// other error, with the store locked or not, is shown as it is.
    #[test]
    fn a_quiet_step_names_the_keyring_only_when_it_caused_the_failure() {
        let unlock = Some("Unlock your keyring, then choose Retry.".to_string());
        let changed = "Client config changed. Review it again.";
        let vault = "Keychain unavailable. Unlock it and retry importing. Client config unchanged.";
        let _fixture = MoveFixture::new(&Registry::default());
        let fail = |error: &str| connect_step(true, || Err::<(), _>(error.to_string())).err();
        crate::secrets::locked_store::write_prompts_during(|| {
            assert_eq!(fail(changed).as_deref(), Some(changed));
            assert_eq!(fail(vault), unlock);
            assert_eq!(
                fail("the keyring is locked; this background write waits for it"),
                unlock
            );
        });
        // A rollback that kept something keeps saying so after the instruction.
        crate::secrets::locked_store::write_prompts_during(|| {
            let kept = format!("{vault} {ROLLBACK_KEPT_EDITS}");
            assert_eq!(
                fail(&kept),
                Some(format!("Unlock your keyring, then choose Retry.\n\n{kept}"))
            );
        });
        // Unlocked, a vault failure is not blamed on the keyring.
        assert_eq!(fail(vault).as_deref(), Some(vault));
        assert_eq!(fail(changed).as_deref(), Some(changed));
    }

    /// A quiet step with the real gateway check: the check probes the server on a thread
    /// of its own, which keeps the step's quiet gate, so a remote server with nothing to
    /// store raises no unlock prompt from there either. A step that may ask still asks.
    #[test]
    fn a_quiet_step_keeps_the_gateway_check_from_asking() {
        let fixture = MoveFixture::new(&Registry::default());
        let codex = "[mcp_servers.second]\nurl = \"http://127.0.0.1:9/mcp\"\n";
        std::fs::write(fixture.codex(), codex).unwrap();
        let review = preview_client_setup("codex").unwrap();
        let attempt = |quiet: bool| {
            connect_step(quiet, || {
                migrate_client_reviewed_inputs(
                    "codex",
                    None,
                    false,
                    &["second".into()],
                    &review.revision,
                    &Default::default(),
                    &Default::default(),
                )
            })
            .err()
        };
        let mut quiet = None;
        let prompts = crate::secrets::locked_store::prompts_during(|| quiet = Some(attempt(true)));
        assert!(
            prompts.is_empty(),
            "a quiet step asked to unlock the keyring: {prompts:?}"
        );
        assert_eq!(
            quiet.flatten().as_deref(),
            Some("Unlock your keyring, then choose Retry.")
        );
        assert_eq!(std::fs::read_to_string(fixture.codex()).unwrap(), codex);
        // The gate is only inherited: a step that may ask still reaches the keyring.
        let prompts = crate::secrets::locked_store::prompts_during(|| {
            attempt(false);
        });
        assert!(!prompts.is_empty(), "a step that may ask was made quiet");
    }

    /// A connect that took effect stays connected when the old HTTP token can't be
    /// removed from the keychain; the leftover token is reported as a warning, as
    /// Disconnect reports it. The token's registry row is gone, so it is unusable.
    #[test]
    fn a_token_left_in_the_keychain_is_a_warning_not_a_failed_connect() {
        let mut registry = Registry::default();
        registry.http_clients.push(crate::registry::HttpClient {
            id: "client:claude-code".into(),
            label: "Claude Code".into(),
            token_sha256: "old-hash".into(),
            profile: String::new(),
            unknown_fields: Default::default(),
        });
        let fixture = MoveFixture::new(&registry);
        let gateway = serde_json::json!({"url": "http://127.0.0.1:8765/mcp",
            "headers": {"Authorization": "Bearer old-token"}});
        std::fs::write(
            fixture.claude(),
            serde_json::to_string(&serde_json::json!({"mcpServers": {"toolport": gateway}}))
                .unwrap(),
        )
        .unwrap();
        let mut result = None;
        crate::secrets::locked_store::prompts_during(|| {
            result = Some(connect_client_stdio("claude-code", Some("default"), true));
        });
        let result = result.unwrap().expect("the connect took effect");
        assert!(
            result
                .outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("could not remove the old keychain token")),
            "{:?}",
            result.outcome.warnings
        );
        assert!(read_registry_exact().unwrap().http_clients.is_empty());
        let config = json_file(&fixture.claude());
        assert!(config["mcpServers"]["toolport"]["command"].is_string());
        assert!(config["mcpServers"]["toolport"].get("url").is_none());
    }

    /// F25: a Connect all step's review that asks to unlock the keyring, dismissed, is the
    /// run's one ask. The review says so, and that step's Connect (quiet, as the batch
    /// then makes it) asks nothing more and says to unlock and choose Retry. Unlocked, the
    /// review asks nothing and says nothing. A review that reads several stored values asks
    /// once, and a locked default collection with nothing to unlock is no ask.
    #[test]
    fn a_review_that_asked_makes_its_steps_connect_quiet() {
        let fixture = MoveFixture::new(&Registry::default());
        // The server has a value Toolport moves into the keyring when it connects.
        let claude = r#"{"mcpServers":{"second":{"command":"second","env":{"PAT":"synthetic"}}}}"#;
        std::fs::write(fixture.claude(), claude).unwrap();
        // A review that reads stored values (two servers' here) and still opens when those
        // reads are refused.
        let read = || {
            let _ = crate::secrets::get_vault_secret_result("second", "PAT");
            let _ = crate::secrets::get_vault_secret_result("third", "TOKEN");
            preview_client_setup("claude-code")
        };
        assert!(!review_step(false, read).unwrap().keyring_asked);
        // Only the collection a new item would go into is locked: the reads find nothing to
        // unlock, so nothing asked.
        let prompts = crate::secrets::locked_store::write_prompts_during(|| {
            assert!(!review_step(false, read).unwrap().keyring_asked);
        });
        assert!(prompts.is_empty(), "{prompts:?}");
        // A quiet review (a later step's) asks nothing, so it says nothing.
        let prompts = crate::secrets::locked_store::prompts_during(|| {
            assert!(!review_step(true, read).is_ok_and(|review| review.keyring_asked));
        });
        assert!(prompts.is_empty(), "{prompts:?}");
        let mut review = None;
        let prompts = crate::secrets::locked_store::prompts_during(|| {
            review = Some(review_step(false, read));
        });
        assert_eq!(prompts, ["second/PAT"], "the review asks once");
        let review = review.unwrap().unwrap();
        assert!(review.keyring_asked);
        let mut error = None;
        let prompts = crate::secrets::locked_store::prompts_during(|| {
            error = connect_step(true, || {
                migrate_client_reviewed_with(
                    "claude-code",
                    None,
                    false,
                    &["second".into()],
                    &review.revision,
                    |_, _, _, _| Ok(Vec::new().into()),
                )
            })
            .err();
        });
        assert!(
            prompts.is_empty(),
            "the step's Connect asked again: {prompts:?}"
        );
        assert_eq!(
            error.as_deref(),
            Some("Unlock your keyring, then choose Retry.")
        );
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), claude);
    }

    #[test]
    fn reviewed_rollback_does_not_merge_positional_arguments() {
        for (previous, staged, latest) in [
            (
                serde_json::json!(["--a", "x", "--a", "y"]),
                serde_json::json!(["--a", "new", "--a", "y"]),
                serde_json::json!(["--a", "new", "--a", "z"]),
            ),
            (
                serde_json::json!(["a", "b"]),
                serde_json::json!(["b", "a"]),
                serde_json::json!(["b", "a", "c"]),
            ),
        ] {
            let mut concurrent = latest.clone();
            assert!(!undo_staged_value(&mut concurrent, &previous, &staged));
            assert_eq!(concurrent, latest);
            let mut unchanged = staged.clone();
            assert!(undo_staged_value(&mut unchanged, &previous, &staged));
            assert_eq!(unchanged, previous);
        }
    }

    #[test]
    fn reviewed_rollback_matches_server_ids_before_unknown_keys() {
        let previous = serde_json::json!([
            {"id":"other","key":"shared","enabled":false},
            {"id":"one","key":"shared","enabled":false}
        ]);
        let staged = serde_json::json!([
            {"id":"other","key":"shared","enabled":false},
            {"id":"one","key":"shared","enabled":true}
        ]);
        let mut latest = serde_json::json!([
            {"id":"other","key":"shared","enabled":true},
            {"id":"one","key":"shared","enabled":true}
        ]);
        assert!(undo_staged_value(&mut latest, &previous, &staged));
        assert_eq!(
            latest,
            serde_json::json!([
                {"id":"other","key":"shared","enabled":true},
                {"id":"one","key":"shared","enabled":false}
            ])
        );
    }

    #[test]
    fn reviewed_rollback_preserves_concurrent_environment_fields() {
        let previous = serde_json::json!({"enabled":false,"env":[{"key":"PAT","value":"old"}]});
        let staged = serde_json::json!({"enabled":true,"env":[{"key":"PAT","value":"staged"}]});
        let mut latest = serde_json::json!({"enabled":true,"env":[{"key":"PAT","value":"concurrent"},{"key":"PORT","value":"3000"}]});
        assert!(!undo_staged_value(&mut latest, &previous, &staged));
        assert_eq!(
            latest,
            serde_json::json!({"enabled":false,"env":[{"key":"PAT","value":"concurrent"},{"key":"PORT","value":"3000"}]})
        );
    }

    #[test]
    fn reviewed_setup_failure_preserves_a_concurrent_registry_edit() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"one":{"command":"one"}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| {
                registry::update(|registry| {
                    registry.add_server(server("concurrent"));
                    Ok(())
                })
                .unwrap();
                Err("Launch failed".into())
            },
        )
        .unwrap_err();
        assert!(error.contains("Launch failed"));
        assert!(
            !read_registry_exact()
                .unwrap()
                .servers
                .iter()
                .any(|s| s.id == "one"),
            "failed staged import must be removed despite unrelated edits"
        );
        assert!(read_registry_exact()
            .unwrap()
            .servers
            .iter()
            .any(|server| server.name == "concurrent"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        assert!(!fixture.move_record("claude-code").exists());
    }

    #[test]
    fn reviewed_setup_refuses_changed_credential_without_importing() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"old"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let changed = r#"{"mcpServers":{"one":{"command":"one","env":{"PAT":"new"}}}}"#;
        std::fs::write(fixture.claude(), changed).unwrap();
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| panic!("must refuse before verification"),
        )
        .unwrap_err();
        assert!(error.contains("changed"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), changed);
        assert!(read_registry_exact().unwrap().servers.is_empty());
    }

    #[test]
    fn reviewed_setup_creates_registry_on_first_run() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::remove_file(registry::resolved_path().unwrap()).unwrap();
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"one":{"command":"one"}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let result = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["one".into()],
            &review.revision,
            |_, _, _, _| Ok(vec![serde_json::json!({"name":"toolport_search_tools"})].into()),
        )
        .unwrap();
        assert_eq!(result.moved, ["one"]);
        assert!(read_registry_exact().unwrap().servers[0].enabled);
    }

    #[test]
    fn reviewed_setup_launch_and_missing_credentials_are_not_success() {
        let _fixture = MoveFixture::new(&Registry::default());
        let mut reg = Registry::default();
        let mut entry = server("broken");
        entry.command = Some("/does-not-exist-toolport-setup".into());
        let id = reg.add_server(entry);
        assert!(
            verify_setup_gateway(&reg, &["broken".into()], "claude-code", None)
                .unwrap_err()
                .contains("could not start")
        );
        reg.servers
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap()
            .env
            .push(registry::EnvVar {
                key: "PAT".into(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            });
        assert!(verify_setup_gateway(&reg, &["broken".into()], "claude-code", None).is_err());
    }

    #[test]
    fn connecting_without_imports_does_not_wait_on_existing_servers() {
        let _fixture = MoveFixture::new(&Registry::default());
        let mut reg = Registry::default();
        let mut entry = server("slow");
        entry.command = Some("/does-not-exist-toolport-setup".into());
        reg.add_server(entry);
        match verify_setup_gateway(&reg, &[], "claude-code", None) {
            Ok(verification) => {
                assert!(verification.tools.is_empty());
                assert!(verification.servers.is_empty());
            }
            Err(error) => assert!(error.contains("locate the Toolport gateway"), "{error}"),
        }
    }

    #[test]
    fn catalog_timeout_survives_install_and_registry_reload() {
        let _fixture = MoveFixture::new(&Registry::default());
        let entry = crate::catalog::search_curated("agent-qa")
            .into_iter()
            .find(|entry| entry.name == "agent-qa")
            .unwrap();
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["requestTimeoutMs"], 6_000_000);
        let entry = serde_json::from_value(json).unwrap();
        let registry = add_catalog_entry(entry).unwrap();
        let server_id = &registry.servers[0].id;
        let reloaded = registry::load().unwrap();
        let server = reloaded
            .servers
            .iter()
            .find(|s| &s.id == server_id)
            .unwrap();
        assert_eq!(server.request_timeout_ms, Some(6_000_000));
        assert!(!server.enabled, "the required config path is still missing");

        for entry in crate::catalog::curated()
            .into_iter()
            .filter(|e| e.name != "agent-qa")
        {
            assert!(serde_json::to_value(&entry)
                .unwrap()
                .get("requestTimeoutMs")
                .is_none());
            assert_eq!(catalog_server(entry).request_timeout_ms, None);
        }
    }

    #[test]
    fn reviewed_catalog_add_enables_valid_definitions() {
        let fixture = MoveFixture::new(&Registry::default());
        let entry = |name: &str, env: Vec<&str>| {
            serde_json::from_value::<crate::catalog::CatalogEntry>(serde_json::json!({"name":name,"description":"fixture","transport":"stdio","command":"fixture","args":[],"url":null,"envKeys":env,"source":"curated","homepage":null,"category":"Local tools"})).unwrap()
        };
        assert!(add_catalog_entry(entry("catalog", vec![])).unwrap().servers[0].enabled);
        let registry = add_catalog_entry(entry("missing", vec!["PAT"])).unwrap();
        assert_eq!(registry.servers.len(), 2);
        assert!(
            !registry
                .servers
                .iter()
                .find(|s| s.name == "missing")
                .unwrap()
                .enabled
        );
        drop(fixture);
    }

    #[test]
    fn reviewed_multi_paste_adds_selection_and_leaves_placeholders_off() {
        let _fixture = MoveFixture::new(&Registry::default());
        let outcome = add_snippet_servers(r#"{"mcpServers":{"ready":{"command":"fixture"},"needs":{"command":"fixture","env":{"PAT":"${PAT}"}},"skipped":{"command":"fixture"}}}"#, &["0".into(),"1".into()]).unwrap();
        // JSON maps have sorted keys: needs, ready, skipped.
        assert_eq!(outcome.registry.servers.len(), 2);
        assert!(
            !outcome
                .registry
                .servers
                .iter()
                .find(|s| s.name == "needs")
                .unwrap()
                .enabled
        );
        assert!(
            outcome
                .registry
                .servers
                .iter()
                .find(|s| s.name == "ready")
                .unwrap()
                .enabled
        );
        assert_eq!(outcome.declared_without_value, ["PAT"]);
    }

    #[test]
    fn reviewed_manual_add_enables_valid_definition() {
        let mut reg = Registry::default();
        let mut entry = server("one");
        entry.enabled = true;
        let id = apply_add_entry(&mut reg, entry);
        assert!(reg.server_enabled(&id));
    }

    #[test]
    fn access_review_creation_rejects_reserved_names() {
        let mut reg = Registry::default();
        for name in ["@all-enabled", " @default-access:default"] {
            assert!(apply_create_profile(&mut reg, name).is_err());
        }
        assert_eq!(reg.profiles.len(), 1);
    }

    #[test]
    fn zcode_import_preview_stays_available_and_selected_safe_servers_can_import() {
        let fixture = ZCodeImportFixture::new(serde_json::json!({
            "unsupported":{"command":"node", "cwd":"/srv/work"},
            "supported":{"command":"supported-node"}
        }));
        let mut unrelated = fixture.client.clone();
        unrelated.id = "cursor".into();
        unrelated.servers.truncate(1);
        unrelated.servers[0].name = "other-client".into();
        unrelated.servers[0].command = Some("other-node".into());
        let detected = [fixture.client.clone(), unrelated];
        let mut registry = Registry::default();
        assert_eq!(servers_to_import(&detected, &registry).len(), 3);
        assert!(selected_servers_to_import(&detected, &registry, None)
            .unwrap_err()
            .contains("'cwd'"));
        assert!(registry.servers.is_empty());
        let selected = std::collections::HashSet::from([
            stdio_key("supported-node", "supported"),
            stdio_key("other-node", "other-client"),
        ]);
        let servers = selected_servers_to_import(&detected, &registry, Some(&selected)).unwrap();
        assert_eq!(servers.len(), 2);
        for (entry, _) in servers {
            registry.add_server(entry);
        }
        assert_eq!(registry.servers.len(), 2);
        assert!(registry.enabled_servers().is_empty());
    }

    #[test]
    fn zcode_client_import_keeps_disabled_entries_disabled_including_legacy_false() {
        let fixture = ZCodeImportFixture::new(serde_json::json!({
            "disabled":{"command":"node", "enabled":false, "enable":true},
            "legacy-disabled":{"command":"legacy-node", "enabled":true, "enable":false}
        }));
        let mut registry = Registry::default();
        for entry in
            selected_servers_to_import(std::slice::from_ref(&fixture.client), &registry, None)
                .unwrap()
        {
            registry.add_server(entry.0);
        }
        assert_eq!(registry.servers.len(), 2);
        assert!(registry.enabled_servers().is_empty());
        let mut migration_registry = Registry::default();
        let error = import_client_servers_for_migration(&mut migration_registry, &fixture.client)
            .unwrap_err();
        assert!(error.contains("disabled"));
        assert!(migration_registry.servers.is_empty());
    }

    #[test]
    fn zcode_unsupported_metadata_refuses_import_and_migration_before_mutation() {
        for (field, value) in [
            ("cwd", serde_json::json!("/srv/work")),
            ("timeoutMs", serde_json::json!(4500)),
            ("protocolVersion", serde_json::json!("auto")),
            ("protocolVersion", serde_json::json!("legacy")),
            ("protocolVersion", serde_json::json!("2026-07-28")),
            (
                "oauth",
                serde_json::json!({"type":"client_credentials", "clientId":"test", "clientSecret":"do-not-print"}),
            ),
        ] {
            let mut definition = if field == "oauth" {
                serde_json::json!({"type":"http", "url":"https://example.test/mcp"})
            } else {
                serde_json::json!({"command":"node"})
            };
            definition[field] = value;
            let fixture = ZCodeImportFixture::new(serde_json::json!({"affected":definition}));
            let mut registry = Registry::default();
            let error =
                selected_servers_to_import(std::slice::from_ref(&fixture.client), &registry, None)
                    .unwrap_err();
            assert!(error.contains(field), "{error}");
            assert!(!error.contains("do-not-print"));
            let error =
                import_client_servers_for_migration(&mut registry, &fixture.client).unwrap_err();
            assert!(error.contains(field), "{error}");
            assert!(registry.servers.is_empty());
        }
    }

    #[test]
    fn zcode_supported_migration_imports_once_without_enabling_and_skips_gateway() {
        let fixture = ZCodeImportFixture::new(serde_json::json!({
            "supported":{"command":"node"},
            "events":{"type":"sse", "url":"https://example.test/sse"},
            "conduit":{"command":"/old/conduit-gateway"}
        }));
        let mut registry = Registry::default();
        let (imported, moved) =
            import_client_servers_for_migration(&mut registry, &fixture.client).unwrap();
        assert_eq!(imported, 2);
        assert_eq!(moved, ["events", "supported"]);
        assert!(registry.enabled_servers().is_empty());
        let (imported, _) =
            import_client_servers_for_migration(&mut registry, &fixture.client).unwrap();
        assert_eq!(imported, 0);
        assert_eq!(registry.servers.len(), 2);
    }

    #[test]
    fn zcode_fallback_migration_is_refused_before_registry_changes() {
        let mut fixture = ZCodeImportFixture::new(serde_json::json!({"shared":{"command":"node"}}));
        std::fs::remove_file(&fixture.client.config_path).unwrap();
        fixture.client.config_exists = false;
        let fallback = fixture.root.join(".agents/mcp.json");
        std::fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        let original = r#"{"mcpServers":{"shared":{"command":"node"}}}"#;
        std::fs::write(&fallback, original).unwrap();
        let mut registry = Registry::default();
        let error =
            import_client_servers_for_migration(&mut registry, &fixture.client).unwrap_err();
        assert!(error.contains("would hide"));
        assert!(registry.servers.is_empty());
        assert_eq!(std::fs::read_to_string(fallback).unwrap(), original);
    }

    /// Every self-hosted catalog entry must be rejected by the one-click add,
    /// because it has no endpoint yet. Both shells route these to the server
    /// editor; this proves nothing can slip past into an unusable server.
    #[test]
    fn self_hosted_catalog_entries_are_refused_by_the_one_click_add() {
        // No data-dir isolation on purpose. The guard returns before
        // `registry::update`, so nothing here reads or writes the registry, and
        // taking the two global test locks only serialized this test against
        // every registry-writing test in the module and reordered them, which
        // failed CI on all three platforms while passing locally.
        let self_hosted = crate::catalog::curated()
            .into_iter()
            .filter(|entry| entry.url_hint.is_some())
            .collect::<Vec<_>>();
        assert!(
            !self_hosted.is_empty(),
            "expected self-hosted catalog entries"
        );
        for entry in self_hosted {
            let name = entry.name.clone();
            let error = add_catalog_entry(entry)
                .expect_err(&format!("{name} was added without an endpoint"));
            assert!(
                error.contains(&name),
                "error should name the server: {error}"
            );
        }
    }

    #[test]
    fn plain_env_edits_keep_credentials_order_and_unchosen_sync_flags() {
        let mut entry = server("plain");
        entry.env = serde_json::from_value(serde_json::json!([
            {"key":"TOKEN","secret":true},
            {"key":"REGION","secret":false,"value":"west","portable":true,"extra":1},
            {"key":"OLD","secret":false,"value":"x"},
            {"key":"OP","secret":true,"source":{"ref":"op://v/i/f"}}
        ]))
        .unwrap();
        let edit = |key: &str, value: &str, local_only| PlainEnvEdit {
            key: key.into(),
            value: value.into(),
            local_only,
        };
        apply_plain_env(
            &mut entry,
            vec![
                edit(" REGION ", "east", None),
                edit("MODE", "fast", Some(true)),
                edit("", "", None),
            ],
        )
        .unwrap();
        let env = serde_json::to_value(&entry.env).unwrap();
        assert_eq!(
            env,
            serde_json::json!([
                {"key":"TOKEN","secret":true},
                {"key":"REGION","secret":false,"value":"east","portable":true,"extra":1},
                {"key":"OP","secret":true,"source":{"ref":"op://v/i/f"}},
                {"key":"MODE","secret":false,"value":"fast","portable":false}
            ])
        );
        // Saving the same list again changes nothing.
        let before = entry.env.clone();
        apply_plain_env(
            &mut entry,
            vec![edit("REGION", "east", None), edit("MODE", "fast", None)],
        )
        .unwrap();
        assert_eq!(entry.env, before);
    }

    #[test]
    fn plain_env_edits_refuse_bad_names_credentials_and_portable_overrides() {
        let mut entry = server("plain");
        entry.env =
            serde_json::from_value(serde_json::json!([{"key":"TOKEN","secret":true}])).unwrap();
        let before = entry.env.clone();
        for (key, local_only) in [
            ("BAD NAME", None),
            ("A=B", None),
            ("TOKEN", None),
            ("NODE_OPTIONS", Some(false)),
        ] {
            let edits = vec![PlainEnvEdit {
                key: key.into(),
                value: "v".into(),
                local_only,
            }];
            assert!(
                apply_plain_env(&mut entry, edits).is_err(),
                "{key} accepted"
            );
        }
        let dup = |key: &str| PlainEnvEdit {
            key: key.into(),
            value: "v".into(),
            local_only: None,
        };
        assert!(apply_plain_env(&mut entry, vec![dup("X"), dup("X")]).is_err());
        assert!(apply_plain_env(&mut entry, vec![dup("")]).is_err());
        assert_eq!(entry.env, before);
    }

    fn server(id: &str) -> ServerEntry {
        ServerEntry {
            enabled: false,
            inherit_env: false,
            id: id.into(),
            name: id.into(),
            transport: "stdio".into(),
            command: Some("tool".into()),
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            cwd: None,
            source: None,
            disabled_tools: Vec::new(),
            hide_prompts: false,
            client_credentials: None,
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    fn fields(name: &str, transport: &str) -> ServerFields {
        ServerFields {
            sync_local_only: false,
            initialize_timeout_ms: None,
            name: name.into(),
            transport: transport.into(),
            command: (transport == "stdio").then(|| "npx".into()),
            args: vec!["-y".into(), "example-server".into()],
            url: (transport != "stdio").then(|| "https://example.com/mcp".into()),
            cwd: (transport == "stdio").then(|| " /tmp/project ".into()),
            inherit_env: None,
            hide_prompts: None,
        }
    }

    /// Each request's headers, in order.
    type SeenHeaders = std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>;

    /// A loopback MCP endpoint that records the headers of every request.
    struct HeaderEndpoint {
        url: String,
        seen: SeenHeaders,
        server: std::sync::Arc<tiny_http::Server>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl HeaderEndpoint {
        fn new() -> Self {
            let server = std::sync::Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
            let url = format!("http://{}/mcp", server.server_addr());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let (endpoint, record) = (server.clone(), seen.clone());
            let worker = std::thread::spawn(move || {
                while let Ok(mut request) = endpoint.recv() {
                    record.lock().unwrap().push(
                        request
                            .headers()
                            .iter()
                            .map(|h| (h.field.to_string(), h.value.to_string()))
                            .collect(),
                    );
                    let mut body = String::new();
                    let _ = request.as_reader().read_to_string(&mut body);
                    let message: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or_default();
                    if message.get("id").is_none() {
                        let _ = request.respond(tiny_http::Response::empty(202));
                        continue;
                    }
                    let result = serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":{
                        "protocolVersion":"2025-06-18","capabilities":{"tools":{}},
                        "serverInfo":{"name":"headers","version":"1"},"tools":[]}});
                    let _ = request.respond(
                        tiny_http::Response::from_string(result.to_string()).with_header(
                            tiny_http::Header::from_bytes("Content-Type", "application/json")
                                .unwrap(),
                        ),
                    );
                }
            });
            Self {
                url,
                seen,
                server,
                worker: Some(worker),
            }
        }

        /// Every value of `name` the endpoint received, in order.
        fn values(&self, name: &str) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .filter(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
                .collect()
        }
    }

    impl Drop for HeaderEndpoint {
        fn drop(&mut self) {
            self.server.unblock();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum AddPath {
        /// React: `add_server` with the form's rows, then `set_secret` per vaulted value.
        React,
        /// GTK: `add_server_with_rows` with the pasted rows.
        Gtk,
    }

    /// Paste `paste` (with `{url}` replaced), save it through one shell's add
    /// path, connect, and return the endpoint that saw the requests.
    fn save_and_connect(paste: &str, path: AddPath) -> Result<HeaderEndpoint, String> {
        let endpoint = HeaderEndpoint::new();
        let paste = paste.replace("{url}", &endpoint.url);
        let parsed = crate::clients::parse_snippet_for_user(&paste)
            .map_err(crate::clients::SnippetError::message)?
            .remove(0);
        let fields = ServerFields {
            sync_local_only: false,
            initialize_timeout_ms: None,
            name: parsed.name.clone(),
            transport: parsed.transport.clone(),
            command: parsed.command.clone(),
            args: parsed.args.clone(),
            url: parsed.url.clone(),
            cwd: None,
            inherit_env: None,
            hide_prompts: None,
        };
        let secret = |e: &crate::clients::SnippetEnvVar| {
            crate::import_credentials::secret_env(&e.key, e.value.as_deref())
        };
        let id = match path {
            AddPath::Gtk => {
                let rows = parsed
                    .env
                    .iter()
                    .map(|e| ValueRow {
                        key: e.key.clone(),
                        value: e.value.clone().unwrap_or_default(),
                        secret: secret(e),
                        ..Default::default()
                    })
                    .collect();
                add_server_with_rows(fields, rows)?;
                registry::load()?
                    .servers
                    .iter()
                    .find(|s| s.name == parsed.name)
                    .ok_or("saved")?
                    .id
                    .clone()
            }
            AddPath::React => {
                let mut entry = entry_from_fields(fields)?;
                entry.env = parsed
                    .env
                    .iter()
                    .map(|e| registry::EnvVar {
                        key: e.key.clone(),
                        value: if secret(e) { None } else { e.value.clone() },
                        secret: secret(e),
                        unknown_fields: Default::default(),
                    })
                    .collect();
                let (_, id) = add_reviewed_entry(entry)?;
                for e in parsed.env.iter().filter(|e| secret(e)) {
                    let Some(value) = &e.value else { continue };
                    set_server_secret_with(&id, &e.key, value, |id, key| {
                        Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0)
                    })?;
                }
                // Then the dialog switches it on, once every row has a value.
                if parsed
                    .env
                    .iter()
                    .all(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()))
                {
                    registry::update(|r| {
                        let profile = r.default_access_id();
                        apply_server_enabled(r, &profile, &id, true, false)
                    })?;
                }
                id
            }
        };
        let server = registry::load()?
            .servers
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("saved")?;
        crate::remote::connect_remote(&server)?;
        Ok(endpoint)
    }

    /// A pasted header Toolport cannot send is refused before either shell saves
    /// anything. Before, both shells saved it as a credential that was never sent
    /// as `X-API-Key` and went out as `Authorization: Bearer abc` instead.
    #[test]
    fn pasted_custom_header_is_refused_in_both_shells() {
        crate::secrets::tests::with_isolated_vault(|| {
            for paste in [
                "claude mcp add -t http demo {url} -H 'X-API-Key: abc'",
                r#"{"mcpServers":{"demo":{"type":"http","url":"{url}","headers":{"X-API-Key":"abc"}}}}"#,
            ] {
                for path in [AddPath::React, AddPath::Gtk] {
                    let Err(error) = save_and_connect(paste, path) else {
                        panic!("{path:?}: {paste} was saved");
                    };
                    assert_eq!(
                        error,
                        crate::clients::unsupported_header_message(),
                        "{path:?}: {paste}"
                    );
                }
            }
            assert!(registry::load().unwrap().servers.is_empty());
        });
    }

    #[test]
    fn pasted_bearer_header_is_sent_as_written() {
        crate::secrets::tests::with_isolated_vault(|| {
            for path in [AddPath::React, AddPath::Gtk] {
                let endpoint = save_and_connect(
                    &format!("claude mcp add -t http bearer-{path:?} {{url}} -H 'Authorization: Bearer abc'"),
                    path,
                )
                .unwrap_or_else(|e| panic!("{path:?}: {e}"));
                let sent = endpoint.values("Authorization");
                assert!(!sent.is_empty(), "{path:?}");
                assert!(sent.iter().all(|v| v == "Bearer abc"), "{path:?}: {sent:?}");
            }
        });
    }

    /// A pasted key renamed to match another row would keep only one value.
    /// The add is refused before anything is read or written.
    #[test]
    fn snippet_add_refuses_a_repeated_variable_name() {
        let env = vec![
            ("API_KEY".to_string(), Some("abc".to_string())),
            ("API_KEY".to_string(), None),
        ];
        let Err(error) =
            add_snippet_server_choices(fields("dup", "stdio"), env, &Default::default())
        else {
            panic!("a repeated name is refused");
        };
        assert_eq!(
            error,
            "Use each variable name once. Rename or remove the second API_KEY."
        );
        let rows = ["API_KEY", " API_KEY "]
            .map(|key| ValueRow {
                key: key.into(),
                ..Default::default()
            })
            .to_vec();
        let Err(error) = add_server_with_rows(fields("dup", "stdio"), rows) else {
            panic!("a repeated name is refused");
        };
        assert_eq!(
            error,
            "Use each variable name once. Rename or remove the second API_KEY."
        );
    }

    /// GTK's Add server saves its rows exactly as the React dialog's sequence
    /// does (add the entry, store each typed secret, switch it on): the same
    /// saved entry, vault and enabled state, for every kind of row.
    #[test]
    fn gtk_rows_save_like_the_react_dialog() {
        let rows = vec![
            ValueRow {
                key: "API_KEY".into(),
                value: "sk-live-typed".into(),
                secret: true,
                ..Default::default()
            },
            ValueRow {
                key: "VAULT_TOKEN".into(),
                secret: true,
                reference: Some("op://Private/Server/token".into()),
                ..Default::default()
            },
            ValueRow {
                key: "REGION".into(),
                value: "eu".into(),
                portable: Some(false),
                ..Default::default()
            },
            ValueRow {
                key: "NOTES_DIR".into(),
                value: "/home/me/notes".into(),
                portable: Some(true),
                ..Default::default()
            },
            ValueRow {
                key: "NODE_OPTIONS".into(),
                value: "--max-old-space-size=4096".into(),
                portable: Some(true),
                ..Default::default()
            },
            ValueRow {
                key: "PLAIN".into(),
                value: "1".into(),
                ..Default::default()
            },
        ];
        // React's entry for these rows (`buildEntry(false)`), and its save calls.
        let react_env = serde_json::json!([
            {"key": "API_KEY", "value": null, "secret": true},
            {"key": "VAULT_TOKEN", "value": null, "secret": true,
             "source": {"ref": "op://Private/Server/token"}},
            {"key": "REGION", "value": "eu", "secret": false, "portable": false},
            {"key": "NOTES_DIR", "value": "/home/me/notes", "secret": false, "portable": true},
            {"key": "NODE_OPTIONS", "value": "--max-old-space-size=4096", "secret": false, "portable": false},
            {"key": "PLAIN", "value": "1", "secret": false},
        ]);
        let saved = |shell: &str| {
            let mut result = None;
            crate::secrets::tests::with_isolated_vault(|| {
                let fields = fields("demo", "stdio");
                let id = if shell == "gtk" {
                    let outcome = add_server_with_rows(fields, rows.clone()).unwrap();
                    assert_eq!(outcome.declared_without_value, Vec::<String>::new());
                    outcome.registry.servers[0].id.clone()
                } else {
                    let mut entry = entry_from_fields(fields).unwrap();
                    entry.env = serde_json::from_value(react_env.clone()).unwrap();
                    entry.enabled = false;
                    let (_, id) = add_reviewed_entry(entry).unwrap();
                    set_server_secret_with(&id, "API_KEY", "sk-live-typed", |id, key| {
                        Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0)
                    })
                    .unwrap();
                    // React switches a new server on only when every row has a
                    // typed value (`declared.every((r) => r.value.trim())`); a
                    // password manager row has none, so these rows stay off.
                    id
                };
                let registry = registry::load().unwrap();
                let server = registry.servers[0].clone();
                result = Some((
                    serde_json::to_value(&server.env).unwrap(),
                    crate::secrets::get_secret_result(&id, "API_KEY").unwrap(),
                    registry.is_enabled(&registry.default_access_id(), &id),
                ));
            });
            result.unwrap()
        };
        let (gtk, react) = (saved("gtk"), saved("react"));
        assert_eq!((&gtk.0, &gtk.1), (&react.0, &react.1));
        assert_eq!(gtk.1.as_deref(), Some("sk-live-typed"));
        // GTK switches it on when no value is missing, a reference counting as
        // one; React leaves a typed row next to a reference off.
        assert!(gtk.2, "GTK: nothing is missing, so it is on");
        assert!(
            !react.2,
            "React: the reference row has no typed value, so it is off"
        );
    }

    fn row(key: &str, value: &str, secret: bool) -> ValueRow {
        ValueRow {
            key: key.into(),
            value: value.into(),
            secret,
            ..Default::default()
        }
    }

    /// Each saved server: its id, whether it is on in the default access, and
    /// whether that access lists it.
    fn saved_servers() -> Vec<(String, bool)> {
        let registry = registry::load().unwrap();
        let profile = registry.default_access_id();
        registry
            .servers
            .iter()
            .map(|s| (s.name.clone(), registry.is_enabled(&profile, &s.id)))
            .collect()
    }

    /// A locked keychain the user does not unlock: Add server asks once for
    /// all its keychain rows, saves nothing and reports the failure, so the
    /// editor stays open with the values.
    #[test]
    fn rows_save_nothing_and_ask_once_when_the_keychain_stays_locked() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut outcome = None;
            let prompts = crate::secrets::locked_store::write_prompts_during(|| {
                outcome = Some(add_server_with_rows(
                    fields("demo", "stdio"),
                    ["API_KEY", "DB_PASSWORD", "SESSION_TOKEN"]
                        .iter()
                        .map(|key| row(key, &format!("sk-live-{key}"), true))
                        .collect(),
                ));
            });
            assert!(outcome.unwrap().is_err());
            assert_eq!(prompts.len(), 1, "{prompts:?}");
            assert!(registry::load().unwrap().servers.is_empty());
        });
        crate::secrets::tests::with_isolated_vault(|| {
            let outcome = crate::secrets::tests::with_failed_write("API_KEY", || {
                add_server_with_rows(
                    fields("demo", "stdio"),
                    vec![
                        row("API_KEY", "sk-live-typed", true),
                        row("REGION", "eu", false),
                    ],
                )
            });
            assert!(outcome.is_err());
            assert!(registry::load().unwrap().servers.is_empty());
        });
    }

    /// The entry React's Add server dialog sends: a typed secret, a plain value
    /// with its sync choice, a password manager row, and a pasted command line
    /// with a plain and a secret launch input.
    fn react_entry(extra_env: serde_json::Value) -> ServerEntry {
        let mut env = vec![
            serde_json::json!({"key": "API_KEY", "value": "sk-live-typed", "secret": true}),
            serde_json::json!({"key": "REGION", "value": "eu", "secret": false, "portable": true}),
            serde_json::json!({"key": "VAULT_TOKEN", "value": null, "secret": true,
                "source": {"ref": "op://Private/Server/token"}}),
        ];
        if !extra_env.is_null() {
            env.push(extra_env);
        }
        serde_json::from_value(serde_json::json!({
            "id": "", "enabled": false, "syncLocalOnly": true, "name": "demo",
            "transport": "stdio", "command": "npx",
            "args": ["-y", "example-server", "<launch-input>", "<launch-input>"],
            "launch": {
                "inputs": [
                    {"key": "ACCOUNT", "label": "Account", "secret": false,
                     "required": true, "value": "AC123"},
                    {"key": "SECRET", "label": "Secret", "secret": true,
                     "required": true, "value": "launch-secret"}
                ],
                "bindings": [
                    {"index": 2, "parts": [{"kind": "input", "key": "ACCOUNT"}]},
                    {"index": 3, "parts": [{"kind": "input", "key": "SECRET"}]}
                ]
            },
            "env": env, "url": null, "source": "manual", "cwd": null,
            "inheritEnv": true, "hidePrompts": false, "initializeTimeoutMs": 45000
        }))
        .unwrap()
    }

    fn add_react(entry: ServerEntry) -> Result<RowsAddOutcome, String> {
        add_server_from_dialog(entry)
    }

    /// The entry React's catalog dialog sends for a curated preset, with each
    /// row's typed value from `fill` and each launch input's from `input`.
    fn preset_entry(
        name: &str,
        fill: &dyn Fn(&str) -> Option<&'static str>,
        input: Option<&str>,
    ) -> ServerEntry {
        let preset = crate::catalog::curated()
            .into_iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no curated {name}"));
        let mut launch = serde_json::to_value(&preset.launch).unwrap();
        if let Some(inputs) = launch.get_mut("inputs").and_then(|v| v.as_array_mut()) {
            for i in inputs {
                i["value"] = input.map_or(serde_json::Value::Null, |v| serde_json::json!(v));
            }
        }
        let env: Vec<_> = preset
            .env_keys
            .iter()
            .map(|key| serde_json::json!({"key": key, "secret": true, "value": fill(key)}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "", "enabled": false, "syncLocalOnly": false, "name": preset.name,
            "transport": preset.transport, "command": preset.command, "args": preset.args,
            "launch": launch, "env": env, "url": preset.url, "source": "catalog:curated",
            "cwd": null, "inheritEnv": false, "hidePrompts": false, "initializeTimeoutMs": null
        }))
        .unwrap()
    }

    /// A curated preset keeps its own required keys: AWS, MongoDB and Qdrant
    /// leave some keys optional, so saving them blank still switches the
    /// server on, and it stays a catalog server.
    #[test]
    fn a_catalog_preset_keeps_its_optional_keys_and_its_source() {
        let blank = |_: &str| None;
        let qdrant_url = |key: &str| (key == "QDRANT_URL").then_some("http://localhost:6333");
        let aws_key = |key: &str| (key == "AWS_ACCESS_KEY_ID").then_some("AKIATYPED");
        type Fill = dyn Fn(&str) -> Option<&'static str>;
        let cases: [(&str, &Fill); 4] = [
            ("AWS", &blank),
            ("MongoDB", &blank),
            ("Qdrant", &qdrant_url),
            ("AWS", &aws_key),
        ];
        for (name, fill) in cases {
            crate::secrets::tests::with_isolated_vault(|| {
                let entry = preset_entry(name, fill, None);
                let preset_required = entry.launch.as_ref().unwrap().required_env.clone();
                let outcome = add_react(entry).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(
                    outcome.declared_without_value.is_empty(),
                    "{name}: {:?}",
                    outcome.declared_without_value
                );
                assert_eq!(saved_servers(), vec![(name.to_string(), true)], "{name}");
                let server = registry::load().unwrap().servers.remove(0);
                assert_eq!(server.source.as_deref(), Some("catalog:curated"), "{name}");
                assert_eq!(
                    server.launch.unwrap().required_env,
                    preset_required,
                    "{name}"
                );
                if name == "AWS" && fill("AWS_ACCESS_KEY_ID").is_some() {
                    assert_eq!(
                        crate::secrets::get_secret_result(&outcome.id, "AWS_ACCESS_KEY_ID")
                            .unwrap()
                            .as_deref(),
                        Some("AKIATYPED")
                    );
                }
            });
        }
    }

    /// A required launch input left blank keeps the server off and is named
    /// by its label.
    #[test]
    fn a_blank_required_launch_input_is_named_by_its_label() {
        crate::secrets::tests::with_isolated_vault(|| {
            let outcome = add_react(preset_entry("Filesystem", &|_| None, None)).unwrap();
            assert_eq!(
                outcome.declared_without_value,
                vec!["Allowed directory".to_string()]
            );
            assert_eq!(saved_servers(), vec![("Filesystem".into(), false)]);
        });
        crate::secrets::tests::with_isolated_vault(|| {
            let mut entry = react_entry(serde_json::Value::Null);
            entry.launch.as_mut().unwrap().inputs[1].value = None;
            let outcome = add_react(entry).unwrap();
            assert_eq!(outcome.declared_without_value, vec!["Secret".to_string()]);
            assert_eq!(saved_servers(), vec![("demo".into(), false)]);
        });
    }

    /// A Duplicate keeps where its original came from and its request timeout:
    /// a remote server through the bearer slot, and a shared one stays shared.
    #[test]
    fn a_duplicate_keeps_its_source_and_request_timeout() {
        for source in ["imported:cursor", "shared"] {
            crate::secrets::tests::with_isolated_vault(|| {
                let entry: ServerEntry = serde_json::from_value(serde_json::json!({
                    "id": "", "enabled": false, "name": "Remote copy", "transport": "http",
                    "command": null, "args": [], "launch": null,
                    "env": [{"key": "Authorization", "value": "Bearer typed-token", "secret": true}],
                    "url": "https://example.com/mcp", "source": source, "cwd": null,
                    "inheritEnv": false, "hidePrompts": false, "requestTimeoutMs": 120000,
                    "initializeTimeoutMs": null
                }))
                .unwrap();
                let outcome = add_react(entry).unwrap_or_else(|e| panic!("{source}: {e}"));
                assert!(outcome.declared_without_value.is_empty(), "{source}");
                let server = registry::load().unwrap().servers.remove(0);
                assert_eq!(server.source.as_deref(), Some(source));
                assert_eq!(server.request_timeout_ms, Some(120000), "{source}");
                assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
                assert_eq!(
                    crate::secrets::get_secret_result(&outcome.id, crate::secrets::HTTP_AUTH_KEY)
                        .unwrap()
                        .as_deref(),
                    Some("typed-token"),
                    "{source}"
                );
                assert!(!serde_json::to_string(&server)
                    .unwrap()
                    .contains("typed-token"));
            });
        }
    }

    /// A Duplicate of a remote server keeps its header credentials and allowed
    /// password manager prefixes, and a bad header reference saves nothing.
    #[test]
    fn a_duplicate_keeps_its_header_credentials() {
        let remote = |headers: serde_json::Value| -> ServerEntry {
            serde_json::from_value(serde_json::json!({
                "id": "", "enabled": false, "name": "Header copy", "transport": "http",
                "command": null, "args": [], "launch": null,
                "env": [{"key": "API_TOKEN", "value": "typed-token", "secret": true}],
                "url": "https://example.com/mcp", "source": "shared", "cwd": null,
                "inheritEnv": false, "hidePrompts": false, "requestTimeoutMs": null,
                "initializeTimeoutMs": null, "headerKeys": headers,
                "secretSources": {"allowedPrefixes": ["op://Team/"]}
            }))
            .unwrap()
        };
        crate::secrets::tests::with_isolated_vault(|| {
            let headers = serde_json::json!([
                {"key": "X-Api-Key", "env": "API_TOKEN"},
                {"key": "X-Team-Key", "source": {"ref": "op://Team/service/key"}}
            ]);
            let outcome = add_react(remote(headers.clone())).unwrap();
            let server = registry::load().unwrap().servers.remove(0);
            assert_eq!(server.unknown_fields.get("headerKeys"), Some(&headers));
            assert_eq!(
                server.unknown_fields["secretSources"],
                serde_json::json!({"allowedPrefixes": ["op://Team/"]})
            );
            assert_eq!(
                crate::secrets::get_secret_result(&outcome.id, "API_TOKEN")
                    .unwrap()
                    .as_deref(),
                Some("typed-token")
            );
        });
        crate::secrets::tests::with_isolated_vault(|| {
            let bad = serde_json::json!([
                {"key": "X-Team-Key", "source": {"ref": "op://Team/service/key", "command": "evil"}}
            ]);
            let mut outcome = None;
            let prompts = crate::secrets::locked_store::write_prompts_during(|| {
                outcome = Some(add_react(remote(bad)));
            });
            assert!(outcome.unwrap().is_err());
            assert!(prompts.is_empty(), "{prompts:?}");
            assert!(registry::load().unwrap().servers.is_empty());
        });
    }

    /// A launch config that does not fit the arguments, and a row named like
    /// a launch input, are refused before anything is saved, without asking to
    /// unlock the keychain for a save that cannot happen.
    #[test]
    fn a_bad_launch_config_or_a_row_named_like_an_input_saves_nothing() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut no_marker = react_entry(serde_json::Value::Null);
            no_marker.args = vec!["-y".into(), "example-server".into()];
            let mut outcome = None;
            let prompts = crate::secrets::locked_store::write_prompts_during(|| {
                outcome = Some(add_react(no_marker));
            });
            assert!(outcome.unwrap().is_err());
            assert!(prompts.is_empty(), "{prompts:?}");
        });
        let mut no_marker = react_entry(serde_json::Value::Null);
        no_marker.args = vec!["-y".into(), "example-server".into()];
        let named_like_input =
            react_entry(serde_json::json!({"key": "SECRET", "value": "row-value", "secret": true}));
        for (case, entry, expected) in [
            ("no marker", no_marker, "launch argument"),
            (
                "row named like an input",
                named_like_input,
                "Use each variable name once",
            ),
        ] {
            crate::secrets::tests::with_isolated_vault(|| {
                let Err(error) = add_react(entry) else {
                    panic!("{case}: saved");
                };
                assert!(error.contains(expected), "{case}: {error}");
                assert!(registry::load().unwrap().servers.is_empty(), "{case}");
                for key in ["API_KEY", "SECRET"] {
                    assert_eq!(
                        crate::secrets::get_secret_result("demo", key).unwrap(),
                        None,
                        "{case}"
                    );
                }
            });
        }
    }

    /// React's new-server save is one step: every value, the reference and the
    /// launch inputs' secret are saved together, with one keychain unlock, and
    /// no secret is left in the registry.
    #[test]
    fn react_add_saves_values_and_launch_inputs_in_one_step() {
        crate::secrets::tests::with_isolated_vault(|| {
            let outcome = add_react(react_entry(serde_json::Value::Null)).unwrap();
            assert!(outcome.declared_without_value.is_empty());
            assert_eq!(saved_servers(), vec![("demo".into(), true)]);
            let id = outcome.id.clone();
            let get = |key: &str| crate::secrets::get_secret_result(&id, key).unwrap();
            assert_eq!(get("API_KEY").as_deref(), Some("sk-live-typed"));
            assert_eq!(get("SECRET").as_deref(), Some("launch-secret"));
            let server = registry::load().unwrap().servers.remove(0);
            let text = serde_json::to_string(&server).unwrap();
            for secret in ["sk-live-typed", "launch-secret"] {
                assert!(!text.contains(secret), "{text}");
            }
            assert!(text.contains("AC123"), "{text}");
            assert!(text.contains("op://Private/Server/token"), "{text}");
            assert_eq!(
                server.unknown_fields.get("syncLocalOnly"),
                Some(&serde_json::json!(true))
            );
            assert!(server.inherit_env);
            assert_eq!(server.initialize_timeout_ms, Some(45000));
            let region = server.env.iter().find(|e| e.key == "REGION").unwrap();
            assert_eq!(region.value.as_deref(), Some("eu"));
            assert_eq!(
                region.unknown_fields.get("portable"),
                Some(&serde_json::json!(true))
            );
        });
    }

    /// A failed keychain write, the launch input's included, saves nothing; a
    /// locked keychain the user does not unlock is asked once for all of them.
    #[test]
    fn react_add_saves_nothing_when_a_write_fails() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut outcome = None;
            let prompts = crate::secrets::locked_store::write_prompts_during(|| {
                outcome = Some(add_react(react_entry(serde_json::Value::Null)));
            });
            assert!(outcome.unwrap().is_err());
            assert_eq!(prompts.len(), 1, "{prompts:?}");
            assert!(registry::load().unwrap().servers.is_empty());
        });
        for key in ["API_KEY", "SECRET"] {
            crate::secrets::tests::with_isolated_vault(|| {
                let outcome = crate::secrets::tests::with_failed_write(key, || {
                    add_react(react_entry(serde_json::Value::Null))
                });
                assert!(outcome.is_err(), "{key}");
                assert!(registry::load().unwrap().servers.is_empty(), "{key}");
                for stored in ["API_KEY", "SECRET"] {
                    assert_eq!(
                        crate::secrets::get_secret_result("demo", stored).unwrap(),
                        None,
                        "{key}: {stored} left behind"
                    );
                }
            });
        }
    }

    /// With launch inputs, a row with no value still keeps the server off and
    /// is named; a launch input read from a password manager does not.
    #[test]
    fn react_add_with_launch_inputs_still_names_rows_without_a_value() {
        crate::secrets::tests::with_isolated_vault(|| {
            let outcome = add_react(react_entry(
                serde_json::json!({"key": "EMPTY", "value": null, "secret": true}),
            ))
            .unwrap();
            assert_eq!(outcome.declared_without_value, vec!["EMPTY".to_string()]);
            assert_eq!(saved_servers(), vec![("demo".into(), false)]);
        });
        crate::secrets::tests::with_isolated_vault(|| {
            let mut entry = react_entry(serde_json::Value::Null);
            let input = &mut entry.launch.as_mut().unwrap().inputs[1];
            input.value = None;
            input.unknown_fields.insert(
                "source".into(),
                serde_json::json!({"ref": "op://Private/Server/secret"}),
            );
            let outcome = add_react(entry).unwrap();
            assert!(
                outcome.declared_without_value.is_empty(),
                "{:?}",
                outcome.declared_without_value
            );
            assert_eq!(saved_servers(), vec![("demo".into(), true)]);
        });
    }

    /// Authorization pasted onto a remote server: a Basic value is refused
    /// before anything is saved; a bare or Bearer token is the bearer token,
    /// and with every value present the server is switched on.
    #[test]
    fn rows_refuse_basic_authorization_and_store_bearer_tokens() {
        crate::secrets::tests::with_isolated_vault(|| {
            let Err(error) = add_server_with_rows(
                fields("demo", "http"),
                vec![row("Authorization", "Basic dXNlcjpwYXNz", true)],
            ) else {
                panic!("a Basic header was saved");
            };
            assert_eq!(
                error,
                "This server uses unsupported HTTP authentication. Keep its native entry."
            );
            assert!(registry::load().unwrap().servers.is_empty());
        });
        for (value, keychain) in [
            ("bare-token", true),
            ("Bearer typed-token", true),
            ("Bearer typed-token", false),
        ] {
            crate::secrets::tests::with_isolated_vault(|| {
                let outcome = add_server_with_rows(
                    fields("demo", "http"),
                    vec![row("Authorization", value, keychain)],
                )
                .unwrap();
                assert_eq!(outcome.declared_without_value, Vec::<String>::new());
                let server = outcome.registry.servers[0].clone();
                assert!(!serde_json::to_string(&server).unwrap().contains("token"));
                assert_eq!(
                    crate::secrets::get_secret_result(&server.id, crate::secrets::HTTP_AUTH_KEY)
                        .unwrap()
                        .as_deref(),
                    Some(value.trim_start_matches("Bearer ")),
                    "{value}"
                );
                assert_eq!(saved_servers(), vec![("demo".into(), true)], "{value}");
            });
        }
    }

    /// A password manager row counts as a value: the new server is switched on,
    /// and can be switched off and on again, since its value is read from the
    /// password manager at start.
    #[test]
    fn a_server_with_a_password_manager_row_can_be_switched_on() {
        for transport in ["stdio", "http"] {
            for reviewed in [false, true] {
                crate::secrets::tests::with_isolated_vault(|| {
                    let key = if transport == "stdio" {
                        "VAULT_TOKEN"
                    } else {
                        "Authorization"
                    };
                    let outcome = add_server_with_rows(
                        fields("demo", transport),
                        vec![ValueRow {
                            key: key.into(),
                            secret: true,
                            reference: Some("op://Private/Server/token".into()),
                            ..Default::default()
                        }],
                    )
                    .unwrap();
                    let id = outcome.registry.servers[0].id.clone();
                    let profile = outcome.registry.default_access_id();
                    assert!(
                        outcome.registry.is_enabled(&profile, &id),
                        "switched on at save"
                    );
                    set_server_enabled(&profile, &id, false, false).unwrap();
                    let registry = set_server_enabled(&profile, &id, true, reviewed)
                        .unwrap_or_else(|e| panic!("{transport} reviewed={reviewed}: {e}"));
                    assert!(registry.is_enabled(&profile, &id));
                });
            }
        }
    }

    /// With a named access set as the default, a server saved with a keychain
    /// row is switched on in that set, as any other added server is.
    #[test]
    fn rows_add_the_server_to_a_named_default_access() {
        for rows in [
            vec![row("API_KEY", "sk-live-typed", true)],
            vec![row("REGION", "eu", false)],
        ] {
            crate::secrets::tests::with_isolated_vault(|| {
                let (_, profile) = registry::update(|r| {
                    let id = r.add_profile("Work");
                    r.set_default_access(Some(&id))?;
                    Ok(id)
                })
                .unwrap();
                add_server_with_rows(fields("demo", "stdio"), rows.clone()).unwrap();
                let registry = registry::load().unwrap();
                let id = registry.servers[0].id.clone();
                let members = &registry
                    .profiles
                    .iter()
                    .find(|p| p.id == profile)
                    .unwrap()
                    .enabled_server_ids;
                assert!(members.contains(&id), "{rows:?}: {members:?}");
            });
        }
    }

    /// A placeholder is no value: the server is saved once, off, and the key is
    /// reported as needing a value, whether the row is plain or in the keychain.
    #[test]
    fn rows_with_a_placeholder_save_the_server_once_and_off() {
        for (transport, key, value, secret) in [
            ("stdio", "API_KEY", "${API_BASE}", false),
            ("stdio", "API_KEY", "${API_KEY}", true),
            ("stdio", "API_KEY", "YOUR_API_KEY", true),
            ("stdio", "API_KEY", "   ", true),
            // A pasted header name with no value.
            ("http", "Authorization", "", true),
            ("http", "Authorization", "   ", true),
            // A placeholder token, bare or after Bearer.
            ("http", "Authorization", "${API_KEY}", true),
            ("http", "Authorization", "Bearer YOUR_API_KEY", true),
        ] {
            crate::secrets::tests::with_isolated_vault(|| {
                let outcome =
                    add_server_with_rows(fields("demo", transport), vec![row(key, value, secret)])
                        .unwrap_or_else(|e| panic!("{transport} {value:?}: {e}"));
                assert_eq!(outcome.declared_without_value, vec![key.to_string()]);
                assert_eq!(saved_servers(), vec![("demo".into(), false)], "{value:?}");
                let id = outcome.registry.servers[0].id.clone();
                for stored in [key, crate::secrets::HTTP_AUTH_KEY] {
                    assert_eq!(
                        crate::secrets::get_secret_result(&id, stored).unwrap(),
                        None
                    );
                }
            });
        }
    }

    /// A placeholder typed after Bearer is refused like any other placeholder.
    #[test]
    fn a_typed_placeholder_bearer_token_is_refused() {
        crate::secrets::tests::with_isolated_vault(|| {
            let (_, id) =
                add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
            for value in ["Bearer ${API_KEY}", "Bearer YOUR_TOKEN", "<your-token>"] {
                let err = set_server_secret(&id, "Authorization", value).unwrap_err();
                assert!(err.contains("Placeholder"), "{value}: {err}");
            }
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY).unwrap(),
                None
            );
        });
    }

    /// A token typed for a password manager Authorization row replaces the
    /// reference, so the typed token is the one sent.
    #[test]
    fn a_typed_token_replaces_a_password_manager_authorization_row() {
        crate::secrets::tests::with_isolated_vault(|| {
            let outcome = add_server_with_rows(
                fields("demo", "http"),
                vec![ValueRow {
                    key: "Authorization".into(),
                    secret: true,
                    reference: Some("op://Private/Server/token".into()),
                    ..Default::default()
                }],
            )
            .unwrap();
            let id = outcome.registry.servers[0].id.clone();
            let registry = set_server_secret(&id, "Authorization", "Bearer typed").unwrap();
            let server = registry.servers.iter().find(|s| s.id == id).unwrap();
            assert!(
                !server
                    .env
                    .iter()
                    .any(|e| e.unknown_fields.contains_key("source")),
                "{:?}",
                server.env
            );
            let required = server
                .launch
                .as_ref()
                .map(|l| l.required_env.clone())
                .unwrap_or_default();
            assert!(
                !required.iter().any(|k| k == "Authorization"),
                "{required:?}"
            );
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY)
                    .unwrap()
                    .as_deref(),
                Some("typed")
            );
            let profile = registry.default_access_id();
            set_server_enabled(&profile, &id, false, false).unwrap();
            set_server_enabled(&profile, &id, true, true).unwrap();
        });
    }

    /// A token typed over a sign-in clears the sign-in; a failed save puts it
    /// back, so the user is not left signed out by a save that did not happen.
    #[test]
    fn a_typed_token_replaces_a_sign_in_only_when_it_is_saved() {
        let signed_in = || {
            let (_, id) =
                add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
            crate::secrets::set_secret(&id, crate::remote::OAUTH_STATE_KEY, "refresh-state")
                .unwrap();
            crate::secrets::set_secret(&id, crate::secrets::HTTP_AUTH_KEY, "oauth-access").unwrap();
            id
        };
        let declare =
            |id: &str, key: &str| Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0);
        crate::secrets::tests::with_isolated_vault(|| {
            let id = signed_in();
            set_server_secret_with(&id, "Authorization", "Bearer typed", declare).unwrap();
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::remote::OAUTH_STATE_KEY).unwrap(),
                None
            );
        });
        for failing in ["keychain", "registry"] {
            crate::secrets::tests::with_isolated_vault(|| {
                let id = signed_in();
                let result = if failing == "keychain" {
                    crate::secrets::tests::with_failed_write(crate::secrets::HTTP_AUTH_KEY, || {
                        set_server_secret_with(&id, "Authorization", "Bearer typed", declare)
                    })
                } else {
                    set_server_secret_with(&id, "Authorization", "Bearer typed", |_, _| {
                        Err("synthetic registry failure".into())
                    })
                };
                assert!(result.is_err(), "{failing}");
                assert_eq!(
                    crate::secrets::get_secret_result(&id, crate::remote::OAUTH_STATE_KEY)
                        .unwrap()
                        .as_deref(),
                    Some("refresh-state"),
                    "{failing}"
                );
                assert_eq!(
                    crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY)
                        .unwrap()
                        .as_deref(),
                    Some("oauth-access"),
                    "{failing}"
                );
            });
        }
    }

    /// A shared Authorization header keeps its row whatever case or spacing the
    /// member types its name in: the value lands under the header's own name and
    /// replaces the team's source.
    #[test]
    fn a_shared_authorization_header_matches_its_name_loosely() {
        for typed in ["authorization", " Authorization ", "AUTHORIZATION"] {
            crate::secrets::tests::with_isolated_vault(|| {
                let (_, id) =
                    add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
                registry::update(|r| {
                    let server = r.servers.iter_mut().find(|s| s.id == id).unwrap();
                    server.unknown_fields.insert(
                        "headerKeys".into(),
                        serde_json::json!([{"key": "Authorization", "source": {"ref": "op://Team/api/token"}}]),
                    );
                    Ok(())
                })
                .unwrap();
                set_server_secret_with(&id, typed, "Bearer member-token", |id, key| {
                    Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0)
                })
                .unwrap();
                let secret = |key: &str| crate::secrets::get_secret_result(&id, key).unwrap();
                assert_eq!(
                    secret("Authorization").as_deref(),
                    Some("Bearer member-token"),
                    "{typed:?}"
                );
                assert_eq!(secret(crate::secrets::HTTP_AUTH_KEY), None, "{typed:?}");
                let server = registry::load()
                    .unwrap()
                    .servers
                    .into_iter()
                    .find(|s| s.id == id)
                    .unwrap();
                assert_eq!(
                    server.unknown_fields["headerKeys"],
                    serde_json::json!([{"key": "Authorization"}]),
                    "{typed:?}: the member's value replaces the team source"
                );
                assert!(
                    server.env.iter().all(|e| e.key == "Authorization"
                        || !e.key.eq_ignore_ascii_case("authorization")),
                    "{typed:?}: no second Authorization row: {:?}",
                    server.env
                );
            });
        }
    }

    /// Both paths read an Authorization value the same way: trimmed, split on any
    /// whitespace, a lone scheme is no token.
    #[test]
    fn authorization_values_read_the_same_in_both_paths() {
        for (value, token) in [
            ("abc123", Ok("abc123")),
            (" Bearer abc123", Ok("abc123")),
            ("Bearer\tabc123", Ok("abc123")),
            ("Bearer abc123 ", Ok("abc123")),
            ("Bearer  abc123", Ok("abc123")),
            ("bearer abc123", Ok("abc123")),
            ("Bearer", Err(AuthValueError::NoToken)),
            ("Basic", Err(AuthValueError::NoToken)),
            ("Basic dXNlcjpwYXNz", Err(AuthValueError::OtherScheme)),
            ("Token abc123", Err(AuthValueError::OtherScheme)),
            ("Bearer a b", Err(AuthValueError::OtherScheme)),
        ] {
            assert_eq!(bearer_token(value), token, "{value:?}");
            let stored = |id: &str| {
                crate::secrets::get_secret_result(id, crate::secrets::HTTP_AUTH_KEY).unwrap()
            };
            crate::secrets::tests::with_isolated_vault(|| {
                let rows = add_server_with_rows(
                    fields("demo", "http"),
                    vec![row("Authorization", value, true)],
                );
                assert_eq!(
                    rows.is_ok(),
                    token.is_ok(),
                    "rows {value:?}: {rows:?}",
                    rows = rows.as_ref().map(|_| ())
                );
                if let Ok(outcome) = rows {
                    let id = outcome.registry.servers[0].id.clone();
                    assert_eq!(
                        stored(&id).as_deref(),
                        token.as_ref().ok().copied(),
                        "rows {value:?}"
                    );
                }
            });
            crate::secrets::tests::with_isolated_vault(|| {
                let (_, id) =
                    add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
                let typed = set_server_secret_with(&id, "Authorization", value, |id, key| {
                    Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0)
                });
                assert_eq!(typed.is_ok(), token.is_ok(), "typed {value:?}");
                assert_eq!(
                    stored(&id).as_deref(),
                    token.as_ref().ok().copied(),
                    "typed {value:?}"
                );
            });
        }
    }

    /// A sign-in whose clear fails half way is put back, and a put-back that
    /// fails is named in the error instead of a claim that it was restored.
    #[test]
    fn a_sign_in_is_put_back_when_its_clear_or_the_save_fails() {
        let vault = std::cell::RefCell::new(std::collections::BTreeMap::from([
            (
                crate::remote::OAUTH_STATE_KEYS[0].to_string(),
                "cc-state".to_string(),
            ),
            (
                crate::remote::OAUTH_STATE_KEYS[1].to_string(),
                "refresh-state".to_string(),
            ),
        ]));
        let get = |_: &str, key: &str| Ok(vault.borrow().get(key).cloned());
        let set = |_: &str, key: &str, value: &str| {
            vault.borrow_mut().insert(key.into(), value.into());
            Ok(())
        };
        // The first delete works, the second fails: the first comes back.
        let delete = |_: &str, key: &str| {
            if key == crate::remote::OAUTH_STATE_KEYS[1] {
                return Err("locked".to_string());
            }
            vault.borrow_mut().remove(key);
            Ok(())
        };
        let error = take_sign_in_using("demo", get, delete, set).unwrap_err();
        assert!(!error.contains("could not be restored"), "{error}");
        assert_eq!(
            vault
                .borrow()
                .get(crate::remote::OAUTH_STATE_KEYS[0])
                .map(String::as_str),
            Some("cc-state")
        );
        // And when putting it back fails too, the error says so.
        let error = take_sign_in_using(
            "demo",
            get,
            |_: &str, key: &str| {
                vault.borrow_mut().remove(key);
                Err("locked".to_string())
            },
            |_: &str, _: &str, _: &str| Err("still locked".to_string()),
        )
        .unwrap_err();
        assert!(
            error.contains("the previous sign-in could not be restored"),
            "{error}"
        );

        // A failed save whose put-back fails says so too.
        crate::secrets::tests::with_isolated_vault(|| {
            let (_, id) =
                add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
            crate::secrets::set_secret(&id, crate::remote::OAUTH_STATE_KEY, "refresh-state")
                .unwrap();
            let error =
                crate::secrets::tests::with_failed_write(crate::remote::OAUTH_STATE_KEY, || {
                    set_server_secret_with(&id, "Authorization", "Bearer typed", |_, _| {
                        Err("synthetic registry failure".into())
                    })
                })
                .unwrap_err();
            assert!(
                error.contains("the previous sign-in could not be restored"),
                "{error}"
            );
        });
    }

    /// Authorization typed onto a remote server: a bare or Bearer token goes to
    /// the bearer slot, another scheme is refused, and a shared Authorization
    /// header keeps its own row so the member's value replaces the team's.
    #[test]
    fn authorization_values_go_where_they_are_sent() {
        crate::secrets::tests::with_isolated_vault(|| {
            let (_, id) =
                add_reviewed_entry(entry_from_fields(fields("demo", "http")).unwrap()).unwrap();
            let store = |value: &str| {
                set_server_secret_with(&id, "Authorization", value, |id, key| {
                    Ok(registry::update(|r| apply_secret_declaration(r, id, key))?.0)
                })
            };
            let bearer = || crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY);
            store("bare-token").unwrap();
            assert_eq!(bearer().unwrap().as_deref(), Some("bare-token"));
            store("Bearer typed-token").unwrap();
            assert_eq!(bearer().unwrap().as_deref(), Some("typed-token"));
            assert!(store("Basic dXNlcjpwYXNz").is_err());
            assert_eq!(bearer().unwrap().as_deref(), Some("typed-token"));

            registry::update(|r| {
                let server = r.servers.iter_mut().find(|s| s.id == id).unwrap();
                server.unknown_fields.insert(
                    "headerKeys".into(),
                    serde_json::json!([{"key": "Authorization", "source": {"ref": "op://Team/api/token"}}]),
                );
                server.unknown_fields.insert(
                    "memberSecretRefs".into(),
                    serde_json::json!({"header:Authorization": {"ref": "op://Team/api/token"}}),
                );
                Ok(())
            })
            .unwrap();
            store("Bearer member-token").unwrap();
            let server = registry::load()
                .unwrap()
                .servers
                .into_iter()
                .find(|s| s.id == id)
                .unwrap();
            assert_eq!(
                server.unknown_fields["headerKeys"],
                serde_json::json!([{"key": "Authorization"}]),
                "the member's value replaces the team source"
            );
            assert!(server.unknown_fields["memberSecretRefs"]
                .get("header:Authorization")
                .is_none());
            assert_eq!(
                crate::secrets::get_secret_result(&id, "Authorization")
                    .unwrap()
                    .as_deref(),
                Some("Bearer member-token")
            );
        });
    }

    fn test_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "toolport-controller-{label}-{}-{}.json",
            std::process::id(),
            scratch_label()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}.lock", path.display()));
        let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    }

    #[test]
    fn unreviewed_team_command_enable_is_refused() {
        let mut registry = Registry::default();
        let mut team = server("team-tool");
        team.source = Some("team:acme".into());
        registry.servers.push(team);

        let error = apply_server_enabled(&mut registry, "default", "team-tool", true, false)
            .expect_err("a team command requires review");
        assert!(error.contains("enable it from Teams after review"));
        assert!(!registry.is_enabled("default", "team-tool"));

        assert!(apply_server_enabled(&mut registry, "default", "team-tool", true, true).is_ok());
        assert!(apply_server_enabled(&mut registry, "default", "team-tool", false, false).is_ok());
    }

    #[test]
    fn essential_settings_report_effective_team_forced_values() {
        let mut registry = Registry::default();
        registry.deny_destructive = false;
        registry.team_min_safety_level = registry::SafetyLevel::Strict;
        registry.team_forced_deny_destructive = true;
        registry.human_approval = false;
        registry.team_forced_human_approval = true;
        registry.pii_redaction = false;
        registry.team_forced_pii_redaction = true;
        registry.lazy_discovery = false;
        registry.code_mode = false;
        registry.live_inspect = true;

        let settings = EssentialSettings::from_registry(&registry);

        assert!(settings.deny_destructive);
        assert!(settings.deny_destructive_forced);
        assert!(settings.human_approval);
        assert!(settings.human_approval_forced);
        assert!(settings.pii_redaction);
        assert!(settings.pii_redaction_forced);
        assert!(!settings.lazy_discovery);
        assert!(!settings.code_mode);
        assert!(settings.live_inspect);
    }

    #[test]
    fn essential_safety_control_reports_floor_and_independent_protections() {
        for floor in [
            registry::SafetyLevel::Off,
            registry::SafetyLevel::Ask,
            registry::SafetyLevel::Strict,
        ] {
            let mut registry = Registry::default();
            registry.set_safety_level(registry::SafetyLevel::Off);
            registry.team_min_safety_level = floor;
            registry.team_forced_quarantine_on_drift = true;
            registry.team_forced_block_on_injection = true;
            let settings = EssentialSettings::from_registry(&registry);
            assert_eq!(settings.safety_level, floor);
            assert_eq!(settings.team_min_safety_level, floor);
            assert_eq!(
                settings.deny_destructive,
                floor == registry::SafetyLevel::Strict
            );
            assert!(settings.quarantine_on_drift && settings.quarantine_on_drift_forced);
            assert!(settings.block_on_injection && settings.block_on_injection_forced);
        }
    }

    #[test]
    fn scoped_http_clients_validate_profiles_and_protect_managed_rows() {
        let mut registry = Registry::default();
        let error = apply_add_http_client(
            &mut registry,
            "external".into(),
            "Open WebUI".into(),
            "hash".into(),
            "missing".into(),
        )
        .expect_err("an unknown profile must be rejected");
        assert!(error.contains("profile no longer exists"));
        assert!(registry.http_clients.is_empty());

        apply_add_http_client(
            &mut registry,
            "external".into(),
            "Open WebUI".into(),
            "hash".into(),
            "default".into(),
        )
        .unwrap();
        registry.http_clients.push(crate::registry::HttpClient {
            id: "client:cursor".into(),
            label: "Client: cursor".into(),
            token_sha256: "managed-hash".into(),
            profile: String::new(),
            unknown_fields: Default::default(),
        });

        assert!(apply_remove_http_client(&mut registry, "client:cursor").is_err());
        assert_eq!(registry.http_clients.len(), 2);
        apply_remove_http_client(&mut registry, "external").unwrap();
        assert_eq!(registry.http_clients.len(), 1);
        assert_eq!(registry.http_clients[0].id, "client:cursor");
    }

    #[test]
    fn shared_profile_mutations_keep_registry_invariants() {
        let mut registry = Registry::default();
        apply_create_profile(&mut registry, "Work").unwrap();
        let work = registry
            .profiles
            .iter()
            .find(|profile| profile.name == "Work")
            .unwrap()
            .id
            .clone();

        apply_delete_profile(&mut registry, &work).unwrap();
        assert_eq!(registry.profiles.len(), 1);
        assert!(apply_delete_profile(&mut registry, "default").is_err());
    }

    #[test]
    fn failed_toggle_does_not_change_registry_bytes() {
        let path = test_path("failed");
        cleanup(&path);
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry::save_to(&path, &registry).unwrap();
        let original = std::fs::read(&path).unwrap();

        assert!(set_server_enabled_at(&path, "default", "missing", true, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        cleanup(&path);
    }

    #[test]
    fn toggle_loads_fresh_state_and_preserves_an_external_change() {
        let path = test_path("fresh");
        cleanup(&path);
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry::save_to(&path, &registry).unwrap();

        let mut external = registry::load_from(&path).unwrap();
        external.servers.push(server("two"));
        registry::save_to(&path, &external).unwrap();

        let updated = set_server_enabled_at(&path, "default", "one", true, false).unwrap();
        assert!(updated.server_enabled("one"));
        assert!(updated.servers.iter().any(|server| server.id == "two"));
        cleanup(&path);
    }

    #[test]
    fn concurrent_toggles_do_not_lose_each_other() {
        // Assert both updates survive contention, independent of runner scheduling.
        let _timeout = registry::LockTimeoutOverride::generous();
        let path = test_path("concurrent");
        cleanup(&path);
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry.servers.push(server("two"));
        registry::save_to(&path, &registry).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = ["one", "two"].map(|id| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                set_server_enabled_at(&path, "default", id, true, false)
            })
        });
        barrier.wait();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let updated = registry::load_from(&path).unwrap();
        assert!(updated.server_enabled("one"));
        assert!(updated.server_enabled("two"));
        cleanup(&path);
    }

    #[test]
    fn native_add_normalizes_fields_and_assigns_a_unique_id() {
        let mut registry = Registry::default();
        let first = apply_add_server(&mut registry, fields(" Example ", "stdio")).unwrap();
        let second = apply_add_server(&mut registry, fields("Example", "stdio")).unwrap();

        assert_eq!(first, "example");
        assert_eq!(second, "example-2");
        assert_eq!(registry.servers[0].name, "Example");
        assert_eq!(registry.servers[0].cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(registry.servers[0].source.as_deref(), Some("manual"));
    }

    #[test]
    fn field_update_preserves_secrets_policy_and_unknown_fields() {
        let mut registry = Registry::default();
        let mut existing = server("one");
        existing.env.push(crate::registry::EnvVar {
            key: "TOKEN".into(),
            value: None,
            secret: true,
            unknown_fields: Default::default(),
        });
        existing.disabled_tools.push("dangerous".into());
        existing
            .unknown_fields
            .insert("futureField".into(), serde_json::json!({"kept": true}));
        registry.servers.push(existing);

        apply_update_server_fields(&mut registry, "one", fields("Remote", "http")).unwrap();
        let updated = &registry.servers[0];
        assert_eq!(updated.name, "Remote");
        assert_eq!(updated.transport, "http");
        assert!(updated.command.is_none());
        assert!(updated.args.is_empty());
        assert!(updated.cwd.is_none());
        assert_eq!(updated.env[0].key, "TOKEN");
        assert_eq!(updated.disabled_tools, ["dangerous"]);
        assert_eq!(updated.unknown_fields["futureField"]["kept"], true);
    }

    #[test]
    fn native_field_edit_sets_or_keeps_the_shell_environment_choice() {
        let mut registry = Registry::default();
        let mut existing = server("one");
        existing.inherit_env = true;
        registry.servers.push(existing);
        apply_update_server_fields(&mut registry, "one", fields("One", "stdio")).unwrap();
        assert!(
            registry.servers[0].inherit_env,
            "None keeps the saved choice"
        );
        let mut off = fields("One", "stdio");
        off.inherit_env = Some(false);
        apply_update_server_fields(&mut registry, "one", off).unwrap();
        assert!(!registry.servers[0].inherit_env);
        let mut on = fields("One", "stdio");
        on.inherit_env = Some(true);
        assert!(entry_from_fields(on).unwrap().inherit_env);
    }

    #[test]
    fn native_field_edit_sets_or_keeps_hidden_prompts_on_any_transport() {
        let mut registry = Registry::default();
        let mut existing = server("one");
        existing.hide_prompts = true;
        registry.servers.push(existing);
        apply_update_server_fields(&mut registry, "one", fields("One", "stdio")).unwrap();
        assert!(
            registry.servers[0].hide_prompts,
            "None keeps the saved choice"
        );
        let mut off = fields("One", "stdio");
        off.hide_prompts = Some(false);
        apply_update_server_fields(&mut registry, "one", off).unwrap();
        assert!(!registry.servers[0].hide_prompts);
        // Unlike the shell environment, hiding prompts applies to remote servers too.
        let mut remote = fields("Remote", "http");
        remote.hide_prompts = Some(true);
        apply_update_server_fields(&mut registry, "one", remote).unwrap();
        assert!(registry.servers[0].hide_prompts);
        let mut new = fields("New", "http");
        new.hide_prompts = Some(true);
        assert!(entry_from_fields(new).unwrap().hide_prompts);
        assert!(
            !entry_from_fields(fields("New", "stdio"))
                .unwrap()
                .hide_prompts
        );
    }

    #[test]
    fn native_field_edit_keeps_or_clears_generated_binding_explicitly() {
        let mut registry = Registry::default();
        let mut existing = server("one");
        existing.command = Some("npx".into());
        existing.args = vec!["-y".into(), "pkg".into(), "<launch-input>".into()];
        existing.source = Some("catalog:curated".into());
        existing.launch = Some(crate::registry::LaunchConfig {
            inputs: vec![crate::registry::LaunchInput {
                key: "ROOT".into(),
                label: "Root".into(),
                secret: false,
                required: true,
                value: Some("/tmp/root".into()),
                unknown_fields: Default::default(),
            }],
            bindings: vec![crate::registry::ArgBinding {
                index: 2,
                parts: vec![crate::registry::ArgPart::Input {
                    key: "ROOT".into(),
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            }],
            ..Default::default()
        });
        registry.servers.push(existing);
        let same = ServerFields {
            sync_local_only: false,
            initialize_timeout_ms: None,
            name: "Renamed".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec!["-y".into(), "pkg".into(), "<launch-input>".into()],
            url: None,
            cwd: None,
            inherit_env: None,
            hide_prompts: None,
        };
        apply_update_server_fields(&mut registry, "one", same.clone()).unwrap();
        assert_eq!(
            registry.servers[0].launch.as_ref().unwrap().inputs[0]
                .value
                .as_deref(),
            Some("/tmp/root")
        );
        let mut stale = same.clone();
        stale.command = Some("node".into());
        assert!(apply_update_server_fields(&mut registry, "one", stale)
            .unwrap_err()
            .contains("Replace <launch-input>"));
        assert!(registry.servers[0].launch.is_some());
        let mut changed = same;
        changed.args[2] = "/literal/root".into();
        apply_update_server_fields(&mut registry, "one", changed).unwrap();
        assert!(registry.servers[0].launch.is_none());
        assert_eq!(registry.servers[0].source.as_deref(), Some("manual"));
    }

    #[test]
    fn invalid_native_edit_does_not_change_registry_bytes() {
        let path = test_path("invalid-edit");
        cleanup(&path);
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry::save_to(&path, &registry).unwrap();
        let original = std::fs::read(&path).unwrap();

        let result = registry::update_at(&path, |registry| {
            apply_update_server_fields(
                registry,
                "one",
                ServerFields {
                    sync_local_only: false,
                    initialize_timeout_ms: None,
                    name: "Broken".into(),
                    transport: "http".into(),
                    command: None,
                    args: Vec::new(),
                    url: Some("file:///tmp/not-an-mcp-server".into()),
                    cwd: None,
                    inherit_env: None,
                    hide_prompts: None,
                },
            )
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        cleanup(&path);
    }

    #[test]
    fn shared_remove_keeps_registry_cleanup_invariants() {
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry.profiles[0].enabled_server_ids.push("one".into());
        registry
            .human_approval_allow
            .push("one/tool/fingerprint".into());

        apply_remove_server(&mut registry, "one").unwrap();

        assert!(registry.servers.is_empty());
        assert!(!registry.profiles[0]
            .enabled_server_ids
            .contains(&"one".to_string()));
        assert!(!registry
            .human_approval_allow
            .contains(&"one/tool/fingerprint".to_string()));
    }

    #[test]
    fn secret_set_rolls_the_vault_back_when_registry_write_fails() {
        let vault = std::rc::Rc::new(std::cell::RefCell::new(
            [("TOKEN".to_string(), "old-value".to_string())]
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>(),
        ));
        let get_vault = vault.clone();
        let set_vault = vault.clone();
        let delete_vault = vault.clone();
        let error = set_server_secret_using(
            "one",
            "TOKEN",
            "new-value",
            move |_, key| Ok(get_vault.borrow().get(key).cloned()),
            move |_, key, value| {
                set_vault
                    .borrow_mut()
                    .insert(key.to_string(), value.to_string());
                Ok(())
            },
            move |_, key| {
                delete_vault.borrow_mut().remove(key);
                Ok(())
            },
            |_, _| Err("disk full".into()),
        )
        .expect_err("the registry failure must propagate");

        assert!(error.contains("rolled back"));
        assert_eq!(
            vault.borrow().get("TOKEN").map(String::as_str),
            Some("old-value")
        );
        assert!(!error.contains("old-value"));
        assert!(!error.contains("new-value"));
    }

    #[test]
    fn failed_first_secret_set_removes_the_new_vault_value() {
        let vault = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::<
            String,
            String,
        >::new()));
        let get_vault = vault.clone();
        let set_vault = vault.clone();
        let delete_vault = vault.clone();
        let error = set_server_secret_using(
            "one",
            "TOKEN",
            "new-value",
            move |_, key| Ok(get_vault.borrow().get(key).cloned()),
            move |_, key, value| {
                set_vault
                    .borrow_mut()
                    .insert(key.to_string(), value.to_string());
                Ok(())
            },
            move |_, key| {
                delete_vault.borrow_mut().remove(key);
                Ok(())
            },
            |_, _| Err("server disappeared".into()),
        )
        .expect_err("the registry failure must propagate");

        assert!(error.contains("rolled back"));
        assert!(vault.borrow().is_empty());
        assert!(!error.contains("new-value"));
    }

    #[test]
    fn secret_delete_rolls_the_vault_back_when_registry_write_fails() {
        let vault = std::rc::Rc::new(std::cell::RefCell::new(
            [("TOKEN".to_string(), "keep-me".to_string())]
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>(),
        ));
        let get_vault = vault.clone();
        let set_vault = vault.clone();
        let delete_vault = vault.clone();
        let error = delete_server_secret_using(
            "one",
            "TOKEN",
            move |_, key| Ok(get_vault.borrow().get(key).cloned()),
            move |_, key, value| {
                set_vault
                    .borrow_mut()
                    .insert(key.to_string(), value.to_string());
                Ok(())
            },
            move |_, key| {
                delete_vault.borrow_mut().remove(key);
                Ok(())
            },
            |_, _| Err("disk full".into()),
        )
        .expect_err("the registry failure must propagate");

        assert!(error.contains("rolled back"));
        assert_eq!(
            vault.borrow().get("TOKEN").map(String::as_str),
            Some("keep-me")
        );
        assert!(!error.contains("keep-me"));
    }

    #[test]
    fn secret_declarations_require_a_real_server_and_never_store_values() {
        let mut registry = Registry::default();
        assert!(apply_secret_declaration(&mut registry, "missing", "TOKEN").is_err());
        registry.servers.push(server("one"));

        apply_secret_declaration(&mut registry, "one", "TOKEN").unwrap();

        let env = &registry.servers[0].env[0];
        assert_eq!(env.key, "TOKEN");
        assert!(env.secret);
        assert!(env.value.is_none());
        assert_eq!(registry.secrets_generation, 1);
    }

    #[test]
    fn auth_mutation_lock_serializes_writes_and_releases_on_drop() {
        let path = test_path("auth-mutation-lock").with_extension("lock");
        cleanup(&path);
        let first = try_acquire_auth_lock(&path)
            .expect("first mutation lock should not fail")
            .expect("first mutation lock should be acquired");
        assert!(
            try_acquire_auth_lock(&path)
                .expect("second mutation lock should not fail")
                .is_none(),
            "a concurrent token or OAuth write must wait"
        );
        drop(first);
        let second = try_acquire_auth_lock(&path)
            .expect("lock reacquisition should not fail")
            .expect("lock should be available after release");
        drop(second);
        cleanup(&path);
    }

    #[test]
    fn customized_client_requires_explicit_force() {
        assert!(refuse_customized_client(Some(GatewayEntryState::Customized), false).is_err());
        assert!(refuse_customized_client(Some(GatewayEntryState::Customized), true).is_ok());
        assert!(refuse_customized_client(Some(GatewayEntryState::Managed), false).is_ok());
        assert!(refuse_customized_client(Some(GatewayEntryState::Absent), false).is_ok());
    }

    #[test]
    fn failed_client_registry_write_restores_the_original_config() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-client-rollback-{}-{}",
            std::process::id(),
            scratch_label()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data_lock = registry::data_dir_test_lock();
        let _data_dir = registry::DataDirOverride::set(dir.join("data"));
        let target = dir.join("client.json");
        // Rollback reads only backups written by `backup_config`, never raw files.
        std::fs::write(&target, "original config").unwrap();
        let backup = clients::backup_config("cursor", &target).unwrap().unwrap();
        std::fs::write(&target, "connected config").unwrap();
        let outcome = WriteOutcome {
            existed_before: true,
            path: target.display().to_string(),
            backup: Some(backup.display().to_string()),
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };

        let error = finish_client_config_mutation(outcome, |_| Err("registry full".into()))
            .expect_err("registry failure must roll the client file back");

        assert!(error.contains("rolled back"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original config");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_first_client_connect_removes_only_the_file_it_just_created() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-client-first-connect-{}-{}",
            std::process::id(),
            scratch_label()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data_lock = registry::data_dir_test_lock();
        let _data_dir = registry::DataDirOverride::set(dir.join("data"));
        let target = dir.join("client.json");
        std::fs::write(&target, "connected config").unwrap();
        let outcome = WriteOutcome {
            existed_before: false,
            path: target.display().to_string(),
            backup: None,
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };

        let error = finish_client_config_mutation(outcome, |_| Err("registry full".into()))
            .expect_err("registry failure must remove the new client file");

        assert!(error.contains("rolled back"));
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn client_rollback_never_overwrites_a_newer_external_change() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-client-race-{}-{}",
            std::process::id(),
            scratch_label()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data_lock = registry::data_dir_test_lock();
        let _data_dir = registry::DataDirOverride::set(dir.join("data"));
        let target = dir.join("client.json");
        let backup = dir.join("backup.json");
        std::fs::write(&backup, "original config").unwrap();
        std::fs::write(&target, "connected config").unwrap();
        let outcome = WriteOutcome {
            existed_before: true,
            path: target.display().to_string(),
            backup: Some(backup.display().to_string()),
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };
        let target_for_write = target.clone();

        let error = finish_client_config_mutation(outcome, |_| {
            std::fs::write(&target_for_write, "newer external config").unwrap();
            Err("registry full".into())
        })
        .expect_err("the newer client file must make rollback fail closed");

        assert!(error.contains("left the newer file untouched"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "newer external config"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pinned_prerequisites_are_named_and_sorted_for_every_shell() {
        let mut registry = Registry::default();
        let mut beta = server("beta-id");
        beta.name = "Beta".into();
        let mut alpha = server("alpha-id");
        alpha.name = "Alpha".into();
        registry.servers.extend([beta, alpha]);
        registry
            .pinned_tools
            .insert("beta-id".into(), vec!["write".into(), "read".into()]);
        registry
            .pinned_tools
            .insert("alpha-id".into(), vec!["search".into()]);

        let pins = pinned_prerequisites_from(&registry);

        assert_eq!(
            pins.iter()
                .map(|pin| format!("{}/{}", pin.server, pin.tool))
                .collect::<Vec<_>>(),
            ["Alpha/search", "Beta/read", "Beta/write"]
        );
    }

    #[test]
    fn persisted_secret_round_trip_keeps_the_value_out_of_the_registry() {
        let _data = registry::data_dir_test_lock();
        let _env = registry::REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "toolport-controller-secret-roundtrip-{}-{}",
            std::process::id(),
            scratch_label()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let override_dir = crate::registry::DataDirOverride::set(&dir);
        let previous_key = std::env::var_os("TOOLPORT_SECRET_KEY");
        struct RestoreKey(Option<std::ffi::OsString>);
        impl Drop for RestoreKey {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("TOOLPORT_SECRET_KEY", value),
                    None => std::env::remove_var("TOOLPORT_SECRET_KEY"),
                }
            }
        }
        let restore_key = RestoreKey(previous_key);
        std::env::set_var("TOOLPORT_SECRET_KEY", "registry-controller-test-secret-key");
        let mut registry = Registry::default();
        registry.servers.push(server("one"));
        registry::save(&registry).unwrap();

        let stored = set_server_secret("one", "TOKEN", "vault-only-value").unwrap();
        assert_eq!(
            crate::secrets::get_secret_result("one", "TOKEN").unwrap(),
            Some("vault-only-value".into())
        );
        assert_eq!(stored.servers[0].env[0].key, "TOKEN");
        assert!(stored.servers[0].env[0].value.is_none());
        assert!(!std::fs::read_to_string(dir.join("registry.json"))
            .unwrap()
            .contains("vault-only-value"));

        let removed = delete_server_secret("one", "TOKEN").unwrap();
        assert!(removed.servers[0].env.is_empty());
        assert_eq!(
            crate::secrets::get_secret_result("one", "TOKEN").unwrap(),
            None
        );

        drop(restore_key);
        drop(override_dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_turns_imported_servers_on_in_the_active_profile() {
        let mut registry = Registry::default();
        let id = apply_import_entry(&mut registry, server("memory"));
        assert!(
            registry.is_enabled("default", &id),
            "UX-01: an import must serve tools"
        );
    }

    /// Every connect and disconnect path names the config in an operating system error.
    #[cfg(unix)]
    #[test]
    fn connect_and_disconnect_errors_name_the_config_file() {
        use std::os::unix::fs::PermissionsExt;
        // Root ignores the permission bits these failures come from.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = MoveFixture::new(&Registry::default());
        let config = fixture.claude();
        let folder = config.parent().unwrap().to_path_buf();
        // As the error shows it: `~/...` when HOME holds the fixture.
        let shown = crate::clients::name_file_in_error(&config, "(os error 0)".into())
            .trim_end_matches(": (os error 0)")
            .to_string();
        let named = |error: Option<String>, what: &str| {
            let error = error.unwrap_or_else(|| panic!("{what} should fail"));
            assert!(
                error.contains("(os error") && error.contains(&shown),
                "{what}: {error}"
            );
        };
        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        std::fs::write(&config, r#"{"mcpServers":{"one":{"command":"one"}}}"#).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let reviewed = |names: &[String]| {
            migrate_client_reviewed_with(
                "claude-code",
                None,
                false,
                names,
                &review.revision,
                |_, _, _, _| Ok(Vec::new().into()),
            )
            .err()
        };

        // The file is read-only and its folder cannot be written: the write fails.
        mode(&config, 0o444);
        mode(&folder, 0o555);
        let connect = connect_client_stdio("claude-code", None, false).err();
        let moved = reviewed(&["one".into()]);
        mode(&folder, 0o755);
        mode(&config, 0o644);
        named(connect, "connect");
        named(moved, "reviewed connect");

        connect_client_stdio("claude-code", None, false).unwrap();
        mode(&folder, 0o555);
        let disconnect = disconnect_client("claude-code").err();
        mode(&folder, 0o755);
        named(disconnect, "disconnect");

        // The config cannot be read: the review and the connect both say which file.
        mode(&config, 0o000);
        let preview = preview_client_setup("claude-code").err();
        let moved = reviewed(&[]);
        mode(&config, 0o644);
        named(preview, "review");
        named(moved, "reviewed connect of an unreadable config");
    }

    /// A scratch home for Claude Code and Codex configs plus an overridden data dir
    /// (registry, config backups, move records). Fields drop in order, so the env
    /// vars and data dir are put back before the locks are released.
    struct MoveFixture {
        root: PathBuf,
        _vars: Vec<clients::EnvRestore>,
        _data_dir: registry::DataDirOverride,
        _data_lock: std::sync::MutexGuard<'static, ()>,
        _env_lock: std::sync::MutexGuard<'static, ()>,
        _registry_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl MoveFixture {
        fn new(registry: &Registry) -> Self {
            let data_lock = registry::data_dir_test_lock();
            let registry_lock = registry::REGISTRY_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let env_lock = clients::env_test_lock();
            let root = std::env::temp_dir().join(format!(
                "toolport-controller-move-{}-{}",
                std::process::id(),
                scratch_label()
            ));
            let _ = std::fs::remove_dir_all(&root);
            for dir in ["data", "claude", "codex"] {
                std::fs::create_dir_all(root.join(dir)).unwrap();
            }
            let data_dir = registry::DataDirOverride::set(root.join("data"));
            let vars = vec![
                clients::EnvRestore::set("CLAUDE_CONFIG_DIR", &root.join("claude")),
                clients::EnvRestore::set("CODEX_HOME", &root.join("codex")),
                clients::EnvRestore::set(
                    "TOOLPORT_SECRET_KEY",
                    std::path::Path::new("synthetic-import-fixture"),
                ),
            ];
            registry::save(registry).unwrap();
            Self {
                root,
                _vars: vars,
                _data_dir: data_dir,
                _data_lock: data_lock,
                _env_lock: env_lock,
                _registry_lock: registry_lock,
            }
        }

        fn claude(&self) -> PathBuf {
            self.root.join("claude").join(".claude.json")
        }

        fn codex(&self) -> PathBuf {
            self.root.join("codex").join("config.toml")
        }

        fn move_record(&self, client_id: &str) -> PathBuf {
            self.root
                .join("data")
                .join("backups")
                .join(client_id)
                .join("moved-servers.json")
        }
    }

    impl Drop for MoveFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn openhands_reviewed_move(layout: &str) {
        for native_add in [false, true] {
            let fixture = MoveFixture::new(&Registry::default());
            let folder = fixture.root.join("openhands");
            std::fs::create_dir_all(&folder).unwrap();
            let _persistence = clients::EnvRestore::set("OPENHANDS_PERSISTENCE_DIR", &folder);
            let path = folder.join("mcp.json");
            let servers = serde_json::json!({
                "git": {"command":"fixture-git", "env":{"PAT":"synthetic-private-pat"}},
                "docs": {"url":"https://example.test/mcp", "headers":{"Authorization":"Bearer synthetic-private-header"}},
                "off": {"command":"off", "enabled":false, "custom":{"keep":true}}
            });
            let original = match layout {
                "root" => servers.clone(),
                "mixed" => {
                    serde_json::json!({"ignored":{"command":"root-extra"}, "mcpServers":servers})
                }
                _ => serde_json::json!({"mcpServers":servers}),
            };
            let bytes = format!("{}\n", serde_json::to_string_pretty(&original).unwrap());
            std::fs::write(&path, &bytes).unwrap();
            let review = preview_client_setup("openhands").unwrap();
            let names = review
                .items
                .iter()
                .map(|item| item.name.clone())
                .collect::<Vec<_>>();
            assert_eq!(names, ["docs", "git"]);
            assert!(review.items.iter().all(|item| item.unsupported.is_none()));
            migrate_client_reviewed_with(
                "openhands",
                None,
                false,
                &names,
                &review.revision,
                |registry, _, _, _| {
                    let git = registry
                        .servers
                        .iter()
                        .find(|server| server.name == "git")
                        .unwrap();
                    assert_eq!(
                        crate::secrets::get_vault_secret_result(&git.id, "PAT")
                            .unwrap()
                            .as_deref(),
                        Some("synthetic-private-pat")
                    );
                    let text = serde_json::to_string(registry).unwrap();
                    assert!(!text.contains("synthetic-private-pat"));
                    assert!(!text.contains("synthetic-private-header"));
                    Ok(Vec::new().into())
                },
            )
            .unwrap();
            let mut connected = json_file(&path);
            assert!(connected["mcpServers"]["toolport"].is_object());
            assert_eq!(connected["mcpServers"]["off"], servers["off"]);
            assert!(connected["mcpServers"].get("git").is_none());
            assert!(connected["mcpServers"].get("docs").is_none());
            if layout == "mixed" {
                assert_eq!(connected["ignored"], original["ignored"]);
            } else {
                assert!(connected.get("git").is_none());
            }
            if native_add {
                connected["mcpServers"]["native"] = serde_json::json!({"command":"native"});
                std::fs::write(&path, connected.to_string()).unwrap();
            }
            let outcome = clients::uninstall_gateway("openhands").unwrap();
            clients::finish_uninstall("openhands", &outcome).unwrap();
            if native_add {
                let mut expected = if layout == "root" {
                    serde_json::json!({"mcpServers":servers})
                } else {
                    original.clone()
                };
                expected["mcpServers"]["native"] = serde_json::json!({"command":"native"});
                assert_eq!(json_file(&path), expected);
            } else {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn openhands_reviewed_move_reads_root_servers_and_private_credentials() {
        openhands_reviewed_move("root");
    }

    #[test]
    fn openhands_reviewed_move_uses_only_mcp_servers_in_wrapped_and_mixed_files() {
        openhands_reviewed_move("wrapped");
        openhands_reviewed_move("mixed");
    }

    fn enabled_names(registry: &Registry, profile: &str) -> Vec<String> {
        let mut names = registry
            .servers
            .iter()
            .filter(|server| registry.is_enabled(&registry.resolve_profile_id(profile), &server.id))
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn json_file(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn assert_stdio_conversion_revokes_shared_http(migrate: bool) {
        for shared_http in [true, false] {
            let mut registry = Registry::default();
            registry.http_clients.push(crate::registry::HttpClient {
                id: "client:other".into(),
                label: "Other client".into(),
                token_sha256: "other-hash".into(),
                profile: String::new(),
                unknown_fields: Default::default(),
            });
            if shared_http {
                registry.http_clients.push(crate::registry::HttpClient {
                    id: "client:claude-code".into(),
                    label: "Claude Code".into(),
                    token_sha256: "old-hash".into(),
                    profile: String::new(),
                    unknown_fields: Default::default(),
                });
            }
            let fixture = MoveFixture::new(&registry);
            let _key = clients::EnvRestore::set(
                "TOOLPORT_SECRET_KEY",
                Path::new("stdio-conversion-test-key"),
            );
            crate::secrets::set_secret(CLIENT_HTTP_VAULT_SERVER, "other", "keep").unwrap();
            if shared_http {
                crate::secrets::set_secret(CLIENT_HTTP_VAULT_SERVER, "claude-code", "old-token")
                    .unwrap();
            }
            let gateway = if shared_http {
                serde_json::json!({"url": "http://127.0.0.1:8765/mcp",
                    "headers": {"Authorization": "Bearer old-token"}})
            } else {
                serde_json::json!({"command": "toolport-gateway", "args": ["--client", "claude-code"]})
            };
            std::fs::write(
                fixture.claude(),
                serde_json::to_string(&serde_json::json!({"mcpServers": {"toolport": gateway}}))
                    .unwrap(),
            )
            .unwrap();
            let result = if migrate {
                connect_fixture("claude-code", Some("default"), true)
                    .unwrap()
                    .result
            } else {
                connect_client_stdio("claude-code", Some("default"), true).unwrap()
            };
            assert_eq!(result.registry.http_clients.len(), 1);
            assert_eq!(result.registry.http_clients[0].id, "client:other");
            assert_eq!(read_registry_exact().unwrap().http_clients.len(), 1);
            assert_eq!(
                crate::secrets::get_secret_result(CLIENT_HTTP_VAULT_SERVER, "claude-code").unwrap(),
                None
            );
            assert_eq!(
                crate::secrets::get_secret_result(CLIENT_HTTP_VAULT_SERVER, "other").unwrap(),
                Some("keep".into())
            );
            let config = json_file(&fixture.claude());
            assert!(config["mcpServers"]["toolport"]["command"].is_string());
            assert!(config["mcpServers"]["toolport"].get("url").is_none());
        }
    }

    #[test]
    fn rescope_to_stdio_revokes_shared_http_row_and_secret() {
        assert_stdio_conversion_revokes_shared_http(false);
    }

    #[test]
    fn migrate_to_stdio_revokes_shared_http_row_and_secret() {
        assert_stdio_conversion_revokes_shared_http(true);
    }

    #[test]
    fn client_save_before_receipt_capture_updates_registry_with_warning() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(), "{\"mcpServers\":{}}").unwrap();
        let outcome = clients::install_gateway("claude-code", None).unwrap();
        let mut native = json_file(&fixture.claude());
        native["session"] = serde_json::json!(2);
        let native = native.to_string();
        std::fs::write(fixture.claude(), &native).unwrap();
        let result = finish_client_config_mutation(outcome, |managed| {
            let (registry, ()) = registry::update(|registry| {
                registry.set_client_managed_entry("claude-code", managed.unwrap());
                Ok(())
            })?;
            Ok(registry)
        })
        .unwrap();
        assert!(result
            .registry
            .client_managed_entries
            .contains_key("claude-code"));
        assert!(result.outcome.warnings[0].contains("exact rollback is unavailable"));
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), native);
        assert!(
            !ClientConfigReceipt::capture(&result.outcome)
                .unwrap()
                .exact_rollback
        );
        let snapshot: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(result.outcome.recovery_path.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot["exactEligible"], false);
        disconnect_client("claude-code").unwrap();
        assert_eq!(json_file(&fixture.claude())["session"], 2);
    }

    /// Every file under the data dir and the client config, byte for byte.
    fn locked_disconnect_snapshot(fixture: &MoveFixture) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if !path.extension().is_some_and(|ext| ext == "lock") {
                    out.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = vec![(fixture.claude(), std::fs::read(fixture.claude()).unwrap())];
        walk(&fixture.root.join("data"), &mut out);
        out.sort();
        out
    }

    const LEGACY_VALUE: &str = "wx-legacy-fixture-value";
    const NEW_VALUE: &str = "mercury-new-fixture-value";

    /// F23: Claude Code connected by 2.3.5, so its recovery files hold
    /// WEATHER_API_KEY in plain text; since then Toolport stores that value
    /// as a secret. With `new_secret`, the user also flagged PLANET secret and
    /// set a new value for it in the client. The backup folder already holds
    /// five old generations of the config.
    fn locked_disconnect_fixture(new_secret: bool) -> (MoveFixture, String) {
        let fixture = MoveFixture::new(&Registry::default());
        let original = format!(
            r#"{{"mcpServers":{{"weather":{{"command":"echo","env":{{"WEATHER_API_KEY":"{LEGACY_VALUE}"}}}}}}}}"#
        );
        std::fs::write(fixture.claude(), &original).unwrap();
        let backups = fixture.root.join("data/backups/claude-code");
        std::fs::create_dir_all(&backups).unwrap();
        let name = fixture
            .claude()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for stamp in 1..=5 {
            std::fs::write(backups.join(format!("{stamp}000000000000-{name}")), "{}").unwrap();
        }
        // Nothing is known secret yet, so nothing is sealed, as in 2.3.5.
        clients::install_gateway("claude-code", None).unwrap();
        assert!(std::fs::read_dir(&backups).unwrap().flatten().any(|entry| {
            std::fs::read_to_string(entry.path()).is_ok_and(|raw| raw.contains(LEGACY_VALUE))
        }));
        registry::update(|registry| {
            let mut env = vec![serde_json::json!({"key":"WEATHER_API_KEY","secret":true})];
            if new_secret {
                env.push(serde_json::json!({"key":"PLANET","secret":true}));
            }
            registry.servers.push(
                serde_json::from_value(serde_json::json!({
                    "id":"weather","name":"Weather","transport":"stdio","env":env
                }))
                .unwrap(),
            );
            Ok(())
        })
        .unwrap();
        crate::secrets::set_secret("weather", "WEATHER_API_KEY", LEGACY_VALUE).unwrap();
        if new_secret {
            crate::secrets::set_secret("weather", "PLANET", NEW_VALUE).unwrap();
            let mut config = json_file(&fixture.claude());
            config["mcpServers"]["planet"] = serde_json::json!({
                "command":"echo","env":{"PLANET":NEW_VALUE}
            });
            std::fs::write(fixture.claude(), config.to_string()).unwrap();
        }
        (fixture, original)
    }

    fn generations(fixture: &MoveFixture) -> usize {
        let name = fixture
            .claude()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::fs::read_dir(fixture.root.join("data/backups/claude-code"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("-{name}"))
            })
            .count()
    }

    fn unlock_prompts(calls: &[String]) -> usize {
        calls.iter().filter(|call| *call == "unlock prompt").count()
    }

    /// F23: a Disconnect the user chose while the keyring is locked asks to
    /// unlock once, then disconnects through the sealed path and restores the
    /// original. On main it failed with "Saved credential recovery: Locked".
    #[test]
    fn locked_disconnect_of_a_2_3_5_client_asks_once_then_restores() {
        let (fixture, original) = locked_disconnect_fixture(false);
        let mut result = None;
        let calls =
            clients::backup_vault::fake::locked_store(clients::backup_vault::Unlock::Open, || {
                result = Some(disconnect_client_asking_to_unlock("claude-code"))
            });
        result.unwrap().unwrap();
        assert_eq!(unlock_prompts(&calls), 1, "{calls:?}");
        assert!(
            !calls.iter().any(|call| call.starts_with("prompting")),
            "{calls:?}"
        );
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
    }

    /// A new secret: after the one unlock the Disconnect finishes and the
    /// value is sealed, in no file in plain text.
    #[test]
    fn locked_disconnect_with_a_new_secret_seals_it_after_one_unlock() {
        let (fixture, _) = locked_disconnect_fixture(true);
        let mut result = None;
        let calls =
            clients::backup_vault::fake::locked_store(clients::backup_vault::Unlock::Open, || {
                result = Some(disconnect_client_asking_to_unlock("claude-code"))
            });
        result.unwrap().unwrap();
        assert_eq!(unlock_prompts(&calls), 1, "{calls:?}");
        let plain: Vec<_> = locked_disconnect_snapshot(&fixture)
            .into_iter()
            .filter(|(path, raw)| {
                *path != fixture.claude() && String::from_utf8_lossy(raw).contains(NEW_VALUE)
            })
            .map(|(path, _)| path)
            .collect();
        assert!(plain.is_empty(), "{plain:?}");
        assert!(!json_file(&fixture.claude())["mcpServers"]
            .as_object()
            .unwrap()
            .contains_key(clients::GATEWAY_ENTRY_NAME));
    }

    /// Cancel: the line says what to do, and the config, the registry and
    /// every backup file (all five old generations too) are unchanged byte
    /// for byte.
    #[test]
    fn locked_disconnect_cancel_changes_nothing() {
        for new_secret in [false, true] {
            let (fixture, _) = locked_disconnect_fixture(new_secret);
            let before = locked_disconnect_snapshot(&fixture);
            let kept = generations(&fixture);
            let mut result = None;
            let calls = clients::backup_vault::fake::locked_store(
                clients::backup_vault::Unlock::StillLocked,
                || result = Some(disconnect_client_asking_to_unlock("claude-code")),
            );
            assert_eq!(
                result.unwrap().unwrap_err(),
                "Unlock your keyring to disconnect Claude Code."
            );
            assert_eq!(unlock_prompts(&calls), 1, "{calls:?}");
            assert!(
                !calls.iter().any(|call| call.starts_with("prompting")),
                "{calls:?}"
            );
            assert_eq!(generations(&fixture), kept);
            assert!(
                locked_disconnect_snapshot(&fixture) == before,
                "new_secret={new_secret}"
            );
            drop(fixture);
        }
    }

    /// Remove from all clients, purge and `--disconnect-all` never ask to
    /// unlock: a locked client fails at once, as on main.
    #[test]
    fn bulk_and_cli_disconnect_never_ask_to_unlock() {
        let (fixture, _) = locked_disconnect_fixture(false);
        let mut single = None;
        let mut all = None;
        let calls =
            clients::backup_vault::fake::locked_store(clients::backup_vault::Unlock::Open, || {
                single = Some(disconnect_client("claude-code"));
                all = Some(clients::disconnect_all(false));
                // Purge runs the same per-client Disconnect.
                let _ = clients::disconnect_all_for_purge(false);
            });
        assert_eq!(unlock_prompts(&calls), 0, "{calls:?}");
        let error = single.unwrap().unwrap_err();
        assert!(clients::backup_vault::is_locked_error(&error), "{error}");
        let results = all.unwrap().unwrap();
        let claude = results
            .iter()
            .find(|result| result.client_id == "claude-code")
            .unwrap();
        assert!(
            claude
                .error
                .as_deref()
                .is_some_and(clients::backup_vault::is_locked_error),
            "{results:?}"
        );
        assert!(json_file(&fixture.claude())["mcpServers"]
            .as_object()
            .unwrap()
            .contains_key(clients::GATEWAY_ENTRY_NAME));
    }

    #[test]
    fn http_disconnect_finishes_before_failed_keychain_revocation() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = "{ \"mcpServers\": {} }";
        std::fs::write(fixture.claude(), original).unwrap();
        clients::install_gateway("claude-code", None).unwrap();
        registry::update(|registry| {
            registry.http_clients.push(registry::HttpClient {
                id: "client:claude-code".into(),
                label: "Claude Code".into(),
                token_sha256: registry::sha256_hex("fixture"),
                profile: String::new(),
                unknown_fields: Default::default(),
            });
            Ok(())
        })
        .unwrap();
        let result = disconnect_client_with_revocation("claude-code", || {
            let record = fixture.root.join("data/backups/claude-code");
            let snapshot = std::fs::read_dir(record)
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| entry.file_name().to_string_lossy().starts_with("released-"))
                .unwrap();
            let record: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(snapshot.path()).unwrap()).unwrap();
            assert_eq!(record["disconnected"], true);
            assert!(
                record["original"].is_null(),
                "verified recovery payload must be released"
            );
            assert!(record["disconnectBefore"].is_null());
            assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
            Err("keychain unreachable".into())
        })
        .unwrap();
        assert!(result.registry.http_clients.is_empty());
        assert!(result
            .outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("keychain unreachable")));
    }

    #[test]
    fn edited_toolport_entry_is_reported_after_disconnect() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(fixture.claude(), "{}").unwrap();
        clients::install_gateway("claude-code", None).unwrap();
        let mut native = json_file(&fixture.claude());
        native["mcpServers"][clients::GATEWAY_ENTRY_NAME]["args"] = serde_json::json!(["--custom"]);
        std::fs::write(fixture.claude(), native.to_string()).unwrap();
        let result = disconnect_client("claude-code").unwrap();
        assert!(result
            .outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("kept the toolport entry you edited")));
        assert_eq!(json_file(&fixture.claude()), native);
    }

    /// UX-02 and UX-03 for Claude Code: a move turns the servers on (including one
    /// Toolport already had, disabled), and Disconnect puts the original entries
    /// back, secrets and unknown fields included, without touching app state.
    /// F1: a retried Disconnect while values are still owed must never delete a
    /// config that existed before it when the registry write then fails. It
    /// takes a backup, and without one rollback refuses instead of removing.
    #[test]
    fn failed_registry_write_on_pending_retry_keeps_the_existing_config() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","server-memory"],"env":{"API_KEY":"sk-pending-retry-fixture"}}}}"#,
        )
        .unwrap();
        connect_fixture("claude-code", None, false).unwrap();
        std::env::set_var("TOOLPORT_SECRET_KEY", "locked-pending-retry");
        disconnect_client("claude-code").unwrap();
        let pending = std::fs::read_to_string(fixture.claude()).unwrap();
        assert!(
            pending.contains("TOOLPORT_MISSING_CREDENTIAL_"),
            "{pending}"
        );
        assert!(fixture.move_record("claude-code").exists());
        let error =
            disconnect_client_stdio_with("claude-code", false, |_| Err("registry full".into()))
                .unwrap_err();
        assert!(fixture.claude().exists(), "{error}");
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), pending);

        // Rollback without a backup removes only a config this operation made.
        let outcome = WriteOutcome {
            existed_before: true,
            path: fixture.claude().display().to_string(),
            backup: None,
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };
        let error =
            finish_client_config_mutation(outcome, |_| Err("registry full".into())).unwrap_err();
        assert!(!error.contains("rolled back"), "{error}");
        assert!(error.contains("no backup"), "{error}");
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), pending);
    }

    #[test]
    fn move_enables_servers_and_disconnect_restores_claude_code() {
        let mut existing = server("seq-thinking");
        existing.id = "seq-thinking".into();
        existing.command = Some("npx".into());
        existing.args = vec![
            "-y".into(),
            "@modelcontextprotocol/server-sequential-thinking".into(),
        ];
        let mut registry = Registry::default();
        registry.servers.push(existing);
        let fixture = MoveFixture::new(&registry);
        let servers = serde_json::json!({
            "memory": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"]},
            "seq-thinking": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-sequential-thinking"],
                "env": {"API_KEY": "kept-secret"},
                "alwaysAllow": ["think"]
            }
        });
        std::fs::write(
            fixture.claude(),
            serde_json::to_string_pretty(&serde_json::json!({
                "numStartups": 3,
                "mcpServers": servers
            }))
            .unwrap(),
        )
        .unwrap();

        let outcome = connect_fixture("claude-code", None, false).unwrap();
        let mut moved = outcome.moved.clone();
        moved.sort();
        assert_eq!(moved, ["memory", "seq-thinking"]);
        assert_eq!(
            enabled_names(&outcome.result.registry, ""),
            ["memory", "seq-thinking"],
            "every moved server must be served after the move"
        );
        let after = json_file(&fixture.claude());
        assert_eq!(
            after["mcpServers"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            [clients::GATEWAY_ENTRY_NAME]
        );
        assert!(fixture.move_record("claude-code").exists());

        let disconnected = disconnect_client("claude-code").unwrap();
        let mut restored = disconnected.outcome.restored.clone();
        restored.sort();
        assert_eq!(restored, ["memory", "seq-thinking"]);
        let after = json_file(&fixture.claude());
        assert_eq!(
            after["mcpServers"], servers,
            "Disconnect must restore the moved entries"
        );
        assert_eq!(after["numStartups"], 3);
        assert!(!fixture.move_record("claude-code").exists());
    }

    /// A Disconnect whose registry update fails rolls the config back to before the
    /// restore, so the move record must survive for the next attempt.
    #[test]
    fn failed_disconnect_keeps_the_move_record() {
        let fixture = MoveFixture::new(&Registry::default());
        let servers = serde_json::json!({
            "memory": {"command": "npx", "env": {"API_KEY": "kept-secret"}}
        });
        std::fs::write(
            fixture.claude(),
            serde_json::to_string(&serde_json::json!({ "mcpServers": servers })).unwrap(),
        )
        .unwrap();
        connect_fixture("claude-code", None, false).unwrap();
        let moved_config = std::fs::read_to_string(fixture.claude()).unwrap();

        let error =
            disconnect_client_stdio_with("claude-code", false, |_| Err("registry full".into()))
                .unwrap_err();
        assert!(error.contains("rolled back"), "{error}");
        assert_eq!(
            std::fs::read_to_string(fixture.claude()).unwrap(),
            moved_config
        );
        assert!(
            fixture.move_record("claude-code").exists(),
            "the record is the only copy of the moved entries"
        );

        disconnect_client("claude-code").unwrap();
        assert_eq!(json_file(&fixture.claude())["mcpServers"], servers);
        assert!(!fixture.move_record("claude-code").exists());
    }

    /// Rollback reads the backup through its wrapper. A vaulted backup restores
    /// the exact native bytes, a locked vault refuses without touching the config,
    /// and a raw file in the backup slot is refused (rollback has no raw fallback).
    #[test]
    fn rollback_decodes_vaulted_backups_and_refuses_raw_ones() {
        let fixture = MoveFixture::new(&Registry::default());
        let target = fixture.claude();
        let secret = "sk-rollback-wrapper-fixture";
        let original = format!(
            "{{\n  \"mcpServers\": {{\"native\": {{\"command\": \"echo\", \"env\": {{\"API_KEY\": \"{secret}\"}}}}}},\n  \"numStartups\": 3\n}}\r\n"
        );
        let connected = "{\"mcpServers\":{\"toolport\":{\"command\":\"toolport-gateway\"}}}";
        let outcome = |backup: &std::path::Path| WriteOutcome {
            existed_before: true,
            path: target.display().to_string(),
            backup: Some(backup.display().to_string()),
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };

        std::fs::write(&target, &original).unwrap();
        clients::backup_storage::register("native", "API_KEY", secret);
        let backup = clients::backup_config("claude-code", &target)
            .unwrap()
            .unwrap();
        clients::backup_storage::clear();
        let stored = std::fs::read_to_string(&backup).unwrap();
        assert!(stored.contains("toolportBackupVersion"), "{stored}");
        assert!(
            !stored.contains(secret),
            "the backup itself holds no plaintext"
        );

        std::fs::write(&target, connected).unwrap();
        let error =
            finish_client_config_mutation(outcome(&backup), |_| Err("registry full".into()))
                .unwrap_err();
        assert!(error.contains("rolled back"), "{error}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original);

        // Locked vault: the native config keeps Toolport's write, never the
        // wrapper or a template with missing-value markers.
        std::fs::write(&target, connected).unwrap();
        let unlock = std::env::var("TOOLPORT_SECRET_KEY").unwrap();
        std::env::set_var("TOOLPORT_SECRET_KEY", "locked-rollback-fixture");
        let error =
            finish_client_config_mutation(outcome(&backup), |_| Err("registry full".into()))
                .unwrap_err();
        std::env::set_var("TOOLPORT_SECRET_KEY", &unlock);
        assert!(error.contains("rollback"), "{error}");
        assert!(!error.contains(secret), "{error}");
        assert!(!error.contains("TOOLPORT_MISSING_CREDENTIAL_"), "{error}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), connected);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), stored);

        // Rollback only ever names a backup this process just wrapped, so a raw
        // file there is refused, never written over the config.
        let legacy = backup.with_file_name("1700000000000-.claude.json");
        std::fs::write(&legacy, &original).unwrap();
        std::fs::write(&target, connected).unwrap();
        finish_client_config_mutation(outcome(&legacy), |_| Err("registry full".into()))
            .unwrap_err();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), connected);
    }

    /// Rollback reads with a wrapper required: a wrapper cut anywhere (Sol's
    /// three truncations included), an empty file, or raw config bytes in the
    /// backup slot are refused and the client config is left as it is.
    #[test]
    fn rollback_refuses_truncated_backup_wrappers() {
        let fixture = MoveFixture::new(&Registry::default());
        let target = fixture.claude();
        let connected = "{\"mcpServers\":{\"toolport\":{\"command\":\"toolport-gateway\"}}}";
        let outcome = |backup: &std::path::Path| WriteOutcome {
            existed_before: true,
            path: target.display().to_string(),
            backup: Some(backup.display().to_string()),
            managed: None,
            restored: Vec::new(),
            used_move_record: false,
            revision: None,
            warnings: Vec::new(),
            recovery_path: None,
        };
        let secret = "sk-truncated-wrapper-fixture";
        let original = format!(
            "{{\"mcpServers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_KEY\":\"{secret}\"}}}}}}}}"
        );
        std::fs::write(&target, &original).unwrap();
        clients::backup_storage::register("native", "API_KEY", secret);
        let vaulted = clients::backup_config("claude-code", &target)
            .unwrap()
            .unwrap();
        clients::backup_storage::clear();
        std::fs::write(&target, "{\"plain\":true}").unwrap();
        let plain = clients::backup_config("claude-code", &target)
            .unwrap()
            .unwrap();
        for backup in [&vaulted, &plain] {
            let full = std::fs::read_to_string(backup).unwrap();
            assert!(full.starts_with("{\"toolport"), "{full}");
            let key = if backup == &vaulted {
                "toolportBackupVersion"
            } else {
                "toolportPlainBackupVersion"
            };
            let cut = full
                .find("\"account\":")
                .or_else(|| full.find("\"sourceConfigPath\":"))
                .unwrap();
            for damaged in [
                full[..cut + 10].to_string(),
                format!("{{\"{}", &key[..key.len() - 4]),
                format!("{{\n  \"{key}\":1,"),
                String::new(),
                "{".into(),
                "{\"".into(),
                // A raw config in the backup slot: rollback has no raw fallback.
                "{\"note\":\"toolportBackupVersion\",\"mcpServers\":{}}".into(),
                "{}".into(),
            ] {
                std::fs::write(backup, &damaged).unwrap();
                std::fs::write(&target, connected).unwrap();
                let error =
                    finish_client_config_mutation(outcome(backup), |_| Err("registry full".into()))
                        .unwrap_err();
                assert!(error.contains("rollback"), "{damaged:?}: {error}");
                assert_eq!(
                    std::fs::read_to_string(&target).unwrap(),
                    connected,
                    "{damaged:?}"
                );
            }
        }
    }

    /// A moved-servers record cut to zero bytes or inside its first key is never
    /// read as a record: Disconnect refuses and leaves the client config as it is.
    #[test]
    fn damaged_move_record_refuses_disconnect_without_touching_the_config() {
        for damaged in ["", "{\"toolportBackupVer"] {
            let fixture = MoveFixture::new(&Registry::default());
            std::fs::write(
                fixture.claude(),
                r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","server-memory"]}}}"#,
            )
            .unwrap();
            connect_fixture("claude-code", None, false).unwrap();
            let record = fixture.move_record("claude-code");
            assert!(record.exists());
            std::fs::write(&record, damaged).unwrap();
            let connected = std::fs::read_to_string(fixture.claude()).unwrap();
            assert!(disconnect_client("claude-code").is_err(), "{damaged:?}");
            assert_eq!(
                std::fs::read_to_string(fixture.claude()).unwrap(),
                connected,
                "{damaged:?}"
            );
            assert_eq!(std::fs::read_to_string(&record).unwrap(), damaged);
        }
    }

    /// F2: a damaged moved-servers record gets its own recovery row with
    /// Discard recovery file, whose review says what the record held. After
    /// it is discarded, Disconnect works again and the servers stay in Toolport.
    /// A record a newer Toolport wrote is listed but never discardable.
    #[test]
    fn damaged_move_record_lists_and_can_be_discarded_alone() {
        for damaged in [&b"{\"configPath\":\"/tmp"[..], &[0xff, 0xfe][..]] {
            let fixture = MoveFixture::new(&Registry::default());
            std::fs::write(
                fixture.claude(),
                r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","server-memory"]}}}"#,
            )
            .unwrap();
            connect_fixture("claude-code", None, false).unwrap();
            let record = fixture.move_record("claude-code");
            std::fs::write(&record, damaged).unwrap();
            assert!(disconnect_client("claude-code").is_err());
            let row = clients::recovery::list()
                .unwrap()
                .into_iter()
                .find(|r| r.backup_path == record.to_string_lossy())
                .expect("the damaged move record is listed");
            assert!(row.can_discard_file && !row.can_discard);
            let review = clients::recovery::prepare_discard(&row.id).unwrap();
            assert!(
                review.description.contains("servers stay in Toolport"),
                "{}",
                review.description
            );
            assert_eq!(review.lines, ["Backup file: moved-servers.json"]);
            let others: Vec<_> = std::fs::read_dir(record.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| *p != record)
                .collect();
            let done = clients::recovery::discard_confirmed(&row.id, &review.manifest).unwrap();
            assert!(done.contains("record of the servers it moved"), "{done}");
            assert!(!done.contains("unreadable recovery file"), "{done}");
            assert!(!record.exists());
            for other in &others {
                assert!(other.exists(), "{}", other.display());
            }
            disconnect_client("claude-code").unwrap();
            assert!(registry::load()
                .unwrap()
                .servers
                .iter()
                .any(|server| server.name == "memory"));
        }
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","server-memory"]}}}"#,
        )
        .unwrap();
        connect_fixture("claude-code", None, false).unwrap();
        let record = fixture.move_record("claude-code");
        std::fs::write(&record, r#"{"toolportBackupVersion":2,"account":"x"}"#).unwrap();
        let row = clients::recovery::list()
            .unwrap()
            .into_iter()
            .find(|r| r.backup_path == record.to_string_lossy())
            .expect("a newer move record is listed");
        assert!(!row.can_discard_file && !row.can_discard);
    }

    #[test]
    fn registry_failure_during_disconnect_retains_exact_restore_for_retry() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{ "mcpServers": {"native":{"command":"native"}}, "setting": 7 }"#;
        std::fs::write(fixture.claude(), original).unwrap();
        connect_fixture("claude-code", None, false).unwrap();
        disconnect_client_stdio_with("claude-code", false, |_| Err("registry full".into()))
            .unwrap_err();
        let result = disconnect_client("claude-code").unwrap();
        assert_eq!(
            std::fs::read_to_string(&result.outcome.path).unwrap(),
            original
        );
    }

    /// UX-03 for Codex: the moved TOML tables come back (nested env table too) into
    /// the profile-scoped client, and an entry the user re-added since is kept.
    #[test]
    fn disconnect_restores_moved_codex_servers_without_clobbering_edits() {
        let mut registry = Registry::default();
        let mut work = registry.profiles[0].clone();
        work.id = "work".into();
        work.name = "Work".into();
        registry.profiles.push(work);
        let fixture = MoveFixture::new(&registry);
        let original = r#"model = "gpt-5"

# my servers
[mcp_servers.memory]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]

[mcp_servers.docs]
command = "uvx"
args = ["docs-mcp"]

[mcp_servers.docs.env]
DOCS_TOKEN = "tok"
"#;
        std::fs::write(fixture.codex(), original).unwrap();

        let outcome = connect_fixture("codex", Some("Work"), false).unwrap();
        assert_eq!(
            enabled_names(&outcome.result.registry, "work"),
            ["docs", "memory"]
        );
        assert!(enabled_names(&outcome.result.registry, "default").is_empty());
        let migrated: toml::Value =
            toml::from_str(&std::fs::read_to_string(fixture.codex()).unwrap()).unwrap();
        assert_eq!(migrated["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            migrated["mcp_servers"]
                .as_table()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            [clients::GATEWAY_ENTRY_NAME]
        );

        // The user puts their own memory back by hand before disconnecting.
        let edited = format!(
            "{}\n[mcp_servers.memory]\ncommand = \"node\"\nargs = [\"mine.js\"]\n",
            std::fs::read_to_string(fixture.codex()).unwrap()
        );
        std::fs::write(fixture.codex(), edited).unwrap();

        let disconnected = disconnect_client("codex").unwrap();
        assert_eq!(disconnected.outcome.restored, ["docs"]);
        let after: toml::Value =
            toml::from_str(&std::fs::read_to_string(fixture.codex()).unwrap()).unwrap();
        let before: toml::Value = toml::from_str(original).unwrap();
        assert_eq!(after["model"], before["model"]);
        assert_eq!(after["mcp_servers"]["docs"], before["mcp_servers"]["docs"]);
        assert_eq!(
            after["mcp_servers"]["memory"]["command"].as_str(),
            Some("node")
        );
        assert_eq!(after["mcp_servers"].as_table().unwrap().len(), 2);
        assert!(!fixture.move_record("codex").exists());
    }

    /// UX-02: when Toolport cannot turn a moved server on, the move fails before
    /// the client's config, the registry or the move record change.
    #[test]
    fn failed_enable_leaves_the_client_config_untouched() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"memory":{"command":"npx","args":["server-memory"]}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();

        let error = connect_fixture("claude-code", Some("missing"), false).unwrap_err();
        assert!(error.contains("left unchanged"), "{error}");
        assert_eq!(std::fs::read_to_string(fixture.claude()).unwrap(), original);
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert!(!fixture.move_record("claude-code").exists());
    }

    /// Opening the import review (a preview) never asks to unlock the keyring, even
    /// when only the keychain could confirm a server Toolport already imported.
    #[test]
    fn the_import_review_never_asks_to_unlock_the_keyring() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"remote":{"type":"http","url":"https://same.invalid/mcp?token=synthetic-a"},
                "arg":{"command":"arg-cmd","args":["--api-key=synthetic-arg-a"]}}}"#,
        )
        .unwrap();
        let keys = preview_client_imports_for("claude-code")
            .unwrap()
            .into_iter()
            .map(|item| item.key)
            .collect::<Vec<_>>();
        assert_eq!(import_client_servers(keys).unwrap().1, 2);
        // The client still lists both (Import does not change the client).
        let mut shown = Vec::new();
        let prompts = crate::secrets::locked_store::prompts_during(|| {
            shown = preview_client_imports_for("claude-code")
                .unwrap()
                .into_iter()
                .map(|item| (item.name, item.unsupported))
                .collect();
        });
        assert!(
            prompts.is_empty(),
            "the review asked to unlock: {prompts:?}"
        );
        // A locked keyring is not a missing value: the row says to unlock it.
        assert_eq!(shown.len(), 2, "{shown:?}");
        for (name, reason) in &shown {
            let reason = reason.as_deref().unwrap_or_default();
            assert!(reason.contains("Keychain unavailable"), "{name}: {reason}");
            assert!(
                !reason.contains("missing a required value"),
                "{name}: {reason}"
            );
        }
    }

    /// Two accounts of one server (same command and arguments, one token each in
    /// env) are two servers: both are offered and imported, and each row is in
    /// Toolport afterwards, its own token stored with it.
    #[test]
    fn two_accounts_of_one_server_are_both_offered_and_imported() {
        let fixture = MoveFixture::new(&Registry::default());
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{
                "slack-work":{"command":"npx","args":["-y","@acme/slack-mcp"],"env":{"SLACK_BOT_TOKEN":"synthetic-work"}},
                "slack-home":{"command":"npx","args":["-y","@acme/slack-mcp"],"env":{"SLACK_BOT_TOKEN":"synthetic-home"}}}}"#,
        )
        .unwrap();
        let client = detected_client("claude-code");
        let registry = read_registry_exact().unwrap();
        let mut offered = names(clients::importable_servers(&client, &registry));
        offered.sort_unstable();
        assert_eq!(offered, ["slack-home", "slack-work"]);
        assert!(clients::managed_server_names(&client, &registry).is_empty());
        let review = preview_client_imports_for("claude-code").unwrap();
        assert_eq!(review.len(), 2);
        let (_, added) =
            import_client_servers(review.iter().map(|item| item.key.clone()).collect()).unwrap();
        assert_eq!(added, 2);
        let saved = read_registry_exact().unwrap();
        for (name, token) in [
            ("slack-work", "synthetic-work"),
            ("slack-home", "synthetic-home"),
        ] {
            let server = saved.servers.iter().find(|s| s.name == name).unwrap();
            assert_eq!(
                crate::secrets::get_vault_secret_result(&server.id, "SLACK_BOT_TOKEN")
                    .unwrap()
                    .as_deref(),
                Some(token)
            );
        }
        let mut managed = clients::managed_server_names(&client, &saved);
        managed.sort_unstable();
        assert_eq!(managed, ["slack-home", "slack-work"]);
        assert!(clients::importable_servers(&client, &saved).is_empty());
    }

    /// After one account is imported, a second account of the same server added to
    /// the client later is still offered, not shown as already in Toolport.
    #[test]
    fn a_later_second_account_is_offered_not_shown_in_toolport() {
        let fixture = MoveFixture::new(&Registry::default());
        let work = r#""slack-work":{"command":"npx","args":["-y","@acme/slack-mcp"],"env":{"SLACK_BOT_TOKEN":"synthetic-work"}}"#;
        let home = r#""slack-home":{"command":"npx","args":["-y","@acme/slack-mcp"],"env":{"SLACK_BOT_TOKEN":"synthetic-home"}}"#;
        std::fs::write(fixture.claude(), format!(r#"{{"mcpServers":{{{work}}}}}"#)).unwrap();
        let keys = preview_client_imports_for("claude-code")
            .unwrap()
            .into_iter()
            .map(|item| item.key)
            .collect::<Vec<_>>();
        assert_eq!(import_client_servers(keys).unwrap().1, 1);
        std::fs::write(
            fixture.claude(),
            format!(r#"{{"mcpServers":{{{work},{home}}}}}"#),
        )
        .unwrap();
        let client = detected_client("claude-code");
        let saved = read_registry_exact().unwrap();
        assert_eq!(
            names(clients::importable_servers(&client, &saved)),
            ["slack-home"]
        );
        assert_eq!(
            clients::managed_server_names(&client, &saved),
            ["slack-work"]
        );
        let review = preview_client_imports_for("claude-code").unwrap();
        assert_eq!(
            review
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["slack-home"]
        );
    }

    /// An account saved as `name (2)` beside a taken name is still that account.
    #[test]
    fn an_account_saved_under_a_suffixed_name_is_still_in_toolport() {
        assert!(super::saved_under_name("Slack (2)", "slack"));
        assert!(super::saved_under_name("slack", "Slack"));
        assert!(!super::saved_under_name("slack-home", "slack"));
        assert!(!super::saved_under_name("slack (x)", "slack"));
        assert!(!super::saved_under_name("slack ()", "slack"));
    }
}
