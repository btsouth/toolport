//! The OpenHands CLI keeps MCP servers in `mcp.json` in its persistence dir,
//! `~/.openhands` unless `OPENHANDS_PERSISTENCE_DIR` moves it. The file is
//! strict JSON in fastmcp's `{"mcpServers": {name: server}}` shape. Without
//! `mcpServers`, fastmcp reads the root as the server map when any value there
//! looks like a server, so Toolport does too and moves them under `mcpServers`
//! when it writes.
//!
//! Each server is `command`/`args`/`env`/`cwd` (stdio) or `url`/`headers` with a
//! `transport` of `http`, `streamable-http` or `sse`. Without one, fastmcp picks
//! SSE for a URL whose path has an `/sse` segment and HTTP otherwise, so Toolport
//! always writes it. `openhands mcp add` writes every unset setting as `null`. A
//! server whose `enabled` is falsy in Python terms is kept but not started, and
//! Toolport leaves it alone. Edits go through the shared JSON rewrite, so the rest
//! of the file keeps its text.

use super::*;
use serde_json::{Map, Value};

const CLIENT: &str = "OpenHands";

/// fastmcp settings a Toolport server can't carry, so importing a server that
/// sets one would change how it runs.
const NOT_IMPORTED: &[&str] = &[
    "cwd",
    "timeout",
    "keep_alive",
    "sse_read_timeout",
    "auth",
    "authentication",
    "tools",
    "include_tags",
    "exclude_tags",
];

/// OpenHands settings a user may add to the Toolport entry, kept when Toolport
/// rewrites it.
const GATEWAY_SETTINGS: &[&str] = &[
    "timeout",
    "keep_alive",
    "description",
    "icon",
    "tools",
    "include_tags",
    "exclude_tags",
];

/// Parse a config and refuse what Toolport can't edit safely: a root that isn't
/// an object and repeated keys.
fn root_from_content(content: &str) -> Result<Value, String> {
    let root = read_existing_json(content, false)?;
    if !root.is_object() {
        return Err(
            "The OpenHands MCP config root must be an object; leaving it untouched.".into(),
        );
    }
    if !content.trim().is_empty() {
        let cst =
            jsonc_parser::cst::CstRootNode::parse(content, &jsonc_parser::ParseOptions::default())
                .map_err(|error| super::diagnostics::jsonc(&error))?;
        if let Some(object) = cst.object_value() {
            zcode::duplicate_keys(&object, "", CLIENT)?;
        }
    }
    Ok(root)
}

fn read_root(path: &Path) -> Result<(Value, Option<String>), String> {
    if let Some(staged) = mutation::read(path) {
        return match staged {
            Some(content) => Ok((root_from_content(&content)?, Some(content))),
            None => Ok((serde_json::json!({}), None)),
        };
    }
    match std::fs::metadata(path) {
        Ok(_) => {
            let content = read_config_file(path)?;
            Ok((root_from_content(&content)?, Some(content)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((serde_json::json!({}), None))
        }
        Err(error) => Err(format!("Could not read the OpenHands MCP config: {error}")),
    }
}

/// fastmcp's `wrap_servers_at_root`: with no `mcpServers`, a root holding any
/// object with a `command` or `url` is the server map.
fn servers_at_root(root: &Value) -> bool {
    root.get("mcpServers").is_none()
        && root.as_object().is_some_and(|map| {
            map.values()
                .any(|value| value.get("command").is_some() || value.get("url").is_some())
        })
}

pub(super) fn server_map(root: &Value) -> Result<Map<String, Value>, String> {
    if servers_at_root(root) {
        return Ok(root.as_object().cloned().unwrap_or_default());
    }
    match root.get("mcpServers") {
        None => Ok(Map::new()),
        Some(servers) => servers
            .as_object()
            .cloned()
            .ok_or_else(|| "OpenHands 'mcpServers' must be an object".into()),
    }
}

/// OpenHands starts a server when `enabled` is missing or truthy in Python
/// terms, so `false`, `null`, `0`, `""`, `[]` and `{}` all turn it off.
fn disabled(definition: &Value) -> bool {
    match definition.get("enabled") {
        None => false,
        Some(Value::Null) => true,
        Some(Value::Bool(on)) => !on,
        Some(Value::Number(number)) => number.as_f64() == Some(0.0),
        Some(Value::String(text)) => text.is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(map)) => map.is_empty(),
    }
}

/// The transport OpenHands uses for a server: a command means stdio, otherwise
/// `transport`, or what fastmcp infers from the case-sensitive URL path.
/// Remote `type` is an ignored extra field in fastmcp 4.1.1.
fn transport(definition: &Value) -> Option<&'static str> {
    let text = |key: &str| {
        definition
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    if text("command").is_some_and(|command| !command.is_empty()) {
        return Some("stdio");
    }
    let url = text("url").filter(|url| !url.is_empty())?;
    match text("transport").as_deref() {
        None => Some(if sse_path(&url) { "sse" } else { "http" }),
        Some("sse") => Some("sse"),
        Some("http" | "streamable-http") => Some("http"),
        Some(_) => None,
    }
}

/// fastmcp's guess for a URL without a transport: SSE when the path has an
/// `/sse` segment (followed by `/`, `?`, `&` or the end).
fn sse_path(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = match rest.find('/') {
        Some(start) => &rest[start..],
        None => return false,
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.match_indices("/sse").any(|(at, _)| {
        matches!(
            path[at + 4..].chars().next(),
            None | Some('/') | Some('?') | Some('&')
        )
    })
}

/// The definition in the shape the shared parsers read, with `type` set.
fn normalized_server(name: &str, definition: &Value) -> Result<Value, String> {
    let mut value = definition
        .as_object()
        .cloned()
        .ok_or_else(|| format!("Malformed OpenHands server '{name}': expected an object"))?;
    let kind = transport(definition).ok_or_else(|| {
        format!("Malformed OpenHands server '{name}': it needs a command or a URL with a supported transport")
    })?;
    value.remove("transport");
    value.insert("type".into(), Value::String(kind.into()));
    Ok(Value::Object(value))
}

fn is_gateway(name: &str, definition: &Value) -> bool {
    gateway_edit_matches(name, definition.get("command").and_then(Value::as_str))
}

/// The servers OpenHands starts, plus a Toolport entry the user turned off, so
/// it still counts as connected and Disconnect removes it.
fn inventory(servers: &Map<String, Value>) -> Result<Vec<McpServer>, String> {
    let mut inventory = servers
        .iter()
        .filter(|(name, definition)| !disabled(definition) || is_gateway(name, definition))
        .map(|(name, definition)| {
            normalized_server(name, definition).map(|value| json_server(name, &value))
        })
        .collect::<Result<Vec<_>, _>>()?;
    inventory.sort_by_key(|server| server.name.to_lowercase());
    Ok(inventory)
}

pub(super) fn parse(content: &str) -> Result<Vec<McpServer>, String> {
    inventory(&server_map(&root_from_content(content)?)?)
}

fn importable(name: &str, definition: &Value, reject_disabled: bool) -> Result<(), String> {
    normalized_server(name, definition)?;
    if let Some(field) = NOT_IMPORTED.iter().find(|field| {
        definition
            .get(**field)
            .is_some_and(|value| !value.is_null())
    }) {
        return Err(format!(
            "Cannot import OpenHands server '{name}': Toolport can't keep its '{field}' setting. Keep this server in OpenHands, or set up the same behavior in Toolport by hand first."
        ));
    }
    if reject_disabled && disabled(definition) {
        return Err(format!(
            "Cannot move turned-off OpenHands server '{name}': this path can't keep it turned off. Turn it on with `openhands mcp enable` first, or keep it there."
        ));
    }
    Ok(())
}

pub(super) fn validate_import(
    path: &Path,
    names: &[String],
    migration: bool,
) -> Result<(), String> {
    let (root, _) = read_root(path)?;
    let servers = server_map(&root)?;
    for name in names {
        let definition = servers.get(name).ok_or_else(|| {
            format!("OpenHands server '{name}' changed since detection; refresh the client list and retry.")
        })?;
        importable(name, definition, migration)?;
    }
    Ok(())
}

/// A Toolport server as an OpenHands entry: the shared JSON shape, with
/// `transport` in place of the `type` hint for a URL and `cwd` for a command.
pub(super) fn entry_value(entry: &ServerEntry) -> Result<Value, String> {
    if entry.command.is_none() && entry.url.is_none() {
        return Err(format!(
            "'{}' has no command or URL; leaving the OpenHands MCP config untouched.",
            entry.name
        ));
    }
    let mut value = entry_to_json(entry);
    let map = value
        .as_object_mut()
        .expect("entry_to_json builds an object");
    if entry.command.is_some() {
        if let Some(cwd) = &entry.cwd {
            map.insert("cwd".into(), Value::String(cwd.clone()));
        }
    } else {
        map.remove("type");
        let kind = if entry.transport.eq_ignore_ascii_case("sse") {
            "sse"
        } else {
            "http"
        };
        map.insert("transport".into(), Value::String(kind.into()));
    }
    Ok(value)
}

/// A gateway the user turned off stays off when Toolport repairs it, and keeps
/// the settings they gave it.
fn retain_gateway_state(servers: &Map<String, Value>, value: &mut Value) {
    for (name, definition) in servers {
        if !is_gateway(name, definition) {
            continue;
        }
        if disabled(definition) {
            value["enabled"] = Value::Bool(false);
        }
        for key in GATEWAY_SETTINGS {
            if let (Some(setting), Some(map)) = (definition.get(*key), value.as_object_mut()) {
                map.entry(*key).or_insert_with(|| setting.clone());
            }
        }
    }
}

fn write_map(
    path: &Path,
    mut root: Value,
    original: Option<&str>,
    servers: Map<String, Value>,
) -> Result<(), String> {
    // Root-level servers move under `mcpServers`; once that key exists fastmcp
    // stops reading the root, so leaving them there would drop them.
    let original = if servers_at_root(&root) {
        root = serde_json::json!({});
        None
    } else {
        original
    };
    root["mcpServers"] = Value::Object(servers);
    atomic_write_json_config(path, original, &root, "mcpServers")
}

pub(super) fn edit_gateway(path: &Path, entry: Option<&ServerEntry>) -> Result<(), String> {
    let (root, original) = read_root(path)?;
    let mut servers = server_map(&root)?;
    let mut value = entry.map(entry_value).transpose()?;
    if let Some(value) = &mut value {
        retain_gateway_state(&servers, value);
    }
    let count = servers.len();
    servers.retain(|name, definition| !is_gateway(name, definition));
    if let Some(value) = value {
        servers.insert(GATEWAY_ENTRY_NAME.into(), value);
    } else if servers.len() == count {
        return Ok(());
    }
    write_map(path, root, original.as_deref(), servers)
}

pub(super) fn remove_selected(path: &Path, names: &[String]) -> Result<(), String> {
    let (root, original) = read_root(path)?;
    let mut servers = server_map(&root)?;
    for name in names {
        servers.remove(name);
    }
    write_map(path, root, original.as_deref(), servers)
}

pub(super) fn write_servers(path: &Path, entries: &[ServerEntry]) -> Result<(), String> {
    let (root, original) = read_root(path)?;
    let previous = server_map(&root)?;
    // Turned-off servers aren't listed, so they can't be in `entries`; keep them.
    let mut servers: Map<String, Value> = previous
        .iter()
        .filter(|(name, definition)| {
            disabled(definition)
                && !entries.iter().any(|entry| &entry.name == *name)
                && !is_gateway(name, definition)
        })
        .map(|(name, definition)| (name.clone(), definition.clone()))
        .collect();
    for entry in entries {
        let mut value = entry_value(entry)?;
        if is_gateway_server(entry) {
            retain_gateway_state(&previous, &mut value);
        }
        servers.insert(entry.name.clone(), value);
    }
    write_map(path, root, original.as_deref(), servers)
}

#[cfg(test)]
#[path = "openhands_tests.rs"]
mod tests;
