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
}

fn container(format: Format) -> Container {
    let json = |key| Container::Json { key, nested: None };
    match format {
        Format::JsonMcpServers
        | Format::JsonCopilotMcpServers
        | Format::JsonDroidMcpServers
        | Format::JsonQwenMcpServers
        | Format::JsonKimiMcpServers => json("mcpServers"),
        Format::JsonAmpMcpServers => json("amp.mcpServers"),
        Format::JsonZCodeMcp => Container::Json {
            key: "mcp",
            nested: Some("servers"),
        },
        Format::JsonServers => json("servers"),
        Format::JsonMcp | Format::JsonOpenCodeMcp => json("mcp"),
        Format::JsonContextServers => json("context_servers"),
        Format::TomlMcpServers => Container::Toml,
        Format::YamlExtensions => Container::YamlMap("extensions"),
        Format::YamlMcpServers => Container::YamlMap("mcp_servers"),
        Format::YamlMcpServersList => Container::YamlList("mcpServers"),
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

/// Copy every non-gateway entry in `path` into the client's move record before
/// migration strips them. Entries already recorded by an earlier move are kept;
/// a name moved again takes its newest definition.
pub(super) fn record(client_id: &str, format: Format, path: &Path) -> Result<(), String> {
    let record_file = record_path(client_id)?;
    let previous = match std::fs::read_to_string(&record_file) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", record_file.display())),
    };
    let entries = if mutation::exists(path) {
        extract(container(format), &read_config_file(path)?)?
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
    for entry in entries {
        record
            .entries
            .retain(|kept| !kept.name.eq_ignore_ascii_case(&entry.name));
        record.entries.push(entry);
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    atomic_write(&record_file, &text)?;
    Ok(())
}

pub(super) fn toml_entry(
    client_id: &str,
    path: &Path,
    name: &str,
) -> Result<Option<toml_edit::Item>, String> {
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
    Ok(doc
        .get("mcp_servers")
        .and_then(|table| table.get(name))
        .cloned())
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
        extract(container(format), &read_config_file(path)?)?
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
    let (names, backup) = insert_missing(container(format), path, &record.entries, backup)?;
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

fn extract(container: Container, text: &str) -> Result<Vec<Moved>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    match container {
        Container::Json { key, nested } => {
            let root = read_existing_json(text, true)?;
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
                table.insert(name, item.clone());
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
            let servers = toml_mcp_servers_mut(&mut doc);
            for entry in entries {
                let Raw::Toml { text } = &entry.raw else {
                    continue;
                };
                if has_name(servers.iter().map(|(name, _)| name), &entry.name) {
                    continue;
                }
                let single = text
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|e| format!("could not read the recorded {}: {e}", entry.name))?;
                let Some(item) = single
                    .get("mcp_servers")
                    .and_then(|item| item.get(&entry.name))
                else {
                    continue;
                };
                servers.insert(&entry.name, item.clone());
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
    }
}
