//! Codewhale keeps MCP servers in `mcp.json`, a strict JSON object whose server
//! map is `servers` or, for compatibility with other clients, `mcpServers`.
//!
//! Codewhale reads either key but refuses a file that has both, and its own
//! writer keeps whichever spelling the file uses, so Toolport edits the key that
//! is there (`servers` for a new file) and never adds the other one. A server
//! with `disabled: true` or `enabled: false` is not started; Toolport leaves it
//! alone. Edits go through the shared JSON rewrite, so the rest of the file keeps
//! its text, and every edit is checked by reading the result back.
//!
//! The rest of the client code sees the file through [`normalized`]:
//! `{"mcpServers": {name: server}, "rest": {...}, "off": {name: server}}`.

use serde_json::{Map, Value as Json};

use super::{McpServer, ServerEntry};

pub(super) const KEY: &str = "mcpServers";

const UNTOUCHED: &str = "leaving the Codewhale MCP config untouched";

fn root(text: &str) -> Result<Map<String, Json>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Json>(text) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(_) => Err(format!(
            "The Codewhale MCP config must be a JSON object; {UNTOUCHED}."
        )),
        Err(e) => Err(super::diagnostics::json(&e)),
    }
}

/// The key the file keeps its servers under.
fn key_of(root: &Map<String, Json>) -> Result<&'static str, String> {
    match (root.contains_key("servers"), root.contains_key("mcpServers")) {
        (true, true) => Err(format!(
            "This Codewhale MCP config has both `servers` and `mcpServers`, which Codewhale won't load; merge them by hand, {UNTOUCHED}."
        )),
        (false, true) => Ok("mcpServers"),
        _ => Ok("servers"),
    }
}

fn entries(root: &Map<String, Json>, key: &str) -> Result<Map<String, Json>, String> {
    match root.get(key) {
        None => Ok(Map::new()),
        Some(Json::Object(map)) => Ok(map.clone()),
        Some(_) => Err(format!(
            "`{key}` in the Codewhale MCP config must be an object; {UNTOUCHED}."
        )),
    }
}

/// Codewhale skips a server that is disabled or not enabled.
fn off(server: &Json) -> bool {
    server.get("disabled") == Some(&Json::Bool(true))
        || server.get("enabled") == Some(&Json::Bool(false))
}

struct Parts {
    on: Map<String, Json>,
    off: Map<String, Json>,
    rest: Map<String, Json>,
}

fn parts(text: &str) -> Result<Parts, String> {
    let mut root = root(text)?;
    let key = key_of(&root)?;
    let all = entries(&root, key)?;
    root.remove(key);
    let (on, off): (Vec<_>, Vec<_>) = all.into_iter().partition(|(_, server)| !off(server));
    Ok(Parts {
        on: on.into_iter().collect(),
        off: off.into_iter().collect(),
        rest: root,
    })
}

/// `{"mcpServers": started servers, "rest": everything else, "off": the rest}`.
pub(super) fn normalized(text: &str) -> Result<Json, String> {
    let parts = parts(text)?;
    let mut out = Map::new();
    for (key, map) in [(KEY, parts.on), ("rest", parts.rest), ("off", parts.off)] {
        if !map.is_empty() {
            out.insert(key.into(), Json::Object(map));
        }
    }
    Ok(Json::Object(out))
}

fn strings(value: Option<&Json>) -> Vec<String> {
    value
        .and_then(Json::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| match item {
                    Json::String(s) => Some(s.clone()),
                    Json::Number(n) => Some(n.to_string()),
                    Json::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The servers Codewhale starts, for import and the client list.
pub(super) fn servers(text: &str) -> Result<Vec<McpServer>, String> {
    Ok(parts(text)?
        .on
        .into_iter()
        .map(|(name, server)| {
            let text = |key: &str| server.get(key).and_then(Json::as_str).map(str::to_string);
            let transport = if text("command").is_some() {
                "stdio"
            } else if text("url").is_none() {
                "unknown"
            } else if text("transport").is_some_and(|t| t.eq_ignore_ascii_case("sse")) {
                "sse"
            } else {
                "http"
            };
            let mut env_keys: Vec<String> = ["env", "headers", "env_headers", "env_http_headers"]
                .iter()
                .filter_map(|key| server.get(*key).and_then(Json::as_object))
                .flat_map(|map| map.keys().cloned())
                .collect();
            env_keys.sort_unstable();
            env_keys.dedup();
            McpServer {
                name,
                transport: transport.into(),
                command: text("command"),
                args: strings(server.get("args")),
                env_keys,
                url: text("url"),
            }
        })
        .collect())
}

/// Refuse imports whose native behavior the shared importer cannot retain.
pub(super) fn validate_import(
    path: &std::path::Path,
    names: &[String],
    migration: bool,
) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path).map_err(|_| {
        "Could not read the Codewhale MCP config; client config unchanged.".to_string()
    })?;
    let root = root(&text)?;
    let servers = entries(&root, key_of(&root)?)?;
    for name in names {
        let definition = servers.get(name).ok_or_else(|| {
            format!("Codewhale server '{name}' changed since detection; refresh the client list and retry.")
        })?;
        if !definition.is_object() {
            return Err(format!(
                "Malformed Codewhale server '{name}': expected an object."
            ));
        }
        let nonempty = |value: &Json| match value {
            Json::Null => false,
            Json::Array(list) => !list.is_empty(),
            Json::Object(map) => !map.is_empty(),
            Json::String(text) => !text.is_empty(),
            Json::Bool(value) => *value,
            _ => true,
        };
        let unsupported = [
            "enabled_tools",
            "disabled_tools",
            "cwd",
            "connect_timeout",
            "execute_timeout",
            "read_timeout",
            "required",
            "env_headers",
            "env_http_headers",
            "allow_private_network",
            "scopes",
            "oauth",
            "oauth_resource",
        ]
        .into_iter()
        .find(|field| definition.get(*field).is_some_and(nonempty));
        let unsupported = unsupported.or_else(|| {
            root.get("timeouts")
                .filter(|value| nonempty(value))
                .map(|_| "timeouts")
        });
        if let Some(field) = unsupported {
            return Err(format!(
                "Cannot import Codewhale server '{name}': Toolport can't keep its '{field}' setting. Keep this server in Codewhale, or set up the same behavior in Toolport by hand first."
            ));
        }
        if migration && off(definition) {
            return Err(format!("Cannot move turned-off Codewhale server '{name}': keep it in Codewhale or turn it on first."));
        }
    }
    Ok(())
}

/// A Toolport server as a Codewhale entry: `command`/`args`/`env`/`cwd`, or a
/// `url` with `headers`, Streamable HTTP unless `transport` says `sse`.
pub(super) fn config_of(entry: &ServerEntry) -> Result<Json, String> {
    if entry.name.trim().is_empty() {
        return Err(format!("A Codewhale MCP server needs a name; {UNTOUCHED}."));
    }
    if entry.command.is_none() && entry.url.is_none() {
        return Err(format!(
            "'{}' has no command or URL; {UNTOUCHED}.",
            entry.name
        ));
    }
    let mut value = super::entry_to_kimi_json(entry);
    if let (Some(_), Some(cwd), Some(map)) = (&entry.command, &entry.cwd, value.as_object_mut()) {
        map.insert("cwd".into(), Json::String(cwd.clone()));
    }
    Ok(value)
}

fn render(original: &str, key: &str, servers: Map<String, Json>) -> Result<String, String> {
    if original.trim().is_empty() {
        let mut root = Map::new();
        root.insert(key.into(), Json::Object(servers));
        let text = serde_json::to_string_pretty(&Json::Object(root)).map_err(|e| e.to_string())?;
        return Ok(text + "\n");
    }
    super::rewrite_json_key_preserving(original, key, &Json::Object(servers))
}

/// Make the servers Codewhale starts exactly `desired`. Turned-off servers stay
/// as they are unless `desired` names one, which then replaces it.
pub(super) fn sync(
    text: Option<&str>,
    desired: &Map<String, Json>,
    _raw: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let original = text.unwrap_or("");
    let root = root(original)?;
    let key = key_of(&root)?;
    let before = parts(original)?;
    if before.on == *desired {
        return Ok(original.to_string());
    }
    let mut next = entries(&root, key)?;
    next.retain(|name, server| off(server) || desired.contains_key(name));
    for (name, want) in desired {
        next.insert(name.clone(), want.clone());
    }
    let out = render(original, key, next)?;
    let mut off_after = before.off;
    off_after.retain(|name, _| !desired.contains_key(name));
    verify(&out, desired, &before.rest, &off_after)?;
    Ok(out)
}

fn verify(
    out: &str,
    desired: &Map<String, Json>,
    rest: &Map<String, Json>,
    off: &Map<String, Json>,
) -> Result<(), String> {
    let after = parts(out)
        .map_err(|e| format!("Toolport produced an unreadable MCP config ({e}); {UNTOUCHED}."))?;
    if after.on != *desired || after.rest != *rest || after.off != *off {
        return Err(format!(
            "Toolport could not apply this change exactly; {UNTOUCHED}."
        ));
    }
    Ok(())
}

/// Each started server as `{name: server}` JSON, for the move record.
pub(super) fn raw_entries(text: &str) -> Result<Vec<(String, String, Json)>, String> {
    let mut out = Vec::new();
    for (name, server) in parts(text)?.on {
        let mut one = Map::new();
        one.insert(name.clone(), server.clone());
        let raw = serde_json::to_string_pretty(&Json::Object(one)).map_err(|e| e.to_string())?;
        out.push((name, raw, server));
    }
    Ok(out)
}

fn recorded(raw: &str) -> Result<(String, Json), String> {
    match serde_json::from_str::<Json>(raw) {
        Ok(Json::Object(map)) if map.len() == 1 => Ok(map.into_iter().next().expect("one entry")),
        _ => Err("Could not read imported settings".into()),
    }
}

/// The fields of one recorded entry.
pub(super) fn definition(raw: &str) -> Result<Json, String> {
    Ok(recorded(raw)?.1)
}

/// Put recorded entries back. Names already in the file are left as they are.
pub(super) fn append_raw(text: Option<&str>, entries_raw: &[String]) -> Result<String, String> {
    let original = text.unwrap_or("");
    let root = root(original)?;
    let key = key_of(&root)?;
    let before = parts(original)?;
    let mut next = entries(&root, key)?;
    let mut desired = before.on.clone();
    for raw in entries_raw {
        let (name, server) = recorded(raw)?;
        if next.contains_key(&name) {
            continue;
        }
        next.insert(name.clone(), server.clone());
        desired.insert(name, server);
    }
    if desired == before.on {
        return Ok(original.to_string());
    }
    let out = render(original, key, next)?;
    verify(&out, &desired, &before.rest, &before.off)?;
    Ok(out)
}

#[cfg(test)]
#[path = "codewhale_tests.rs"]
mod tests;
