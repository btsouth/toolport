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
    created_parents: Vec<PathBuf>,
    exact_eligible: bool,
    preexisting_gateways: Vec<String>,
    #[serde(default)]
    disconnected: bool,
    #[serde(default)]
    jsonc_settings: bool,
    #[serde(default)]
    disconnect_before: Option<String>,
}

pub(super) fn record_path(client_id: &str, path: &Path) -> Result<PathBuf, String> {
    let hash = crate::registry::sha256_hex(&path.to_string_lossy());
    Ok(backup_dir(client_id)
        .ok_or("Could not resolve backup dir")?
        .join(format!("original-{hash}.json")))
}

fn load(client_id: &str, path: &Path) -> Result<Option<Snapshot>, String> {
    let file = record_path(client_id, path)?;
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", file.display())),
    };
    let record: Snapshot = serde_json::from_str(&text)
        .map_err(|e| format!("could not read {}: {e}", file.display()))?;
    if record.version != 1
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
    let file = record_path(client_id, path)?;
    let previous = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let mut record = match load(client_id, path)? {
        Some(record) => record,
        None => {
            let mut created_parents = Vec::new();
            let mut parent = path.parent();
            while let Some(dir) = parent.filter(|dir| !dir.exists()) {
                created_parents.push(dir.into());
                parent = dir.parent();
            }
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
                created_parents,
                exact_eligible: preexisting_gateways.is_empty(),
                preexisting_gateways,
                disconnected: false,
                jsonc_settings,
                disconnect_before: None,
            }
        }
    };
    if previous.is_some() && before.map(crate::registry::sha256_hex) != record.last_written_hash {
        record.exact_eligible = false;
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
            rebase_value(&mut record.baseline, &native, &path);
        }
    }
    if disconnecting {
        record.baseline = mutation::value(format, after)?;
    }
    record.disconnect_before = if disconnecting {
        before.map(str::to_string)
    } else {
        None
    };
    record.disconnected = disconnecting;
    record.last_written = after.map(str::to_string);
    record.last_written_hash = after.map(crate::registry::sha256_hex);
    let file = record_path(client_id, path)?;
    secure_backup_dir(file.parent().ok_or("Could not resolve snapshot parent")?)?;
    crate::registry::atomic_write(
        &file,
        &serde_json::to_string(&record).map_err(|e| e.to_string())?,
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

fn key(format: Format) -> &'static str {
    match format {
        Format::JsonServers => "servers",
        Format::JsonMcp | Format::JsonOpenCodeMcp | Format::JsonZCodeMcp => "mcp",
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
    let mut names = if moved::missing_names(client_id, format, path)?.is_empty() {
        Vec::new()
    } else {
        gateway_names(format, &mutation::value(format, text)?)
    };
    let registry = crate::registry_controller::registry_for_disconnect()?;
    if let (Some(record), Some(text)) = (registry.client_managed_entries.get(client_id), text) {
        for server in parse_client_content(format, text)? {
            if detected_is_gateway(&server)
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
    if matches!(format, Format::JsonZCodeMcp) {
        servers = servers.and_then(|value| value.get("servers"));
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

pub(super) fn needs_moved(client_id: &str, path: &Path) -> Result<bool, String> {
    Ok(load(client_id, path)?.is_none_or(|record| !record.preexisting_gateways.is_empty()))
}

pub(super) fn apply(client_id: &str, format: Format, path: &Path) -> Result<bool, String> {
    let Some(record) = load(client_id, path)? else {
        return Ok(false);
    };
    mutation::disconnecting();
    let current = if mutation::exists(path) {
        Some(read_config_file(path)?)
    } else {
        None
    };
    if record.disconnected
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
        && current.as_deref().map(crate::registry::sha256_hex) == record.last_written_hash
    {
        match record.original {
            Some(original) => atomic_write(path, &original)?,
            None => mutation::remove(path)?,
        }
        return Ok(true);
    }
    let mut before = record.baseline.clone();
    // A preview/1.x install can already have our gateway at first capture.
    // Its real pre-install bytes are unknown, so remove that owned entry and
    // restore the legacy moved record rather than resurrecting a dead gateway.
    let mut owned = Vec::new();
    let original = mutation::value(format, record.original.as_deref())?;
    for name in &record.preexisting_gateways {
        let key = key(format);
        let entry = |root: &Value| {
            let mut servers = root.get(key)?;
            if matches!(format, Format::JsonZCodeMcp) {
                servers = servers.get("servers")?;
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
        if matches!(format, Format::JsonZCodeMcp) {
            map = map.and_then(|value| value.get_mut("servers"));
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
    let restored = undo(
        before.get(key),
        written.get(key),
        latest.get(key),
        1,
        if matches!(format, Format::JsonZCodeMcp) {
            3
        } else {
            2
        },
    );
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
                .map_err(|e| e.to_string())?;
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
            let servers = toml_mcp_servers_mut(&mut doc);
            for name in existing {
                if desired.is_none_or(|map| !map.contains_key(&name)) {
                    servers.remove(&name);
                }
            }
            if let Some(desired) = desired {
                for (name, value) in desired {
                    if mutation::value(format, current.as_deref())?[key].get(name) != Some(value) {
                        if let Some(item) = moved::toml_entry(client_id, path, name)? {
                            servers.insert(name, item);
                        } else if mutation::value(format, record.original.as_deref())?[key]
                            .get(name)
                            == Some(value)
                        {
                            if let Some(item) = original.get(key).and_then(|table| table.get(name))
                            {
                                servers.insert(name, item.clone());
                            }
                        } else {
                            let value: toml::Value =
                                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                            servers.insert(name, toml_value_to_item(&value));
                        }
                    }
                }
            }
            if latest.get(key).is_none() {
                doc.remove(key);
            }
            atomic_write(path, &doc.to_string())?;
        }
        Format::YamlExtensions | Format::YamlMcpServers | Format::YamlMcpServersList => {
            let root = serde_yaml::to_value(&latest).map_err(|e| e.to_string())?;
            atomic_write_yaml_config(path, current.as_deref(), &root, key)?;
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
    if load(client_id, path)?.is_some_and(|record| record.last_written_hash.as_deref() != expected)
    {
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
    if load(client_id, path)?.is_some_and(|record| record.jsonc_settings) {
        mutation::settings(client_id, path, edit)
    } else {
        mutation::run(client_id, path, format, edit)
    }
}

pub(super) fn finish(client_id: &str, path: &Path, expected: Option<&str>) -> Result<(), String> {
    check_finished(client_id, path, expected)?;
    if let Some(record) = load(client_id, path)? {
        for parent in record.created_parents {
            let _ = std::fs::remove_dir(parent);
        } // Empty directories only.
        std::fs::remove_file(record_path(client_id, path)?).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn after_rollback(
    file: &Path,
    target: &Path,
    written: Option<&str>,
) -> Result<(), String> {
    let mut record: Snapshot =
        serde_json::from_str(&std::fs::read_to_string(file).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if record.config_path != target.to_string_lossy() {
        return Err("Client rollback recovery path mismatch".into());
    }
    record.disconnected = false;
    record.disconnect_before = None;
    record.last_written = written.map(str::to_string);
    record.last_written_hash = written.map(crate::registry::sha256_hex);
    crate::registry::atomic_write(
        file,
        &serde_json::to_string(&record).map_err(|e| e.to_string())?,
    )
}

pub(super) fn recorded_paths() -> Vec<(String, PathBuf, Result<Format, String>)> {
    let mut paths = Vec::new();
    for def in defs() {
        let Some(dir) = backup_dir(def.id) else {
            continue;
        };
        let files = match std::fs::read_dir(&dir) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                paths.push((def.id.into(), dir, Err(e.to_string())));
                continue;
            }
        };
        for file in files {
            let file = match file {
                Ok(file) => file.path(),
                Err(e) => {
                    paths.push((def.id.into(), dir.clone(), Err(e.to_string())));
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
            let parsed = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    serde_json::from_str::<Snapshot>(&text)
                        .map_err(|_| "Client recovery record is invalid".to_string())
                });
            match parsed {
                Ok(record) => paths.push((
                    def.id.into(),
                    PathBuf::from(record.config_path),
                    Ok(record.format),
                )),
                Err(error) => paths.push((def.id.into(), file, Err(error))),
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
            (Format::JsonServers, "{ // user's comment\r\n \"servers\": {\"native\":{\"command\":\"native\",},},\r\n \"theme\": \"dark\",\r\n}"),
            (Format::JsonMcp, r#"{"mcp":{"native":{"command":"native","type":"stdio"}},"theme":"dark"}"#),
            (Format::JsonOpenCodeMcp, "{ // comment\n \"mcp\": {\"native\":{\"type\":\"local\",\"command\":[\"native\"],\"enabled\":true,},},\n}"),
            (Format::JsonContextServers, "{ // comment\n \"context_servers\": {\"native\":{\"command\":{\"path\":\"native\",\"args\":[]},},},\n}"),
            (Format::TomlMcpServers, "# annotation\r\ntheme = 'dark'\r\n[mcp_servers.native]\r\ncommand = 'native' # keep\r\nargs = [ ]"),
            (Format::YamlExtensions, "# annotation\r\ntheme: dark\r\nextensions:\r\n  native:\r\n    enabled: true\r\n    type: stdio\r\n    cmd: native\r\n    args: []"),
            (Format::YamlMcpServers, "# annotation\r\ntheme: dark\r\nmcp_servers:\r\n  native:\r\n    command: native\r\n    args: []"),
            (Format::YamlMcpServersList, "# annotation\r\nname: Example\r\nversion: 1.0.0\r\nschema: v1\r\nmcpServers:\r\n  - name: native\r\n    command: native\r\n    args: []"),
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
}
