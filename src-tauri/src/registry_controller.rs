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
    pub key: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
}

const AUTH_LOCK_LEASE_SECS: u64 = 180;
const AUTH_LOCK_WAIT_SECS: u64 = 30;
const AUTH_LOCK_POLL_MS: u64 = 250;

pub(crate) struct AuthMutationLock {
    path: std::path::PathBuf,
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
            }))
        }
        Err(error) => Err(format!("could not create auth mutation lock file: {error}")),
    }
}

pub(crate) fn acquire_auth_lock(server_id: &str) -> Result<AuthMutationLock, String> {
    acquire_auth_owner_lock(&crate::local_auth::owner(server_id)?)
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
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub cwd: Option<String>,
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
            written,
            recovery_path: outcome.recovery_path.clone(),
            exact_rollback,
        })
    }

    fn rollback(&self) -> Result<(), String> {
        if !self.exact_rollback {
            return Err("exact rollback is unavailable because the client saved after this operation; newer edits were left untouched".into());
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
            .map(std::fs::read_to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        registry::client_file::commit(&self.target, &revision, original.as_deref())?;
        if let Some(file) = &self.recovery_path {
            clients::record_config_rollback(file, &self.target, original.as_deref())?;
        }
        Ok(())
    }
}

impl ServerFields {
    fn normalized(mut self) -> Result<Self, String> {
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
        inherit_env: false,
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
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: None,
        launch: None,
        unknown_fields: serde_json::Map::new(),
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

pub fn preview_client_imports() -> Result<Vec<ClientImportCandidate>, String> {
    let registry = read_registry_exact_or_default()?;
    let detected = clients::detect_clients();
    Ok(servers_to_import(&detected, &registry)
        .into_iter()
        .map(|server| ClientImportCandidate {
            key: clients::import_dedupe_key(&server.name, server.command.as_deref(), &server.args),
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
        .collect())
}

pub fn import_client_servers(selected: Vec<String>) -> Result<(Registry, usize), String> {
    let detected = clients::detect_clients();
    let selected = selected
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let current = read_registry_exact_or_default()?;
    let servers = selected_servers_to_import(&detected, &current, Some(&selected))?;
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
        prepared.push(crate::import_credentials::Import::prepare(
            entry,
            definition.as_ref(),
        )?);
    }
    let (registry, added) = registry::update(|registry| {
        let mut added = 0;
        for import in prepared {
            // Concurrent imports may already have created this definition.
            if registry
                .servers
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&import.entry.name))
            {
                continue;
            }
            let id = registry.add_server(import.entry.clone());
            let missing = import.transfer(&id)?;
            if missing.is_empty() {
                let profile = registry.default_access_id();
                apply_server_enabled(registry, &profile, &id, true, false)?;
                let _ = registry.set_access_server(&profile, &id, true);
            }
            added += 1;
        }
        if added > 0 {
            registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        }
        Ok(added)
    })?;
    Ok((registry, added))
}

pub fn apply_update_entry(registry: &mut Registry, entry: ServerEntry) -> Result<(), String> {
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
    server.name = fields.name;
    server.transport = fields.transport;
    server.command = fields.command;
    server.args = fields.args;
    server.url = fields.url;
    server.cwd = fields.cwd;
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
    let import = crate::import_credentials::Import::prepare(entry, Some(&definition))?;
    let (registry, id) = registry::update(|registry| {
        let mut entry = import.entry.clone();
        entry.enabled = false;
        let id = registry.add_server(entry);
        let missing = import.transfer(&id)?;
        if missing.is_empty() {
            let profile = registry.default_access_id();
            if apply_server_enabled(registry, &profile, &id, true, false).is_ok() {
                let _ = registry.set_access_server(&profile, &id, true);
            }
        }
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
        Ok(id)
    })?;
    Ok((registry, id))
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
    /// Env keys that could not be declared or vaulted (invalid name, locked
    /// keychain). The server itself was still added.
    pub failed: Vec<String>,
}

/// Add a server parsed from a pasted config snippet, vaulting its pasted env
/// values the same way an explicit credentials save does: the value goes to the
/// OS keychain and only the key name is declared on the registry entry.
///
/// One bad env entry must not abort the rest - the server add has already
/// committed, so per-key problems are collected and reported instead.
pub fn add_snippet_server(
    fields: ServerFields,
    env: Vec<(String, Option<String>)>,
) -> Result<SnippetAddOutcome, String> {
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
    let import = crate::import_credentials::Import::prepare(entry, Some(&definition))?;
    let (registry, id) =
        registry::update(|registry| Ok(registry.add_server(import.entry.clone())))?;
    let mut outcome = SnippetAddOutcome {
        registry,
        declared_without_value: Vec::new(),
        failed: Vec::new(),
    };
    match import.transfer(&id) {
        Ok(missing) => outcome.declared_without_value = missing,
        Err(_) => outcome.failed.push("credentials".into()),
    }
    if outcome.failed.is_empty() && outcome.declared_without_value.is_empty() {
        let (registry, ()) = registry::update(|registry| {
            let profile = registry.default_access_id();
            apply_server_enabled(registry, &profile, &id, true, false)?;
            let _ = registry.set_access_server(&profile, &id, true);
            registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
            Ok(())
        })?;
        outcome.registry = registry;
    }
    Ok(outcome)
}

/// Shared by both shells. Selection is by index in the exact pasted document;
/// values never go through the registry or a log.
pub fn add_snippet_servers(text: &str, selected: &[String]) -> Result<SnippetAddOutcome, String> {
    let parsed = clients::parse_snippet(text).map_err(|_| "Could not parse the pasted config")?;
    let mut outcome = SnippetAddOutcome {
        registry: read_registry_exact_or_default()?,
        declared_without_value: Vec::new(),
        failed: Vec::new(),
    };
    for (i, server) in parsed.into_iter().enumerate() {
        if !selected.contains(&i.to_string()) {
            continue;
        }
        if outcome
            .registry
            .servers
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&server.name))
        {
            continue;
        }
        let added = add_snippet_server(
            ServerFields {
                name: server.name,
                transport: server.transport,
                command: server.command,
                args: server.args,
                url: server.url,
                cwd: None,
            },
            server.env.into_iter().map(|e| (e.key, e.value)).collect(),
        )?;
        outcome.registry = added.registry;
        outcome.failed.extend(added.failed);
        outcome
            .declared_without_value
            .extend(added.declared_without_value);
    }
    Ok(outcome)
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
        client_credentials: None,
        request_timeout_ms: None,
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

pub fn add_catalog_stack(
    entries: Vec<crate::catalog::CatalogEntry>,
) -> Result<(Registry, usize), String> {
    registry::update(|registry| {
        let mut names = registry
            .servers
            .iter()
            .map(|server| server.name.to_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let mut added = 0usize;
        for entry in entries {
            if names.insert(entry.name.to_lowercase()) {
                apply_add_entry(registry, catalog_server(entry));
                added += 1;
            }
        }
        Ok(added)
    })
}

pub fn update_server_fields(server_id: &str, fields: ServerFields) -> Result<Registry, String> {
    let (registry, ()) =
        registry::update(|registry| apply_update_server_fields(registry, server_id, fields))?;
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
                inherit_env: false,
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
                client_credentials: None,
                request_timeout_ms: None,
                initialize_timeout_ms: None,
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
    Ok(registry)
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
    let mut detected = clients::detect_clients();
    clients::apply_entry_states(&mut detected, managed);
    detected
        .into_iter()
        .find(|client| client.id == client_id)
        .map(|client| client.entry_state)
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
    let outcome = clients::install_gateway(client_id, profile)?;
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
    let result = finish_client_config_mutation(outcome, write_registry)?;
    clients::finish_uninstall(client_id, &result.outcome)?;
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
    let mut summary = ReleaseAllSummary::default();
    for profile in profiles {
        let scope = (!profile.is_empty()).then_some(profile.as_str());
        match crate::integrity::release_all(scope) {
            Ok(outcome) => {
                summary.released += outcome.released;
                summary.skipped.extend(outcome.skipped);
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
pub(crate) fn import_client_servers_for_migration(
    registry: &mut Registry,
    client: &clients::DetectedClient,
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
        let import = crate::import_credentials::Import::prepare(
            server_from_detected(server, &client.id),
            definition.as_ref(),
        )?;
        let id = if let Some(existing) = registry
            .servers
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&server.name))
        {
            if existing.command != import.entry.command
                || existing.args != import.entry.args
                || existing.url != import.entry.url
                || existing.transport != import.entry.transport
            {
                return Err(format!("{} already exists with a different definition. Resolve it under Servers before connecting. Client config unchanged.", server.name));
            }
            let id = existing.id.clone();
            let entry = registry.servers.iter_mut().find(|e| e.id == id).unwrap();
            entry.env = import.entry.env.clone();
            entry.launch = import.entry.launch.clone();
            id
        } else {
            imported += 1;
            registry.add_server(import.entry.clone())
        };
        let missing = import.transfer(&id)?;
        if !missing.is_empty() {
            return Err(format!("{} needs credentials. Add the missing values in its native config or Credentials, then review again. Client config unchanged.", server.name));
        }
    }
    if !moved.is_empty() {
        registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
    }
    Ok((imported, moved))
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
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetupReview {
    pub config_path: String,
    pub backup_dir: String,
    pub revision: String,
    pub items: Vec<SetupItem>,
}

pub fn preview_client_setup(client_id: &str) -> Result<ClientSetupReview, String> {
    let revision = clients::setup_revision(client_id)?;
    let client = clients::detect_clients()
        .into_iter()
        .find(|c| c.id == client_id)
        .ok_or("Unknown client")?;
    if client.error.is_some() {
        return Err("Could not read this client config. Fix it before connecting.".into());
    }
    let registry = read_registry_exact_or_default()?;
    let items = client
        .servers
        .iter()
        .filter(|s| !clients::detected_is_gateway(s))
        .map(|s| SetupItem {
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
        .collect();
    if clients::setup_revision(client_id)? != revision {
        return Err("Client config changed. Review it again.".into());
    }
    Ok(ClientSetupReview {
        config_path: client.config_path,
        revision,
        items,
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

fn rollback_imports(previous: &Registry, staged: &Registry) -> Result<(), String> {
    let (_, complete) = registry::update(|latest| {
        let mut value = serde_json::to_value(&*latest).map_err(|e| e.to_string())?;
        let complete = undo_staged_value(
            &mut value,
            &serde_json::to_value(previous).map_err(|e| e.to_string())?,
            &serde_json::to_value(staged).map_err(|e| e.to_string())?,
        );
        *latest = serde_json::from_value(value).map_err(|e| e.to_string())?;
        Ok(complete)
    })?;
    if complete {
        Ok(())
    } else {
        Err("Registry changed during setup. Concurrent edits were kept. Review Servers before retrying.".into())
    }
}

fn migrate_client_reviewed_with(
    client_id: &str,
    profile: Option<&str>,
    force: bool,
    names: &[String],
    revision: &str,
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
    let mut client = clients::detect_clients()
        .into_iter()
        .find(|c| c.id == client_id)
        .ok_or("Unknown client")?;
    if names.iter().any(|name| {
        !client
            .servers
            .iter()
            .any(|s| &s.name == name && !clients::detected_is_gateway(s))
    }) {
        return Err("Reviewed server no longer exists. Review the client config again.".into());
    }
    client.servers.retain(|s| names.contains(&s.name));
    let mut imported = 0;
    let mut moved = Vec::new();
    let mut verification = SetupVerification::default();
    let mut staged = None;
    let outcome = clients::migrate_reviewed(client_id, profile, names, revision, || {
        let (registry, result) = registry::update(|registry| {
            let previous = registry.clone();
            let (added, moved) = import_client_servers_for_migration(registry, &client)?;
            enable_moved_servers(registry, profile, &moved)?;
            Ok((added, moved, previous))
        })?;
        imported = result.0;
        moved = result.1;
        staged = Some((result.2, registry.clone()));
        verification = verify(&registry, &moved, client_id, profile)?;
        Ok(())
    });
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some((previous, prepared)) = staged {
                if let Err(rollback) = rollback_imports(&previous, &prepared) {
                    return Err(format!("{error} {rollback}"));
                }
            }
            return Err(error);
        }
    };
    let mut result = finish_client_stdio_mutation(client_id, outcome, |managed_entry| {
        registry::update(|registry| {
            Ok(apply_client_stdio_update(
                registry,
                client_id,
                profile,
                managed_entry,
            ))
        })
    })?;
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
                "{} needs credentials. Open Credentials and retry. Client config unchanged.",
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
            credential_state: if server.env.iter().any(|env| env.secret) {
                "stored"
            } else {
                "none"
            }
            .into(),
        });
    }
    Ok(SetupVerification {
        tools: gateway.tools.clone(),
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

const CLIENT_HTTP_VAULT_SERVER: &str = "__toolport_http_clients__";

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
    clients::finish_uninstall(client_id, &result.outcome)?;
    if let Err(error) = revoke() {
        result.outcome.warnings.push(format!(
            "config restored; could not remove the revoked keychain token: {error}"
        ));
    }
    Ok(())
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
    let current = read_registry_exact_or_default()?;
    let http_id = format!("client:{client_id}");
    let has_shared_http_token = current
        .http_clients
        .iter()
        .any(|client| client.id == http_id);
    if !has_shared_http_token {
        return disconnect_client_stdio_with(client_id, false, |_| {
            let (registry, ()) = registry::update(|registry| {
                registry.set_client_scope(client_id, None);
                registry.clear_client_managed_entry(client_id);
                Ok(())
            })?;
            Ok(registry)
        });
    }

    let _lock = acquire_auth_lock(&format!("client-config:{client_id}"))?;
    let outcome = clients::uninstall_gateway(client_id)?;
    let mut result = finish_client_config_mutation(outcome, |_| {
        let (registry, ()) = registry::update(|registry| {
            registry.set_client_scope(client_id, None);
            registry.clear_client_managed_entry(client_id);
            registry.http_clients.retain(|row| row.id != http_id);
            Ok(())
        })?;
        Ok(registry)
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

pub fn set_client_credentials(
    server_id: &str,
    client_id: &str,
    client_secret: Option<String>,
    token_endpoint_auth_method: Option<&str>,
    scope: Option<&str>,
) -> Result<Registry, String> {
    let _mutation = acquire_auth_lock(server_id)?;
    if crate::local_auth::owner(server_id)? != server_id {
        return Err(
            "Edit the personal original to change the shared local sign-in configuration.".into(),
        );
    }
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
    let current = read_registry_exact()?;
    if !current.servers.iter().any(|server| server.id == server_id) {
        return Err(format!("no server with id {server_id:?}"));
    }
    let secret_to_store = client_secret.filter(|secret| !secret.trim().is_empty());
    if secret_to_store.is_none() && !has_client_secret(server_id)? {
        return Err("no client secret is stored for this server yet; enter one".into());
    }
    crate::remote::reset_client_credentials(server_id)?;
    let (registry, ()) = registry::update(|registry| {
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
    if crate::local_auth::owner(server_id)? != server_id {
        return Err(
            "Edit the personal original to change the shared local sign-in configuration.".into(),
        );
    }
    crate::remote::reset_client_credentials(server_id)?;
    let (registry, ()) = registry::update(|registry| {
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
    match server.env.iter_mut().find(|entry| entry.key == key) {
        Some(entry) => {
            entry.secret = true;
            entry.value = None;
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
        .iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("No server with id '{server_id}'"))?;
    if !server.launch.as_ref().is_some_and(|launch| {
        launch
            .inputs
            .iter()
            .any(|input| input.key == key && input.secret)
    }) {
        return Err("not a declared secret launch input".into());
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
                crate::launch_inputs::resolve_args(server)?;
            }
            server.check_enable_allowed(reviewed)?;
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

#[cfg(test)]
mod tests {

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
                    config_path: path.display().to_string(),
                    config_exists: true,
                    app_present: true,
                    servers: inventory,
                    plugin_servers: Vec::new(),
                    gateway_installed: false,
                    entry_state: GatewayEntryState::Absent,
                    error: None,
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
    fn reviewed_failed_setup_rolls_back_vault_and_allows_changed_retry() {
        let fixture = MoveFixture::new(&Registry::default());
        let original = r#"{"mcpServers":{"one":{"command":"fixture","env":{"PAT":"synthetic-first"}}}}"#;
        std::fs::write(fixture.claude(), original).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        assert!(migrate_client_reviewed_with("claude-code",None,false,&["one".into()],&review.revision,|_,_,_,_| Err("Launch failed".into())).is_err());
        assert!(crate::secrets::get_vault_secret_result("one", "PAT").unwrap().is_none());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        std::fs::write(fixture.claude(), original.replace("synthetic-first", "synthetic-retry")).unwrap();
        let review = preview_client_setup("claude-code").unwrap();
        migrate_client_reviewed_with("claude-code",None,false,&["one".into()],&review.revision,|_,_,_,_| Ok(Vec::new().into())).unwrap();
        assert_eq!(crate::secrets::get_vault_secret_result("one", "PAT").unwrap().as_deref(),Some("synthetic-retry"));
    }

    #[test]
    fn reviewed_multi_paste_is_atomic_on_invalid_later_server() {
        let _fixture = MoveFixture::new(&Registry::default());
        let text = r#"{"mcpServers":{"first":{"command":"fixture","env":{"PAT":"synthetic-first"}},"second":{"command":"fixture","env":{"BAD=NAME":"synthetic-second"}}}}"#;
        assert!(add_snippet_servers(text,&["0".into(),"1".into()]).is_err());
        assert!(read_registry_exact().unwrap().servers.is_empty());
        assert!(crate::secrets::get_vault_secret_result("first", "PAT").unwrap().is_none());
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
        assert_eq!(
            crate::secrets::get_vault_secret_result(&entry.id, "PORT")
                .unwrap()
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
    fn reviewed_catalog_and_collection_add_enable_valid_definitions() {
        let fixture = MoveFixture::new(&Registry::default());
        let entry = |name: &str, env: Vec<&str>| {
            serde_json::from_value::<crate::catalog::CatalogEntry>(serde_json::json!({"name":name,"description":"fixture","transport":"stdio","command":"fixture","args":[],"url":null,"envKeys":env,"source":"curated","homepage":null,"category":"Local tools"})).unwrap()
        };
        assert!(add_catalog_entry(entry("catalog", vec![])).unwrap().servers[0].enabled);
        let (registry, count) = add_catalog_stack(vec![
            entry("collection", vec![]),
            entry("missing", vec!["PAT"]),
        ])
        .unwrap();
        assert_eq!(count, 2);
        assert!(
            registry
                .servers
                .iter()
                .find(|s| s.name == "collection")
                .unwrap()
                .enabled
        );
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
        for entry in selected_servers_to_import(&[fixture.client.clone()], &registry, None).unwrap()
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
                selected_servers_to_import(&[fixture.client.clone()], &registry, None).unwrap_err();
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
            client_credentials: None,
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    fn fields(name: &str, transport: &str) -> ServerFields {
        ServerFields {
            name: name.into(),
            transport: transport.into(),
            command: (transport == "stdio").then(|| "npx".into()),
            args: vec!["-y".into(), "example-server".into()],
            url: (transport != "stdio").then(|| "https://example.com/mcp".into()),
            cwd: (transport == "stdio").then(|| " /tmp/project ".into()),
        }
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
            name: "Renamed".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec!["-y".into(), "pkg".into(), "<launch-input>".into()],
            url: None,
            cwd: None,
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
                    name: "Broken".into(),
                    transport: "http".into(),
                    command: None,
                    args: Vec::new(),
                    url: Some("file:///tmp/not-an-mcp-server".into()),
                    cwd: None,
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
        let backup = dir.join("backup.json");
        std::fs::write(&backup, "original config").unwrap();
        std::fs::write(&target, "connected config").unwrap();
        let outcome = WriteOutcome {
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
                .find(|entry| entry.file_name().to_string_lossy().starts_with("original-"))
                .unwrap();
            let record: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(snapshot.path()).unwrap()).unwrap();
            assert_eq!(record["disconnected"], true);
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

/// Update the member's safety choice without changing releasable team policy.
pub fn set_safety_level(level: registry::SafetyLevel) -> Result<Registry, String> {
    let (registry, _) = registry::update(|registry| {
        registry.validate_safety_level(level)?;
        registry.set_safety_level(level);
        Ok(())
    })?;
    Ok(registry)
}
