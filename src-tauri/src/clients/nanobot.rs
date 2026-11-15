//! nanobot (HKUDS/nanobot) keeps MCP servers under `tools.mcpServers` in
//! `~/.nanobot/config.json`, a strict JSON file that also holds its providers,
//! channels and agent settings. nanobot also reads snake_case keys, but writes
//! camelCase, so a file that spells the server map `mcp_servers` is refused
//! rather than edited under a second key.
//!
//! Each server is `command`/`args`/`env`/`cwd` (stdio) or `url`/`headers` with a
//! `type` of `streamableHttp` or `sse`. Without one, a URL ending in `/sse` is SSE
//! and any other is streamable HTTP, so Toolport always writes it. nanobot has no
//! per-server off switch. Edits go through the shared JSON rewrite, so the rest
//! of the file keeps its text.

use super::*;
use serde_json::{Map, Value};

const CLIENT: &str = "nanobot";

/// nanobot settings a Toolport server can't carry, so importing a server that
/// sets one would change how it runs. `enabledTools` other than `["*"]` is
/// checked separately.
const NOT_IMPORTED: &[&str] = &["cwd", "auth", "toolTimeout", "tool_timeout"];

/// nanobot settings a user may add to the Toolport entry, kept when Toolport
/// rewrites it.
const GATEWAY_SETTINGS: &[&str] = &[
    "toolTimeout",
    "tool_timeout",
    "enabledTools",
    "enabled_tools",
];

/// `~/.nanobot`, the instance root nanobot uses unless started with `--home`.
pub(crate) fn home_dir(user_home: &Path) -> PathBuf {
    user_home.join(".nanobot")
}

/// The default agent workspace: `agents.defaults.workspace` in the config with
/// `~` expanded, else `~/.nanobot/workspace`. A relative setting depends on
/// nanobot's working directory, so it gives `None`.
pub(crate) fn workspace(user_home: &Path) -> Option<PathBuf> {
    let config = home_dir(user_home).join("config.json");
    let root = match std::fs::read_to_string(config) {
        Ok(text) => json_object_from_content(&text).ok()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(_) => return None,
    };
    let defaults = match root.get("agents") {
        None => None,
        Some(agents) => match agents.as_object()?.get("defaults") {
            None => None,
            Some(defaults) => Some(defaults.as_object()?),
        },
    };
    let configured = defaults.and_then(|defaults| defaults.get("workspace"));
    let configured = match configured {
        Some(value) => Some(value.as_str()?.to_string()),
        None => None,
    };
    let Some(raw) = configured else {
        return Some(home_dir(user_home).join("workspace"));
    };
    let path = if raw == "~" {
        user_home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        // pathlib normalizes repeated separators before expanding `~`. Keep the
        // suffix relative so `~//workspace` cannot replace the user's home.
        user_home.join(rest.trim_start_matches(|ch| ch == '/' || (cfg!(windows) && ch == '\\')))
    } else if cfg!(windows) && raw.starts_with("~\\") {
        user_home.join(raw[2..].trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(raw)
    };
    path.is_absolute().then_some(path)
}

/// Parse nanobot's strict JSON object, independently of what Toolport can edit.
/// Read-only workspace lookup follows nanobot's last-key-wins JSON semantics.
fn json_object_from_content(content: &str) -> Result<Value, String> {
    let root: Value = if content.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(content).map_err(|error| super::diagnostics::json(&error))?
    };
    if !root.is_object() {
        return Err("The nanobot config root must be an object; leaving it untouched.".into());
    }
    Ok(root)
}

/// Skill edits change `agents`, so either spelling of the MCP map can remain.
/// Repeated keys are still unsafe for any write.
pub(crate) fn root_for_skill_edit(content: &str) -> Result<Value, String> {
    let root = json_object_from_content(content)?;
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

/// Guard all MCP writes, including Disconnect and restore, against repeated keys
/// and the alternate map spelling that the MCP writer cannot safely rewrite.
pub(crate) fn root_from_content(content: &str) -> Result<Value, String> {
    let root = root_for_skill_edit(content)?;
    if root
        .get("tools")
        .and_then(|tools| tools.get("mcp_servers"))
        .is_some()
    {
        return Err("This nanobot config spells its servers tools.mcp_servers; rename it to tools.mcpServers, as nanobot writes it, and Toolport can connect. Leaving it untouched.".into());
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
        Err(error) => Err(format!("Could not read the nanobot config: {error}")),
    }
}

fn server_map(root: &Value) -> Result<Map<String, Value>, String> {
    let Some(tools) = root.get("tools") else {
        return Ok(Map::new());
    };
    let tools = tools
        .as_object()
        .ok_or("nanobot 'tools' must be an object")?;
    match tools.get("mcpServers") {
        None => Ok(Map::new()),
        Some(servers) => servers
            .as_object()
            .cloned()
            .ok_or_else(|| "nanobot 'tools.mcpServers' must be an object".into()),
    }
}

/// The transport nanobot uses for a server: its `type`, else stdio for a
/// command, SSE for a URL ending in `/sse` and streamable HTTP for any other.
fn transport(definition: &Value) -> Option<&'static str> {
    let text = |key: &str| definition.get(key).and_then(Value::as_str);
    match text("type") {
        Some("stdio") => return Some("stdio"),
        Some("sse") => return Some("sse"),
        Some("streamableHttp") => return Some("http"),
        Some(_) => return None,
        None => {}
    }
    if text("command").is_some_and(|command| !command.is_empty()) {
        return Some("stdio");
    }
    let url = text("url").filter(|url| !url.is_empty())?;
    Some(if url.trim_end_matches('/').ends_with("/sse") {
        "sse"
    } else {
        "http"
    })
}

/// The definition in the shape the shared parsers read, with `type` set.
pub(super) fn normalized_server(name: &str, definition: &Value) -> Result<Value, String> {
    let mut value = definition
        .as_object()
        .cloned()
        .ok_or_else(|| format!("Malformed nanobot server '{name}': expected an object"))?;
    let kind = transport(definition).ok_or_else(|| {
        format!(
            "Malformed nanobot server '{name}': it needs a command or a URL with a supported type"
        )
    })?;
    // nanobot honors an explicit type before command/URL inference. Strip the
    // inactive transport's fields before the shared parser classifies by command.
    let active_field = if kind == "stdio" { "command" } else { "url" };
    if !definition
        .get(active_field)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(format!(
            "Malformed nanobot server '{name}': its '{kind}' transport needs '{active_field}'"
        ));
    }
    let inactive: &[&str] = if kind == "stdio" {
        &["url", "headers"]
    } else {
        &["command", "args", "env", "cwd"]
    };
    for key in inactive {
        value.remove(*key);
    }
    // These belong to other client schemas. nanobot ignores unknown keys;
    // the shared credential importer must not turn them into live credentials.
    for key in [
        "environment",
        "envs",
        "http_headers",
        "bearerTokenEnvVar",
        "bearer_token_env_var",
        "env_http_headers",
        "requestInit",
    ] {
        value.remove(key);
    }
    value.insert("type".into(), Value::String(kind.into()));
    Ok(Value::Object(value))
}

fn inventory(servers: &Map<String, Value>) -> Result<Vec<McpServer>, String> {
    let mut inventory = Vec::new();
    for (name, definition) in servers {
        if !definition.is_object() {
            return Err(format!(
                "Malformed nanobot server '{name}': expected an object"
            ));
        }
        if definition.get("type").is_some_and(|kind| {
            !kind.is_null() && !matches!(kind.as_str(), Some("stdio" | "sse" | "streamableHttp"))
        }) {
            return Err(format!(
                "Malformed nanobot server '{name}': unsupported transport type"
            ));
        }
        // nanobot skips incomplete entries while connecting the remaining servers.
        // Import still validates each requested entry and returns its hard error.
        if let Ok(value) = normalized_server(name, definition) {
            inventory.push(json_server(name, &value));
        }
    }
    inventory.sort_by_key(|server| server.name.to_lowercase());
    Ok(inventory)
}

pub(super) fn parse(content: &str) -> Result<Vec<McpServer>, String> {
    inventory(&server_map(&root_from_content(content)?)?)
}

fn is_gateway(name: &str, definition: &Value) -> bool {
    transport(definition) == Some("stdio")
        && gateway_edit_matches(name, definition.get("command").and_then(Value::as_str))
}

/// A tool allow-list other than nanobot's default `["*"]`.
fn filters_tools(definition: &Value) -> bool {
    ["enabledTools", "enabled_tools"].iter().any(|key| {
        definition
            .get(*key)
            .is_some_and(|tools| tools != &serde_json::json!(["*"]))
    })
}

fn importable(name: &str, definition: &Value) -> Result<(), String> {
    normalized_server(name, definition)?;
    let field = NOT_IMPORTED
        .iter()
        .find(|field| {
            definition.get(**field).is_some_and(|value| match **field {
                "cwd" => value != "",
                "auth" => !value.is_null(),
                "toolTimeout" | "tool_timeout" => value.as_f64() != Some(30.0),
                _ => unreachable!("known nanobot setting"),
            })
        })
        .copied()
        .or_else(|| filters_tools(definition).then_some("enabledTools"));
    if let Some(field) = field {
        return Err(format!(
            "Cannot import nanobot server '{name}': Toolport can't keep its '{field}' setting. Keep this server in nanobot, or set up the same behavior in Toolport by hand first."
        ));
    }
    Ok(())
}

pub(super) fn validate_import(path: &Path, names: &[String]) -> Result<(), String> {
    let (root, _) = read_root(path)?;
    let servers = server_map(&root)?;
    for name in names {
        let definition = servers.get(name).ok_or_else(|| {
            format!("nanobot server '{name}' changed since detection; refresh the client list and retry.")
        })?;
        importable(name, definition)?;
    }
    Ok(())
}

/// A Toolport server as a nanobot entry: the shared JSON shape, with nanobot's
/// `type` for a URL and `cwd` for a command.
pub(super) fn entry_value(entry: &ServerEntry) -> Result<Value, String> {
    if entry.command.is_none() && entry.url.is_none() {
        return Err(format!(
            "'{}' has no command or URL; leaving the nanobot config untouched.",
            entry.name
        ));
    }
    let mut value = entry_to_json(entry);
    let map = value
        .as_object_mut()
        .expect("entry_to_json builds an object");
    if entry.command.is_some() {
        map.remove("type");
        if let Some(cwd) = &entry.cwd {
            map.insert("cwd".into(), Value::String(cwd.clone()));
        }
    } else {
        let kind = if entry.transport.eq_ignore_ascii_case("sse") {
            "sse"
        } else {
            "streamableHttp"
        };
        map.insert("type".into(), Value::String(kind.into()));
    }
    Ok(value)
}

/// The nanobot settings a user gave the Toolport entry stay when Toolport
/// repairs it.
fn retain_gateway_state(servers: &Map<String, Value>, value: &mut Value) {
    for (name, definition) in servers {
        if !is_gateway(name, definition) {
            continue;
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
    if root.get("tools").is_none() {
        root["tools"] = serde_json::json!({});
    }
    root["tools"]["mcpServers"] = Value::Object(servers);
    atomic_write_json_config(path, original, &root, "tools")
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

pub(super) fn write_servers(path: &Path, entries: &[ServerEntry]) -> Result<(), String> {
    let (root, original) = read_root(path)?;
    let previous = server_map(&root)?;
    let mut servers = Map::new();
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
#[path = "nanobot_tests.rs"]
mod tests;
