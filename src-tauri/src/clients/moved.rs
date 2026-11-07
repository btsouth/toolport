//! Servers that "Move into gateway" took out of a client's config, kept so
//! Disconnect can put them back (UX-03).
//!
//! Migration rewrites a client's server list down to the gateway entry. Before it
//! does, every other entry is copied here in the client's own format (the JSON
//! value, the TOML table text, the YAML node), env values included. The record is
//! owner-only and sits beside the client's config backups, never in the registry,
//! because those values can be API keys. Disconnect re-inserts each recorded entry
//! whose name is not in the config any more, so an entry the user re-added or
//! edited since is never overwritten, then drops the record.

use super::*;
use serde::Deserialize;

const RECORD_FILE: &str = "moved-servers.json";

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Record {
    /// The config file the entries came from. A record for another file (a moved
    /// `CLAUDE_CONFIG_DIR`, say) is never applied.
    config_path: String,
    entries: Vec<Moved>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Moved {
    name: String,
    #[serde(flatten)]
    raw: Raw,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "format", rename_all = "lowercase")]
enum Raw {
    Json {
        value: serde_json::Value,
    },
    /// A one-entry `[mcp_servers.<name>]` document, so nested tables and their
    /// comments survive the round trip.
    Toml {
        text: String,
    },
    Yaml {
        text: String,
    },
    /// One server of a synced format, verbatim: a DeepSeek Harness entry mapping
    /// or a one-table Reasonix document, so its comments and expressions go back
    /// as they were. Records from builds before Reasonix call it `dsh`.
    #[serde(alias = "dsh")]
    Native {
        text: String,
    },
}

/// Where a format keeps its name -> definition server list.
enum Container {
    /// A top-level JSON object, optionally one level down (`mcp.servers`).
    Json {
        key: &'static str,
        nested: Option<&'static str>,
    },
    Toml,
    YamlMap(&'static str),
    /// Continue's list of server objects, each carrying its own `name`.
    YamlList(&'static str),
    /// A format edited through its normalized view; see `synced`.
    Synced(synced::Synced),
}

fn container(format: Format) -> Container {
    let json = |key| Container::Json { key, nested: None };
    match format {
        Format::JsonMcpServers
        | Format::JsonCopilotMcpServers
        | Format::JsonDroidMcpServers
        | Format::JsonQwenMcpServers
        | Format::JsonKimiMcpServers
        | Format::JsonAuggieMcpServers
        | Format::JsonOpenHandsMcpServers => json("mcpServers"),
        Format::JsonAmpMcpServers => json("amp.mcpServers"),
        Format::JsonZCodeMcp | Format::JsonOpenClawMcp => Container::Json {
            key: "mcp",
            nested: Some("servers"),
        },
        Format::JsonNanobotMcp => Container::Json {
            key: "tools",
            nested: Some("mcpServers"),
        },
        Format::JsonServers => json("servers"),
        Format::JsonMcp | Format::JsonOpenCodeMcp => json("mcp"),
        Format::JsonContextServers => json("context_servers"),
        Format::TomlMcpServers => Container::Toml,
        Format::YamlExtensions => Container::YamlMap("extensions"),
        Format::YamlMcpServers => Container::YamlMap("mcp_servers"),
        Format::YamlMcpServersList => Container::YamlList("mcpServers"),
        Format::YamlDshPatch | Format::TomlReasonixPlugins | Format::JsonCodewhaleServers => {
            Container::Synced(synced::Synced::of(format).expect("synced format"))
        }
    }
}

pub(super) fn server_container(format: Format, root: &serde_json::Value) -> serde_json::Value {
    if matches!(format, Format::JsonOpenHandsMcpServers) {
        return openhands::server_map(root)
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);
    }
    match container(format) {
        Container::Json { key, nested } => {
            let value = &root[key];
            nested.map_or_else(|| value.clone(), |key| value[key].clone())
        }
        Container::Toml => root["mcp_servers"].clone(),
        Container::YamlMap(key) | Container::YamlList(key) => root[key].clone(),
        Container::Synced(_) => root[synced::KEY].clone(),
    }
}

fn record_path(client_id: &str) -> Result<PathBuf, String> {
    Ok(backup_dir(client_id)
        .ok_or("Could not resolve backup dir")?
        .join(RECORD_FILE))
}

pub(super) fn has_record(client_id: &str) -> bool {
    record_path(client_id).is_ok_and(|path| path.exists())
}

fn load(client_id: &str) -> Result<Option<Record>, String> {
    let path = record_path(client_id)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("could not read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

pub(super) fn matches_path(client_id: &str, path: &Path) -> Result<bool, String> {
    Ok(load(client_id)?.is_some_and(|record| record.config_path == path.to_string_lossy()))
}

pub(super) fn recorded_paths() -> Vec<(String, PathBuf, Result<Format, String>)> {
    let mut paths = Vec::new();
    for def in machine_defs(false) {
        if !has_record(def.client_id()) {
            continue;
        }
        match load(def.client_id()) {
            Ok(Some(record)) => {
                let path = PathBuf::from(record.config_path);
                if path.is_absolute() {
                    paths.push((def.client_id().into(), path, Ok(def.format)));
                } else {
                    paths.push((
                        def.client_id().into(),
                        record_path(def.client_id()).unwrap_or_default(),
                        Err("Legacy move record has no absolute config path".into()),
                    ));
                }
            }
            Ok(None) => {}
            Err(error) => paths.push((
                def.client_id().into(),
                record_path(def.client_id()).unwrap_or_default(),
                Err(error),
            )),
        }
    }
    paths
}

/// Raw values stay inside the backend and the owner-only config transaction.
pub(super) fn definition(
    format: Format,
    path: &Path,
    name: &str,
) -> Result<Option<serde_json::Value>, String> {
    let entries = extract(format, &read_config_file(path)?)?;
    let Some(entry) = entries.into_iter().find(|entry| entry.name == name) else {
        return Ok(None);
    };
    let value = match entry.raw {
        Raw::Json { value } => value,
        Raw::Toml { text } => {
            let doc: toml::Value =
                toml::from_str(&text).map_err(|_| "Could not read imported settings")?;
            serde_json::to_value(&doc["mcp_servers"][name])
                .map_err(|_| "Could not read imported settings")?
        }
        Raw::Yaml { text } => {
            let doc: serde_yaml::Value =
                serde_yaml::from_str(&text).map_err(|_| "Could not read imported settings")?;
            serde_json::to_value(doc).map_err(|_| "Could not read imported settings")?
        }
        Raw::Native { text } => match container(format) {
            Container::Synced(kind) => kind.definition(&text)?,
            _ => return Err("Could not read imported settings".into()),
        },
    };
    Ok(Some(value))
}

/// Copy every non-gateway entry in `path` into the client's move record before
/// migration strips them. Entries already recorded by an earlier move are kept;
/// a name moved again takes its newest definition.
#[cfg(test)]
pub(super) fn record(client_id: &str, format: Format, path: &Path) -> Result<(), String> {
    let names = extract(format, &read_config_file(path)?)?
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    record_selected(client_id, format, path, &names)
}

pub(super) fn record_selected(
    client_id: &str,
    format: Format,
    path: &Path,
    names: &[String],
) -> Result<(), String> {
    let record_file = record_path(client_id)?;
    let previous = match std::fs::read_to_string(&record_file) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", record_file.display())),
    };
    let entries = if mutation::exists(path) {
        extract(format, &read_config_file(path)?)?
    } else {
        Vec::new()
    };
    let config_path = path.display().to_string();
    let mut record = match previous.as_deref() {
        Some(text) => serde_json::from_str::<Record>(text)
            .map_err(|e| format!("could not read {}: {e}", record_file.display()))?,
        None => Record::default(),
    };
    if record.config_path != config_path {
        record = Record {
            config_path,
            entries: Vec::new(),
        };
    }
    if entries.is_empty() && record.entries.is_empty() {
        return Ok(());
    }
    for entry in entries
        .into_iter()
        .filter(|entry| names.contains(&entry.name))
    {
        record
            .entries
            .retain(|kept| !kept.name.eq_ignore_ascii_case(&entry.name));
        record.entries.push(entry);
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    atomic_write(&record_file, &text)?;
    Ok(())
}

/// Remove only the definitions reviewed for this transaction. Other entries,
/// including unknown adapter fields and inline credentials, stay in their format.
pub(super) fn remove_selected(format: Format, path: &Path, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    if matches!(format, Format::JsonOpenHandsMcpServers) {
        return openhands::remove_selected(path, names);
    }
    match container(format) {
        Container::Json { key, nested } => {
            let original = read_config_file(path)?;
            let mut root = read_existing_json(&original, true)?;
            let mut servers = root.get_mut(key);
            if let Some(nested) = nested {
                servers = servers.and_then(|v| v.get_mut(nested));
            }
            let servers = servers
                .and_then(|v| v.as_object_mut())
                .ok_or("Could not read the reviewed server list")?;
            for name in names {
                servers.remove(name);
            }
            atomic_write_json_config(path, Some(&original), &root, key)
        }
        Container::Toml => {
            let mut doc = load_toml_document(path)?;
            toml_preservation::remove(&mut doc, names);
            atomic_write(path, &doc.to_string())
        }
        Container::YamlMap(key) | Container::YamlList(key) => {
            let list = matches!(container(format), Container::YamlList(_));
            let (original, mut root) = read_existing_yaml_with_source(path)?;
            let servers = root
                .get_mut(key)
                .ok_or("Could not read the reviewed server list")?;
            if list {
                servers
                    .as_sequence_mut()
                    .ok_or("Expected a server list")?
                    .retain(|v| {
                        !v.get("name")
                            .and_then(|v| v.as_str())
                            .is_some_and(|n| names.iter().any(|name| name == n))
                    });
            } else {
                let servers = servers.as_mapping_mut().ok_or("Expected a server map")?;
                for name in names {
                    servers.remove(serde_yaml::Value::String(name.clone()));
                }
            }
            atomic_write_yaml_config(path, original.as_deref(), &root, key)
        }
        Container::Synced(kind) => {
            let original = read_config_file(path)?;
            let mut desired = kind.servers_map(&original)?;
            for name in names {
                desired.remove(name);
            }
            write_synced(kind, path, &original, &desired)
        }
    }
}

fn write_synced(
    kind: synced::Synced,
    path: &Path,
    original: &str,
    desired: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let out = kind.sync(Some(original), desired, &Default::default())?;
    if out != original {
        atomic_write(path, &out)?;
    }
    Ok(())
}

/// Add only an identity env key to an existing owned entry. Preserve raw fields.
pub(super) fn backfill_identity(
    format: Format,
    path: &Path,
    name: &str,
    id: &str,
) -> Result<(), String> {
    let reclaim_main = main_location_identity_changed(
        id,
        location_launch_id(format, name, &read_config_file(path)?)?.as_deref(),
    );
    let keys = if id.contains('@') || reclaim_main {
        vec![crate::brand::ATTRIBUTION_ID, crate::brand::CLIENT_ID]
    } else {
        vec![crate::brand::ATTRIBUTION_ID]
    };
    match container(format) {
        Container::Json {
            key: container_key,
            nested,
        } => {
            let original = read_config_file(path)?;
            let mut root = read_existing_json(&original, true)?;
            let mut servers = root
                .get_mut(container_key)
                .ok_or("Missing server container")?;
            if let Some(nested) = nested {
                servers = servers.get_mut(nested).ok_or("Missing server container")?;
            }
            let entry = servers.get_mut(name).ok_or("Missing owned gateway")?;
            let env_key = if matches!(format, Format::JsonOpenCodeMcp) {
                "environment"
            } else {
                "env"
            };
            if entry.get(env_key).is_none() {
                entry[env_key] = serde_json::json!({});
            }
            for key in &keys {
                entry
                    .get_mut(env_key)
                    .and_then(serde_json::Value::as_object_mut)
                    .ok_or("Invalid gateway environment")?
                    .insert((*key).into(), serde_json::json!(id));
            }
            atomic_write_json_config(path, Some(&original), &root, container_key)
        }
        Container::Toml => {
            let mut doc = load_toml_document(path)?;
            let entry = &mut toml_mcp_servers_mut(&mut doc)[name];
            if entry.get("env").is_none() {
                entry["env"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            for key in &keys {
                entry["env"]
                    .as_table_like_mut()
                    .ok_or("Invalid gateway environment")?
                    .insert(key, toml_edit::value(id));
            }
            atomic_write(path, &doc.to_string())
        }
        Container::YamlMap(container_key) | Container::YamlList(container_key) => {
            let (original, mut root) = read_existing_yaml_with_source(path)?;
            let servers = root
                .get_mut(container_key)
                .ok_or("Missing server container")?;
            let entry = if matches!(container(format), Container::YamlList(_)) {
                servers
                    .as_sequence_mut()
                    .ok_or("Invalid server list")?
                    .iter_mut()
                    .find(|s| s["name"].as_str() == Some(name))
                    .ok_or("Missing owned gateway")?
            } else {
                servers.get_mut(name).ok_or("Missing owned gateway")?
            };
            let env_key = if matches!(format, Format::YamlExtensions) {
                "envs"
            } else {
                "env"
            };
            if entry.get(env_key).is_none() {
                entry[env_key] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
            }
            for key in &keys {
                entry
                    .get_mut(env_key)
                    .and_then(serde_yaml::Value::as_mapping_mut)
                    .ok_or("Invalid gateway environment")?
                    .insert(
                        serde_yaml::Value::String((*key).into()),
                        serde_yaml::Value::String(id.into()),
                    );
            }
            atomic_write_yaml_config(path, original.as_deref(), &root, container_key)
        }
        Container::Synced(kind) => {
            let original = read_config_file(path)?;
            let mut desired = kind.servers_map(&original)?;
            let env = desired
                .get_mut(name)
                .ok_or("Missing owned gateway")?
                .as_object_mut()
                .ok_or("Invalid gateway entry")?
                .entry("env")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or("Invalid gateway environment")?;
            for key in &keys {
                env.insert((*key).into(), serde_json::json!(id));
            }
            write_synced(kind, path, &original, &desired)
        }
    }
}

pub(super) fn toml_entry(
    client_id: &str,
    path: &Path,
    name: &str,
) -> Result<Option<(toml_edit::Key, toml_edit::Item)>, String> {
    let Some(record) = load(client_id)? else {
        return Ok(None);
    };
    if record.config_path != path.to_string_lossy() {
        return Ok(None);
    }
    let Some(entry) = record.entries.iter().find(|entry| entry.name == name) else {
        return Ok(None);
    };
    let Raw::Toml { text } = &entry.raw else {
        return Ok(None);
    };
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    Ok(toml_preservation::raw_entry(&doc, name))
}

/// Recorded native entries (synced formats) for `path`, by server name.
pub(super) fn native_entries(
    client_id: &str,
    path: &Path,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let Some(record) = load(client_id)? else {
        return Ok(Default::default());
    };
    if record.config_path != path.to_string_lossy() {
        return Ok(Default::default());
    }
    Ok(record
        .entries
        .into_iter()
        .filter_map(|entry| match entry.raw {
            Raw::Native { text } => Some((entry.name, text)),
            _ => None,
        })
        .collect())
}

pub(super) fn missing_names(
    client_id: &str,
    format: Format,
    path: &Path,
) -> Result<Vec<String>, String> {
    let Some(record) = load(client_id)? else {
        return Ok(Vec::new());
    };
    if record.config_path != path.to_string_lossy() {
        return Ok(Vec::new());
    }
    let existing = if mutation::exists(path) {
        extract(format, &read_config_file(path)?)?
    } else {
        Vec::new()
    };
    Ok(record
        .entries
        .into_iter()
        .filter(|entry| {
            !existing
                .iter()
                .any(|current| current.name.eq_ignore_ascii_case(&entry.name))
        })
        .map(|entry| entry.name)
        .collect())
}

/// What [`restore`] put back.
pub(super) struct Restored {
    pub names: Vec<String>,
    /// The copy taken before the restore wrote anything, if it did.
    pub backup: Option<PathBuf>,
}

/// Re-insert the client's recorded entries that are missing from `path`. `None`
/// when there is no record for this file, so the caller knows not to forget it.
pub(super) fn restore(
    client_id: &str,
    format: Format,
    path: &Path,
) -> Result<Option<Restored>, String> {
    let Some(record) = load(client_id)? else {
        return Ok(None);
    };
    if record.config_path != path.display().to_string() {
        return Ok(None);
    }
    let backup = || backup_file(client_id, path);
    let (names, backup) =
        insert_missing(client_id, container(format), path, &record.entries, backup)?;
    Ok(Some(Restored { names, backup }))
}

/// Drop the client's record once Disconnect has finished with it.
pub(super) fn forget(client_id: &str) -> Result<(), String> {
    let path = record_path(client_id)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not remove client move record: {e}")),
    }
}

fn json_command(definition: &serde_json::Value) -> Option<&str> {
    let command = definition.get("command")?;
    command
        .as_str()
        // OpenCode keeps the whole argv in `command`.
        .or_else(|| command.as_array()?.first()?.as_str())
}

fn yaml_command(definition: &serde_yaml::Value) -> Option<&str> {
    definition
        .get("command")
        .or_else(|| definition.get("cmd"))
        .and_then(|value| value.as_str())
}

fn extract(format: Format, text: &str) -> Result<Vec<Moved>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    match container(format) {
        Container::Json { key, nested } => {
            let mut root = read_existing_json(text, true)?;
            if matches!(format, Format::JsonOpenHandsMcpServers) {
                root = serde_json::json!({"mcpServers": openhands::server_map(&root)?});
            }
            let mut servers = root.get(key);
            if let Some(nested) = nested {
                servers = servers.and_then(|value| value.get(nested));
            }
            for (name, definition) in servers
                .and_then(|value| value.as_object())
                .into_iter()
                .flatten()
            {
                if !gateway_identity_matches(name, name, json_command(definition)) {
                    out.push(Moved {
                        name: name.clone(),
                        raw: Raw::Json {
                            value: definition.clone(),
                        },
                    });
                }
            }
        }
        Container::Toml => {
            read_existing_toml(text)?;
            let doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| format!("Could not parse the existing config ({e})"))?;
            let Some(servers) = doc.get("mcp_servers").and_then(|item| item.as_table_like()) else {
                return Ok(out);
            };
            for (name, item) in servers.iter() {
                let command = item.get("command").and_then(|value| value.as_str());
                if gateway_identity_matches(name, name, command) {
                    continue;
                }
                let mut table = toml_edit::Table::new();
                table.set_implicit(true);
                table.insert_formatted(servers.key(name).unwrap(), item.clone());
                let mut single = toml_edit::DocumentMut::new();
                single.insert("mcp_servers", toml_edit::Item::Table(table));
                out.push(Moved {
                    name: name.to_string(),
                    raw: Raw::Toml {
                        text: single.to_string(),
                    },
                });
            }
        }
        Container::YamlMap(key) => {
            let root = parse_existing_yaml_content(text)?;
            for (name, definition) in root
                .get(key)
                .and_then(|value| value.as_mapping())
                .into_iter()
                .flatten()
            {
                let Some(name) = name.as_str() else { continue };
                if !gateway_identity_matches(name, name, yaml_command(definition)) {
                    out.push(Moved {
                        name: name.to_string(),
                        raw: Raw::Yaml {
                            text: serde_yaml::to_string(definition).map_err(|e| e.to_string())?,
                        },
                    });
                }
            }
        }
        Container::Synced(kind) => {
            for (name, raw, config) in kind.raw_entries(text)? {
                let command = config.get("command").and_then(|value| value.as_str());
                if !gateway_identity_matches(&name, &name, command) {
                    out.push(Moved {
                        name,
                        raw: Raw::Native { text: raw },
                    });
                }
            }
        }
        Container::YamlList(key) => {
            let root = parse_existing_yaml_content(text)?;
            for definition in root
                .get(key)
                .and_then(|value| value.as_sequence())
                .into_iter()
                .flatten()
            {
                let Some(name) = definition.get("name").and_then(|value| value.as_str()) else {
                    continue;
                };
                if !gateway_identity_matches(name, name, yaml_command(definition)) {
                    out.push(Moved {
                        name: name.to_string(),
                        raw: Raw::Yaml {
                            text: serde_yaml::to_string(definition).map_err(|e| e.to_string())?,
                        },
                    });
                }
            }
        }
    }
    Ok(out)
}

fn has_name<'a>(mut existing: impl Iterator<Item = &'a str>, name: &str) -> bool {
    existing.any(|existing| existing.eq_ignore_ascii_case(name))
}

/// Insert each recorded entry whose name (ignoring case) the config no longer
/// has. Writes nothing, and takes no backup, when every name is already there.
fn insert_missing(
    client_id: &str,
    container: Container,
    path: &Path,
    entries: &[Moved],
    backup: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> Result<(Vec<String>, Option<PathBuf>), String> {
    let mut names = Vec::new();
    match container {
        Container::Json { key, nested } => {
            let original = if mutation::exists(path) {
                Some(read_config_file(path)?)
            } else {
                None
            };
            let mut root = match original.as_deref() {
                Some(text) if !text.trim().is_empty() => read_existing_json(text, true)?,
                _ => serde_json::Value::Object(serde_json::Map::new()),
            };
            let object = root
                .as_object_mut()
                .ok_or("Client config root must be an object; leaving it untouched.")?;
            let mut servers = object
                .entry(key)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(nested) = nested {
                servers = servers
                    .as_object_mut()
                    .ok_or(format!(
                        "'{key}' must be an object; leaving the client config untouched."
                    ))?
                    .entry(nested)
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            }
            let servers = servers
                .as_object_mut()
                .ok_or("The server list must be an object; leaving the client config untouched.")?;
            for entry in entries {
                let Raw::Json { value } = &entry.raw else {
                    continue;
                };
                if !has_name(servers.keys().map(String::as_str), &entry.name) {
                    servers.insert(entry.name.clone(), value.clone());
                    names.push(entry.name.clone());
                }
            }
            if names.is_empty() {
                return Ok((names, None));
            }
            let backup = backup()?;
            atomic_write_json_config(path, original.as_deref(), &root, key)?;
            Ok((names, backup))
        }
        Container::Toml => {
            let mut doc = load_toml_document(path)?;
            let provenance = restore::toml_prefix_provenance(client_id, path)?;
            let evidence_doc = doc.clone();
            for entry in entries {
                let Raw::Toml { text } = &entry.raw else {
                    continue;
                };
                if has_name(
                    toml_mcp_servers_mut(&mut doc).iter().map(|(name, _)| name),
                    &entry.name,
                ) {
                    continue;
                }
                let single = text
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|e| format!("could not read the recorded {}: {e}", entry.name))?;
                let Some((mut key, mut item)) = toml_preservation::raw_entry(&single, &entry.name)
                else {
                    continue;
                };
                if let Some((original, written)) = &provenance {
                    toml_preservation::dedup_raw_prefix(
                        original,
                        written,
                        &evidence_doc,
                        &mut key,
                        &mut item,
                    );
                }
                toml_preservation::put_raw(&mut doc, key, item);
                names.push(entry.name.clone());
            }
            if names.is_empty() {
                return Ok((names, None));
            }
            let backup = backup()?;
            atomic_write(path, &doc.to_string())?;
            Ok((names, backup))
        }
        Container::YamlMap(key) | Container::YamlList(key) => {
            let list = matches!(container, Container::YamlList(_));
            let (original, mut root) = read_existing_yaml_with_source(path)?;
            for entry in entries {
                let Raw::Yaml { text } = &entry.raw else {
                    continue;
                };
                let value: serde_yaml::Value = serde_yaml::from_str(text)
                    .map_err(|e| format!("could not read the recorded {}: {e}", entry.name))?;
                let inserted = if list {
                    let servers = continue_servers_mut(&mut root);
                    let present = has_name(
                        servers
                            .iter()
                            .filter_map(|server| server.get("name").and_then(|v| v.as_str())),
                        &entry.name,
                    );
                    if !present {
                        servers.push(value);
                    }
                    !present
                } else {
                    let servers = if key == "extensions" {
                        yaml_extensions_mut(&mut root)
                    } else {
                        hermes_mcp_servers_mut(&mut root)
                    };
                    let present = has_name(servers.keys().filter_map(|k| k.as_str()), &entry.name);
                    if !present {
                        servers.insert(serde_yaml::Value::String(entry.name.clone()), value);
                    }
                    !present
                };
                if inserted {
                    names.push(entry.name.clone());
                }
            }
            if names.is_empty() {
                return Ok((names, None));
            }
            let backup = backup()?;
            atomic_write_yaml_config(path, original.as_deref(), &root, key)?;
            Ok((names, backup))
        }
        Container::Synced(kind) => {
            let original = if mutation::exists(path) {
                Some(read_config_file(path)?)
            } else {
                None
            };
            let present = kind.servers_map(original.as_deref().unwrap_or(""))?;
            let mut texts = Vec::new();
            for entry in entries {
                let Raw::Native { text } = &entry.raw else {
                    continue;
                };
                if !has_name(present.keys().map(String::as_str), &entry.name)
                    && !names.contains(&entry.name)
                {
                    texts.push(text.clone());
                    names.push(entry.name.clone());
                }
            }
            if names.is_empty() {
                return Ok((names, None));
            }
            let backup = backup()?;
            atomic_write(path, &kind.append_raw(original.as_deref(), &texts)?)?;
            Ok((names, backup))
        }
    }
}
