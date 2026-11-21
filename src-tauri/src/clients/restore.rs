//! Exact first-write provenance, separate from rotating historical backups.
use super::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    version: u32,
    format: Format,
    config_path: String,
    original: Option<String>,
    original_hash: Option<String>,
    baseline: Value,
    captured_at: u128,
    toolport_version: String,
    last_written: Option<String>,
    last_written_hash: Option<String>,
    #[serde(default)]
    published_hash: Option<String>,
    created_parents: Vec<PathBuf>,
    exact_eligible: bool,
    preexisting_gateways: Vec<String>,
    #[serde(default)]
    disconnected: bool,
    #[serde(default)]
    jsonc_settings: bool,
    #[serde(default)]
    disconnect_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_recovery: Option<String>,
    #[serde(skip)]
    recovered_vault: bool,
    #[serde(default)]
    recovery_dismissed: bool,
}

/// Read only valid writer provenance when a forgotten location has no store row.
pub(super) fn purge_backup_config_paths(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let hash = name
                .strip_prefix("original-")
                .or_else(|| name.strip_prefix("released-"))?
                .strip_suffix(".json")?;
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || crate::purge::is_link(&metadata) {
                return None;
            }
            let record: Snapshot = serde_json::from_str(&backup_storage::read(&path).ok()?).ok()?;
            if record.version != 1
                || !matches!(record.format, Format::JsonMcpServers)
                || crate::registry::sha256_hex(&record.config_path) != hash
                || record.original.as_deref().map(crate::registry::sha256_hex)
                    != record.original_hash
                || record
                    .last_written
                    .as_deref()
                    .map(crate::registry::sha256_hex)
                    != record.last_written_hash
            {
                return None;
            }
            Some(record.config_path)
        })
        .collect()
}

pub(super) fn record_path(client_id: &str, path: &Path) -> Result<PathBuf, String> {
    let hash = crate::registry::sha256_hex(&path.to_string_lossy());
    Ok(backup_dir(client_id)
        .ok_or("Could not resolve backup dir")?
        .join(format!("original-{hash}.json")))
}

fn released_path(client_id: &str, path: &Path) -> Result<PathBuf, String> {
    let original = record_path(client_id, path)?;
    Ok(
        original.with_file_name(original.file_name().unwrap().to_string_lossy().replacen(
            "original-",
            "released-",
            1,
        )),
    )
}

/// A started Discard may have released some values, so nothing may restore,
/// recapture or re-encode this receipt (which would also drop its phase). The
/// phase never authorizes deletion itself; read-only inventory still lists it.
fn refuse_started(file: &Path) -> Result<(), String> {
    match std::fs::read_to_string(file) {
        Ok(raw) if backup_storage::discard_phase(&raw).is_some() => {
            Err(recovery::discard_started_refusal())
        }
        _ => Ok(()),
    }
}

// Every caller of `load` is part of a restore, Connect, reconnect, uninstall
// or move path. Read-only listing (`recorded_paths`, recovery inventory) checks
// the phase itself and never reaches this refusal.
/// The config text recorded before Toolport first changed this file, if it existed.
pub(super) fn original_text(client_id: &str, path: &Path) -> Result<Option<String>, String> {
    Ok(load(client_id, path)?.and_then(|record| record.original))
}

fn load(client_id: &str, path: &Path) -> Result<Option<Snapshot>, String> {
    let file = record_path(client_id, path)?;
    refuse_started(&file)?;
    let text = match backup_storage::read(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match backup_storage::read(&released_path(client_id, path)?) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(e) => return Err(format!("Could not read {}: {e}", file.display())),
    };
    let mut record: Snapshot =
        serde_json::from_str(&text).map_err(|e| diagnostics::at(&file, diagnostics::json(&e)))?;
    if let Some(recovery) = record.vault_recovery.as_deref() {
        let recovered: Snapshot = serde_json::from_str(&backup_storage::decode(recovery)?)
            .map_err(|e| diagnostics::json(&e))?;
        if recovered.vault_recovery.is_none() {
            record.original = recovered.original;
            record.original_hash = recovered.original_hash;
            record.baseline = recovered.baseline;
            record.vault_recovery = None;
            record.recovered_vault = true;
        }
    }
    if record.version != 1
        || record
            .published_hash
            .as_ref()
            .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        || record.config_path != path.to_string_lossy()
        || record.original.as_deref().map(crate::registry::sha256_hex) != record.original_hash
        || record
            .last_written
            .as_deref()
            .map(crate::registry::sha256_hex)
            != record.last_written_hash
    {
        return Err(
            "Client original snapshot provenance is invalid; leaving config untouched".into(),
        );
    }
    Ok(Some(record))
}

// Prime missing-value context before rotating the current config on a retry.
// The current file can already contain placeholders and credential field names.
pub(super) fn pending_values(client_id: &str, path: &Path) -> Result<bool, String> {
    let raw = match std::fs::read_to_string(record_path(client_id, path)?) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("Could not inspect saved recovery".into()),
    };
    let (record, refs) = backup_storage::raw_metadata(&raw)?;
    Ok(record["disconnected"] == true && !refs.is_empty())
}

pub(super) fn prepare_disconnect(client_id: &str, path: &Path) -> Result<(), String> {
    load(client_id, path).map(|_| ())
}

pub(super) struct Receipt {
    path: PathBuf,
    previous: Option<String>,
}
impl Receipt {
    pub(super) fn rollback(self) -> Result<(), String> {
        match self.previous {
            Some(text) => crate::registry::atomic_write(&self.path, &text),
            None => std::fs::remove_file(self.path).map_err(|e| e.to_string()),
        }
    }
}

pub(super) fn checkpoint(path: &Path) -> Result<Receipt, String> {
    let previous = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    Ok(Receipt {
        path: path.into(),
        previous,
    })
}

fn missing_parents(path: &Path) -> Vec<PathBuf> {
    let mut parents = Vec::new();
    let mut parent = path.parent();
    while let Some(dir) = parent.filter(|dir| !dir.exists()) {
        parents.push(dir.into());
        parent = dir.parent();
    }
    parents
}

fn retired_hook_count(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Object(map)) => {
            let own = map
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    command.contains(crate::hooks::HOOK_MARKER)
                        || command.contains(crate::guard_cleanup::GUARD_MARKER)
                });
            usize::from(own)
                + map
                    .values()
                    .map(|value| retired_hook_count(Some(value)))
                    .sum::<usize>()
        }
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| retired_hook_count(Some(value)))
            .sum(),
        _ => 0,
    }
}

/// Persist recovery before publishing the corresponding client revision. If the
/// process dies between the two writes, the old config cannot match last_written
/// and the next disconnect takes the conservative merge path.
pub(super) fn remember(
    client_id: &str,
    path: &Path,
    format: Format,
    before: Option<&str>,
    after: Option<&str>,
    disconnecting: bool,
    jsonc_settings: bool,
) -> Result<Receipt, String> {
    // Removing a retired Toolport hook is already a disconnect. Keep provenance
    // for rollback, but later removal must never resurrect the retired hook.
    let disconnecting = disconnecting
        || (jsonc_settings && {
            let before = mutation::value(format, before)?;
            let after = mutation::value(format, after)?;
            retired_hook_count(before.get("hooks")) > retired_hook_count(after.get("hooks"))
        });
    let file = record_path(client_id, path)?;
    let previous = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let loaded = load(client_id, path)?;
    // A new snapshot would take the placeholders as the original and drop
    // the owed values. Only Retry, Discard or another disconnect may proceed.
    if !disconnecting
        && loaded.as_ref().is_some_and(|record| record.disconnected)
        && previous
            .as_deref()
            .is_some_and(|raw| backup_storage::raw_markers(raw).is_ok_and(|refs| !refs.is_empty()))
    {
        let name = find_def(client_id)
            .map(|def| def.name.to_string())
            .unwrap_or_else(|| client_id.into());
        return Err(crate::i18n::trf("{client} still has saved recovery values. Choose Retry to restore them, or Discard these saved values to remove their copies, before connecting it again.", &[("client", &name)]));
    }
    let fresh = loaded.as_ref().is_none_or(|record| {
        record.disconnected && record.disconnect_before.is_none() && !disconnecting
    });
    let mut record = match loaded.filter(|_| !fresh) {
        Some(record) => record,
        None => {
            let created_parents = missing_parents(path);
            let preexisting_gateways = original_gateways(client_id, format, path, before)?;
            Snapshot {
                version: 1,
                format,
                config_path: path.to_string_lossy().into_owned(),
                original: before.map(str::to_string),
                original_hash: before.map(crate::registry::sha256_hex),
                baseline: mutation::value(format, before)?,
                captured_at: epoch_millis(),
                toolport_version: env!("CARGO_PKG_VERSION").into(),
                last_written: None,
                last_written_hash: None,
                published_hash: None,
                created_parents,
                exact_eligible: preexisting_gateways.is_empty(),
                preexisting_gateways,
                disconnected: false,
                jsonc_settings,
                disconnect_before: None,
                vault_recovery: None,
                recovered_vault: false,
                recovery_dismissed: false,
            }
        }
    };
    if disconnecting && record.disconnected {
        if let Some(raw) = previous
            .as_deref()
            .filter(|raw| backup_storage::raw_markers(raw).is_ok_and(|refs| !refs.is_empty()))
        {
            let eligible = record.exact_eligible
                && before.map(crate::registry::sha256_hex).as_deref()
                    == record
                        .published_hash
                        .as_deref()
                        .or(record.last_written_hash.as_deref());
            let published = after.map(crate::registry::sha256_hex);
            backup_storage::remember_pending_revision(&file, raw, published.as_deref(), eligible)?;
            return Ok(Receipt {
                path: file,
                previous,
            });
        }
    }
    let released = record.disconnected && record.disconnect_before.is_none();
    if released && !disconnecting {
        record.created_parents = missing_parents(path);
    }
    if !fresh && before.map(crate::registry::sha256_hex) != record.last_written_hash {
        // A setting the client saved in Toolport's entry is not an edit to the file.
        if !only_client_settings_changed(client_id, format, record.last_written.as_deref(), before)
        {
            record.exact_eligible = false;
        }
        let previous_written = mutation::value(format, record.last_written.as_deref())?;
        let native = mutation::value(format, before)?;
        let mut paths = Vec::new();
        mutation::changes(
            Some(&previous_written),
            Some(&native),
            &mut Vec::new(),
            &mut paths,
        );
        for path in paths {
            // A setting the client saved in Toolport's own entry belongs to that entry
            // (a rewrite carries it), not to the file Disconnect puts back.
            if client_setting_in_gateway(client_id, format, &path) {
                continue;
            }
            rebase_value(&mut record.baseline, &native, &path);
        }
    }
    if disconnecting {
        record.baseline = mutation::value(format, after)?;
    }
    record.disconnect_before = if disconnecting && !released {
        before.map(str::to_string)
    } else {
        None
    };
    record.disconnected = disconnecting;
    record.last_written = after.map(str::to_string);
    record.last_written_hash = after.map(crate::registry::sha256_hex);
    record.published_hash = record.last_written_hash.clone();
    let file = record_path(client_id, path)?;
    secure_backup_dir(file.parent().ok_or("Could not resolve snapshot parent")?)?;
    crate::registry::atomic_write(
        &file,
        &backup_storage::encode_recovery(
            &file,
            &serde_json::to_string(&record).map_err(|e| e.to_string())?,
            disconnecting,
        )?,
    )?;
    Ok(Receipt {
        path: file,
        previous,
    })
}

fn rebase_value(baseline: &mut Value, native: &Value, path: &[String]) {
    let Some((key, rest)) = path.split_first() else {
        *baseline = native.clone();
        return;
    };
    if !baseline.is_object() {
        *baseline = serde_json::json!({});
    }
    let object = baseline.as_object_mut().unwrap();
    if rest.is_empty() {
        match native.get(key) {
            Some(value) => {
                object.insert(key.clone(), value.clone());
            }
            None => {
                object.remove(key);
            }
        }
    } else {
        rebase_value(
            object
                .entry(key.clone())
                .or_insert_with(|| serde_json::json!({})),
            &native[key],
            rest,
        );
    }
}

/// Whether a changed `path` is one of the [`client_entry_settings`] inside Toolport's
/// gateway entry, such as Pi's `enabled`, or a value nested in one, such as one tool
/// in Pi's `toolExposure`.
fn client_setting_in_gateway(client_id: &str, format: Format, path: &[String]) -> bool {
    matches!(path, [container, name, field, ..]
        if container == key(format)
            && name == GATEWAY_ENTRY_NAME
            && client_entry_settings(client_id).contains(&field.as_str()))
}

/// Whether `current` differs from Toolport's last write `written` only by the
/// [`client_entry_settings`] the client saved in Toolport's entry (and how it laid out
/// the file), so Disconnect can still put the original bytes back.
fn only_client_settings_changed(
    client_id: &str,
    format: Format,
    written: Option<&str>,
    current: Option<&str>,
) -> bool {
    if client_entry_settings(client_id).is_empty() || written.is_none() || current.is_none() {
        return false;
    }
    let (Ok(mut written), Ok(mut current)) = (
        mutation::value(format, written),
        mutation::value(format, current),
    ) else {
        return false;
    };
    let inner_key = nested_in(format, &written);
    take_client_settings(client_id, format, inner_key, &mut written);
    take_client_settings(client_id, format, inner_key, &mut current);
    written == current
}

/// Toolport's entry in a server map (`servers`, one level down for nested formats).
fn gateway_in<'a>(servers: Option<&'a Value>, inner_key: Option<&str>) -> Option<&'a Value> {
    let servers = match inner_key {
        Some(inner) => servers?.get(inner),
        None => servers,
    };
    servers?.get(GATEWAY_ENTRY_NAME)
}

/// Remove the [`client_entry_settings`] from Toolport's entry in `root` and return them.
fn take_client_settings(
    client_id: &str,
    format: Format,
    inner_key: Option<&str>,
    root: &mut Value,
) -> serde_json::Map<String, Value> {
    let mut taken = serde_json::Map::new();
    let mut servers = root.get_mut(key(format));
    if let Some(inner) = inner_key {
        servers = servers.and_then(|value| value.get_mut(inner));
    }
    if let Some(entry) = servers
        .and_then(|servers| servers.get_mut(GATEWAY_ENTRY_NAME))
        .and_then(Value::as_object_mut)
    {
        for setting in client_entry_settings(client_id) {
            if let Some(value) = entry.remove(*setting) {
                taken.insert(setting.to_string(), value);
            }
        }
    }
    taken
}

/// Undo only leaves whose value still matches Toolport's last write. Missing
/// moved entries come back; a re-added/edited entry wins over our original.
fn undo(
    before: Option<&Value>,
    written: Option<&Value>,
    current: Option<&Value>,
    depth: usize,
    entry_depth: usize,
) -> Option<Value> {
    if before == written {
        return current.cloned();
    }
    if current == written {
        return before.cloned();
    }
    if depth >= entry_depth {
        return current.cloned();
    }
    if let Some(Value::Object(current)) = current {
        if before.is_none_or(Value::is_object) && written.is_none_or(Value::is_object) {
            let mut result = current.clone();
            let empty = serde_json::Map::new();
            let before = before.and_then(Value::as_object).unwrap_or(&empty);
            let written = written.and_then(Value::as_object).unwrap_or(&empty);
            let keys: std::collections::BTreeSet<_> = before.keys().chain(written.keys()).collect();
            for key in keys {
                if depth + 1 == entry_depth
                    && !current.contains_key(key)
                    && current.keys().any(|name| name.eq_ignore_ascii_case(key))
                {
                    continue;
                }
                match undo(
                    before.get(key),
                    written.get(key),
                    current.get(key),
                    depth + 1,
                    entry_depth,
                ) {
                    Some(value) => {
                        result.insert(key.clone(), value);
                    }
                    None => {
                        result.remove(key);
                    }
                }
            }
            return Some(Value::Object(result));
        }
    }
    current.cloned()
}

/// The key the server map sits under inside the format's key, for nested formats.
fn nested(format: Format) -> Option<&'static str> {
    match format {
        Format::JsonZCodeMcp | Format::JsonOpenClawMcp => Some("servers"),
        Format::JsonNanobotMcp => Some("mcpServers"),
        _ => None,
    }
}

/// [`nested`] for one file: an OpenCode config already on the 2.x `mcp.servers` map
/// keeps its servers one level down.
fn nested_in(format: Format, root: &Value) -> Option<&'static str> {
    nested(format).or_else(|| {
        (matches!(format, Format::JsonOpenCodeMcp)
            && root["mcp"]
                .as_object()
                .is_some_and(opencode_uses_server_map))
        .then_some("servers")
    })
}

fn key(format: Format) -> &'static str {
    match format {
        Format::JsonServers => "servers",
        Format::JsonMcp
        | Format::JsonOpenCodeMcp
        | Format::JsonZCodeMcp
        | Format::JsonOpenClawMcp => "mcp",
        Format::JsonNanobotMcp => "tools",
        Format::JsonAmpMcpServers => "amp.mcpServers",
        Format::JsonContextServers => "context_servers",
        Format::TomlMcpServers | Format::YamlMcpServers => "mcp_servers",
        Format::YamlExtensions => "extensions",
        _ => "mcpServers",
    }
}

fn original_gateways(
    client_id: &str,
    format: Format,
    path: &Path,
    text: Option<&str>,
) -> Result<Vec<String>, String> {
    // An existing legacy move record proves the first captured gateway was
    // already installed by Toolport. Unowned customized gateway definitions
    // are part of the original and must round-trip just like other entries.
    let mut names =
        if client_id.contains('@') || moved::missing_names(client_id, format, path)?.is_empty() {
            Vec::new()
        } else {
            gateway_names(format, &mutation::value(format, text)?)
        };
    // Main adapters and automatic v1 profiles retain binary ownership. New
    // locations only adopt reserved names. Disconnect must not resurrect a gateway
    // from the initial snapshot after repair renamed or deduplicated it.
    let registry = crate::registry_controller::registry_for_disconnect()?;
    let servers = text
        .map(|text| parse_client_content(format, text))
        .transpose()?
        .unwrap_or_default();
    let main_adapter = find_def(client_id).is_some()
        && locations::legacy_gateway_ownership(client_id)
        && entry_state_for(
            gateway_entry_for_client(client_id, &servers),
            registry.client_managed_entries.get(client_id),
        ) == GatewayEntryState::Managed;
    if client_id.contains('@') || main_adapter {
        if let Some(text) = text {
            for server in parse_client_content(format, text)? {
                if (main_adapter || gateway_identity_matches(&server.name, &server.name, None))
                    && command_is_gateway_binary(server.command.as_deref().unwrap_or(""))
                    && !names.contains(&server.name)
                {
                    names.push(server.name);
                }
            }
        }
    }
    if let (Some(record), Some(text)) = (registry.client_managed_entries.get(client_id), text) {
        for server in parse_client_content(format, text)? {
            if (locations::legacy_gateway_ownership(client_id)
                || gateway_identity_matches(&server.name, &server.name, None))
                && detected_is_gateway(&server)
                && managed_matches_detected(&server, record)
                && !names.contains(&server.name)
            {
                names.push(server.name);
            }
        }
    }
    Ok(names)
}

fn gateway_names(format: Format, root: &Value) -> Vec<String> {
    let key = key(format);
    let mut servers = root.get(key);
    if let Some(inner) = nested_in(format, root) {
        servers = servers.and_then(|value| value.get(inner));
    }
    let owned = |entry: &Value| {
        let command = entry.get("command").or_else(|| entry.get("cmd"));
        command
            .and_then(|command| {
                command
                    .as_str()
                    .or_else(|| command.as_array()?.first()?.as_str())
                    .or_else(|| command.get("path")?.as_str())
            })
            .is_some_and(command_is_gateway_binary)
    };
    match servers {
        Some(Value::Object(map)) => map
            .iter()
            .filter(|(_, entry)| owned(entry))
            .map(|(name, _)| name.clone())
            .collect(),
        Some(Value::Array(list)) => list
            .iter()
            .filter(|entry| owned(entry))
            .filter_map(|entry| entry.get("name")?.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn named_list(root: &mut Value, key: &str) -> Result<(), String> {
    if let Some(list) = root.get_mut(key).and_then(Value::as_array_mut) {
        let map: serde_json::Map<String, Value> = list
            .iter()
            .filter_map(|entry| Some((entry.get("name")?.as_str()?.into(), entry.clone())))
            .collect();
        if map.len() != list.len() {
            return Err("Client config conflict: server list has missing or duplicate names; leaving it untouched".into());
        }
        root[key] = Value::Object(map);
    }
    Ok(())
}

pub(super) fn check_legacy_gateway(
    format: Format,
    path: &Path,
    managed: Option<&ManagedEntry>,
) -> Result<(), String> {
    if !mutation::exists(path) {
        return Ok(());
    }
    for server in parse_client_content(format, &read_config_file(path)?)? {
        if detected_is_gateway(&server)
            && !managed.map_or_else(
                || {
                    server
                        .command
                        .as_deref()
                        .is_some_and(command_is_gateway_binary)
                },
                |record| managed_matches_detected(&server, record),
            )
        {
            return Err(format!("Client config conflict at {}: customized legacy gateway has no original byte snapshot. Config unchanged.", path.display()));
        }
    }
    Ok(())
}

/// The whole entry v1.24.x wrote for `client_id`'s Shared HTTP connection, rebuilt from
/// the record v1 kept in the registry. The bearer v1 never stored is taken from
/// `current`, and only from the one place v1 put it. `None` for a record or client v1 did
/// not write this way: such an entry is never proved to be Toolport's.
fn v1_shared_http_entry(
    client_id: &str,
    format: Format,
    record: &ManagedEntry,
    current: &Value,
) -> Option<Value> {
    if !record.transport.eq_ignore_ascii_case("sharedHttp") {
        return None;
    }
    let url = record.url.as_deref()?;
    let bearer = |value: Option<&Value>, prefix: &str| {
        value
            .and_then(Value::as_str)
            .filter(|value| {
                value
                    .strip_prefix(prefix)
                    .is_some_and(|token| !token.trim().is_empty())
            })
            .map(str::to_owned)
    };
    if record.command == "npx" {
        // `npx -y mcp-remote URL --header "Authorization: Bearer T"`, nothing else, for
        // the clients v1 bridged (`client_uses_mcp_remote_bridge`).
        let bridged = matches!(
            format,
            Format::JsonMcpServers | Format::JsonCursorMcpServers | Format::TomlMcpServers
        ) && client_id != "devin-cli";
        if !bridged || !record.env.is_empty() || record.args != ["-y", "mcp-remote", url] {
            return None;
        }
        let auth = bearer(current.get("args")?.get(4), "Authorization: Bearer ")?;
        return Some(serde_json::json!({
            "command": "npx",
            "args": ["-y", "mcp-remote", url, "--header", auth],
        }));
    }
    if !record.command.is_empty() || !record.args.is_empty() {
        return None;
    }
    let mut headers: serde_json::Map<String, Value> = record
        .env
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let auth = bearer(current.get("headers")?.get("Authorization"), "Bearer ")?;
    headers.insert("Authorization".into(), Value::String(auth));
    Some(match format {
        // entry_to_json's native remote, for Devin CLI and VS Code's `servers`.
        Format::JsonMcpServers if client_id == "devin-cli" => {
            serde_json::json!({"url": url, "type": "http", "headers": headers})
        }
        Format::JsonServers => serde_json::json!({"url": url, "type": "http", "headers": headers}),
        Format::JsonCopilotMcpServers => {
            serde_json::json!({"url": url, "type": "http", "headers": headers, "tools": ["*"]})
        }
        Format::JsonOpenCodeMcp => {
            serde_json::json!({"enabled": true, "type": "remote", "url": url, "headers": headers})
        }
        _ => return None,
    })
}

/// Toolport's entry where v1.24.x put it, with its name: the format's server map, and
/// for OpenCode `mcp` itself (v1 never wrote `mcp.servers`), even in a file that now
/// also has OpenCode 2.x's `mcp.servers` map.
fn v1_slot(format: Format, root: &Value) -> Option<(String, Value)> {
    let mut servers = root.get(key(format))?;
    if let Some(inner) = nested(format) {
        servers = servers.get(inner)?;
    }
    let servers = servers.as_object()?;
    let name = owned_gateway_name(servers.keys().map(String::as_str))?;
    Some((name.to_owned(), servers.get(name)?.clone()))
}

fn v1_slot_mut(format: Format, root: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    let mut servers = root.get_mut(key(format))?;
    if let Some(inner) = nested(format) {
        servers = servers.get_mut(inner)?;
    }
    servers.as_object_mut()
}

/// What v1's Shared HTTP connection left in the registry: its ownership record and the
/// hash of its token from the `client:<id>` row. An unreadable registry, or no record
/// or row, is no proof.
fn v1_proof(client_id: &str) -> Option<(ManagedEntry, String)> {
    let registry = crate::registry_controller::registry_for_disconnect().ok()?;
    let record = registry.client_managed_entries.get(client_id)?.clone();
    let row = format!("client:{client_id}");
    let hash = registry
        .http_clients
        .iter()
        .find(|http| http.id == row)?
        .token_sha256
        .clone();
    Some((record, hash))
}

/// The token in the one place v1 put it.
fn v1_token(entry: &Value) -> Option<&str> {
    match entry.get("args") {
        Some(args) => args
            .get(4)?
            .as_str()?
            .strip_prefix("Authorization: Bearer "),
        None => entry
            .get("headers")?
            .get("Authorization")?
            .as_str()?
            .strip_prefix("Bearer "),
    }
}

/// Whether `entry` is, every field and the token too, the one v1.24.x wrote for this
/// client's Shared HTTP connection. Anything else is kept as the user's.
fn is_v1_entry(
    client_id: &str,
    format: Format,
    proof: &(ManagedEntry, String),
    entry: &Value,
) -> bool {
    v1_shared_http_entry(client_id, format, &proof.0, entry).as_ref() == Some(entry)
        && v1_token(entry).is_some_and(|token| crate::registry::sha256_hex(token) == proof.1)
}

/// Whether `text` holds v1's Shared HTTP entry. A file that can't be read proves nothing.
fn legacy_shared_http_entry(client_id: &str, format: Format, text: &str) -> bool {
    let Some((_, entry)) = mutation::value(format, Some(text))
        .ok()
        .and_then(|root| v1_slot(format, &root))
    else {
        return false;
    };
    v1_proof(client_id).is_some_and(|proof| is_v1_entry(client_id, format, &proof, &entry))
}

/// After a 2.x Connect, a v1 Shared HTTP Connect and then this upgrade, v1's entry is not
/// this record's last write, so undo would keep it as a hand edit. Two places hold it:
/// the current file, or the baseline, when a later 2.x write (Codex's launch repair)
/// rebased it there. Where it is proved to be v1's, the current entry is treated as the
/// last write and the baseline drops it, so undo removes it. Says whether it did.
fn adopt_legacy_shared_http(
    client_id: &str,
    format: Format,
    written: &Value,
    before: &mut Value,
    latest: &mut Value,
) -> bool {
    let written = |name: &str| {
        let mut servers = written.get(key(format))?;
        if let Some(inner) = nested(format) {
            servers = servers.get(inner)?;
        }
        servers.get(name).cloned()
    };
    let mut proof = None;
    let mut proved = |entry: &Value| {
        proof
            .get_or_insert_with(|| v1_proof(client_id))
            .as_ref()
            .is_some_and(|proof| is_v1_entry(client_id, format, proof, entry))
    };
    let mut adopted = false;
    if let Some((name, entry)) = v1_slot(format, latest) {
        let last = written(&name);
        if last.as_ref() != Some(&entry) && proved(&entry) {
            if let Some(servers) = v1_slot_mut(format, latest) {
                match last {
                    Some(last) => servers.insert(name, last),
                    None => servers.remove(&name),
                };
                adopted = true;
            }
        }
    }
    if let Some((name, entry)) = v1_slot(format, before) {
        if written(&name).as_ref() != Some(&entry) && proved(&entry) {
            if let Some(servers) = v1_slot_mut(format, before) {
                servers.remove(&name);
                adopted = true;
            }
        }
    }
    adopted
}

pub(super) fn released(client_id: &str, path: &Path) -> Result<bool, String> {
    Ok(load(client_id, path)?
        .is_some_and(|record| record.disconnected && record.disconnect_before.is_none()))
}

pub(super) fn needs_moved(client_id: &str, path: &Path) -> Result<bool, String> {
    Ok(load(client_id, path)?.is_none_or(|record| {
        !(record.disconnected && record.disconnect_before.is_none())
            && !record.preexisting_gateways.is_empty()
    }))
}

pub(super) fn toml_prefix_provenance(
    client_id: &str,
    path: &Path,
) -> Result<Option<(toml_edit::DocumentMut, toml_edit::DocumentMut)>, String> {
    let Some(record) = load(client_id, path)? else {
        return Ok(None);
    };
    if !matches!(record.format, Format::TomlMcpServers) {
        return Ok(None);
    }
    let written = if record.disconnected {
        record
            .disconnect_before
            .as_deref()
            .or(record.last_written.as_deref())
    } else {
        record.last_written.as_deref()
    };
    let original_text = record.original.as_deref().unwrap_or("");
    let original = original_text
        .parse()
        .map_err(|e: toml_edit::TomlError| diagnostics::offset("TOML", original_text, e.span()))?;
    let written_text = written.unwrap_or("");
    let written = written_text
        .parse()
        .map_err(|e: toml_edit::TomlError| diagnostics::offset("TOML", written_text, e.span()))?;
    Ok(Some((original, written)))
}

/// [`apply_adopting`] without the adoption flag, for tests.
#[cfg(test)]
pub(super) fn apply(client_id: &str, format: Format, path: &Path) -> Result<bool, String> {
    apply_inner(client_id, format, path, &mut false)
}

/// [`apply`], also saying whether it took a Shared HTTP entry v1 wrote as Toolport's, so
/// the caller puts back the servers v1's Move into gateway took out of the file.
pub(super) fn apply_adopting(
    client_id: &str,
    format: Format,
    path: &Path,
) -> Result<(bool, bool), String> {
    let mut adopted = false;
    let handled = apply_inner(client_id, format, path, &mut adopted)?;
    Ok((handled, adopted))
}

fn apply_inner(
    client_id: &str,
    format: Format,
    path: &Path,
    adopted: &mut bool,
) -> Result<bool, String> {
    let Some(record) = load(client_id, path)? else {
        return Ok(false);
    };
    mutation::disconnecting();
    let current = if mutation::exists(path) {
        Some(read_config_file(path)?)
    } else {
        None
    };
    // Toolport already took its entry out. An entry v1 wrote for Shared HTTP since then
    // is still Toolport's: the caller's plain Disconnect removes it.
    let legacy_since_disconnect = record.disconnected
        && current
            .as_deref()
            .is_some_and(|text| legacy_shared_http_entry(client_id, format, text));
    if record.disconnected
        && record.original.is_none()
        && record.vault_recovery.is_none()
        && record.disconnect_before.is_none()
    {
        return Ok(!legacy_since_disconnect);
    }
    if record.disconnected {
        let receipt = record_path(client_id, path)?;
        let raw = std::fs::read_to_string(&receipt)
            .map_err(|_| "Could not read the saved recovery file")?;
        if !backup_storage::raw_markers(&raw)?.is_empty() {
            // This config was already disconnected. Restore only the owed
            // placeholder leaves; do not run the server-entry undo merge again.
            let values = backup_storage::recovery_values(&receipt)?;
            if record.recovered_vault
                && record.exact_eligible
                && current
                    .as_deref()
                    .map(crate::registry::sha256_hex)
                    .as_deref()
                    == record
                        .published_hash
                        .as_deref()
                        .or(record.last_written_hash.as_deref())
            {
                if let Some(original) = record.original.as_deref() {
                    atomic_write(path, original)?;
                }
            } else if let Some(current) = current.as_deref() {
                let replacements = values
                    .iter()
                    .filter_map(|v| Some((v.marker.clone(), v.secret.clone()?)))
                    .collect::<Vec<_>>();
                let patched = recovery_values::patch(format, current, &replacements)?;
                if patched != current {
                    atomic_write(path, &patched)?;
                }
            }
            let after = mutation::read(path).flatten();
            recovery_values::remember_progress(
                format,
                current.as_deref(),
                after.as_deref(),
                &values,
                &backup_storage::resolved_markers(&raw),
            )?;
            return Ok(true);
        }
        if record.disconnect_before.is_none() {
            return Ok(!legacy_since_disconnect);
        }
    }
    if record.disconnected
        && !record.recovered_vault
        && current.as_deref().map(crate::registry::sha256_hex) == record.last_written_hash
    {
        return Ok(true);
    }
    let written_text = if record.disconnected {
        record.disconnect_before.as_deref()
    } else {
        record.last_written.as_deref()
    };
    if record.exact_eligible
        && (current.as_deref().map(crate::registry::sha256_hex) == record.last_written_hash
            || only_client_settings_changed(
                client_id,
                format,
                record.last_written.as_deref(),
                current.as_deref(),
            ))
    {
        match record.original {
            Some(original) => atomic_write(path, &original)?,
            None => mutation::remove(path)?,
        }
        return Ok(true);
    }
    // OpenHands Connect can move the entire root server map under mcpServers.
    // After native edits, undoing that conversion would hide restored servers
    // beside mcpServers. Keep the current layout and remove only our entry;
    // the unchanged-file path above still restores the original bytes.
    if matches!(format, Format::JsonOpenHandsMcpServers) {
        super::openhands::edit_gateway(path, None)?;
        moved::restore(client_id, format, path)?;
        return Ok(true);
    }
    let mut before = record.baseline.clone();
    // The layout Toolport last wrote decides where the server map sits.
    let shape = mutation::value(
        format,
        if record.disconnected {
            record.disconnect_before.as_deref()
        } else {
            record.last_written.as_deref()
        }
        .or(current.as_deref()),
    )?;
    let inner_key = nested_in(format, &shape);
    // A preview/1.x install can already have our gateway at first capture.
    // Its real pre-install bytes are unknown, so remove that owned entry and
    // restore the legacy moved record rather than resurrecting a dead gateway.
    let mut owned = Vec::new();
    let original = mutation::value(format, record.original.as_deref())?;
    for name in &record.preexisting_gateways {
        let key = key(format);
        let entry = |root: &Value| {
            let mut servers = root.get(key)?;
            if let Some(inner) = inner_key {
                servers = servers.get(inner)?;
            }
            servers.get(name).cloned()
        };
        if entry(&before) == entry(&original) && !owned.contains(name) {
            owned.push(name.clone());
        }
    }
    let key = key(format);
    if matches!(format, Format::YamlMcpServersList) {
        if let Some(list) = before.get_mut(key).and_then(Value::as_array_mut) {
            list.retain(|entry| {
                !entry
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| owned.iter().any(|owned| owned == name))
            });
        }
    } else {
        let mut map = before.get_mut(key);
        if let Some(inner) = inner_key {
            map = map.and_then(|value| value.get_mut(inner));
        }
        if let Some(map) = map.and_then(Value::as_object_mut) {
            for name in owned {
                map.remove(&name);
            }
        }
    }
    let mut written = mutation::value(format, written_text)?;
    let mut latest = mutation::value(format, current.as_deref())?;
    let list = matches!(format, Format::YamlMcpServersList);
    let order: Vec<String> = if list {
        latest[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["name"].as_str().map(str::to_string))
            .collect()
    } else {
        Vec::new()
    };
    if list {
        named_list(&mut before, key)?;
        named_list(&mut written, key)?;
        named_list(&mut latest, key)?;
    }
    *adopted |= adopt_legacy_shared_http(client_id, format, &written, &mut before, &mut latest);
    // Settings the client saved in Toolport's entry (such as Pi's `enabled`) are not
    // an edit to it: compare without them, so the entry leaves with them. An entry
    // kept for another reason keeps them too.
    take_client_settings(client_id, format, inner_key, &mut written);
    let settings = take_client_settings(client_id, format, inner_key, &mut latest);
    let mut restored = undo(
        before.get(key),
        written.get(key),
        latest.get(key),
        1,
        if inner_key.is_some() { 3 } else { 2 },
    );
    let kept = gateway_in(latest.get(key), inner_key).cloned();
    if let Some(entry) = restored
        .as_mut()
        .and_then(|servers| match inner_key {
            Some(inner) => servers.get_mut(inner),
            None => Some(servers),
        })
        .and_then(|servers| servers.get_mut(GATEWAY_ENTRY_NAME))
        .filter(|entry| kept.as_ref() == Some(&**entry))
        .and_then(Value::as_object_mut)
    {
        entry.extend(settings);
    }
    let object = latest
        .as_object_mut()
        .ok_or("Client config root must be an object")?;
    match restored {
        Some(value) => {
            object.insert(key.into(), value);
        }
        None => {
            object.remove(key);
        }
    }
    if list {
        if let Some(map) = latest.get_mut(key).and_then(Value::as_object_mut) {
            let mut entries = Vec::new();
            for name in order {
                if let Some(entry) = map.remove(&name) {
                    entries.push(entry);
                }
            }
            entries.extend(std::mem::take(map).into_values());
            latest[key] = Value::Array(entries);
        }
    }
    match format {
        Format::TomlMcpServers => {
            let mut doc = load_toml_document(path)?;
            let original = record
                .original
                .as_deref()
                .unwrap_or("")
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e: toml_edit::TomlError| {
                    diagnostics::offset("TOML", record.original.as_deref().unwrap_or(""), e.span())
                })?;
            let written_doc = written_text
                .unwrap_or("")
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e: toml_edit::TomlError| {
                    diagnostics::offset("TOML", written_text.unwrap_or(""), e.span())
                })?;
            let mut restore_entries = Vec::new();
            let mut recorded_order = std::collections::HashMap::new();
            let desired = latest.get(key).and_then(Value::as_object);
            let existing = doc
                .get(key)
                .and_then(|item| item.as_table_like())
                .map(|table| {
                    table
                        .iter()
                        .map(|(name, _)| name.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let removed: Vec<_> = existing
                .into_iter()
                .filter(|name| desired.is_none_or(|map| !map.contains_key(name)))
                .collect();
            if let Some(desired) = desired {
                for (name, value) in desired {
                    if mutation::value(format, current.as_deref())?[key].get(name) != Some(value) {
                        if let Some((entry_key, item, index)) =
                            moved::toml_entry(client_id, path, name)?
                        {
                            recorded_order.insert(entry_key.get().to_string(), index);
                            restore_entries.push((entry_key, item));
                        } else if mutation::value(format, record.original.as_deref())?[key]
                            .get(name)
                            == Some(value)
                        {
                            if let Some((entry_key, item)) =
                                toml_preservation::raw_entry(&original, name)
                            {
                                // A removed gateway may still occupy this exact
                                // alias slot from the recorded Connect rename.
                                let restored_slot =
                                    removed.iter().any(|removed| removed == GATEWAY_ENTRY_NAME)
                                        && toml_preservation::restore_renamed_slot(
                                            &original,
                                            &written_doc,
                                            &mut doc,
                                            name,
                                            &item,
                                        );
                                if !restored_slot {
                                    restore_entries.push((entry_key, item));
                                }
                            }
                        } else {
                            let value: toml::Value = serde_json::from_value(value.clone())
                                .map_err(|e| diagnostics::json(&e))?;
                            restore_entries
                                .push((toml_edit::Key::new(name), toml_value_to_item(&value)));
                        }
                    }
                }
            }
            restore_entries.sort_by_key(|(key, _)| {
                recorded_order.get(key.get()).copied().unwrap_or(usize::MAX)
            });
            let provenance = moved::toml_provenance(client_id, path, &restore_entries)?;
            toml_preservation::restore_raw_entries_with_provenance(
                &original,
                &written_doc,
                &mut doc,
                restore_entries,
                &provenance,
            );
            toml_preservation::remove(&mut doc, &removed);
            if latest.get(key).is_none() && !super::toml_keeps_servers_header(&doc) {
                doc.remove(key);
            }
            atomic_write(path, &doc.to_string())?;
        }
        Format::YamlDshPatch | Format::TomlReasonixPlugins | Format::JsonCodewhaleServers => {
            let kind = super::synced::Synced::of(format).expect("synced format");
            let desired = latest
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            // A server going back is written as it was: its moved entry, else
            // its entry in the original file.
            let mut raw: std::collections::BTreeMap<String, String> = record
                .original
                .as_deref()
                .map(|text| kind.raw_entries(text))
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|(name, text, _)| (name, text))
                .collect();
            raw.extend(moved::native_entries(client_id, path)?);
            let output = kind.sync(current.as_deref(), &desired, &raw)?;
            if current.as_deref() != Some(output.as_str()) {
                atomic_write(path, &output)?;
            }
        }
        Format::YamlExtensions | Format::YamlMcpServers | Format::YamlMcpServersList => {
            let root = serde_yaml::to_value(&latest).map_err(|e| e.to_string())?;
            if let (Some(current), Some(value)) = (current.as_deref(), root.get(key)) {
                let output = rewrite_yaml_key_preserving_seed(
                    current,
                    key,
                    value,
                    record.original.as_deref(),
                )?;
                parse_existing_yaml_content(&output)?;
                atomic_write(path, &output)?;
            } else {
                atomic_write_yaml_config(path, current.as_deref(), &root, key)?;
            }
        }
        _ => {
            let original_root = record.baseline.clone();
            let written_root = mutation::value(format, written_text)?;
            let keys: std::collections::BTreeSet<_> = original_root
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, _)| key)
                .chain(
                    written_root
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(key, _)| key),
                )
                .collect();
            let mut output = current.clone().unwrap_or_else(|| "{}".into());
            for changed_key in keys {
                if changed_key == key {
                    continue;
                }
                if original_root.get(changed_key) != written_root.get(changed_key) {
                    let restored = undo(
                        original_root.get(changed_key),
                        written_root.get(changed_key),
                        latest.get(changed_key),
                        2,
                        2,
                    );
                    let object = latest
                        .as_object_mut()
                        .ok_or("Client config must be an object")?;
                    match restored {
                        Some(value) => {
                            object.insert(changed_key.clone(), value);
                        }
                        None => {
                            object.remove(changed_key);
                        }
                    }
                    output = render_settings_key(Some(&output), &latest, changed_key)?;
                }
            }
            output = render_settings_key(Some(&output), &latest, key)?;
            atomic_write(path, &output)?;
        }
    }
    Ok(true)
}

pub(super) fn check_finished(
    client_id: &str,
    path: &Path,
    expected: Option<&str>,
) -> Result<(), String> {
    if load(client_id, path)?.is_some_and(|record| {
        record
            .published_hash
            .as_deref()
            .or(record.last_written_hash.as_deref())
            != expected
            || !record.disconnected
    }) {
        return Err("Client was changed by another Toolport operation before disconnect finished; recovery retained".into());
    }
    Ok(())
}

pub(super) fn run<T>(
    client_id: &str,
    path: &Path,
    format: Format,
    edit: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    // Every restore caller shares one bounded, noninteractive store session.
    crate::secrets::with_backup_batch(|| {
        if load(client_id, path)?.is_some_and(|record| record.jsonc_settings) {
            mutation::settings(client_id, path, edit)
        } else {
            mutation::run(client_id, path, format, edit)
        }
    })
}

pub(super) fn finish(client_id: &str, path: &Path, expected: Option<&str>) -> Result<(), String> {
    crate::secrets::with_backup_batch(|| finish_batched(client_id, path, expected))
}
fn finish_batched(client_id: &str, path: &Path, expected: Option<&str>) -> Result<(), String> {
    check_finished(client_id, path, expected)?;
    if let Some(mut record) = load(client_id, path)? {
        for parent in &record.created_parents {
            match std::fs::remove_dir(parent) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => {
                    return Err(format!(
                        "Config restored, but could not remove empty parent {}: {error}",
                        parent.display()
                    ))
                }
            }
        }
        // Completion is about the committed native file, not vault availability.
        // Recheck every owed marker after the optimistic write and before any
        // payload/receipt deletion. Removed or user-replaced leaves are accounted.
        let receipt = record_path(client_id, path)?;
        let has_values = std::fs::read_to_string(&receipt)
            .ok()
            .map(|raw| backup_storage::raw_markers(&raw))
            .transpose()?
            .is_some_and(|values| !values.is_empty());
        if has_values {
            let current = match std::fs::read_to_string(path) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    return Err(
                        "Could not verify the restored client config; recovery retained".into(),
                    )
                }
            };
            if current
                .as_deref()
                .map(crate::registry::sha256_hex)
                .as_deref()
                != expected
            {
                return Err(
                    "Client config changed before recovery was verified; recovery retained".into(),
                );
            }
            let values = backup_storage::recovery_values(&receipt)?;
            let mut pending = Vec::new();
            let mut resolved = Vec::new();
            for value in values {
                if current
                    .as_deref()
                    .map(|text| recovery_values::present(record.format, text, &value.marker))
                    .transpose()?
                    .unwrap_or(false)
                {
                    pending.push(backup_storage::RecoveryIssue {
                        reference: value.reference,
                        marker: value.marker,
                        state: value.state,
                    });
                } else if !resolved.contains(&value.marker) {
                    resolved.push(value.marker);
                }
            }
            if !pending.is_empty() {
                backup_storage::set_issues(&receipt, &pending, &resolved)?;
                return Ok(());
            }
        }
        // Keep only an ownership tombstone; release original config payloads
        // after the committed revision was checked against the actual file.
        record.original = None;
        record.vault_recovery = None;
        record.original_hash = None;
        record.baseline = Value::Null;
        record.last_written = None;
        record.last_written_hash = None;
        record.published_hash = None;
        record.disconnected = true;
        record.disconnect_before = None;
        let copies = recovery_copies(client_id, path)?;
        // Release every owned payload before unlinking even one envelope. If a
        // later release fails, the complete inventory survives for an idempotent retry.
        for copy in &copies {
            if let Err(error) = backup_storage::release(copy) {
                if let Some(state) = recovery::state_for_instructions(&error) {
                    backup_storage::set_cleanup_state(&record_path(client_id, path)?, state)?;
                }
                return Err(error);
            }
        }
        crate::registry::atomic_write(
            &released_path(client_id, path)?,
            &serde_json::to_string(&record).map_err(|e| e.to_string())?,
        )?;
        for copy in copies {
            match std::fs::remove_file(copy) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(())
}

fn backup_names(path: &Path) -> [String; 3] {
    [
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config")
            .to_string(),
        claude_settings_backup_name(path),
        secondary_claude_backup_name(path),
    ]
}
/// Backup names another recorded config in this folder can also produce. A
/// plain (unvaulted) timestamped copy has no source binding, so it belongs to
/// this config only when no other recorded config shares its name. An
/// unreadable receipt could belong to any config, so it makes every name shared.
fn shared_backup_names(dir: &Path, path: &Path, own: &[PathBuf]) -> Vec<String> {
    let names = backup_names(path);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return names.to_vec();
    };
    let mut shared = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return names.to_vec();
        };
        let file = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if own.contains(&file)
            || !(name.starts_with("original-") || name.starts_with("released-"))
            || !name.ends_with(".json")
        {
            continue;
        }
        let other = std::fs::symlink_metadata(&file)
            .ok()
            .filter(|meta| meta.is_file() && !crate::purge::is_link(meta))
            .and_then(|_| std::fs::read_to_string(&file).ok())
            .and_then(|raw| backup_storage::raw_metadata(&raw).ok())
            .and_then(|(record, _)| record["configPath"].as_str().map(PathBuf::from));
        let Some(other) = other else {
            return names.to_vec();
        };
        if other == path {
            continue;
        }
        let theirs = backup_names(&other);
        shared.extend(names.iter().filter(|n| theirs.contains(n)).cloned());
    }
    shared
}

/// Keep only path provenance when explicitly discarding a disconnected record.
/// Without it, another same-named location could claim an ambiguous legacy copy
/// after this receipt disappears. This tombstone contains no config or values.
pub(super) fn remember_discarded_path(
    client_id: &str,
    path: &Path,
    format: Format,
) -> Result<(), String> {
    let record = Snapshot {
        version: 1,
        format,
        config_path: path.to_string_lossy().into_owned(),
        original: None,
        original_hash: None,
        baseline: Value::Null,
        captured_at: 0,
        toolport_version: env!("CARGO_PKG_VERSION").into(),
        last_written: None,
        last_written_hash: None,
        published_hash: None,
        created_parents: Vec::new(),
        exact_eligible: false,
        preexisting_gateways: Vec::new(),
        disconnected: true,
        jsonc_settings: false,
        disconnect_before: None,
        vault_recovery: None,
        recovered_vault: false,
        recovery_dismissed: false,
    };
    crate::registry::atomic_write(
        &released_path(client_id, path)?,
        &serde_json::to_string(&record).map_err(|e| diagnostics::json(&e))?,
    )
}

pub(super) fn recovery_copies(client_id: &str, path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut copies = Vec::new();
    let original = record_path(client_id, path)?;
    let names = backup_names(path);
    if let Some(dir) = backup_dir(client_id) {
        let shared = shared_backup_names(
            &dir,
            path,
            &[original.clone(), released_path(client_id, path)?],
        );
        for entry in
            std::fs::read_dir(dir).map_err(|_| "Could not inventory saved recovery copies")?
        {
            let entry = entry.map_err(|_| "Could not inventory a saved recovery copy")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path() == original
                || name.split_once('-').is_some_and(|(stamp, suffix)| {
                    stamp.parse::<u128>().is_ok() && names.iter().any(|n| n == suffix)
                })
            {
                let meta = std::fs::symlink_metadata(entry.path())
                    .map_err(|_| "Could not inspect a saved recovery copy")?;
                if !meta.is_file() || crate::purge::is_link(&meta) {
                    return Err(
                        "Saved recovery copy is not a regular file; recovery retained".into(),
                    );
                }
                if entry.path() != original {
                    match backup_storage::source_path(&entry.path())? {
                        Some(source) if source != path.to_string_lossy() => continue,
                        Some(_) => {}
                        // Unbound copies are claimed only when unambiguous;
                        // never delete another config's history by basename.
                        None if name
                            .split_once('-')
                            .is_some_and(|(_, suffix)| shared.iter().any(|n| n == suffix)) =>
                        {
                            continue
                        }
                        None => {}
                    }
                }
                copies.push(entry.path());
            }
        }
    }
    if let Some(moved) = moved::recovery_copy(client_id, path)? {
        if !copies.contains(&moved) {
            copies.push(moved);
        }
    }
    copies.sort();
    Ok(copies)
}

pub(crate) fn after_capture_conflict(
    file: &Path,
    target: &Path,
    expected: Option<&str>,
) -> Result<(), String> {
    let dir = crate::registry::conduit_dir().ok_or("Could not resolve mutation lock dir")?;
    let _lock = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    refuse_started(file)?;
    let mut record: Snapshot =
        serde_json::from_str(&backup_storage::read(file).map_err(|e| e.to_string())?)
            .map_err(|e| diagnostics::at(file, diagnostics::json(&e)))?;
    if record.config_path != target.to_string_lossy() {
        return Err("Client receipt recovery path mismatch".into());
    }
    // A newer Toolport generation owns its own eligibility decision.
    if record
        .published_hash
        .as_deref()
        .or(record.last_written_hash.as_deref())
        == expected
    {
        record.exact_eligible = false;
        crate::registry::atomic_write(
            file,
            &backup_storage::encode(
                file,
                &serde_json::to_string(&record).map_err(|e| e.to_string())?,
            )?,
        )?;
    }
    Ok(())
}

pub(crate) fn after_rollback(
    file: &Path,
    target: &Path,
    written: Option<&str>,
) -> Result<(), String> {
    refuse_started(file)?;
    let mut record: Snapshot =
        serde_json::from_str(&backup_storage::read(file).map_err(|e| e.to_string())?)
            .map_err(|e| diagnostics::at(file, diagnostics::json(&e)))?;
    if record.config_path != target.to_string_lossy() {
        return Err("Client rollback recovery path mismatch".into());
    }
    if written.map(crate::registry::sha256_hex).as_deref()
        != record
            .published_hash
            .as_deref()
            .or(record.last_written_hash.as_deref())
    {
        record.disconnected = false;
        record.disconnect_before = None;
    }
    record.last_written = written.map(str::to_string);
    record.last_written_hash = written.map(crate::registry::sha256_hex);
    crate::registry::atomic_write(
        file,
        &backup_storage::encode(
            file,
            &serde_json::to_string(&record).map_err(|e| e.to_string())?,
        )?,
    )
}

pub(super) fn recorded_paths() -> Vec<(String, PathBuf, Result<Format, String>)> {
    let mut paths = Vec::new();
    for def in machine_defs(false) {
        let Some(dir) = backup_dir(def.client_id()) else {
            continue;
        };
        let files = match std::fs::read_dir(&dir) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                paths.push((def.client_id().into(), dir, Err(e.to_string())));
                continue;
            }
        };
        for file in files {
            let file = match file {
                Ok(file) => file.path(),
                Err(e) => {
                    paths.push((def.client_id().into(), dir.clone(), Err(e.to_string())));
                    continue;
                }
            };
            if !file
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("original-"))
                || file.extension().is_none_or(|ext| ext != "json")
            {
                continue;
            }
            let parsed = backup_storage::read(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    serde_json::from_str::<Snapshot>(&text)
                        .map_err(|_| "Client recovery record is invalid".to_string())
                });
            match parsed {
                // Listing stays read-only and complete: a started Discard is
                // still a recorded path, so `disconnect --all` reports the
                // shared refusal for that config instead of a receipt error.
                Ok(record)
                    if std::fs::read_to_string(&file)
                        .is_ok_and(|raw| backup_storage::discard_phase(&raw).is_some()) =>
                {
                    paths.push((
                        def.client_id().into(),
                        PathBuf::from(record.config_path),
                        Ok(record.format),
                    ))
                }
                Ok(record) => {
                    let path = PathBuf::from(record.config_path);
                    match released(def.client_id(), &path) {
                        Ok(true) => {}
                        Ok(false) => paths.push((def.client_id().into(), path, Ok(record.format))),
                        Err(error) => paths.push((def.client_id().into(), file, Err(error))),
                    }
                }
                Err(error) => paths.push((def.client_id().into(), file, Err(error))),
            }
        }
    }
    paths.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> ServerEntry {
        serde_json::from_value(serde_json::json!({"id":"toolport","name":"toolport","transport":"stdio","command":"/fixture/toolport-gateway"})).unwrap()
    }
    fn fixtures() -> Vec<(Format, &'static str)> {
        vec![
            (Format::JsonMcpServers, r#"{ "setting" : 7, "mcpServers" : {"native":{"command":"native","args":[]},"toolport":{"command":"custom","args":["--custom"]}} }"#),
            (Format::JsonCopilotMcpServers, r#"{"mcpServers":{"native":{"command":"native","tools":["*"]}}}"#),
            (Format::JsonDroidMcpServers, r#"{"mcpServers":{"native":{"command":"native","type":"stdio"}}}"#),
            (Format::JsonQwenMcpServers, r#"{"mcpServers":{"native":{"httpUrl":"https://example.test","headers":{"Authorization":"fixture"}}}}"#),
            (Format::JsonKimiMcpServers, r#"{"mcpServers":{"native":{"command":"native"}}}"#),
            (Format::JsonAmpMcpServers, r#"{"amp.mcpServers":{"native":{"command":"native"}},"theme":"dark"}"#),
            (Format::JsonZCodeMcp, r#"{"mcp":{"servers":{"native":{"command":"native"}}},"theme":"dark"}"#),
            (Format::JsonOpenClawMcp, "{\n  // gateway\n  gateway: { port: 18789 },\n  mcp: { servers: { native: { command: 'native', args: [] } } },\n}\n"),
            (Format::JsonNanobotMcp, "{\n  \"agents\": {\"defaults\": {\"model\": \"m\"}},\n  \"tools\": {\n    \"web\": {\"enabled\": true},\n    \"mcpServers\": {\"native\": {\"command\": \"native\", \"args\": []}}\n  }\n}\n"),
            (Format::JsonOpenHandsMcpServers, "{\n  \"mcpServers\": {\n    \"native\": {\"command\": \"native\", \"args\": [], \"enabled\": true}\n  }\n}\n"),
            (Format::JsonServers, "{ // user's comment\r\n \"servers\": {\"native\":{\"command\":\"native\",},},\r\n \"theme\": \"dark\",\r\n}"),
            (Format::JsonMcp, r#"{"mcp":{"native":{"command":"native","type":"stdio"}},"theme":"dark"}"#),
            (Format::JsonOpenCodeMcp, "{ // comment\n \"mcp\": {\"native\":{\"type\":\"local\",\"command\":[\"native\"],\"enabled\":true,},},\n}"),
            (Format::JsonContextServers, "{ // comment\n \"context_servers\": {\"native\":{\"command\":{\"path\":\"native\",\"args\":[]},},},\n}"),
            (Format::TomlMcpServers, "# annotation\r\ntheme = 'dark'\r\n[mcp_servers.native]\r\ncommand = 'native' # keep\r\nargs = [ ]"),
            (Format::YamlExtensions, "# annotation\r\ntheme: dark\r\nextensions:\r\n  native:\r\n    enabled: true\r\n    type: stdio\r\n    cmd: native\r\n    args: []"),
            (Format::YamlMcpServers, "# annotation\r\ntheme: dark\r\nmcp_servers:\r\n  native:\r\n    command: native\r\n    args: []"),
            (Format::YamlMcpServersList, "# annotation\r\nname: Example\r\nversion: 1.0.0\r\nschema: v1\r\nmcpServers:\r\n  - name: native\r\n    command: native\r\n    args: []"),
            (Format::JsonCodewhaleServers, "{\r\n  \"timeouts\": {\"connect_timeout\": 30},\r\n  \"mcpServers\": {\"native\": {\"command\": \"native\", \"args\": []}}\r\n}"),
            (Format::TomlReasonixPlugins, "# annotation\r\ndefault_model = 'deepseek-flash'\r\n[[plugins]]\r\nname = 'native'\r\ncommand = 'native' # keep\r\nargs = [ ]"),
            (Format::YamlDshPatch, "# annotation\r\n- id: llm-pi-ai\r\n  config: {theme: dark}\r\n- insert:\r\n    - id: mcp-native\r\n      name: '@deepseek-ai/dsh-mcp-client'\r\n      config:\r\n        serverName: native\r\n        transport: stdio\r\n        command: native # keep\r\n        args: []"),
        ]
    }
    fn disconnect(id: &str, path: &Path, format: Format) -> Result<(), String> {
        mutation::run(id, path, format, || {
            assert!(apply(id, format, path)?);
            if needs_moved(id, path)? {
                moved::restore(id, format, path)?;
            }
            Ok(())
        })?;
        let revision = if path.exists() {
            Some(crate::registry::sha256_hex(&read_config_file(path)?))
        } else {
            None
        };
        finish(id, path, revision.as_deref())
    }
    #[test]
    fn plain_backups_of_same_named_configs_are_cleaned_only_by_their_owner() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let a = data.join("a").join("mcp.json");
            let b = data.join("b").join("mcp.json");
            let original = "{ \"mcpServers\": {\"native\":{\"command\":\"native\"}} }\r\n";
            for path in [&a, &b] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, original).unwrap();
                mutation::run("cursor", path, Format::JsonMcpServers, || {
                    backup_file("cursor", path)?;
                    edit_format(Format::JsonMcpServers, path, Some(&entry()), true)
                })
                .unwrap();
            }
            let dir = backup_dir("cursor").unwrap();
            // A legacy unbound copy stays ambiguous and belongs to neither.
            let legacy = dir.join("5-mcp.json");
            std::fs::write(&legacy, original).unwrap();
            let timestamped = |path: &Path| {
                recovery_copies("cursor", path)
                    .unwrap()
                    .into_iter()
                    .filter(|f| {
                        f.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .split_once('-')
                            .is_some_and(|(stamp, _)| stamp.parse::<u128>().is_ok())
                    })
                    .collect::<Vec<_>>()
            };
            let (a_copies, b_copies) = (timestamped(&a), timestamped(&b));
            assert!(!a_copies.is_empty() && !b_copies.is_empty());
            assert!(a_copies.iter().all(|f| !b_copies.contains(f)));
            assert!(!a_copies.contains(&legacy) && !b_copies.contains(&legacy));
            for (copies, path) in [(&a_copies, &a), (&b_copies, &b)] {
                for file in copies {
                    assert_eq!(
                        backup_storage::source_path(file).unwrap().as_deref(),
                        Some(path.to_string_lossy().as_ref()),
                        "every new timestamped copy is bound to its config"
                    );
                    assert_eq!(read_config_file(file).unwrap(), original);
                }
            }
            disconnect("cursor", &a, Format::JsonMcpServers).unwrap();
            assert_eq!(std::fs::read_to_string(&a).unwrap(), original);
            for file in &a_copies {
                assert!(!file.exists(), "owner cleanup removes {}", file.display());
            }
            for file in &b_copies {
                assert!(file.exists(), "another config kept {}", file.display());
            }
            assert!(legacy.exists());
            disconnect("cursor", &b, Format::JsonMcpServers).unwrap();
            assert_eq!(std::fs::read_to_string(&b).unwrap(), original);
            for file in &b_copies {
                assert!(!file.exists());
            }
            assert!(legacy.exists(), "ambiguous legacy history is never claimed");
        });
    }
    #[test]
    fn legacy_plaintext_upgrade_seals_backups_without_reimport_and_keeps_locked_recovery() {
        for available in [true, false] {
            crate::secrets::tests::with_isolated_vault(|| {
                backup_storage::reset_missing();
                let data = crate::registry::conduit_dir().unwrap();
                let path = data.join("mcp.json");
                let original = "{\r\n \"mcpServers\":{\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-fake-legacy-secret\"}}}\r\n}\r\n";
                std::fs::write(&path, original).unwrap();
                crate::secrets::set_secret("old-import", "API_KEY", "sk-fake-legacy-secret")
                    .unwrap();
                mutation::run("cursor", &path, Format::JsonMcpServers, || {
                    backup_file("cursor", &path)?;
                    moved::record("cursor", Format::JsonMcpServers, &path)?;
                    write_format(Format::JsonMcpServers, &path, &[entry()], true)
                })
                .unwrap();
                let dir = backup_dir("cursor").unwrap();
                let before: Vec<_> = std::fs::read_dir(&dir)
                    .unwrap()
                    .flatten()
                    .map(|file| (file.path(), std::fs::read(file.path()).unwrap()))
                    .collect();
                assert!(
                    before
                        .iter()
                        .any(|(_, raw)| String::from_utf8_lossy(raw)
                            .contains("sk-fake-legacy-secret"))
                );
                if !available {
                    std::env::set_var("TOOLPORT_SECRET_KEY", "wrong-original-vault-key");
                }
                let upgraded = backup_storage::upgrade_legacy();
                if available {
                    assert!(upgraded.unwrap() >= 3);
                    assert_eq!(backup_storage::upgrade_legacy().unwrap(), 0);
                    for file in std::fs::read_dir(&dir).unwrap().flatten() {
                        assert!(!std::fs::read_to_string(file.path())
                            .unwrap()
                            .contains("sk-fake-legacy-secret"));
                    }
                } else {
                    assert!(upgraded.unwrap_err().contains("Original retained"));
                    for (path, raw) in before {
                        assert_eq!(std::fs::read(path).unwrap(), raw);
                    }
                }
                disconnect("cursor", &path, Format::JsonMcpServers).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
            });
        }
    }
    #[test]
    fn vaulted_backup_restores_exact_bytes_and_recovers_without_vault() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            for available in [true, false] {
                backup_storage::reset_missing();
                let id = if available {
                    "vault-present"
                } else {
                    "vault-missing"
                };
                let path = data.join(format!("{id}-config.json"));
                let original = "{\r\n  \"mcpServers\" : {\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-fake-hc-install-secret\"}}}, \"theme\": \"dark\"\r\n}\r\n";
                std::fs::write(&path, original).unwrap();
                crate::secrets::set_secret(id, "API_KEY", "sk-fake-hc-install-secret").unwrap();
                let registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":id,"name":"Native","transport":"stdio","env":[{"key":"API_KEY","secret":true}]}]})).unwrap();
                crate::registry::save(&registry).unwrap();
                backup_storage::register(id, "API_KEY", "sk-fake-hc-install-secret");
                mutation::run(id, &path, Format::JsonMcpServers, || {
                    backup_file(id, &path)?;
                    moved::record(id, Format::JsonMcpServers, &path)?;
                    write_format(Format::JsonMcpServers, &path, &[entry()], true)
                })
                .unwrap();
                backup_storage::clear();
                let dir = backup_dir(id).unwrap();
                for file in std::fs::read_dir(&dir).unwrap().flatten() {
                    assert!(
                        !std::fs::read_to_string(file.path())
                            .unwrap()
                            .contains("sk-fake-hc-install-secret"),
                        "{}",
                        file.path().display()
                    );
                }
                if !available {
                    // A different key simulates a locked/lost original vault.
                    std::env::set_var("TOOLPORT_SECRET_KEY", "unavailable-original-vault");
                }
                disconnect(id, &path, Format::JsonMcpServers).unwrap();
                let restored = std::fs::read_to_string(&path).unwrap();
                if available {
                    assert_eq!(restored.as_bytes(), original.as_bytes());
                    for file in std::fs::read_dir(&dir).unwrap().flatten() {
                        assert!(!std::fs::read_to_string(file.path())
                            .unwrap()
                            .contains("sk-fake-hc-install-secret"));
                    }
                } else {
                    assert_eq!(
                        restored,
                        original
                            .replace("sk-fake-hc-install-secret", "TOOLPORT_MISSING_CREDENTIAL_0")
                    );
                    assert!(backup_storage::missing());
                    assert!(
                        std::fs::read_dir(&dir).unwrap().count() >= 2,
                        "recovery retained"
                    );
                    std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
                    backup_storage::reset_missing();
                    disconnect(id, &path, Format::JsonMcpServers).unwrap();
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        original.as_bytes(),
                        "unlock and retry restores every original byte"
                    );
                }
            }
        });
    }
    #[test]
    fn pending_notices_are_vault_free_path_specific_and_keep_dismissal() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_KEY","secret":true}]}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            crate::secrets::set_secret("native", "API_KEY", "sk-recovery-notice-fixture").unwrap();
            let original = "{\r\n  \"mcpServers\":{\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-recovery-notice-fixture\"}}}\r\n}\r\n";
            let paths = [
                data.join("historical-a.json"),
                data.join("historical-b.json"),
            ];
            for path in &paths {
                std::fs::write(path, original).unwrap();
                backup_storage::register("native", "API_KEY", "sk-recovery-notice-fixture");
                mutation::run("cursor", path, Format::JsonMcpServers, || {
                    backup_file("cursor", path)?;
                    write_format(Format::JsonMcpServers, path, &[entry()], true)
                })
                .unwrap();
                backup_storage::clear();
            }
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-recovery-notices");
            for path in &paths {
                disconnect("cursor", path, Format::JsonMcpServers).unwrap();
            }
            let cli = recovery::warnings("cursor", &paths[1]);
            backup_storage::reset_missing();
            assert_eq!(
                backup_storage::upgrade_legacy().unwrap(),
                0,
                "pending placeholders are not legacy secrets"
            );
            let pending = recovery::list().unwrap();
            assert_eq!(pending.len(), 2);
            let notice = pending
                .iter()
                .find(|row| row.path == paths[1].to_string_lossy())
                .unwrap();
            assert_eq!(
                cli,
                vec![format!(
                    "{}\n\n{}\n\n{}\n\n{}",
                    notice.message,
                    notice.instructions,
                    notice.retention,
                    crate::i18n::tr(
                        "To put the saved values back from a terminal:\ntoolport-gateway --disconnect-all"
                    )
                )],
                "CLI and both shell DTOs share exact prose and retention"
            );
            assert!(
                !backup_storage::missing(),
                "listing must not decode the vault"
            );
            assert!(backup_storage::warnings().is_empty());
            assert!(pending[0].message.contains("Cursor"));
            assert!(pending[0].message.contains("API_KEY for native"));
            assert!(!pending[0].message.contains("TOOLPORT_MISSING"));
            let first = &pending[0];
            let config_before = std::fs::read(&first.path).unwrap();
            let receipt_before: Value =
                serde_json::from_slice(&std::fs::read(&first.backup_path).unwrap()).unwrap();
            recovery::dismiss(&first.id, true).unwrap();
            assert_eq!(std::fs::read(&first.path).unwrap(), config_before);
            let mut receipt_after: Value =
                serde_json::from_slice(&std::fs::read(&first.backup_path).unwrap()).unwrap();
            receipt_after
                .as_object_mut()
                .unwrap()
                .remove("recoveryDismissed");
            let mut receipt_before = receipt_before;
            receipt_before
                .as_object_mut()
                .unwrap()
                .remove("recoveryDismissed");
            assert_eq!(
                receipt_before, receipt_after,
                "dismiss only changes notice metadata"
            );
            assert!(
                recovery::list()
                    .unwrap()
                    .iter()
                    .find(|r| r.id == first.id)
                    .unwrap()
                    .dismissed
            );
            assert!(
                !backup_storage::missing(),
                "dismiss/show must not decode the vault"
            );
            let still_locked = recovery::retry(&first.id).unwrap().unwrap();
            assert!(
                still_locked.dismissed,
                "retry must retain knowing dismissal"
            );
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            assert!(
                recovery::list()
                    .unwrap()
                    .iter()
                    .find(|r| r.id == first.id)
                    .unwrap()
                    .dismissed,
                "unlock/reopen does not consume recovery or reset dismissal"
            );
            recovery::dismiss(&first.id, false).unwrap();
            let second_before = std::fs::read(&pending[1].path).unwrap();
            assert!(recovery::retry(&first.id).unwrap().is_none());
            assert_eq!(std::fs::read(&first.path).unwrap(), original.as_bytes());
            assert_eq!(
                std::fs::read(&pending[1].path).unwrap(),
                second_before,
                "retry must not touch another historical config"
            );
            assert_eq!(recovery::list().unwrap().len(), 1);
            // Native edits made while recovery is pending must survive retry.
            let edited = std::fs::read_to_string(&pending[1].path).unwrap().replacen(
                "{",
                "{\n  \"userSetting\": 42,",
                1,
            );
            std::fs::write(&pending[1].path, &edited).unwrap();
            recovery::retry(&pending[1].id).unwrap();
            let restored = std::fs::read_to_string(&pending[1].path).unwrap();
            assert!(restored.contains("\"userSetting\": 42"));
            assert!(
                restored.contains("sk-recovery-notice-fixture"),
                "edited config must recover its owed credential"
            );
            assert!(!restored.contains("TOOLPORT_MISSING_CREDENTIAL_"));
            assert!(
                !Path::new(&pending[1].backup_path).exists(),
                "only verified complete recovery releases the receipt"
            );
        });
    }

    #[test]
    fn eleven_edited_credentials_recover_exact_values_and_retain_partial_ten() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let env = (0..11)
                .map(|i| {
                    (
                        format!("API_{i:02}"),
                        Value::String(format!("sk-eleven-original-value-{i:02}")),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            let registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":env.keys().map(|key|serde_json::json!({"key":key,"secret":true})).collect::<Vec<_>>()}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            for partial in [false, true] {
                let path = data.join(format!("eleven-{partial}.json"));
                let original=serde_json::json!({"mcpServers":{"native":{"command":"echo","env":env}},"keepSetting":41}).to_string();
                std::fs::write(&path, &original).unwrap();
                for (key, value) in &env {
                    let value = value.as_str().unwrap();
                    crate::secrets::set_secret("native", key, value).unwrap();
                    backup_storage::register("native", key, value);
                }
                mutation::run("cursor", &path, Format::JsonMcpServers, || {
                    backup_file("cursor", &path)?;
                    write_format(Format::JsonMcpServers, &path, &[entry()], true)
                })
                .unwrap();
                backup_storage::clear();
                std::env::set_var("TOOLPORT_SECRET_KEY", "eleven-locked");
                disconnect("cursor", &path, Format::JsonMcpServers).unwrap();
                let row = recovery::list()
                    .unwrap()
                    .into_iter()
                    .find(|r| r.path == path.to_string_lossy())
                    .unwrap();
                let current = std::fs::read_to_string(&path).unwrap().replace("41", "42");
                std::fs::write(&path, current).unwrap();
                std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
                let (account, key, _) =
                    backup_storage::test_recovery_key(Path::new(&row.backup_path));
                if partial {
                    crate::secrets::delete_backup_secret(&account, &key).unwrap();
                    crate::secrets::delete_backup_secret(&account, &format!("{key}-credential-10"))
                        .unwrap();
                    crate::secrets::delete_secret("native", "API_10").unwrap();
                }
                let result = recovery::retry_with_report(&row.id).unwrap();
                let current: Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(current["keepSetting"], 42);
                for (key, value) in &env {
                    if partial && key == "API_10" {
                        continue;
                    }
                    assert_eq!(
                        &current["mcpServers"]["native"]["env"][key], value,
                        "{key} must recover its own authenticated value"
                    );
                }
                if partial {
                    assert_eq!(
                        current["mcpServers"]["native"]["env"]["API_10"],
                        "TOOLPORT_MISSING_CREDENTIAL_10"
                    );
                    assert_eq!((result.restored, result.remaining), (10, 1));
                    assert!(result.pending.is_some());
                    assert!(Path::new(&row.backup_path).exists());
                    assert!(crate::secrets::backup_secret_status(
                        &account,
                        &format!("{key}-credential-0")
                    )
                    .unwrap()
                    .is_some());
                    crate::secrets::set_secret("native", "API_10", env["API_10"].as_str().unwrap())
                        .unwrap();
                    let second = recovery::retry_with_report(&row.id).unwrap();
                    assert!(second.pending.is_none());
                    assert_eq!(
                        (second.restored, second.accounted, second.remaining),
                        (1, 0, 0),
                        "{}",
                        second.message
                    );
                    let final_config: Value =
                        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    assert_eq!(
                        final_config["mcpServers"]["native"]["env"]["API_10"],
                        env["API_10"]
                    );
                } else {
                    assert_eq!((result.restored, result.remaining), (11, 0));
                    assert!(result.pending.is_none());
                }
                assert!(!Path::new(&row.backup_path).exists());
            }
        });
    }
    /// D2: a credential pasted into a config with a syntax error never reaches
    /// a Retry error, the UI-safe error, the listed rows or the receipt.
    #[test]
    fn retry_on_broken_config_never_echoes_a_pasted_credential() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let registry=serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_ONE","secret":true}]}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            let one = "sk-d2-saved-original-value";
            let pasted = "sk-d2-pasted-credential-text";
            let formats = [
                (Format::JsonMcpServers, "json", "cursor", format!("{{\n  \"mcpServers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_ONE\":\"{one}\"}}}}}}\n}}\n")),
                (Format::JsonServers, "jsonc", "vscode", format!("// keep\n{{\n  \"servers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_ONE\":\"{one}\"}}}}}},\n}}\n")),
                (Format::TomlMcpServers, "toml", "cursor", format!("[mcp_servers.native]\ncommand = 'echo'\n[mcp_servers.native.env]\nAPI_ONE = '{one}'\n")),
                (Format::YamlMcpServers, "yaml", "cursor", format!("mcp_servers:\n  native:\n    command: echo\n    env:\n      API_ONE: '{one}'\n")),
            ];
            for (format, extension, client, original) in formats {
                backup_storage::reset_missing();
                let path = data.join(format!("d2-broken.{extension}"));
                std::fs::write(&path, &original).unwrap();
                crate::secrets::set_secret("native", "API_ONE", one).unwrap();
                backup_storage::register("native", "API_ONE", one);
                mutation::run(client, &path, format, || {
                    backup_file(client, &path)?;
                    write_format(format, &path, &[entry()], true)
                })
                .unwrap();
                backup_storage::clear();
                std::env::set_var("TOOLPORT_SECRET_KEY", "d2-locked");
                disconnect(client, &path, format).unwrap();
                let row = recovery::list()
                    .unwrap()
                    .into_iter()
                    .find(|r| r.path == path.to_string_lossy())
                    .unwrap();
                // The user pastes a key over the placeholder and breaks the syntax.
                let broken = std::fs::read_to_string(&path).unwrap().replacen(
                    "TOOLPORT_MISSING_CREDENTIAL_0",
                    pasted,
                    1,
                ) + &format!("{{{{{{ {pasted}\n");
                std::fs::write(&path, &broken).unwrap();
                std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
                let error = match recovery::retry_with_report(&row.id) {
                    Err(error) => error,
                    Ok(attempt) => {
                        panic!("{extension}: broken config retried: {}", attempt.message)
                    }
                };
                assert!(!error.contains(pasted), "{extension}: {error}");
                assert!(error.contains("line"), "{extension}: {error}");
                let shown = recovery::safe_action_error(&row.id, &error);
                assert!(!shown.contains(pasted), "{extension}: {shown}");
                assert!(
                    shown.contains("line") && shown.contains("Retry"),
                    "{extension}: {shown}"
                );
                let rows = serde_json::to_string(&recovery::list().unwrap()).unwrap();
                assert!(!rows.contains(pasted), "{extension}: {rows}");
                assert!(
                    rows.contains(&row.id),
                    "the receipt stays listed: {extension}"
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
                for file in std::fs::read_dir(backup_dir(client).unwrap()).unwrap() {
                    let text = std::fs::read_to_string(file.unwrap().path()).unwrap_or_default();
                    assert!(
                        !text.contains(pasted),
                        "{extension}: backup holds the pasted key"
                    );
                }
                std::fs::remove_file(&path).unwrap();
            }
            // Anything the allow-list does not recognize becomes generic text.
            let unknown = format!("unexpected error near `API_ONE = \"{pasted}\"`");
            let shown = recovery::safe_action_error("no-such-recovery", &unknown);
            assert!(!shown.contains(pasted), "{shown}");
            assert!(shown.contains("Retry"), "{shown}");
        });
    }
    /// Connecting again while values are still owed would snapshot the
    /// placeholders as the new original and drop the saved values.
    #[test]
    fn connect_during_pending_recovery_is_refused_and_keeps_owed_values() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let path = data.join("reconnect-pending.json");
            let secret = "sk-reconnect-pending-fixture";
            let original = format!(
                r#"{{"mcpServers":{{"native":{{"command":"echo","env":{{"API_KEY":"{secret}"}}}}}}}}"#
            );
            std::fs::write(&path, &original).unwrap();
            crate::secrets::set_secret("native", "API_KEY", secret).unwrap();
            backup_storage::register("native", "API_KEY", secret);
            mutation::run("cursor", &path, Format::JsonMcpServers, || {
                backup_file("cursor", &path)?;
                write_format(Format::JsonMcpServers, &path, &[entry()], true)
            })
            .unwrap();
            backup_storage::clear();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-reconnect-pending");
            disconnect("cursor", &path, Format::JsonMcpServers).unwrap();
            let pending = std::fs::read_to_string(&path).unwrap();
            assert!(pending.contains("TOOLPORT_MISSING_CREDENTIAL_0"));
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let error = mutation::run("cursor", &path, Format::JsonMcpServers, || {
                backup_file("cursor", &path)?;
                write_format(Format::JsonMcpServers, &path, &[entry()], true)
            })
            .unwrap_err();
            assert!(error.contains("Choose Retry"), "{error}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), pending);
            let row = recovery::list()
                .unwrap()
                .into_iter()
                .find(|r| r.path == path.to_string_lossy())
                .expect("the owed value stays listed");
            assert!(recovery::retry(&row.id).unwrap().is_none());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        });
    }
    /// A receipt cut to zero bytes or inside its first key is never read as a
    /// record: Connect is refused with the config unchanged, the recovery list
    /// offers Discard recovery file, and Discard deletes only that file.
    #[test]
    fn unreadable_receipt_blocks_writes_and_can_be_discarded_alone() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let secret = "sk-unreadable-receipt-fixture";
            crate::secrets::set_secret("native", "API_KEY", secret).unwrap();
            let connect = |path: &Path| {
                mutation::run("cursor", path, Format::JsonMcpServers, || {
                    backup_file("cursor", path)?;
                    write_format(Format::JsonMcpServers, path, &[entry()], true)
                })
            };
            let mut pending = Vec::new();
            for name in ["damaged.json", "kept.json"] {
                let path = data.join(name);
                std::fs::write(
                    &path,
                    format!(r#"{{"mcpServers":{{"native":{{"command":"echo","env":{{"API_KEY":"{secret}"}}}}}}}}"#),
                )
                .unwrap();
                backup_storage::register("native", "API_KEY", secret);
                connect(&path).unwrap();
                backup_storage::clear();
                pending.push(path);
            }
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-unreadable-receipt");
            for path in &pending {
                disconnect("cursor", path, Format::JsonMcpServers).unwrap();
            }
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let (path, kept) = (&pending[0], &pending[1]);
            let file = record_path("cursor", path).unwrap();
            let kept_file = record_path("cursor", kept).unwrap();
            let kept_raw = std::fs::read_to_string(&kept_file).unwrap();
            let (account, key, _) = backup_storage::test_recovery_key(&kept_file);
            let folder: Vec<_> = std::fs::read_dir(file.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|other| *other != file)
                .collect();
            let config = std::fs::read_to_string(path).unwrap();
            assert!(config.contains("TOOLPORT_MISSING_CREDENTIAL_0"));
            let mut row = None;
            for damaged in ["", "{\"toolportBackupVer"] {
                std::fs::write(&file, damaged).unwrap();
                assert!(connect(path).is_err(), "{damaged:?}");
                assert_eq!(std::fs::read_to_string(path).unwrap(), config);
                let rows = recovery::list().unwrap();
                let found = rows
                    .iter()
                    .find(|r| r.backup_path == file.to_string_lossy())
                    .expect("the damaged receipt is listed")
                    .clone();
                assert!(found.error.is_some(), "{damaged:?}");
                assert!(found.can_discard_file && !found.can_discard, "{damaged:?}");
                assert!(rows
                    .iter()
                    .any(|r| r.path == kept.to_string_lossy() && r.can_discard));
                row = Some(found);
            }
            let row = row.unwrap();
            let review = recovery::prepare_discard(&row.id).unwrap();
            assert_eq!(
                review.lines,
                [format!(
                    "Backup file: {}",
                    file.file_name().unwrap().to_string_lossy()
                )]
            );
            recovery::discard_confirmed(&row.id, &review.manifest).unwrap();
            assert!(!file.exists());
            assert_eq!(std::fs::read_to_string(&kept_file).unwrap(), kept_raw);
            assert!(crate::secrets::get_backup_secret_result(&account, &key)
                .unwrap()
                .is_some());
            for other in &folder {
                assert!(other.exists(), "{}", other.display());
            }
            assert_eq!(std::fs::read_to_string(path).unwrap(), config);
            connect(path).unwrap();
        });
    }
    #[test]
    fn edited_recovery_accounts_for_each_leaf_without_losing_user_edits() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let registry=serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_ONE","secret":true},{"key":"API_TWO","secret":true}]}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            let one = "sk-d1-one-original-value";
            let two = "sk-d1-two-original-value";
            let formats=[
                (Format::JsonMcpServers,"json",format!("{{\n  \"mcpServers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_ONE\":\"{one}\",\"API_TWO\":\"{two}\"}}}}}},\n  \"userSetting\":41\n}}\n")),
                (Format::JsonServers,"jsonc",format!("// keep first\n{{\n  \"mcpServers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_ONE\":\"{one}\",\"API_TWO\":\"{two}\"}}}}}},\n  // keep setting order\n  \"userSetting\":41,\n}}\n")),
                (Format::TomlMcpServers,"toml",format!("# keep first\nuserSetting = 41\n[mcp_servers.native] # keep entry\ncommand = 'echo'\n[mcp_servers.native.env]\nAPI_ONE = '{one}' # keep value\nAPI_TWO = '{two}'\n")),
                (Format::YamlMcpServers,"yaml",format!("# keep first\nuserSetting: 41\nmcp_servers:\n  native: # keep entry\n    command: echo\n    env:\n      API_ONE: '{one}' # keep value\n      API_TWO: '{two}'\n")),
            ];
            for (format, extension, original) in formats {
                let client = if extension == "jsonc" {
                    "vscode"
                } else {
                    "cursor"
                };
                let original = if extension == "jsonc" {
                    original.replace("mcpServers", "servers")
                } else {
                    original
                };
                for variant in ["edited", "partial", "deleted", "replaced", "moved"] {
                    backup_storage::reset_missing();
                    let path = data.join(format!("d1-{extension}-{variant}.{extension}"));
                    std::fs::write(&path, &original).unwrap();
                    for (key, value) in [("API_ONE", one), ("API_TWO", two)] {
                        crate::secrets::set_secret("native", key, value).unwrap();
                        backup_storage::register("native", key, value);
                    }
                    mutation::run(client, &path, format, || {
                        backup_file(client, &path)?;
                        write_format(format, &path, &[entry()], true)
                    })
                    .unwrap();
                    backup_storage::clear();
                    std::env::set_var("TOOLPORT_SECRET_KEY", "d1-locked");
                    disconnect(client, &path, format).unwrap();
                    let row = recovery::list()
                        .unwrap()
                        .into_iter()
                        .find(|r| r.path == path.to_string_lossy())
                        .unwrap();
                    let raw = std::fs::read_to_string(&path).unwrap();
                    let mut edited = raw.replace("41", "42");
                    match variant {
                        "deleted" => {
                            edited = match extension {
                                "json" | "jsonc" => {
                                    "{\"mcpServers\":{},\"userSetting\":42}\n".into()
                                }
                                "toml" => "# deleted server\nuserSetting = 42\n".into(),
                                _ => "# deleted server\nuserSetting: 42\nmcp_servers: {}\n".into(),
                            };
                        }
                        "replaced" => {
                            edited = edited.replace(
                                "TOOLPORT_MISSING_CREDENTIAL_0",
                                "sk-user-replaced-keep-this",
                            )
                        }
                        "moved" => edited = edited.replace("native", "renamed"),
                        _ => {}
                    }
                    std::fs::write(&path, &edited).unwrap();
                    // Edited config plus a still-locked retry must retain the receipt.
                    let locked_retry = recovery::retry(&row.id);
                    if variant == "deleted" {
                        assert!(
                            locked_retry.is_err(),
                            "locked cleanup must keep the receipt"
                        );
                    } else {
                        assert!(locked_retry.unwrap().is_some());
                        // The locked attempt already counts the user's own replacement.
                        let accounted = usize::from(variant == "replaced");
                        assert_eq!(
                            recovery_values::progress(),
                            (0, accounted),
                            "{extension}/{variant}"
                        );
                    }
                    assert!(Path::new(&row.backup_path).exists());
                    for file in std::fs::read_dir(backup_dir(client).unwrap())
                        .unwrap()
                        .flatten()
                    {
                        let backup = std::fs::read_to_string(file.path()).unwrap();
                        assert!(
                            !backup.contains(one)
                                && !backup.contains(two)
                                && !backup.contains("sk-user-replaced-keep-this"),
                            "pending retries must not persist plaintext values"
                        );
                    }
                    std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
                    let mut lost = None;
                    if variant == "partial" {
                        let (account, key, _) =
                            backup_storage::test_recovery_key(Path::new(&row.backup_path));
                        crate::secrets::delete_backup_secret(&account, &key).unwrap();
                        crate::secrets::delete_backup_secret(
                            &account,
                            &format!("{key}-credential-1"),
                        )
                        .unwrap();
                        crate::secrets::delete_secret("native", "API_TWO").unwrap();
                        lost = Some((account, key));
                    }
                    let attempt = recovery::retry_with_report(&row.id).unwrap();
                    let pending = attempt.pending.clone();
                    let counts = (attempt.restored, attempt.accounted, attempt.remaining);
                    let expected = match variant {
                        "partial" => (1, 0, 1),
                        // Counted once, by the earlier locked attempt.
                        "replaced" => (1, 0, 0),
                        "deleted" => (0, 2, 0),
                        _ => (2, 0, 0),
                    };
                    assert_eq!(
                        counts, expected,
                        "{extension}/{variant}: {}",
                        attempt.message
                    );
                    let output = std::fs::read_to_string(&path).unwrap();
                    assert!(output.contains("42"));
                    if variant == "partial" {
                        assert!(
                            pending.is_some(),
                            "partial recovery must remain pending: {extension}"
                        );
                        assert!(Path::new(&row.backup_path).exists());
                        assert!(output.contains(one));
                        assert!(!output.contains(two));
                        assert!(output.contains("TOOLPORT_MISSING_CREDENTIAL_1"));
                        let (account, key) = lost.unwrap();
                        crate::secrets::set_secret(&account, &format!("{key}-credential-1"), two)
                            .unwrap();
                        let second = recovery::retry_with_report(&row.id).unwrap();
                        assert!(second.pending.is_none());
                        // The value put back by the first Retry is not recounted.
                        assert_eq!(
                            (second.restored, second.accounted, second.remaining),
                            (1, 0, 0),
                            "{extension}: {}",
                            second.message
                        );
                    } else {
                        assert!(
                            pending.is_none(),
                            "completed {extension}/{variant} must release receipt"
                        );
                    }
                    let output = std::fs::read_to_string(&path).unwrap();
                    if variant == "deleted" {
                        assert_eq!(output, edited, "never resurrect removed entries");
                    } else {
                        assert!(!output.contains("TOOLPORT_MISSING_CREDENTIAL_"));
                        assert!(output.contains(two));
                        if variant == "replaced" {
                            assert!(output.contains("sk-user-replaced-keep-this"));
                            assert!(!output.contains(one));
                        } else {
                            assert!(output.contains(one));
                        }
                        if extension != "json" {
                            assert!(output.contains("keep first"));
                            assert!(output.contains("keep setting order") || extension != "jsonc");
                        }
                        if variant == "moved" {
                            assert!(output.contains("renamed"));
                            assert!(!output.contains("native"));
                        }
                    }
                    assert!(!Path::new(&row.backup_path).exists());
                }
            }
        });
    }

    #[test]
    fn pending_locations_cannot_be_forgotten_and_stale_retry_cannot_disconnect() {
        crate::secrets::tests::with_isolated_vault(|| {
            let folder = crate::registry::conduit_dir()
                .unwrap()
                .join("manual-cursor");
            std::fs::create_dir_all(&folder).unwrap();
            let path = folder.join("mcp.json");
            let original = r#"{"mcpServers":{"native":{"command":"echo","env":{"API_KEY":"sk-pending-location-fixture"}}}}"#;
            std::fs::write(&path, original).unwrap();
            let id = locations::add_client_location("cursor", folder.to_str().unwrap()).unwrap();
            backup_storage::register("native", "API_KEY", "sk-pending-location-fixture");
            mutation::run(&id, &path, Format::JsonMcpServers, || {
                backup_file(&id, &path)?;
                write_format(Format::JsonMcpServers, &path, &[entry()], true)
            })
            .unwrap();
            backup_storage::clear();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-pending-location");
            disconnect::restore_path(&id, &path, Format::JsonMcpServers, None).unwrap();
            let notice = recovery::list()
                .unwrap()
                .into_iter()
                .find(|r| r.client_id == id)
                .unwrap();
            assert!(locations::remove_client_location(&id)
                .unwrap_err()
                .contains("Choose Retry"));
            recovery::dismiss(&notice.id, true).unwrap();
            assert!(
                locations::remove_client_location(&id).is_err(),
                "dismissal never forgets recovery"
            );
            assert!(recovery::list().unwrap().iter().any(|r| r.id == notice.id));
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let reconnect_id = id.clone();
            let reconnect_path = path.clone();
            recovery::BEFORE_RETRY_LOCK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    // A reconnect racing Retry is refused while values are owed.
                    assert!(mutation::run(
                        &reconnect_id,
                        &reconnect_path,
                        Format::JsonMcpServers,
                        || write_format(Format::JsonMcpServers, &reconnect_path, &[entry()], true),
                    )
                    .unwrap_err()
                    .contains("Choose Retry"));
                }))
            });
            assert!(recovery::retry(&notice.id).unwrap().is_none());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            assert!(locations::lookup(&id).is_some());
            locations::remove_client_location(&id).unwrap();
            assert!(locations::lookup(&id).is_none());
        });
    }

    #[test]
    fn every_writer_import_connect_move_disconnect_restores_exact_bytes() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-exact-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for (index, (format, original)) in fixtures().into_iter().enumerate() {
            for (ending, original) in [
                original.to_string(),
                format!("{}\n", original.replace("\r\n", "\n")),
                format!(
                    "{}\r\n",
                    original.replace("\r\n", "\n").replace('\n', "\r\n")
                ),
            ]
            .into_iter()
            .enumerate()
            {
                for moved in [false, true] {
                    let id = "vscode".to_string();
                    let path = dir.join(format!("config-{index}-{ending}-{moved}"));
                    std::fs::write(&path, &original).unwrap();
                    // Import's read-only inventory must leave original bytes intact.
                    let imported =
                        parse_client_content(format, &read_config_file(&path).unwrap()).unwrap();
                    assert!(imported.iter().any(|server| server.name == "native"));
                    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
                    mutation::run(&id, &path, format, || {
                        edit_format(format, &path, Some(&entry()), true)
                    })
                    .unwrap();
                    if moved {
                        mutation::run(&id, &path, format, || {
                            moved::record(&id, format, &path)?;
                            write_format(format, &path, &[entry()], true)
                        })
                        .unwrap();
                    }
                    disconnect(&id, &path, format).unwrap();
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        original.as_bytes(),
                        "format {index}, moved={moved}"
                    );
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn absent_file_and_parent_are_restored_for_json_toml_and_yaml() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-absent-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for (index, (format, _)) in fixtures().into_iter().enumerate() {
            for missing_parent in [false, true] {
                let id = format!("fixture-{index}-{missing_parent}");
                let parent = dir
                    .join(format!("parent-{index}-{missing_parent}"))
                    .join("client")
                    .join("cli");
                if !missing_parent {
                    std::fs::create_dir_all(&parent).unwrap();
                }
                let path = parent.join("config");
                mutation::run(&id, &path, format, || {
                    edit_format(format, &path, Some(&entry()), true)
                })
                .unwrap();
                disconnect(&id, &path, format).unwrap();
                assert!(!path.exists());
                assert_eq!(parent.exists(), !missing_parent);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_disconnect_edit_survives_and_moved_entries_return() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-undo-race-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        let original = r#"{"session":1,"mcpServers":{"native":{"command":"native","env":{"TOKEN":"fixture"}}}}"#;
        std::fs::write(&path, original).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            moved::record("fixture", Format::JsonMcpServers, &path)?;
            write_format(Format::JsonMcpServers, &path, &[entry()], true)
        })
        .unwrap();
        mutation::BEFORE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|path| {
                let mut root = parse_json_value(&std::fs::read_to_string(path).unwrap()).unwrap();
                root["session"] = serde_json::json!(2);
                root["mcpServers"]["new"] = serde_json::json!({"command":"new"});
                std::fs::write(path, serde_json::to_string(&root).unwrap()).unwrap();
            }))
        });
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let root = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        assert_eq!(root["session"], 2);
        assert_eq!(root["mcpServers"]["native"]["env"]["TOKEN"], "fixture");
        assert_eq!(root["mcpServers"]["new"]["command"], "new");
        assert!(root["mcpServers"].get("toolport").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_overlapping_disconnect_edit_is_a_conflict_without_overwrite() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-disconnect-conflict-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"mcpServers":{}}"#).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        let native = r#"{"mcpServers":{"toolport":{"command":"custom"}}}"#;
        mutation::BEFORE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path| {
                std::fs::write(path, native).unwrap();
            }))
        });
        assert!(disconnect("fixture", &path, Format::JsonMcpServers)
            .unwrap_err()
            .contains("conflict"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), native);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn changed_config_preserves_custom_gateway_and_readded_moved_entry() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-custom-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"mcpServers":{"native":{"command":"old"},"toolport":{"command":"original-custom"}}}"#).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            write_format(Format::JsonMcpServers, &path, &[entry()], true)
        })
        .unwrap();
        let native = r#"{"theme":"new","mcpServers":{"native":{"command":"new"},"toolport":{"command":"user-custom"}}}"#;
        std::fs::write(&path, native).unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(
            parse_json_value(&read_config_file(&path).unwrap()).unwrap(),
            parse_json_value(native).unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn edit_before_repoint_survives_disconnect_even_when_last_write_matches() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-repoint-edit-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"session":1,"mcpServers":{}}"#).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        let mut native = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        native["session"] = serde_json::json!(2);
        std::fs::write(&path, serde_json::to_string(&native).unwrap()).unwrap();
        let mut repointed = entry();
        repointed.command = Some("/fixture/toolport-gateway-2".into());
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&repointed), true)
        })
        .unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let restored = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        assert_eq!(restored["session"], 2);
        assert!(restored["mcpServers"].get("toolport").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_preview_move_record_restores_after_first_two_point_zero_repoint() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-legacy-restore-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"native":{"command":"native","env":{"TOKEN":"fixture"}}}}"#,
        )
        .unwrap();
        // This is the pre-snapshot 1.x/preview record shape, with no version field.
        moved::record("fixture", Format::JsonMcpServers, &path).unwrap();
        write_format(Format::JsonMcpServers, &path, &[entry()], true).unwrap();
        let mut repointed = entry();
        repointed.command = Some("/fixture/toolport-gateway-2".into());
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&repointed), true)
        })
        .unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let restored = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        assert_eq!(restored["mcpServers"]["native"]["env"]["TOKEN"], "fixture");
        assert!(restored["mcpServers"].get("toolport").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn edited_comments_inside_unrelated_server_nodes_survive_disconnect() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-comment-edits-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for (index, format) in [Format::JsonServers, Format::YamlMcpServers]
            .into_iter()
            .enumerate()
        {
            let path = dir.join(format!("config-{index}"));
            let original = if index == 0 {
                "{\n \"servers\": {\n  \"native\": {\"command\": \"native\"}\n }\n}"
            } else {
                "mcp_servers:\n  native:\n    command: native\n"
            };
            std::fs::write(&path, original).unwrap();
            mutation::run("vscode", &path, format, || {
                edit_format(format, &path, Some(&entry()), true)
            })
            .unwrap();
            let edited = read_config_file(&path)
                .unwrap()
                .replace("native\"", "native\" /* newly annotated */")
                .replace("command: native", "command: native # newly annotated");
            std::fs::write(&path, edited).unwrap();
            disconnect("vscode", &path, format).unwrap();
            assert!(read_config_file(&path).unwrap().contains("newly annotated"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn interrupted_disconnect_and_completed_cleanup_retry_are_safe() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-disconnect-journal-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        let original = "{ \"mcpServers\": {\"native\": {\"command\": \"native\"}} }";
        std::fs::write(&path, original).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            moved::record("fixture", Format::JsonMcpServers, &path)?;
            write_format(Format::JsonMcpServers, &path, &[entry()], true)
        })
        .unwrap();
        let connected = read_config_file(&path).unwrap();
        // Crash after recovery was persisted, before the config rename.
        remember(
            "fixture",
            &path,
            Format::JsonMcpServers,
            Some(&connected),
            Some(original),
            true,
            false,
        )
        .unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            apply("fixture", Format::JsonMcpServers, &path)?;
            moved::restore("fixture", Format::JsonMcpServers, &path)?;
            Ok(())
        })
        .unwrap();
        let restored = read_config_file(&path).unwrap();
        assert_eq!(
            parse_json_value(&restored).unwrap(),
            parse_json_value(original).unwrap()
        );
        // Cleanup failed after a completed config write. Retrying cannot remove
        // the entries that the preceding disconnect just restored.
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(read_config_file(&path).unwrap(), restored);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn jsonc_settings_recovery_keeps_comments_and_owner_only_provenance() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-settings-recovery-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("settings.json");
        let original = "{ // setting\n \"theme\": \"dark\",\n}";
        std::fs::write(&path, original).unwrap();
        let mut root = parse_json_value(original).unwrap();
        root["hooks"] = serde_json::json!({"fixture": ["toolport"]});
        write_settings_key(&path, Some(original), &root, "hooks").unwrap();
        let snapshot = record_path("claude-code", &path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(snapshot.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(recorded_paths()
            .iter()
            .any(|(id, recorded, _)| id == "claude-code" && recorded == &path));
        run("claude-code", &path, Format::JsonMcpServers, || {
            apply("claude-code", Format::JsonMcpServers, &path).map(|_| ())
        })
        .unwrap();
        assert_eq!(read_config_file(&path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unowned_custom_gateway_round_trips_and_survives_unrelated_edits() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-original-gateway-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let original = r#"{ "mcpServers": {"toolport":{"command":"/custom/toolport-gateway","args":["--profile","custom"],"env":{"EXTRA":"keep"}}}, "session":1 }"#;
        for edited in [false, true] {
            let path = dir.join(format!("custom-{edited}.json"));
            std::fs::write(&path, original).unwrap();
            mutation::run("fixture", &path, Format::JsonMcpServers, || {
                edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
            })
            .unwrap();
            if edited {
                let mut root = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
                root["session"] = serde_json::json!(2);
                std::fs::write(&path, serde_json::to_string(&root).unwrap()).unwrap();
            }
            disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
            let restored = read_config_file(&path).unwrap();
            assert!(released("fixture", &path).unwrap());
            disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
            assert_eq!(read_config_file(&path).unwrap(), restored);
            assert_eq!(
                load("fixture", &path).unwrap().unwrap().original.as_deref(),
                None, // Successful disconnect must release the original payload.
            );
            if edited {
                let root = parse_json_value(&restored).unwrap();
                assert_eq!(root["session"], 2);
                assert_eq!(
                    root["mcpServers"],
                    parse_json_value(original).unwrap()["mcpServers"]
                );
            } else {
                assert_eq!(restored, original);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exact_original_wins_over_a_stale_move_record() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-stale-move-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"mcpServers":{"stale":{"command":"stale"}}}"#).unwrap();
        moved::record("fixture", Format::JsonMcpServers, &path).unwrap();
        let original = r#"{ "mcpServers": {"native":{"command":"native"}} }"#;
        std::fs::write(&path, original).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(read_config_file(&path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn yaml_removal_and_list_restore_keep_native_and_original_annotations() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-yaml-annotations-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for (index, (format, original)) in [
            (Format::YamlMcpServers, "# user's setting\ntheme: dark # keep\n"),
            (Format::YamlMcpServersList, "# header\nname: Example\nmcpServers:\n  - name: native\n    command: native # original note\n"),
        ].into_iter().enumerate() {
            for moved in [false, true] {
                let path = dir.join(format!("config-{index}-{moved}"));
                std::fs::write(&path, original).unwrap();
                mutation::run("fixture", &path, format, || {
                    if moved { moved::record("fixture", format, &path)?; write_format(format, &path, &[entry()], true) }
                    else { edit_format(format, &path, Some(&entry()), true) }
                }).unwrap();
                let current = read_config_file(&path).unwrap();
                let edited = format!("{}session: 2 # native note\n", current.replace("command: native # original note", "command: native # edited note"));
                std::fs::write(&path, edited).unwrap();
                disconnect("fixture", &path, format).unwrap();
                let restored = read_config_file(&path).unwrap();
                assert!(restored.contains("# native note"));
                assert!(restored.contains(if index == 0 { "# keep" } else if moved { "# original note" } else { "# edited note" }), "{restored}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn removing_clients_does_not_resurrect_a_retired_toolport_hook() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-retired-hook-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("settings.json");
        let original = serde_json::json!({"theme":"dark", "hooks":{"PostToolUse":[{"hooks":[{"command":"toolport-gateway --toolport-hook"}, {"command":"user-hook"}]}]}}).to_string();
        std::fs::write(&path, &original).unwrap();
        let cleaned = crate::hooks::strip_hooks(&parse_json_value(&original).unwrap());
        write_settings_json(&path, Some(&original), &cleaned).unwrap();
        let removed = read_config_file(&path).unwrap();
        run("claude-code", &path, Format::JsonMcpServers, || {
            apply("claude-code", Format::JsonMcpServers, &path).map(|_| ())
        })
        .unwrap();
        assert_eq!(read_config_file(&path).unwrap(), removed);
        assert!(!removed.contains("--toolport-hook"));
        assert!(removed.contains("user-hook"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reconnect_captures_fresh_original_and_releases_old_payloads() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-fresh-original-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"secret":"old-secret","mcpServers":{}}"#).unwrap();
        let connect = || {
            mutation::run("fixture", &path, Format::JsonMcpServers, || {
                edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
            })
        };
        connect().unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let fresh = "{ \"mcpServers\": {}, \"session\": 2 }";
        std::fs::write(&path, fresh).unwrap();
        connect().unwrap();
        let record = load("fixture", &path).unwrap().unwrap();
        assert_eq!(record.original.as_deref(), Some(fresh));
        assert!(record.exact_eligible);
        assert!(
            !std::fs::read_to_string(record_path("fixture", &path).unwrap())
                .unwrap()
                .contains("old-secret")
        );
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), fresh);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exact_restore_exchange_replays_native_save() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-restore-exchange-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"mcpServers":{},"session":1}"#).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        crate::registry::client_file::HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "commit",
                Box::new(|path| {
                    let mut native =
                        parse_json_value(&std::fs::read_to_string(path).unwrap()).unwrap();
                    native["session"] = serde_json::json!(2);
                    let tmp = path.with_extension("native");
                    std::fs::write(&tmp, native.to_string()).unwrap();
                    #[cfg(windows)]
                    std::fs::remove_file(path).unwrap();
                    std::fs::rename(tmp, path).unwrap();
                }),
            ))
        });
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let root = parse_json_value(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["session"], 2);
        assert!(root["mcpServers"].get(GATEWAY_ENTRY_NAME).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn created_config_removal_preserves_racing_client_save() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-removal-race-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        let native = r#"{"session":2,"mcpServers":{"native":{"command":"native"}}}"#;
        crate::registry::client_file::HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "remove",
                Box::new(move |path| {
                    let tmp = path.with_extension("native");
                    std::fs::write(&tmp, native).unwrap();
                    #[cfg(windows)]
                    std::fs::remove_file(path).unwrap();
                    std::fs::rename(tmp, path).unwrap();
                }),
            ))
        });
        assert!(disconnect("fixture", &path, Format::JsonMcpServers)
            .unwrap_err()
            .contains("conflict"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), native);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn exact_restore_preserves_original_config_mode() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-restore-mode-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            std::fs::metadata(released_path("fixture", &path).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn repeated_disconnect_preserves_native_edits_after_release() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-released-native-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        std::fs::write(&path, "{}").unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        let native = r#"{ "session": 2, "mcpServers": {"toolport":{"command":"/custom/toolport-gateway","args":["--profile","custom"]}} }"#;
        std::fs::write(&path, native).unwrap();
        disconnect("fixture", &path, Format::JsonMcpServers).unwrap();
        assert_eq!(read_config_file(&path).unwrap(), native);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_same_byte_connect_cannot_be_finalized_by_an_older_disconnect() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-finish-generation-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        write_format(Format::JsonMcpServers, &path, &[entry()], true).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            apply("fixture", Format::JsonMcpServers, &path).map(|_| ())
        })
        .unwrap();
        let expected = crate::registry::sha256_hex(&read_config_file(&path).unwrap());
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &path, Some(&entry()), true)
        })
        .unwrap();
        assert_eq!(
            crate::registry::sha256_hex(&read_config_file(&path).unwrap()),
            expected
        );
        assert!(finish("fixture", &path, Some(&expected))
            .unwrap_err()
            .contains("another Toolport operation"));
        assert!(!released("fixture", &path).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
