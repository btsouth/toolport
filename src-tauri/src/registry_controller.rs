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
}

const AUTH_LOCK_LEASE_SECS: u64 = 180;
const AUTH_LOCK_WAIT_SECS: u64 = 30;
const AUTH_LOCK_POLL_MS: u64 = 250;

pub(crate) struct AuthMutationLock {
    path: std::path::PathBuf,
    /// What this process wrote. Past the lease another process may take the
    /// lock over; its file is then left alone.
    contents: String,
    _personal_owner: Option<crate::local_auth::OwnerPin>,
}

impl Drop for AuthMutationLock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|current| current == self.contents) {
            let _ = std::fs::remove_file(&self.path);
        }
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
                contents,
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
                contents,
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
        registry::client_file::commit(&self.target, &revision, original.as_deref())?;
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

pub(crate) fn servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
) -> Vec<ServerEntry> {
    let mut picked = Vec::new();
    let mut import_keys = existing
        .servers
        .iter()
        .map(|server| {
            clients::import_dedupe_key(&server.name, server.command.as_deref(), &server.args)
        })
        .collect::<std::collections::HashSet<_>>();
    for client in detected {
        for server in client.servers.iter().chain(client.plugin_servers.iter()) {
            let entry = server_from_detected(server, &client.id);
            if clients::is_gateway_server(&entry) {
                continue;
            }
            let key =
                clients::import_dedupe_key(&entry.name, entry.command.as_deref(), &entry.args);
            if import_keys.insert(key) {
                picked.push(entry);
            }
        }
    }
    picked
}

pub(crate) fn selected_servers_to_import(
    detected: &[clients::DetectedClient],
    existing: &Registry,
    selected: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<ServerEntry>, String> {
    let picked = servers_to_import(detected, existing)
        .into_iter()
        .filter(|server| {
            selected.is_none_or(|keys| {
                keys.contains(&clients::import_dedupe_key(
                    &server.name,
                    server.command.as_deref(),
                    &server.args,
                ))
            })
        })
        .collect::<Vec<_>>();
    for client in detected {
        let source = format!("imported:{}", client.id);
        let names = picked
            .iter()
            .filter(|server| server.source.as_deref() == Some(source.as_str()))
            .map(|server| server.name.clone())
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
    definition: Option<&serde_json::Value>,
) -> Result<String, String> {
    let reviewed = serde_json::to_string(&(server, definition)).map_err(|e| e.to_string())?;
    Ok(format!(
        "client-import:{}",
        serde_json::to_string(&(
            client_id,
            clients::import_dedupe_key(&server.name, server.command.as_deref(), &server.args),
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
    servers_to_import(&detected, &registry)
        .into_iter()
        .map(|server| {
            let client = detected
                .iter()
                .find(|client| {
                    server.source.as_deref() == Some(format!("imported:{}", client.id).as_str())
                })
                .ok_or("Missing import client")?;
            let definition = clients::import_definition(client, &server.name)?;
            let key = if client_id.is_some() {
                scoped_import_key(&client.id, &server, definition.as_ref())?
            } else {
                clients::import_dedupe_key(&server.name, server.command.as_deref(), &server.args)
            };
            let (credentials, unsupported) = match crate::import_credentials::Import::prepare(
                server.clone(),
                definition.as_ref(),
            ) {
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
                name: server.name,
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
        })
        .collect::<Result<Vec<_>, String>>()
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
/// Shown when a server editor save loses a race with another registry write.
/// The GTK shell translates it; React's editor saves the whole entry and never
/// meets this race.
/// A keychain variable made plain with no value typed would lose its only copy.
pub const KEEP_IN_KEYCHAIN: &str = "Enter a value, or leave it in the keychain.";
/// A stored keychain variable whose name was cleared.
pub const NAME_THIS_VARIABLE: &str = "Give this variable a name, or remove it.";
/// A keychain variable renamed onto a secret argument's keychain name.
pub const NAME_USED_BY_ARGUMENT: &str =
    "This name is already used by a secret argument. Choose another name.";
pub const EDITOR_SAVE_RACE: &str = "Servers changed while this editor was open. Nothing was saved. Select Save again to apply your changes.";

fn commit_imports(previous: &Registry, prepared: Registry) -> Result<Registry, String> {
    commit_unless_changed(
        previous,
        prepared,
        "Servers changed during import. Review again before retrying.",
    )
}

fn commit_unless_changed(
    previous: &Registry,
    prepared: Registry,
    changed: &str,
) -> Result<Registry, String> {
    registry::update(|latest| {
        if !registry_matches(latest, previous)? {
            return Err(changed.into());
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
    for entry in servers {
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
            && !reviewed.contains(&scoped_import_key(client_id, &entry, definition.as_ref())?)
        {
            return Err("Server changed. Refresh the client and review again.".into());
        }
        let name = entry.name.clone();
        match crate::import_credentials::Import::prepare_with_choices(
            entry,
            definition.as_ref(),
            choices.get(&name),
        ) {
            Ok(mut import) => {
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

/// Keychain values an edit leaves behind: variables stored in the keychain
/// that the edit removes, makes plain, or points at a password manager. A
/// secret launch input with the same key keeps its value.
pub(crate) fn dropped_vault_keys(
    old: &ServerEntry,
    env: &[registry::EnvVar],
    launch: Option<&registry::LaunchConfig>,
) -> Vec<String> {
    old.env
        .iter()
        .filter(|old| old.secret && !old.unknown_fields.contains_key("source"))
        .filter(|old| {
            env.iter()
                .find(|new| new.key.trim() == old.key.trim())
                .is_none_or(|new| !new.secret || new.unknown_fields.contains_key("source"))
        })
        .filter(|old| {
            !launch.is_some_and(|launch| {
                launch
                    .inputs
                    .iter()
                    .any(|input| input.secret && input.key.trim() == old.key.trim())
            })
        })
        .map(|old| old.key.clone())
        .collect()
}

/// What the server editor asks of the keychain besides the values typed in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretKeyChanges {
    /// Keychain variables the user removed, made plain with a new value, or
    /// pointed at a password manager in this dialog.
    pub removed: Vec<String>,
    /// Keychain variables renamed without a new value, as (old, new). Their
    /// stored value moves to the new name.
    pub renamed: Vec<(String, String)>,
    /// GTK editor only: every variable the dialog opened with. A variable
    /// added elsewhere while it was open is kept. `None` replaces the list.
    pub opened_with: Option<Vec<String>>,
}

impl SecretKeyChanges {
    #[cfg(test)]
    pub(crate) fn removing(keys: &[&str]) -> Self {
        Self {
            removed: keys.iter().map(|key| key.to_string()).collect(),
            ..Self::default()
        }
    }
}

/// One editor row for [`editor_secret_changes`]: the name it had when the
/// dialog opened if its value was in the keychain, and the row as it is now.
pub struct EditorSecretRow<'a> {
    pub stored_as: Option<&'a str>,
    pub row: &'a registry::EnvVar,
}

/// The keychain changes an editor's rows ask for, and the rows that cannot be
/// saved as they are, each with the message to show on it (`blocked`, keyed
/// by the name the row was stored under). Both shells compute this the same
/// way; the backend checks it again. `launch_secret_keys` are the keychain
/// names of the server's secret arguments, which variables cannot take.
pub fn editor_secret_changes(
    vaulted_at_open: &[String],
    rows: &[EditorSecretRow<'_>],
    launch_secret_keys: &[String],
) -> (SecretKeyChanges, Vec<(String, &'static str)>) {
    let mut changes = SecretKeyChanges::default();
    let mut blocked = Vec::new();
    for EditorSecretRow { stored_as, row } in rows {
        let Some(old) = stored_as.map(str::trim) else {
            continue;
        };
        let new = row.key.trim();
        let blank = row.value.as_deref().is_none_or(|v| v.trim().is_empty());
        let keychain = row.secret && !row.unknown_fields.contains_key("source");
        if new.is_empty() {
            blocked.push((old.to_string(), NAME_THIS_VARIABLE));
        } else if !row.secret && blank {
            blocked.push((old.to_string(), KEEP_IN_KEYCHAIN));
        } else if !keychain {
            changes.removed.push(old.to_string());
        } else if new != old {
            if launch_secret_keys.iter().any(|key| key.trim() == new) {
                blocked.push((old.to_string(), NAME_USED_BY_ARGUMENT));
            } else if blank {
                changes.renamed.push((old.to_string(), new.to_string()));
            } else {
                changes.removed.push(old.to_string());
            }
        }
    }
    for key in vaulted_at_open {
        if !rows
            .iter()
            .any(|row| row.stored_as.is_some_and(|old| old.trim() == key.trim()))
        {
            changes.removed.push(key.trim().to_string());
        }
    }
    (changes, blocked)
}

/// Keychain changes one save makes. Moves are written inside the save's
/// transaction; deletes run only after the registry commit, so a crash or a
/// failed commit never leaves a declared variable without its value.
#[derive(Debug, Default)]
struct VaultPlan {
    moves: Vec<(String, String)>,
    deletes: Vec<String>,
}

impl VaultPlan {
    /// Reads every move source before writing any destination, so a swap or
    /// a rename onto a name that is being freed moves the right values. A
    /// destination whose source holds nothing is cleared after the commit.
    fn write_moves(
        &mut self,
        id: &str,
        writes: &mut crate::import_credentials::VaultWrites,
    ) -> Result<(), String> {
        let mut values = Vec::new();
        for (from, to) in &self.moves {
            let stored = crate::secrets::get_vault_secret_result(id, from)
                .map_err(|_| "Could not read the keychain. Nothing was saved.".to_string())?;
            values.push((to.clone(), stored));
        }
        for (to, stored) in values {
            match stored {
                Some(value) => writes.write(id, &to, &value)?,
                None => self.deletes.push(to),
            }
        }
        Ok(())
    }

    /// After the commit. A delete that fails only leaves an unused value.
    fn delete_after_commit(&self, id: &str) {
        for key in &self.deletes {
            let _ = crate::secrets::delete_secret(id, key);
        }
    }
}

/// Whether another server sharing this one's keychain namespace (a team copy
/// and its personal original) still declares `key` as a keychain variable.
/// When unsure, the value is kept.
fn shared_owner_declares(registry: &Registry, id: &str, key: &str) -> bool {
    let Ok(owner) = crate::local_auth::owner_in(registry, id) else {
        return true;
    };
    registry.servers.iter().filter(|s| s.id != id).any(|other| {
        other.env.iter().any(|env| {
            env.key.trim() == key.trim() && env.secret && !env.unknown_fields.contains_key("source")
        }) && crate::local_auth::owner_in(registry, &other.id).map_or(true, |o| o == owner)
    })
}

/// Checks an edit against the stored values and plans the keychain changes.
/// A stored value is never deleted unless its replacement is in hand: a
/// keychain variable made plain needs a value, and a renamed one keeps its
/// value under the new name.
fn plan_vault_changes(
    previous: &Registry,
    old: &ServerEntry,
    env: &[registry::EnvVar],
    launch: Option<&registry::LaunchConfig>,
    changes: &SecretKeyChanges,
) -> Result<VaultPlan, String> {
    let stored = |key: &str| {
        old.env.iter().any(|e| {
            e.key.trim() == key.trim() && e.secret && !e.unknown_fields.contains_key("source")
        })
    };
    let listed = |key: &str| env.iter().find(|e| e.key.trim() == key.trim());
    let blank = |e: &registry::EnvVar| e.value.as_deref().is_none_or(|v| v.trim().is_empty());
    for row in env.iter().filter(|row| !row.secret && blank(row)) {
        let was = changes
            .renamed
            .iter()
            .find(|(_, to)| to.trim() == row.key.trim())
            .map_or(row.key.as_str(), |(from, _)| from.as_str());
        if stored(was) {
            return Err(format!("{}: {KEEP_IN_KEYCHAIN}", row.key.trim()));
        }
    }
    // A name may be taken over when its own row is removed or renamed away
    // in the same save.
    let leaving = |key: &str| {
        changes.removed.iter().any(|k| k.trim() == key)
            || changes
                .renamed
                .iter()
                .any(|(from, to)| from.trim() == key && to.trim() != key)
    };
    let launch_secret = |key: &str| {
        launch.is_some_and(|launch| {
            launch
                .inputs
                .iter()
                .any(|input| input.secret && input.key.trim() == key)
        })
    };
    let mut moves: Vec<(String, String)> = Vec::new();
    for (from, to) in &changes.renamed {
        let (from, to) = (from.trim(), to.trim());
        if from == to || !stored(from) {
            continue;
        }
        let Some(row) = listed(to) else {
            continue;
        };
        if !row.secret || row.unknown_fields.contains_key("source") || !blank(row) {
            continue;
        }
        if launch_secret(to) {
            return Err(format!("{to}: {NAME_USED_BY_ARGUMENT}"));
        }
        if stored(to) && !leaving(to) {
            return Err(format!(
                "{to} already has a stored value. Remove that variable first."
            ));
        }
        moves.push((from.to_string(), to.to_string()));
    }
    let destination = |key: &str| moves.iter().any(|(_, to)| to == key);
    let sources = moves.iter().map(|(from, _)| from.clone());
    let mut deletes: Vec<String> = dropped_vault_keys(old, env, launch)
        .into_iter()
        .filter(|key| changes.removed.iter().any(|gone| gone.trim() == key.trim()))
        .chain(sources)
        .filter(|key| !destination(key.trim()))
        .filter(|key| !shared_owner_declares(previous, &old.id, key))
        .collect();
    deletes.dedup();
    Ok(VaultPlan { moves, deletes })
}

/// Updates a whole server entry, as the React editor does. Keychain values
/// of variables the user removed in that editor are deleted and renamed ones
/// move, as [`SecretKeyChanges`] says. `write` saves the registry; if it
/// fails, the keychain is put back.
pub fn update_server_entry_with<T>(
    entry: ServerEntry,
    changes: &SecretKeyChanges,
    write: impl FnOnce(ServerEntry) -> Result<T, String>,
) -> Result<T, String> {
    let mut entry = entry;
    rename_required_env(entry.launch.as_mut(), changes);
    let plan_now = || -> Result<VaultPlan, String> {
        let previous = read_registry_exact_or_default()?;
        match previous.servers.iter().find(|server| server.id == entry.id) {
            Some(old) => {
                plan_vault_changes(&previous, old, &entry.env, entry.launch.as_ref(), changes)
            }
            None => Ok(VaultPlan::default()),
        }
    };
    // A save that removes or renames no keychain variable touches no stored
    // value, so it needs no credential lock; it is still checked.
    if changes.removed.is_empty() && changes.renamed.is_empty() {
        plan_now()?;
        return write(entry);
    }
    let _auth = acquire_auth_lock(&entry.id)?;
    let mut plan = plan_now()?;
    let id = entry.id.clone();
    let saved = crate::import_credentials::transaction(|writes| {
        plan.write_moves(&id, writes)?;
        write(entry)
    })?;
    plan.delete_after_commit(&id);
    Ok(saved)
}

/// A renamed variable keeps its place in the launch requirements, or the
/// server would refuse to start asking for the old name.
fn rename_required_env(launch: Option<&mut registry::LaunchConfig>, changes: &SecretKeyChanges) {
    let Some(launch) = launch else {
        return;
    };
    for (from, to) in &changes.renamed {
        let (from, to) = (from.trim(), to.trim());
        if launch.required_env.iter().any(|key| key == to) {
            continue;
        }
        for key in launch
            .required_env
            .iter_mut()
            .filter(|key| key.trim() == from)
        {
            *key = to.to_string();
        }
    }
}

/// Variables the editor lists without a value or a password manager
/// reference. A new server with any of these stays off until they are filled.
pub fn editor_missing_values(edits: &[registry::EnvVar]) -> Vec<String> {
    edits
        .iter()
        .filter(|e| {
            !e.key.trim().is_empty()
                && e.value.as_deref().is_none_or(|v| v.trim().is_empty())
                && !e
                    .unknown_fields
                    .get("source")
                    .and_then(|s| s.get("ref"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
        })
        .map(|e| e.key.trim().to_string())
        .collect()
}

/// Save manual editor values in one registry/vault transaction. A failed vault
/// write keeps the editor open and restores prior values; plaintext never enters disk.
///
/// `changes` names the keychain variables the user removed or renamed in this
/// dialog. Only those lose or move their values: a variable added elsewhere
/// while the dialog was open, or one the dialog could not show, is kept.
///
/// A new server goes through the same credential review as every other add,
/// so a secret in its arguments or URL is stored in the keychain, never in
/// the registry.
pub fn save_server_editor_fields(
    server_id: Option<&str>,
    fields: ServerFields,
    edits: Vec<registry::EnvVar>,
    changes: &SecretKeyChanges,
    launch: Option<registry::LaunchConfig>,
    launch_values: &[(registry::LaunchInput, String)],
) -> Result<Registry, String> {
    save_server_editor_fields_with(
        server_id,
        fields,
        edits,
        changes,
        launch,
        launch_values,
        || {},
    )
}

fn save_server_editor_fields_with(
    server_id: Option<&str>,
    fields: ServerFields,
    edits: Vec<registry::EnvVar>,
    changes: &SecretKeyChanges,
    launch: Option<registry::LaunchConfig>,
    launch_values: &[(registry::LaunchInput, String)],
    mut before_allocation_lock: impl FnMut(),
) -> Result<Registry, String> {
    let complete = editor_missing_values(&edits).is_empty();
    let is_new = server_id.is_none();
    // Secret arguments, a URL carrying a token and secret launch inputs move
    // to the keychain exactly as when the same server is pasted or imported.
    let import = if is_new {
        let mut entry = entry_from_fields(fields.clone())?;
        if let Some(launch) = &launch {
            launch.validate(&entry.args, true)?;
        }
        entry.launch = launch.clone();
        entry.enabled = false;
        Some(
            crate::import_credentials::Import::prepare_with_choices(entry, None, None)
                // No client config is involved in the editor.
                .map_err(|error| {
                    error
                        .trim_end_matches(" Client config unchanged.")
                        .to_string()
                })?,
        )
    } else {
        None
    };
    // Allocate while holding the same per-server auth lock as credential edits.
    // Recheck after locking: another editor may have committed this candidate ID.
    let (_auth, previous, mut prepared, id) = loop {
        let snapshot = read_registry_exact_or_default()?;
        let mut proposed = snapshot.clone();
        let candidate = match (server_id, &import) {
            (Some(id), _) => id.to_string(),
            (None, Some(import)) => proposed.add_server(import.entry.clone()),
            (None, None) => unreachable!("a new server is always prepared"),
        };
        before_allocation_lock();
        let lock = acquire_auth_lock(&candidate)?;
        let previous = read_registry_exact_or_default()?;
        let mut prepared = previous.clone();
        let id = match (server_id, &import) {
            (Some(id), _) => {
                apply_update_server_fields(&mut prepared, id, fields.clone())?;
                id.to_string()
            }
            (None, Some(import)) => prepared.add_server(import.entry.clone()),
            (None, None) => unreachable!("a new server is always prepared"),
        };
        if candidate == id {
            break (lock, previous, prepared, id);
        }
    };
    let server = prepared
        .servers
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or("Missing server")?;
    let mut seen = std::collections::HashSet::new();
    let mut pending = Vec::new();
    // Password manager references go through the same path as the Credentials
    // dialog, which records team overrides and this machine's approval.
    let mut references = Vec::new();
    let mut env = Vec::new();
    for mut edit in edits {
        edit.key = edit.key.trim().to_string();
        if edit.key.is_empty() && edit.value.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let key = normalize_secret_key(&edit.key)?;
        if !seen.insert(key.clone()) {
            // The same sentence as React and the paste path.
            return Err(crate::i18n::trf(
                "Use each variable name once. Rename or remove the second {value0}.",
                &[("value0", &key)],
            ));
        }
        let mut value = server
            .env
            .iter()
            .find(|e| e.key == key)
            .cloned()
            .unwrap_or(edit.clone());
        value.secret = edit.secret;
        value.value = edit.value.clone().filter(|v| !v.is_empty());
        value.unknown_fields.remove("source");
        if let Some(source) = edit.unknown_fields.get("source") {
            let reference = source
                .get("ref")
                .and_then(serde_json::Value::as_str)
                .ok_or("Enter a credential reference")?;
            crate::secret_refs::parse(reference).map_err(|e| e.to_string())?;
            value.secret = true;
            value.value = None;
            value.unknown_fields.insert("source".into(), source.clone());
            references.push((key.clone(), reference.to_string()));
        } else if value.secret {
            if let Some(secret) = value.value.take() {
                pending.push((key.clone(), secret));
            }
        }
        if value.secret {
            // Sync never copies keychain values, so a choice made while the
            // variable was plain no longer applies.
            value.unknown_fields.remove("portable");
        }
        if let Some(portable) = edit.unknown_fields.get("portable") {
            if portable == &serde_json::json!(true) && crate::personal_sync::risky_sync_env(&key) {
                return Err(format!(
                    "{key} changes how programs run, so it stays on this machine."
                ));
            }
            value
                .unknown_fields
                .insert("portable".into(), portable.clone());
        }
        env.push(value);
    }
    let mut plan = match previous.servers.iter().find(|s| s.id == id) {
        Some(old) => plan_vault_changes(&previous, old, &env, server.launch.as_ref(), changes)?,
        None => VaultPlan::default(),
    };
    rename_required_env(server.launch.as_mut(), changes);
    // A variable added elsewhere while the dialog was open (the other shell, a
    // sync pull, a team update) keeps its declaration.
    if let Some(opened_with) = &changes.opened_with {
        for current in &server.env {
            let key = current.key.trim();
            if !opened_with.iter().any(|k| k.trim() == key)
                && !env.iter().any(|e| e.key.trim() == key)
            {
                env.push(current.clone());
            }
        }
    }
    server.env = env;
    for (edit, value) in launch_values {
        let input = server
            .launch
            .as_mut()
            .and_then(|l| l.inputs.iter_mut().find(|i| i.key == edit.key))
            .ok_or("launch input no longer exists")?;
        if let Some(reference) =
            crate::secret_refs::source(&edit.unknown_fields).map_err(|e| e.to_string())?
        {
            crate::secret_refs::parse(reference).map_err(|e| e.to_string())?;
            input
                .unknown_fields
                .insert("source".into(), serde_json::json!({"ref":reference}));
            input.value = None;
            references.push((input.key.clone(), reference.to_string()));
        } else {
            input.unknown_fields.remove("source");
            if input.secret {
                input.value = None;
                if !value.is_empty() {
                    pending.push((input.key.clone(), value.clone()));
                }
            } else {
                input.value = (!value.is_empty()).then(|| value.clone());
            }
        }
    }
    // A value typed here replaces any team override for that variable, as the
    // Credentials dialog does; otherwise the override would win at launch.
    if let Some(overrides) = server
        .unknown_fields
        .get_mut("memberSecretRefs")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (key, _) in &pending {
            for location in ["env", "header", "input"] {
                overrides.remove(&format!("{location}:{key}"));
            }
        }
    }
    for (key, reference) in &references {
        apply_secret_reference(&mut prepared, &id, key, reference)?;
    }
    prepared.secrets_generation = prepared.secrets_generation.wrapping_add(1);
    // A value typed here for a moved-from name is the new value, not stale.
    plan.deletes
        .retain(|key| !pending.iter().any(|(typed, _)| typed == key));
    let saved = crate::import_credentials::transaction(|writes| {
        plan.write_moves(&id, writes)?;
        let missing: Vec<String> = match &import {
            // The caller already holds this server's credential lock.
            Some(import) => import.transfer_locked(&id, writes, false)?,
            None => Vec::new(),
        }
        .into_iter()
        // Values typed in this dialog and password manager references are
        // written below, so they are not missing.
        .filter(|key| {
            !pending.iter().any(|(typed, _)| typed == key)
                && !references.iter().any(|(referenced, _)| referenced == key)
        })
        .collect();
        for (key, value) in &pending {
            writes.write(&id, key, value)?;
        }
        if is_new && !references.is_empty() {
            // Before the commit, so a failure leaves nothing saved and Save
            // can simply be pressed again.
            approve_own_reference(&prepared, &id)?;
        }
        #[cfg(test)]
        if let Some(hook) = BEFORE_EDITOR_COMMIT.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
        if is_new && complete && missing.is_empty() {
            // The same access set every other add path turns new servers on in.
            let profile = prepared.default_access_id();
            if apply_server_enabled(&mut prepared, &profile, &id, true, false).is_ok() {
                let _ = prepared.set_access_server(&profile, &id, true);
            }
        }
        commit_unless_changed(&previous, prepared, EDITOR_SAVE_RACE)
    })?;
    plan.delete_after_commit(&id);
    if !is_new && !references.is_empty() {
        approve_own_reference(&saved, &id)?;
    }
    Ok(saved)
}

#[cfg(test)]
thread_local! {
    /// Runs once just before the editor's registry commit.
    static BEFORE_EDITOR_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
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
    let result = finish_client_config_mutation(outcome, |managed_entry| {
        let (registry, removed_http_row) = write_registry(managed_entry)?;
        revoked = removed_http_row;
        Ok(registry)
    })?;
    if revoked {
        crate::secrets::delete_secret(CLIENT_HTTP_VAULT_SERVER, client_id)?;
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
    let (mut result, imported, moved, verification) = crate::import_credentials::transaction(
        |writes| {
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
            // Verifying probes every server and starts the gateway, which can
            // outlast the credential lock lease. The servers are committed, so
            // edits to them may go ahead; a rollback below only restores
            // values nobody has changed since.
            writes.release_locks();
            let attempt = (|| {
                let verification = verify(&staged, &moved, client_id, profile)?;
                let outcome =
                    clients::migrate_reviewed(client_id, profile, &moved, revision, || {
                        if !registry_matches(&read_registry_exact()?, &staged)? {
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
                        Err(format!("{error} Registry changed during setup. Concurrent edits were kept. Review Servers before retrying. Credentials for retained servers were kept."))
                    }
                    Err(rollback) => {
                        writes.keep();
                        Err(format!(
                            "{error} {rollback} Imported credential values were kept."
                        ))
                    }
                },
            }
        },
    )?;
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
    let key = normalize_secret_key(key)?;
    let _lock = acquire_auth_lock(server_id)?;
    set_server_secret_using(
        server_id,
        &key,
        value,
        crate::secrets::get_secret_result,
        crate::secrets::set_secret,
        crate::secrets::delete_secret,
        write_registry,
    )
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
    if !crate::import_credentials::provided(value) {
        return Err("Enter a credential value. Placeholder values cannot be saved.".into());
    }
    let registry = read_registry_exact()?;
    let remote = registry
        .servers
        .iter()
        .find(|s| s.id == server_id)
        .is_some_and(|s| s.transport != "stdio");
    let (key, value) = if remote && key.eq_ignore_ascii_case("authorization") {
        let (scheme, token) = value
            .split_once(' ')
            .ok_or("Enter a Bearer token for Authorization")?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(
                "Only Bearer authentication is supported for imported Authorization headers".into(),
            );
        }
        (crate::secrets::HTTP_AUTH_KEY, token)
    } else {
        (key, value)
    };
    set_server_secret_with(server_id, key, value, |server_id, key| {
        let (registry, ()) =
            registry::update(|registry| apply_secret_declaration(registry, server_id, key))?;
        Ok(registry)
    })
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

    /// A migration import takes the credential owner lock, which lives in the
    /// data directory, so the fixture gives it a scratch one.
    struct ZCodeImportFixture {
        root: PathBuf,
        client: clients::DetectedClient,
        _data_dir: registry::DataDirOverride,
        _data_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ZCodeImportFixture {
        fn new(servers: serde_json::Value) -> Self {
            let data_lock = registry::data_dir_test_lock();
            let root = test_path("zcode-import");
            std::fs::create_dir_all(root.join("data")).unwrap();
            let data_dir = registry::DataDirOverride::set(root.join("data"));
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
                _data_dir: data_dir,
                _data_lock: data_lock,
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
    fn an_import_can_release_its_credential_locks_once_committed() {
        let _fixture = MoveFixture::new(&Registry::default());
        let import = crate::import_credentials::Import::prepare(
            server("one"),
            Some(&serde_json::json!({"env":{"PAT":"synthetic-new"}})),
        )
        .unwrap();
        let path = auth_lock_path("one").unwrap();
        crate::import_credentials::transaction(|writes| {
            import.transfer_into("one", writes, false)?;
            assert!(try_acquire_auth_lock(&path)?.is_none());
            assert_eq!(writes.owner_pins_held(), 1);
            writes.release_locks();
            assert!(try_acquire_auth_lock(&path)?.is_some());
            // The pin stays, so verification and rollback resolve the same
            // keychain names as the writes did.
            assert_eq!(writes.owner_pins_held(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_failed_setup_verification_frees_the_lock_and_rolls_back_under_it() {
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
        std::fs::write(
            fixture.claude(),
            r#"{"mcpServers":{"a":{"command":"a","env":{"PAT":"new"}}}}"#,
        )
        .unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        let path = auth_lock_path("a").unwrap();
        let held = std::time::Duration::from_millis(1500);
        let mut other = None;
        let error = migrate_client_reviewed_with(
            "claude-code",
            None,
            false,
            &["a".into()],
            &review.revision,
            |_, _, _, _| {
                // Verification runs without the lock, so other edits go ahead.
                assert!(try_acquire_auth_lock(&path).unwrap().is_some());
                // Another process takes it before the rollback below.
                let (taken, wait) = std::sync::mpsc::channel();
                let path = path.clone();
                other = Some(std::thread::spawn(move || {
                    let lock = try_acquire_auth_lock(&path).unwrap().unwrap();
                    taken.send(()).unwrap();
                    std::thread::sleep(held);
                    drop(lock);
                }));
                wait.recv().unwrap();
                Err("Launch failed".into())
            },
        )
        .unwrap_err();
        let finished = std::time::Instant::now();
        other.unwrap().join().unwrap();
        assert!(error.contains("Launch failed"), "{error}");
        // The rollback waited for the other process's lock.
        assert!(
            finished.elapsed() < std::time::Duration::from_millis(200),
            "the rollback finished before the other process let go"
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("a", "PAT")
                .unwrap()
                .as_deref(),
            Some("old")
        );
    }

    #[test]
    fn a_lock_taken_over_by_another_process_is_not_deleted() {
        let _fixture = MoveFixture::new(&Registry::default());
        let path = auth_lock_path("taken-over").unwrap();
        let ours = try_acquire_auth_lock(&path).unwrap().unwrap();
        // Past the lease another process rewrites the file as its own.
        std::fs::write(&path, "mutation_id=other\n").unwrap();
        drop(ours);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mutation_id=other\n"
        );
        std::fs::remove_file(&path).unwrap();
        let ours = try_acquire_auth_lock(&path).unwrap().unwrap();
        drop(ours);
        assert!(!path.exists());
    }

    #[test]
    fn a_save_that_removes_no_stored_value_does_not_wait_for_the_credential_lock() {
        let (initial, entry) = stored_server(&[("TOKEN", "t")]);
        let _fixture = MoveFixture::new(&initial);
        let _held = acquire_auth_lock("srv").unwrap();
        let started = std::time::Instant::now();
        let mut renamed = entry;
        renamed.name = "renamed".into();
        update_server_entry_with(renamed, &SecretKeyChanges::default(), save_registry).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(AUTH_LOCK_WAIT_SECS / 2));
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
    fn import_batch_shares_one_lock_for_credential_owner_aliases() {
        let _fixture = MoveFixture::new(&Registry::default());
        let personal = entry_from_fields(fields("personal", "stdio")).unwrap();
        let import = crate::import_credentials::Import::prepare(
            personal,
            Some(&serde_json::json!({"env":{"TOKEN":"synthetic-import-key"}})),
        )
        .unwrap();
        let _alias = crate::local_auth::pin_credential_owner("team-alias", "personal");
        crate::import_credentials::transaction(|writes| {
            import.transfer_into("personal", writes, false)?;
            import.transfer_into("team-alias", writes, false)?;
            assert!(try_acquire_auth_lock(&auth_lock_path("personal")?)
                .unwrap()
                .is_none());
            Ok(())
        })
        .unwrap();
        assert!(try_acquire_auth_lock(&auth_lock_path("personal").unwrap())
            .unwrap()
            .is_some());
    }

    #[test]
    fn import_and_native_editor_do_not_share_a_credential_allocation() {
        let _fixture = MoveFixture::new(&Registry::default());
        let (written_tx, written_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let importer = std::thread::spawn(move || {
            let previous = read_registry_exact().unwrap();
            let mut entry = entry_from_fields(fields("shared", "stdio")).unwrap();
            entry.env.push(registry::EnvVar {
                key: "TOKEN".into(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            });
            let import = crate::import_credentials::Import::prepare(
                entry,
                Some(&serde_json::json!({"env":{"TOKEN":"synthetic-import-key"}})),
            )
            .unwrap();
            let mut prepared = previous.clone();
            let id = prepared.add_server(import.entry.clone());
            crate::import_credentials::transaction(|writes| {
                import.transfer_into(&id, writes, false)?;
                written_tx.send(id.clone()).unwrap();
                release_rx
                    .recv()
                    .map_err(|_| "audit interleaving cancelled")?;
                commit_imports(&previous, prepared)
            })
            .unwrap();
            id
        });
        let imported_id = written_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        let guarded = try_acquire_auth_lock(&auth_lock_path(&imported_id).unwrap())
            .unwrap()
            .is_none();
        if !guarded {
            release_tx.send(()).unwrap();
            importer.join().unwrap();
            panic!("an import must retain the allocation's auth lock until commit or rollback");
        }
        let (candidate_tx, candidate_rx) = std::sync::mpsc::channel();
        let editor = std::thread::spawn(move || {
            let mut candidate_tx = Some(candidate_tx);
            save_server_editor_fields_with(
                None,
                fields("shared", "stdio"),
                vec![registry::EnvVar {
                    key: "TOKEN".into(),
                    value: Some("synthetic-editor-key".into()),
                    secret: true,
                    unknown_fields: Default::default(),
                }],
                &SecretKeyChanges::default(),
                None,
                &[],
                || {
                    if let Some(tx) = candidate_tx.take() {
                        tx.send(()).unwrap();
                    }
                },
            )
            .unwrap()
            .servers
            .last()
            .unwrap()
            .id
            .clone()
        });
        candidate_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(importer.join().unwrap(), imported_id);
        let edited_id = editor.join().unwrap();
        assert_ne!(imported_id, edited_id);
        assert_eq!(read_registry_exact().unwrap().servers.len(), 2);
        assert_eq!(
            crate::secrets::get_vault_secret_result(&imported_id, "TOKEN")
                .unwrap()
                .as_deref(),
            Some("synthetic-import-key")
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result(&edited_id, "TOKEN")
                .unwrap()
                .as_deref(),
            Some("synthetic-editor-key")
        );
    }

    /// GTK Add with two rows of one name: refused with the sentence React and
    /// the paste path use, and nothing is saved.
    #[test]
    fn native_editor_refuses_a_repeated_variable_name() {
        let _fixture = MoveFixture::new(&Registry::default());
        let row = |value: &str| registry::EnvVar {
            key: "API_KEY".into(),
            value: Some(value.into()),
            secret: true,
            unknown_fields: Default::default(),
        };
        let error = save_server_editor_fields(
            None,
            fields("twice", "stdio"),
            vec![row("first"), row("second")],
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Use each variable name once. Rename or remove the second API_KEY."
        );
        assert!(read_registry_exact().unwrap().servers.is_empty());
    }

    #[test]
    fn native_editor_concurrent_adds_keep_each_servers_credentials() {
        let _fixture = MoveFixture::new(&Registry::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers = ["first", "second"].map(|value| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut first_candidate = true;
                let result = save_server_editor_fields_with(
                    None,
                    fields("same-name", "stdio"),
                    vec![registry::EnvVar {
                        key: "TOKEN".into(),
                        value: Some(value.into()),
                        secret: true,
                        unknown_fields: Default::default(),
                    }],
                    &SecretKeyChanges::default(),
                    None,
                    &[],
                    || {
                        if first_candidate {
                            first_candidate = false;
                            barrier.wait();
                        }
                    },
                )
                .unwrap();
                let id = result.servers.last().unwrap().id.clone();
                (id, value)
            })
        });
        let results = workers.map(|worker| worker.join().unwrap());
        assert_ne!(results[0].0, results[1].0);
        assert_eq!(read_registry_exact().unwrap().servers.len(), 2);
        for (id, value) in results {
            assert_eq!(
                crate::secrets::get_vault_secret_result(&id, "TOKEN")
                    .unwrap()
                    .as_deref(),
                Some(value)
            );
        }
    }

    #[test]
    fn native_editor_removed_secret_is_deleted_and_restored_if_save_fails() {
        let env = |key: &str, value: Option<&str>| registry::EnvVar {
            key: key.into(),
            value: value.map(str::to_string),
            secret: true,
            unknown_fields: Default::default(),
        };
        let mut initial = Registry::default();
        let mut entry = entry_from_fields(fields("manual", "stdio")).unwrap();
        entry.id = "manual".into();
        // ADDED_ELSEWHERE arrived after the dialog opened, so the dialog never
        // listed it and saving must not delete its value.
        entry.env = vec![
            env("REMOVED_TOKEN", None),
            env("KEPT_TOKEN", None),
            env("ADDED_ELSEWHERE", None),
        ];
        initial.servers.push(entry);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("manual", "ADDED_ELSEWHERE", "other-window").unwrap();
        crate::secrets::set_secret("manual", "REMOVED_TOKEN", "old-removed").unwrap();
        crate::secrets::set_secret("manual", "KEPT_TOKEN", "old-kept").unwrap();
        let before = serde_json::to_value(read_registry_exact().unwrap()).unwrap();
        let result = crate::secrets::tests::with_failed_write("KEPT_TOKEN", || {
            save_server_editor_fields(
                Some("manual"),
                fields("manual", "stdio"),
                vec![env("KEPT_TOKEN", Some("new-kept"))],
                &SecretKeyChanges::removing(&["REMOVED_TOKEN"]),
                None,
                &[],
            )
        });
        assert!(result.is_err());
        assert_eq!(
            serde_json::to_value(read_registry_exact().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("manual", "REMOVED_TOKEN")
                .unwrap()
                .as_deref(),
            Some("old-removed")
        );
        save_server_editor_fields(
            Some("manual"),
            fields("manual", "stdio"),
            vec![env("KEPT_TOKEN", None)],
            &SecretKeyChanges {
                opened_with: Some(vec!["REMOVED_TOKEN".into(), "KEPT_TOKEN".into()]),
                ..SecretKeyChanges::removing(&["REMOVED_TOKEN"])
            },
            None,
            &[],
        )
        .unwrap();
        assert!(
            crate::secrets::get_vault_secret_result("manual", "REMOVED_TOKEN")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("manual", "KEPT_TOKEN")
                .unwrap()
                .as_deref(),
            Some("old-kept")
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("manual", "ADDED_ELSEWHERE")
                .unwrap()
                .as_deref(),
            Some("other-window")
        );
        // Its declaration stays too, so the value is not orphaned.
        let keys: Vec<String> = read_registry_exact().unwrap().servers[0]
            .env
            .iter()
            .map(|env| env.key.clone())
            .collect();
        assert_eq!(keys, ["KEPT_TOKEN", "ADDED_ELSEWHERE"]);
    }

    #[test]
    fn whole_entry_update_deletes_dropped_keychain_values_and_restores_them_on_failure() {
        let env = |key: &str, secret: bool| registry::EnvVar {
            key: key.into(),
            value: None,
            secret,
            unknown_fields: Default::default(),
        };
        let mut initial = Registry::default();
        let mut entry = entry_from_fields(fields("web", "stdio")).unwrap();
        entry.id = "web".into();
        entry.env = vec![
            env("REMOVED_TOKEN", true),
            env("NOW_PLAIN", true),
            env("KEPT_TOKEN", true),
            env("ADDED_ELSEWHERE", true),
        ];
        initial.servers.push(entry.clone());
        let _fixture = MoveFixture::new(&initial);
        for key in [
            "REMOVED_TOKEN",
            "NOW_PLAIN",
            "KEPT_TOKEN",
            "ADDED_ELSEWHERE",
        ] {
            crate::secrets::set_secret("web", key, "old").unwrap();
        }
        let mut edited = entry;
        // Made plain with its new value typed in.
        let mut now_plain = env("NOW_PLAIN", false);
        now_plain.value = Some("visible".into());
        edited.env = vec![now_plain, env("KEPT_TOKEN", true)];
        let save = |entry: ServerEntry| {
            registry::update(|registry| apply_update_entry(registry, entry)).map(|(r, _)| r)
        };
        let stored = |key: &str| crate::secrets::get_vault_secret_result("web", key).unwrap();

        // The registry write fails: every deleted value comes back.
        // The dialog was opened before ADDED_ELSEWHERE existed, so it lists only
        // what the user removed or made plain.
        let changes = SecretKeyChanges::removing(&["REMOVED_TOKEN", "NOW_PLAIN"]);
        let failed =
            update_server_entry_with(edited.clone(), &changes, |_| -> Result<(), String> {
                Err("disk full".into())
            });
        assert!(failed.is_err());
        for key in ["REMOVED_TOKEN", "NOW_PLAIN", "KEPT_TOKEN"] {
            assert_eq!(stored(key).as_deref(), Some("old"), "{key}");
        }

        update_server_entry_with(edited, &changes, save).unwrap();
        assert!(stored("REMOVED_TOKEN").is_none());
        assert!(stored("NOW_PLAIN").is_none());
        assert_eq!(stored("KEPT_TOKEN").as_deref(), Some("old"));
        assert_eq!(stored("ADDED_ELSEWHERE").as_deref(), Some("old"));
    }

    fn keychain_env(key: &str, value: Option<&str>, secret: bool) -> registry::EnvVar {
        registry::EnvVar {
            key: key.into(),
            value: value.map(str::to_string),
            secret,
            unknown_fields: Default::default(),
        }
    }

    /// One saved server "srv" whose listed keys are stored in the keychain.
    fn stored_server(keys: &[(&str, &str)]) -> (Registry, ServerEntry) {
        let mut registry = Registry::default();
        let mut entry = entry_from_fields(fields("srv", "stdio")).unwrap();
        entry.id = "srv".into();
        entry.env = keys
            .iter()
            .map(|(key, _)| keychain_env(key, None, true))
            .collect();
        registry.servers.push(entry.clone());
        (registry, entry)
    }

    fn stored(key: &str) -> Option<String> {
        crate::secrets::get_vault_secret_result("srv", key).unwrap()
    }

    fn save_registry(entry: ServerEntry) -> Result<Registry, String> {
        registry::update(|registry| apply_update_entry(registry, entry)).map(|(r, _)| r)
    }

    #[test]
    fn native_editor_add_keeps_secret_arguments_and_url_tokens_out_of_the_registry() {
        let _fixture = MoveFixture::new(&Registry::default());
        let mut local = fields("argy", "stdio");
        local.args = vec![
            "-y".into(),
            "example-server".into(),
            "--api-key".into(),
            "sk-live-synthetic0123456789abcdef".into(),
        ];
        let saved = save_server_editor_fields(
            None,
            local,
            Vec::new(),
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap();
        let argy = saved.servers.iter().find(|s| s.name == "argy").unwrap();
        assert!(argy.args.iter().any(|arg| arg == "<launch-input>"));
        let mut remote = fields("hooky", "http");
        remote.url = Some("https://example.com/mcp?token=synthetic-url-token".into());
        let saved = save_server_editor_fields(
            None,
            remote,
            Vec::new(),
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap();
        let hooky = saved.servers.iter().find(|s| s.name == "hooky").unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result(&hooky.id, crate::secrets::IMPORTED_URL_KEY)
                .unwrap()
                .as_deref(),
            Some("https://example.com/mcp?token=synthetic-url-token")
        );
        let on_disk = std::fs::read_to_string(registry::resolved_path().unwrap()).unwrap();
        assert!(!on_disk.contains("sk-live-synthetic0123456789abcdef"));
        assert!(!on_disk.contains("synthetic-url-token"));
    }

    #[test]
    fn a_keychain_variable_made_plain_without_a_value_keeps_its_stored_value() {
        let (initial, entry) = stored_server(&[("TOKEN", "old")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "TOKEN", "old").unwrap();
        let before = serde_json::to_value(read_registry_exact().unwrap()).unwrap();
        let plain = keychain_env("TOKEN", None, false);
        let (changes, blocked) = editor_secret_changes(
            &["TOKEN".into()],
            &[EditorSecretRow {
                stored_as: Some("TOKEN"),
                row: &plain,
            }],
            &[],
        );
        assert_eq!(blocked, [("TOKEN".to_string(), KEEP_IN_KEYCHAIN)]);
        assert!(changes.removed.is_empty());

        // The backend refuses it too, whatever the dialog sent, in both shells.
        let gtk = save_server_editor_fields(
            Some("srv"),
            fields("srv", "stdio"),
            vec![plain.clone()],
            &SecretKeyChanges::removing(&["TOKEN"]),
            None,
            &[],
        );
        assert!(gtk.unwrap_err().contains(KEEP_IN_KEYCHAIN));
        let mut edited = entry;
        edited.env = vec![plain];
        let react = update_server_entry_with(
            edited,
            &SecretKeyChanges::removing(&["TOKEN"]),
            save_registry,
        );
        assert!(react.unwrap_err().contains(KEEP_IN_KEYCHAIN));
        assert_eq!(stored("TOKEN").as_deref(), Some("old"));
        assert_eq!(
            serde_json::to_value(read_registry_exact().unwrap()).unwrap(),
            before
        );

        // With its new value typed in, it becomes plain and leaves the keychain.
        let typed = keychain_env("TOKEN", Some("visible"), false);
        let (changes, blocked) = editor_secret_changes(
            &["TOKEN".into()],
            &[EditorSecretRow {
                stored_as: Some("TOKEN"),
                row: &typed,
            }],
            &[],
        );
        assert!(blocked.is_empty());
        save_server_editor_fields(
            Some("srv"),
            fields("srv", "stdio"),
            vec![typed],
            &changes,
            None,
            &[],
        )
        .unwrap();
        assert!(stored("TOKEN").is_none());
    }

    #[test]
    fn renaming_a_keychain_variable_moves_its_value_in_both_shells() {
        let (initial, entry) = stored_server(&[("GITHUB_TOKN", "abc")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "GITHUB_TOKN", "abc").unwrap();
        let renamed = keychain_env("GITHUB_TOKEN", None, true);
        let (changes, blocked) = editor_secret_changes(
            &["GITHUB_TOKN".into()],
            &[EditorSecretRow {
                stored_as: Some("GITHUB_TOKN"),
                row: &renamed,
            }],
            &[],
        );
        assert!(blocked.is_empty());
        assert!(changes.removed.is_empty());
        assert_eq!(
            changes.renamed,
            [("GITHUB_TOKN".to_string(), "GITHUB_TOKEN".to_string())]
        );

        // A failed write leaves the value where it was.
        let failed = crate::secrets::tests::with_failed_write("GITHUB_TOKEN", || {
            save_server_editor_fields(
                Some("srv"),
                fields("srv", "stdio"),
                vec![renamed.clone()],
                &changes,
                None,
                &[],
            )
        });
        assert!(failed.is_err());
        assert_eq!(stored("GITHUB_TOKN").as_deref(), Some("abc"));
        assert!(stored("GITHUB_TOKEN").is_none());

        save_server_editor_fields(
            Some("srv"),
            fields("srv", "stdio"),
            vec![renamed.clone()],
            &changes,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(stored("GITHUB_TOKEN").as_deref(), Some("abc"));
        assert!(stored("GITHUB_TOKN").is_none());

        // React's whole-entry update moves it the same way.
        let mut back = entry;
        back.env = vec![keychain_env("GITHUB_TOKN", None, true)];
        update_server_entry_with(
            back,
            &SecretKeyChanges {
                renamed: vec![("GITHUB_TOKEN".into(), "GITHUB_TOKN".into())],
                ..SecretKeyChanges::default()
            },
            save_registry,
        )
        .unwrap();
        assert_eq!(stored("GITHUB_TOKN").as_deref(), Some("abc"));
        assert!(stored("GITHUB_TOKEN").is_none());
    }

    #[test]
    fn an_editor_commit_failure_puts_every_keychain_change_back() {
        let (initial, _) = stored_server(&[("REMOVED", "r"), ("OLD_NAME", "m"), ("TYPED", "t")]);
        let _fixture = MoveFixture::new(&initial);
        for (key, value) in [("REMOVED", "r"), ("OLD_NAME", "m"), ("TYPED", "t")] {
            crate::secrets::set_secret("srv", key, value).unwrap();
        }
        let before = serde_json::to_value(&read_registry_exact().unwrap().servers).unwrap();
        // Another window saves between the keychain work and the commit.
        BEFORE_EDITOR_COMMIT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|| {
                registry::update(|registry| {
                    registry.secrets_generation = registry.secrets_generation.wrapping_add(7);
                    Ok(())
                })
                .unwrap();
            }));
        });
        let result = save_server_editor_fields(
            Some("srv"),
            fields("srv", "stdio"),
            vec![
                keychain_env("NEW_NAME", None, true),
                keychain_env("TYPED", Some("new"), true),
            ],
            &SecretKeyChanges {
                removed: vec!["REMOVED".into()],
                renamed: vec![("OLD_NAME".into(), "NEW_NAME".into())],
                opened_with: Some(vec!["REMOVED".into(), "OLD_NAME".into(), "TYPED".into()]),
            },
            None,
            &[],
        );
        assert_eq!(result.unwrap_err(), EDITOR_SAVE_RACE);
        assert_eq!(stored("REMOVED").as_deref(), Some("r"));
        assert_eq!(stored("OLD_NAME").as_deref(), Some("m"));
        assert_eq!(stored("TYPED").as_deref(), Some("t"));
        assert!(stored("NEW_NAME").is_none());
        assert_eq!(
            serde_json::to_value(&read_registry_exact().unwrap().servers).unwrap(),
            before
        );
    }

    #[test]
    fn a_failed_restore_is_reported_with_the_original_error() {
        let (initial, entry) = stored_server(&[("OLD_NAME", "m")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "OLD_NAME", "m").unwrap();
        let mut edited = entry;
        edited.env = vec![keychain_env("NEW_NAME", None, true)];
        let error = update_server_entry_with(
            edited,
            &SecretKeyChanges {
                renamed: vec![("OLD_NAME".into(), "NEW_NAME".into())],
                ..SecretKeyChanges::default()
            },
            |_| -> Result<(), String> {
                // Another process changes the moved value before the failure,
                // so the rollback cannot restore it.
                crate::secrets::set_secret("srv", "NEW_NAME", "concurrent").unwrap();
                Err("disk full".into())
            },
        )
        .unwrap_err();
        assert_eq!(stored("NEW_NAME").as_deref(), Some("concurrent"));
        assert_eq!(stored("OLD_NAME").as_deref(), Some("m"));
        assert!(error.contains("disk full"), "{error}");
        assert!(
            error.contains("Could not undo every keychain change"),
            "{error}"
        );
    }

    #[test]
    fn a_team_copy_never_deletes_a_value_its_personal_original_still_declares() {
        let mut initial = Registry {
            team: Some(
                serde_json::from_value(serde_json::json!({"serverUrl":"https://example.com","teamId":"solo","role":"admin","reportingDeviceId":"fixture","accountStatus":{"personalSync":true}}))
                    .unwrap(),
            ),
            ..Default::default()
        };
        let mut personal = entry_from_fields(fields("docs", "http")).unwrap();
        personal.id = "docs".into();
        personal.env = vec![keychain_env("TOKEN", None, true)];
        let mut managed = personal.clone();
        managed.id = "team_solo_docs".into();
        managed.source = Some("team:solo".into());
        managed
            .unknown_fields
            .insert("teamOriginalId".into(), serde_json::json!("docs"));
        initial
            .team
            .as_mut()
            .unwrap()
            .managed_server_ids
            .insert(managed.id.clone(), personal.id.clone());
        initial.servers = vec![personal.clone(), managed.clone()];
        crate::local_auth::bind(&mut initial, &managed, &personal).unwrap();
        let _fixture = MoveFixture::new(&initial);
        assert_eq!(crate::local_auth::owner("team_solo_docs").unwrap(), "docs");
        crate::secrets::set_secret("docs", "TOKEN", "shared").unwrap();
        let mut edited = managed;
        edited.env.clear();
        update_server_entry_with(edited, &SecretKeyChanges::removing(&["TOKEN"]), |_| {
            Ok::<_, String>(())
        })
        .unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result("docs", "TOKEN")
                .unwrap()
                .as_deref(),
            Some("shared")
        );
    }

    #[test]
    fn a_new_server_whose_reference_approval_fails_is_not_saved() {
        let personal = Registry {
            team: Some(
                serde_json::from_value(serde_json::json!({"serverUrl":"https://example.com","teamId":"solo","role":"admin","reportingDeviceId":"fixture","accountStatus":{"personalSync":true}}))
                    .unwrap(),
            ),
            ..Default::default()
        };
        let _fixture = MoveFixture::new(&personal);
        let approvals = registry::conduit_dir()
            .unwrap()
            .join("secret-reference-approvals.json");
        // A directory where the approvals file goes makes approving fail.
        std::fs::create_dir_all(&approvals).unwrap();
        let mut reference = keychain_env("TOKEN", None, true);
        reference.unknown_fields.insert(
            "source".into(),
            serde_json::json!({"ref":"op://Vault/Item/field"}),
        );
        let save = || {
            save_server_editor_fields(
                None,
                fields("referenced", "stdio"),
                vec![reference.clone()],
                &SecretKeyChanges::default(),
                None,
                &[],
            )
        };
        assert!(save().is_err());
        assert!(read_registry_exact_or_default().unwrap().servers.is_empty());
        std::fs::remove_dir_all(&approvals).unwrap();
        save().unwrap();
        assert_eq!(read_registry_exact().unwrap().servers.len(), 1);
    }

    #[test]
    fn a_new_server_with_a_typed_secret_argument_turns_on() {
        let _fixture = MoveFixture::new(&Registry::default());
        let mut form = fields("dup", "stdio");
        form.args.push("<launch-input>".into());
        let input = registry::LaunchInput {
            key: "LAUNCH_TOKEN".into(),
            label: "Token".into(),
            secret: true,
            required: true,
            value: None,
            unknown_fields: Default::default(),
        };
        let launch = registry::LaunchConfig {
            inputs: vec![input.clone()],
            bindings: vec![registry::ArgBinding {
                index: 2,
                parts: vec![registry::ArgPart::Input {
                    key: "LAUNCH_TOKEN".into(),
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            }],
            ..Default::default()
        };
        let saved = save_server_editor_fields(
            None,
            form,
            Vec::new(),
            &SecretKeyChanges::default(),
            Some(launch),
            &[(input, "typed-launch".into())],
        )
        .unwrap();
        let server = saved.servers.iter().find(|s| s.name == "dup").unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result(&server.id, "LAUNCH_TOKEN")
                .unwrap()
                .as_deref(),
            Some("typed-launch")
        );
        assert!(crate::import_credentials::ready(server).unwrap());
        assert!(server.enabled, "a complete new server is turned on");
    }

    /// Saves the editor's rows the way both shells do: changes computed from
    /// the rows, every opened variable listed.
    fn save_rows(
        opened: &[&str],
        rows: &[(Option<&str>, registry::EnvVar)],
    ) -> Result<Registry, String> {
        let opened: Vec<String> = opened.iter().map(|k| k.to_string()).collect();
        let (mut changes, blocked) = editor_secret_changes(
            &opened,
            &rows
                .iter()
                .map(|(stored_as, row)| EditorSecretRow {
                    stored_as: *stored_as,
                    row,
                })
                .collect::<Vec<_>>(),
            &[],
        );
        assert!(blocked.is_empty(), "{blocked:?}");
        changes.opened_with = Some(opened);
        save_server_editor_fields(
            Some("srv"),
            fields("srv", "stdio"),
            rows.iter().map(|(_, row)| row.clone()).collect(),
            &changes,
            None,
            &[],
        )
    }

    #[test]
    fn renaming_onto_a_removed_keychain_name_moves_the_value() {
        let (initial, _) = stored_server(&[("GH_TOKEN", "new"), ("GITHUB_TOKEN", "stale")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "GH_TOKEN", "new").unwrap();
        crate::secrets::set_secret("srv", "GITHUB_TOKEN", "stale").unwrap();
        // The GITHUB_TOKEN row is deleted and GH_TOKEN renamed to it.
        save_rows(
            &["GH_TOKEN", "GITHUB_TOKEN"],
            &[(Some("GH_TOKEN"), keychain_env("GITHUB_TOKEN", None, true))],
        )
        .unwrap();
        assert_eq!(stored("GITHUB_TOKEN").as_deref(), Some("new"));
        assert!(stored("GH_TOKEN").is_none());
    }

    #[test]
    fn swapping_two_keychain_names_swaps_their_values() {
        let (initial, _) = stored_server(&[("A_TOKEN", "a"), ("B_TOKEN", "b")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "A_TOKEN", "a").unwrap();
        crate::secrets::set_secret("srv", "B_TOKEN", "b").unwrap();
        save_rows(
            &["A_TOKEN", "B_TOKEN"],
            &[
                (Some("A_TOKEN"), keychain_env("B_TOKEN", None, true)),
                (Some("B_TOKEN"), keychain_env("A_TOKEN", None, true)),
            ],
        )
        .unwrap();
        assert_eq!(stored("B_TOKEN").as_deref(), Some("a"));
        assert_eq!(stored("A_TOKEN").as_deref(), Some("b"));
    }

    #[test]
    fn a_new_row_with_a_renamed_variables_old_name_does_not_inherit_its_value() {
        let (initial, _) = stored_server(&[("A_TOKEN", "a")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "A_TOKEN", "a").unwrap();
        save_rows(
            &["A_TOKEN"],
            &[
                (Some("A_TOKEN"), keychain_env("B_TOKEN", None, true)),
                (None, keychain_env("A_TOKEN", None, true)),
            ],
        )
        .unwrap();
        assert_eq!(stored("B_TOKEN").as_deref(), Some("a"));
        assert!(stored("A_TOKEN").is_none());
    }

    #[test]
    fn a_rename_onto_a_secret_arguments_name_is_refused() {
        let (mut initial, _) = stored_server(&[("A_TOKEN", "a")]);
        initial.servers[0].args[1] = "<launch-input>".into();
        initial.servers[0].launch = Some(registry::LaunchConfig {
            inputs: vec![registry::LaunchInput {
                key: "IMPORTED_ARG_0".into(),
                label: "Key".into(),
                secret: true,
                required: true,
                value: None,
                unknown_fields: Default::default(),
            }],
            bindings: vec![registry::ArgBinding {
                index: 1,
                parts: vec![registry::ArgPart::Input {
                    key: "IMPORTED_ARG_0".into(),
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            }],
            ..Default::default()
        });
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "A_TOKEN", "a").unwrap();
        crate::secrets::set_secret("srv", "IMPORTED_ARG_0", "arg").unwrap();
        let renamed = keychain_env("IMPORTED_ARG_0", None, true);
        let (_, blocked) = editor_secret_changes(
            &["A_TOKEN".into()],
            &[EditorSecretRow {
                stored_as: Some("A_TOKEN"),
                row: &renamed,
            }],
            &["IMPORTED_ARG_0".into()],
        );
        assert_eq!(blocked, [("A_TOKEN".to_string(), NAME_USED_BY_ARGUMENT)]);
        let mut form = fields("srv", "stdio");
        form.args[1] = "<launch-input>".into();
        let error = save_server_editor_fields(
            Some("srv"),
            form,
            vec![renamed],
            &SecretKeyChanges {
                renamed: vec![("A_TOKEN".into(), "IMPORTED_ARG_0".into())],
                opened_with: Some(vec!["A_TOKEN".into()]),
                ..SecretKeyChanges::default()
            },
            None,
            &[],
        )
        .unwrap_err();
        assert!(error.contains(NAME_USED_BY_ARGUMENT), "{error}");
        assert_eq!(stored("A_TOKEN").as_deref(), Some("a"));
        assert_eq!(stored("IMPORTED_ARG_0").as_deref(), Some("arg"));
    }

    #[test]
    fn a_renamed_required_variable_still_starts() {
        let _fixture = MoveFixture::new(&Registry::default());
        // Added the way React's Add dialog and a pasted config add a server.
        let mut entry = entry_from_fields(fields("gh", "stdio")).unwrap();
        entry.env = vec![keychain_env("GITHUB_TOKN", Some("abc"), true)];
        let (added, id) = add_reviewed_entry(entry).unwrap();
        let server = added.servers.iter().find(|s| s.id == id).unwrap();
        assert!(crate::launch_inputs::resolve_args(server).is_ok());
        let renamed = keychain_env("GITHUB_TOKEN", None, true);
        let opened = vec!["GITHUB_TOKN".to_string()];
        let (mut changes, _) = editor_secret_changes(
            &opened,
            &[EditorSecretRow {
                stored_as: Some("GITHUB_TOKN"),
                row: &renamed,
            }],
            &[],
        );
        changes.opened_with = Some(opened);
        let mut form = fields("gh", "stdio");
        form.cwd = server.cwd.clone();
        let saved =
            save_server_editor_fields(Some(&id), form, vec![renamed], &changes, None, &[]).unwrap();
        let server = saved.servers.iter().find(|s| s.id == id).unwrap();
        assert!(crate::launch_inputs::resolve_args(server).is_ok());
        // React's whole-entry save renames it the same way.
        let mut back = server.clone();
        back.env = vec![keychain_env("GITHUB_TOKN", None, true)];
        let saved = update_server_entry_with(
            back,
            &SecretKeyChanges {
                renamed: vec![("GITHUB_TOKEN".into(), "GITHUB_TOKN".into())],
                ..SecretKeyChanges::default()
            },
            save_registry,
        )
        .unwrap();
        let server = saved.servers.iter().find(|s| s.id == id).unwrap();
        assert!(crate::launch_inputs::resolve_args(server).is_ok());
    }

    #[test]
    fn stored_values_are_deleted_only_after_the_registry_commit() {
        let (initial, _) = stored_server(&[("REMOVED", "r")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "REMOVED", "r").unwrap();
        BEFORE_EDITOR_COMMIT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|| {
                assert_eq!(
                    stored("REMOVED").as_deref(),
                    Some("r"),
                    "deleted before commit"
                );
            }));
        });
        save_rows(&["REMOVED"], &[]).unwrap();
        assert!(stored("REMOVED").is_none());
    }

    #[test]
    fn a_react_save_deletes_stored_values_only_after_the_registry_write() {
        let (initial, entry) = stored_server(&[("REMOVED", "r")]);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("srv", "REMOVED", "r").unwrap();
        let mut edited = entry;
        edited.env.clear();
        update_server_entry_with(edited, &SecretKeyChanges::removing(&["REMOVED"]), |entry| {
            assert_eq!(
                stored("REMOVED").as_deref(),
                Some("r"),
                "deleted before commit"
            );
            save_registry(entry)
        })
        .unwrap();
        assert!(stored("REMOVED").is_none());
    }

    #[test]
    fn a_stored_variable_with_its_name_cleared_is_refused() {
        let cleared = keychain_env("", None, true);
        let (changes, blocked) = editor_secret_changes(
            &["TOKEN".into()],
            &[EditorSecretRow {
                stored_as: Some("TOKEN"),
                row: &cleared,
            }],
            &[],
        );
        assert_eq!(blocked, [("TOKEN".to_string(), NAME_THIS_VARIABLE)]);
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn the_editor_refuses_a_credential_in_the_command_without_client_config_words() {
        let _fixture = MoveFixture::new(&Registry::default());
        let mut form = fields("leaky", "stdio");
        form.command = Some("API_KEY=synthetic-secret-value npx".into());
        let error = save_server_editor_fields(
            None,
            form,
            Vec::new(),
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap_err();
        assert!(
            error.contains("The command contains a credential"),
            "{error}"
        );
        assert!(!error.contains("Client config"), "{error}");
    }

    #[test]
    fn a_gtk_edit_of_a_personal_original_keeps_the_value_its_team_copy_declares() {
        let mut initial = Registry {
            team: Some(
                serde_json::from_value(serde_json::json!({"serverUrl":"https://example.com","teamId":"solo","role":"admin","reportingDeviceId":"fixture","accountStatus":{"personalSync":true}}))
                    .unwrap(),
            ),
            ..Default::default()
        };
        let mut personal = entry_from_fields(fields("docs", "stdio")).unwrap();
        personal.id = "docs".into();
        personal.env = vec![keychain_env("TOKEN", None, true)];
        let mut managed = personal.clone();
        managed.id = "team_solo_docs".into();
        managed.source = Some("team:solo".into());
        managed
            .unknown_fields
            .insert("teamOriginalId".into(), serde_json::json!("docs"));
        initial
            .team
            .as_mut()
            .unwrap()
            .managed_server_ids
            .insert(managed.id.clone(), personal.id.clone());
        initial.servers = vec![personal.clone(), managed.clone()];
        crate::local_auth::bind(&mut initial, &managed, &personal).unwrap();
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("docs", "TOKEN", "shared").unwrap();
        save_server_editor_fields(
            Some("docs"),
            fields("docs", "stdio"),
            Vec::new(),
            &SecretKeyChanges {
                opened_with: Some(vec!["TOKEN".into()]),
                ..SecretKeyChanges::removing(&["TOKEN"])
            },
            None,
            &[],
        )
        .unwrap();
        assert_eq!(
            crate::secrets::get_vault_secret_result("docs", "TOKEN")
                .unwrap()
                .as_deref(),
            Some("shared")
        );
    }

    #[test]
    fn an_editor_save_that_loses_a_race_says_so_in_its_own_words() {
        let _fixture = MoveFixture::new(&Registry::default());
        let previous = read_registry_exact_or_default().unwrap();
        let mut prepared = previous.clone();
        prepared.add_server(entry_from_fields(fields("mine", "stdio")).unwrap());
        // Another window saves first.
        registry::update(|registry| {
            registry.add_server(entry_from_fields(fields("theirs", "stdio")).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            commit_unless_changed(&previous, prepared, EDITOR_SAVE_RACE).unwrap_err(),
            EDITOR_SAVE_RACE
        );
        let names: Vec<String> = read_registry_exact()
            .unwrap()
            .servers
            .into_iter()
            .map(|server| server.name)
            .collect();
        assert_eq!(names, ["theirs"]);
    }

    #[test]
    fn native_editor_launch_vault_failure_rolls_back_environment_and_fields() {
        let mut existing = server("manual");
        let mut form = fields("Changed", "stdio");
        form.args.push("<launch-input>".into());
        existing.command = form.command.clone();
        existing.args = form.args.clone();
        let input = registry::LaunchInput {
            key: "LAUNCH_TOKEN".into(),
            label: "Token".into(),
            secret: true,
            required: true,
            value: None,
            unknown_fields: Default::default(),
        };
        existing.launch = Some(registry::LaunchConfig {
            inputs: vec![input.clone()],
            bindings: vec![registry::ArgBinding {
                index: 2,
                parts: vec![registry::ArgPart::Input {
                    key: "LAUNCH_TOKEN".into(),
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            }],
            ..Default::default()
        });
        existing.env = vec![registry::EnvVar {
            key: "TOKEN".into(),
            value: None,
            secret: true,
            unknown_fields: Default::default(),
        }];
        let mut initial = Registry::default();
        initial.servers.push(existing);
        let _fixture = MoveFixture::new(&initial);
        crate::secrets::set_secret("manual", "TOKEN", "old-env").unwrap();
        let before = serde_json::to_value(read_registry_exact().unwrap()).unwrap();
        let result = crate::secrets::tests::with_failed_write("LAUNCH_TOKEN", || {
            save_server_editor_fields(
                Some("manual"),
                form,
                vec![registry::EnvVar {
                    key: "TOKEN".into(),
                    value: Some("new-env".into()),
                    secret: true,
                    unknown_fields: Default::default(),
                }],
                &SecretKeyChanges::default(),
                None,
                &[(input, "new-launch".into())],
            )
        });
        assert!(result.unwrap_err().contains("Keychain unavailable"));
        assert_eq!(
            serde_json::to_value(read_registry_exact().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            crate::secrets::get_vault_secret_result("manual", "TOKEN")
                .unwrap()
                .as_deref(),
            Some("old-env")
        );
        assert!(
            crate::secrets::get_vault_secret_result("manual", "LAUNCH_TOKEN")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn native_editor_saves_credentials_before_first_save_without_plaintext_and_preserves_on_edit() {
        let _fixture = MoveFixture::new(&Registry::default());
        let env = |key: &str, value: Option<&str>, secret| registry::EnvVar {
            key: key.into(),
            value: value.map(str::to_string),
            secret,
            unknown_fields: Default::default(),
        };
        let saved = save_server_editor_fields(
            None,
            fields("manual-credentials", "stdio"),
            vec![
                env("TOKEN", Some("synthetic-manual-key"), true),
                env("REGION", Some("east"), false),
            ],
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap();
        let server = &saved.servers[0];
        let id = server.id.clone();
        assert!(server.enabled);
        assert_eq!(server.env[0].value, None);
        assert_eq!(
            crate::secrets::get_vault_secret_result(&id, "TOKEN")
                .unwrap()
                .as_deref(),
            Some("synthetic-manual-key")
        );
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("synthetic-manual-key"));
        let edited = save_server_editor_fields(
            Some(&id),
            fields("edited", "stdio"),
            vec![env("TOKEN", None, true), env("REGION", Some("west"), false)],
            &SecretKeyChanges::default(),
            None,
            &[],
        )
        .unwrap();
        assert_eq!(edited.servers[0].env[1].value.as_deref(), Some("west"));
        assert_eq!(
            crate::secrets::get_vault_secret_result(&id, "TOKEN")
                .unwrap()
                .as_deref(),
            Some("synthetic-manual-key")
        );
        assert!(save_server_editor_fields(
            Some(&id),
            fields("invalid", "stdio"),
            vec![
                env("TOKEN", Some("replacement"), true),
                env("TOKEN", Some("duplicate"), true)
            ],
            &SecretKeyChanges::default(),
            None,
            &[]
        )
        .is_err());
        assert_eq!(
            serde_json::to_value(read_registry_exact().unwrap()).unwrap(),
            serde_json::to_value(edited).unwrap()
        );
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
        let (registry, count) = import_client_servers(vec!["name:one".into()]).unwrap();
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
        assert_eq!(import_client_servers(vec!["name:one".into()]).unwrap().1, 0);
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
        let (registry, _) = import_client_servers(vec!["name:one".into()]).unwrap();
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
            "supported":{"command":"node"}
        }));
        let mut unrelated = fixture.client.clone();
        unrelated.id = "cursor".into();
        unrelated.servers.truncate(1);
        unrelated.servers[0].name = "other-client".into();
        let detected = [fixture.client.clone(), unrelated];
        let mut registry = Registry::default();
        assert_eq!(servers_to_import(&detected, &registry).len(), 3);
        assert!(selected_servers_to_import(&detected, &registry, None)
            .unwrap_err()
            .contains("'cwd'"));
        assert!(registry.servers.is_empty());
        let selected =
            std::collections::HashSet::from(["name:supported".into(), "name:other-client".into()]);
        let servers = selected_servers_to_import(&detected, &registry, Some(&selected)).unwrap();
        assert_eq!(servers.len(), 2);
        for entry in servers {
            registry.add_server(entry);
        }
        assert_eq!(registry.servers.len(), 2);
        assert!(registry.enabled_servers().is_empty());
    }

    #[test]
    fn zcode_client_import_keeps_disabled_entries_disabled_including_legacy_false() {
        let fixture = ZCodeImportFixture::new(serde_json::json!({
            "disabled":{"command":"node", "enabled":false, "enable":true},
            "legacy-disabled":{"command":"node", "enabled":true, "enable":false}
        }));
        let mut registry = Registry::default();
        for entry in
            selected_servers_to_import(std::slice::from_ref(&fixture.client), &registry, None)
                .unwrap()
        {
            registry.add_server(entry);
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
        /// GTK: the editor's rows, filled from the paste, through `save_server_editor_fields`.
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
                // The pasted rows fill the editor's variable rows, which save
                // through the editor's own path.
                let rows = parsed
                    .env
                    .iter()
                    .map(|e| registry::EnvVar {
                        key: e.key.clone(),
                        value: e.value.clone(),
                        secret: secret(e),
                        unknown_fields: Default::default(),
                    })
                    .collect();
                save_server_editor_fields(
                    None,
                    fields,
                    rows,
                    &SecretKeyChanges::default(),
                    None,
                    &[],
                )?;
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
            .any(|warning| warning.contains("kept your edited toolport entry")));
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
}
