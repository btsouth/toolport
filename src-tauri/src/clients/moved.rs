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
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_optional_live"
    )]
    live_prefixes: Vec<toml_preservation::LivePrefix>,
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
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_optional_capture"
        )]
        capture: Option<toml_preservation::TomlCapture>,
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

// Provenance is optional recovery metadata. Invalid metadata disables its use,
// but must not prevent loading the raw entry needed to restore legacy records.
fn deserialize_optional_capture<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<toml_preservation::TomlCapture>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

fn deserialize_optional_live<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<toml_preservation::LivePrefix>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
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
    let text = match mutation::read(&path) {
        Some(value) => value,
        None => match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("could not read {}: {e}", path.display())),
        },
    };
    match text {
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("could not read {}: {e}", path.display())),
        None => Ok(None),
    }
}

// Record only transfers observed by the actual writer, applying the same offset
// changes to existing witnesses. Layout simulations must not issue new evidence.
pub(super) fn observe_toml_removal(
    path: &Path,
    before: &toml_edit::DocumentMut,
    after: &toml_edit::DocumentMut,
    transfers: &[toml_preservation::PrefixTransfer],
) -> Result<(), String> {
    let Some(client_id) = mutation::client_id(path) else {
        return Ok(());
    };
    // This evidence is optional, so an unreadable record must not block the write.
    let Ok(Some(mut record)) = load(&client_id) else {
        return Ok(());
    };
    if record.config_path != path.display().to_string() {
        return Ok(());
    }
    let unchanged = serde_json::to_string(&record.live_prefixes).ok();
    let mut live = Vec::new();
    for old in &record.live_prefixes {
        let Some(bytes) = toml_preservation::valid_live(before, old) else {
            continue;
        };
        let moved = transfers
            .iter()
            .find(|transfer| transfer.source == old.destination);
        let (destination, range) = if let Some(transfer) = moved {
            let Some(end) = transfer.landing.start.checked_add(old.range.end) else {
                continue;
            };
            let Some(start) = transfer.landing.start.checked_add(old.range.start) else {
                continue;
            };
            (transfer.destination.clone(), start..end)
        } else {
            let shift: usize = transfers
                .iter()
                .filter(|t| t.destination == old.destination)
                .map(|t| t.prefix.len())
                .sum();
            let Some(start) = old.range.start.checked_add(shift) else {
                continue;
            };
            let Some(end) = old.range.end.checked_add(shift) else {
                continue;
            };
            (old.destination.clone(), start..end)
        };
        let Some(decor) = toml_preservation::destination_prefix(after, &destination) else {
            continue;
        };
        if decor.get(range.clone()) != Some(bytes) {
            continue;
        }
        live.push(toml_preservation::LivePrefix {
            source_id: old.source_id.clone(),
            source_range: old.source_range.clone(),
            destination,
            range,
            decor_hash: crate::registry::sha256_hex(decor),
        });
    }
    for entry in &record.entries {
        let entry_transfers: Vec<_> = transfers
            .iter()
            .filter(|t| t.source_name == entry.name)
            .collect();
        if entry_transfers.is_empty() {
            continue;
        }
        let Raw::Toml {
            text,
            capture: Some(capture),
        } = &entry.raw
        else {
            continue;
        };
        let Some(actual_text) = toml_raw_text(before, &entry.name) else {
            continue;
        };
        // A saved capture alone is insufficient: the current mutation must have
        // captured this exact raw entry, and each capture can issue evidence once.
        // Otherwise a re-added entry could reuse an old prefix's identity.
        if actual_text != *text || !mutation::consume_toml_capture(path, &capture.id) {
            continue;
        }
        let Ok(raw) = text.parse::<toml_edit::DocumentMut>() else {
            continue;
        };
        let Some((key, item)) = toml_preservation::raw_entry(&raw, &entry.name) else {
            continue;
        };
        let parent = toml_preservation::source_destination(&entry.name, &item);
        for transfer in entry_transfers {
            let (id, prefix, hash) = if transfer.source == parent {
                (
                    &capture.id,
                    toml_preservation::capture_prefix(&key, &item),
                    &capture.prefix_hash,
                )
            } else {
                let Some(child) = capture
                    .children
                    .iter()
                    .find(|child| child.source == transfer.source)
                else {
                    continue;
                };
                let Some(prefix) = toml_preservation::destination_prefix(&raw, &child.source)
                else {
                    continue;
                };
                (&child.id, prefix, &child.prefix_hash)
            };
            if prefix != transfer.prefix || crate::registry::sha256_hex(prefix) != *hash {
                continue;
            }
            let Some(decor) = toml_preservation::destination_prefix(after, &transfer.destination)
            else {
                continue;
            };
            if decor.get(transfer.landing.clone()) != Some(prefix) {
                continue;
            }
            live.push(toml_preservation::LivePrefix {
                source_id: id.clone(),
                source_range: 0..prefix.len(),
                destination: transfer.destination.clone(),
                range: transfer.landing.clone(),
                decor_hash: crate::registry::sha256_hex(decor),
            });
        }
    }
    record.live_prefixes = live;
    if serde_json::to_string(&record.live_prefixes).ok() == unchanged {
        return Ok(());
    }
    atomic_write(
        &record_path(&client_id)?,
        &serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?,
    )
}

// Follow renames and EOF transfers only at structural locations fixed by the writer.
pub(super) fn observe_toml_relocation(
    path: &Path,
    before: &toml_edit::DocumentMut,
    after: &toml_edit::DocumentMut,
    from: &toml_preservation::PrefixDestination,
    to: &toml_preservation::PrefixDestination,
) -> Result<(), String> {
    let Some(client_id) = mutation::client_id(path) else {
        return Ok(());
    };
    // This evidence is optional, so an unreadable record must not block the write.
    let Ok(Some(mut record)) = load(&client_id) else {
        return Ok(());
    };
    if record.config_path != path.display().to_string() {
        return Ok(());
    }
    let unchanged = serde_json::to_string(&record.live_prefixes).ok();
    record.live_prefixes.retain_mut(|live| {
        // Only an actual MCP server table rename moves descendant paths; EOF
        // transfers and unrelated tables must never rewrite a nested witness.
        let destination = if &live.destination == from {
            to.clone()
        } else if let (
            toml_preservation::PrefixDestination::Table(old_root),
            toml_preservation::PrefixDestination::Table(new_root),
            toml_preservation::PrefixDestination::Table(path),
        ) = (from, to, &live.destination)
        {
            if old_root.len() != 2
                || new_root.len() != 2
                || old_root[0] != ("mcp_servers".into(), None)
                || new_root[0] != old_root[0]
                || old_root[1].1.is_some()
                || new_root[1].1.is_some()
                || old_root[1].0 == new_root[1].0
                || path.len() <= old_root.len()
                || !path.starts_with(old_root)
            {
                return true;
            }
            let mut renamed = new_root.clone();
            renamed.extend_from_slice(&path[old_root.len()..]);
            toml_preservation::PrefixDestination::Table(renamed)
        } else {
            return true;
        };
        let Some(bytes) = toml_preservation::valid_live(before, live) else {
            return false;
        };
        let Some(old_decor) = toml_preservation::destination_prefix(before, &live.destination)
        else {
            return false;
        };
        let Some(new_decor) = toml_preservation::destination_prefix(after, &destination) else {
            return false;
        };
        let valid_decor = if &live.destination == from {
            new_decor.starts_with(old_decor)
        } else {
            new_decor == old_decor
        };
        if !valid_decor || new_decor.get(live.range.clone()) != Some(bytes) {
            return false;
        }
        live.destination = destination;
        live.decor_hash = crate::registry::sha256_hex(new_decor);
        true
    });
    if serde_json::to_string(&record.live_prefixes).ok() == unchanged {
        return Ok(());
    }
    atomic_write(
        &record_path(&client_id)?,
        &serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?,
    )
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
        Raw::Toml { text, .. } => {
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
    // A second move in the same mutation must see the first move's staged record,
    // including its live witnesses, rather than the older copy still on disk.
    let previous = match mutation::read(&record_file) {
        Some(value) => value,
        None => match std::fs::read_to_string(&record_file) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("could not read {}: {e}", record_file.display())),
        },
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
            live_prefixes: Vec::new(),
        };
    }
    if entries.is_empty() && record.entries.is_empty() {
        return Ok(());
    }
    let current = if matches!(format, Format::TomlMcpServers) && mutation::exists(path) {
        Some(load_toml_document(path)?)
    } else {
        None
    };
    let mut fresh_captures = Vec::new();
    for mut entry in entries
        .into_iter()
        .filter(|entry| names.contains(&entry.name))
    {
        if let (Some(doc), Raw::Toml { text, capture }) = (&current, &mut entry.raw) {
            let single: toml_edit::DocumentMut = text
                .parse()
                .map_err(|e: toml_edit::TomlError| e.to_string())?;
            if let Some((key, item)) = toml_preservation::raw_entry(&single, &entry.name) {
                let prefix = toml_preservation::capture_prefix(&key, &item);
                let destination = toml_preservation::source_destination(&entry.name, &item);
                let mut id = [0u8; 16];
                getrandom::getrandom(&mut id)
                    .map_err(|e| format!("Could not generate capture ID: {e}"))?;
                // A later move may capture comments transferred from an earlier
                // entry. Record that relationship only while the source remains
                // absent and the recorded ranges and destination decor still match.
                let inherited_for = |destination: &toml_preservation::PrefixDestination,
                                     prefix: &str| {
                    record.live_prefixes.iter().filter_map(|live| {
                    if &live.destination != destination { return None; }
                    let source = record.entries.iter().find(|source| matches!(&source.raw, Raw::Toml { capture: Some(c), .. } if c.id == live.source_id || c.children.iter().any(|child| child.id == live.source_id)))?;
                    if doc.get("mcp_servers").and_then(|s| s.get(&source.name)).is_some() { return None; }
                    let Raw::Toml { text: source_text, capture: Some(source_capture) } = &source.raw else { return None; };
                    let source_doc: toml_edit::DocumentMut = source_text.parse().ok()?;
                    let (source_key, source_item) = toml_preservation::raw_entry(&source_doc, &source.name)?;
                    let (source_prefix, source_hash) = if source_capture.id == live.source_id {
                        (toml_preservation::capture_prefix(&source_key, &source_item), &source_capture.prefix_hash)
                    } else {
                        let child = source_capture.children.iter().find(|child| child.id == live.source_id)?;
                        (toml_preservation::destination_prefix(&source_doc, &child.source)?, &child.prefix_hash)
                    };
                    if crate::registry::sha256_hex(source_prefix) != *source_hash { return None; }
                    let observed = toml_preservation::valid_live(doc, live)?;
                    if source_prefix.get(live.source_range.clone())? != observed || prefix.get(live.range.clone())? != observed { return None; }
                    Some(toml_preservation::PrefixInheritance { range: live.range.clone(), source_id: live.source_id.clone(), source_range: live.source_range.clone() })
                }).collect()
                };
                let inherited = inherited_for(&destination, prefix);
                let mut children = Vec::new();
                for (source, prefix) in toml_preservation::descendant_prefixes(&single, &entry.name)
                {
                    let mut child_id = [0u8; 16];
                    getrandom::getrandom(&mut child_id)
                        .map_err(|e| format!("Could not generate capture ID: {e}"))?;
                    children.push(toml_preservation::ChildCapture {
                        id: child_id.iter().map(|b| format!("{b:02x}")).collect(),
                        inherited: inherited_for(&source, &prefix),
                        source,
                        prefix_hash: crate::registry::sha256_hex(&prefix),
                    });
                }
                *capture = Some(toml_preservation::TomlCapture {
                    id: id.iter().map(|b| format!("{b:02x}")).collect(),
                    prefix_hash: crate::registry::sha256_hex(prefix),
                    inherited,
                    children,
                });
                fresh_captures.push(capture.as_ref().unwrap().id.clone());
            }
        }
        record
            .entries
            .retain(|kept| !kept.name.eq_ignore_ascii_case(&entry.name));
        record.entries.push(entry);
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    atomic_write(&record_file, &text)?;
    // Enable new witnesses only after the recovery record has been staged.
    mutation::remember_toml_captures(path, client_id, fresh_captures);
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
            let before = doc.clone();
            let transfers = toml_preservation::remove(&mut doc, names);
            observe_toml_removal(path, &before, &doc, &transfers)?;
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
) -> Result<Option<(toml_edit::Key, toml_edit::Item, usize)>, String> {
    let Some(record) = load(client_id)? else {
        return Ok(None);
    };
    if record.config_path != path.to_string_lossy() {
        return Ok(None);
    }
    let Some((index, entry)) = record
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.name == name)
    else {
        return Ok(None);
    };
    let Raw::Toml { text, .. } = &entry.raw else {
        return Ok(None);
    };
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    Ok(toml_preservation::raw_entry(&doc, name).map(|(key, item)| (key, item, index)))
}

pub(super) fn toml_provenance(
    client_id: &str,
    path: &Path,
    entries: &[(toml_edit::Key, toml_edit::Item)],
) -> Result<Vec<toml_preservation::TomlProvenance>, String> {
    let record = load(client_id)?.filter(|record| record.config_path == path.to_string_lossy());
    Ok(entries
        .iter()
        .map(|(key, _)| {
            let capture = record
                .as_ref()
                .and_then(|record| record.entries.iter().find(|entry| entry.name == key.get()))
                .and_then(|entry| match &entry.raw {
                    Raw::Toml { capture, .. } => capture.clone(),
                    _ => None,
                });
            toml_preservation::TomlProvenance {
                capture,
                live: record
                    .as_ref()
                    .map(|record| record.live_prefixes.clone())
                    .unwrap_or_default(),
            }
        })
        .collect())
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

fn toml_raw_text(doc: &toml_edit::DocumentMut, name: &str) -> Option<String> {
    let servers = doc.get("mcp_servers")?.as_table_like()?;
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    table.insert_formatted(servers.key(name)?, servers.get(name)?.clone());
    let mut single = toml_edit::DocumentMut::new();
    single.insert("mcp_servers", toml_edit::Item::Table(table));
    Some(single.to_string())
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
                        capture: None,
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
            let mut restore_entries = Vec::new();
            let mut capture_provenance = Vec::new();
            let record = load(client_id)?;
            for entry in entries {
                let Raw::Toml { text, .. } = &entry.raw else {
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
                let Some((key, item)) = toml_preservation::raw_entry(&single, &entry.name) else {
                    continue;
                };
                restore_entries.push((key, item));
                let Raw::Toml { capture, .. } = &entry.raw else {
                    unreachable!()
                };
                capture_provenance.push(toml_preservation::TomlProvenance {
                    capture: capture.clone(),
                    live: record
                        .as_ref()
                        .map(|r| r.live_prefixes.clone())
                        .unwrap_or_default(),
                });
                names.push(entry.name.clone());
            }
            if names.is_empty() {
                return Ok((names, None));
            }
            if let Some((original, written)) = &provenance {
                toml_preservation::restore_raw_entries_with_provenance(
                    original,
                    written,
                    &mut doc,
                    restore_entries,
                    &capture_provenance,
                );
            } else {
                for (key, item) in restore_entries {
                    toml_preservation::put_raw(&mut doc, key, item);
                }
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
