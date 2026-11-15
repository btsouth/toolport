//! OpenClaw keeps MCP servers under `mcp.servers` in its gateway config,
//! `openclaw.json`, which it reads as JSON5 and which also holds the user's
//! channels, agents and models.
//!
//! Each server is `command`/`args`/`env`/`cwd` (stdio) or `url`/`headers` with a
//! `transport` of `streamable-http` or `sse`. OpenClaw reads only `transport`, and
//! a URL without one is SSE, so Toolport always writes it. A server with
//! `enabled: false` is kept but not started. Edits patch the `mcp` object in the
//! existing text, so comments, unquoted keys and every other setting stay. A
//! config that pulls `mcp` in with `$include` is refused: OpenClaw itself won't
//! write through those, and an entry written next to the include would override
//! it.

use super::*;
use serde_json::{Map, Value};

const CLIENT: &str = "OpenClaw";

/// Fields OpenClaw honors that a Toolport server can't carry, so importing a
/// server that sets one would change how it runs.
const NOT_IMPORTED: &[&str] = &[
    "cwd",
    "workingDirectory",
    "connectionTimeoutMs",
    "requestTimeoutMs",
    "supportsParallelToolCalls",
    "auth",
    "oauth",
    "sslVerify",
    "clientCert",
    "clientKey",
    "toolFilter",
    "codex",
];

/// Parse a config and refuse what Toolport can't edit safely: a root that isn't
/// an object, repeated keys and `$include`. The mutation guard runs this before
/// every write, Disconnect and restores included.
pub(super) fn root_from_content(content: &str) -> Result<Value, String> {
    let root = read_existing_json(content, true)?;
    if !root.is_object() {
        return Err("The OpenClaw config root must be an object; leaving it untouched.".into());
    }
    if !content.trim().is_empty() {
        let cst =
            jsonc_parser::cst::CstRootNode::parse(content, &jsonc_parser::ParseOptions::default())
                .map_err(|error| super::diagnostics::jsonc(&error))?;
        if let Some(object) = cst.object_value() {
            zcode::duplicate_keys(&object, "", CLIENT)?;
        }
    }
    refuse_includes(&root)?;
    Ok(root)
}

/// `$include` at the root, in `mcp`, in `mcp.servers` or in a server.
fn refuse_includes(root: &Value) -> Result<(), String> {
    let mcp = root.get("mcp");
    let servers = mcp.and_then(|mcp| mcp.get("servers"));
    let mut places = vec![(root, ""), (mcp.unwrap_or(&Value::Null), "mcp")];
    places.push((servers.unwrap_or(&Value::Null), "mcp.servers"));
    if let Some(Value::Object(map)) = servers {
        places.extend(map.values().map(|server| (server, "mcp.servers")));
    }
    match places
        .into_iter()
        .find(|(value, _)| value.get("$include").is_some())
    {
        Some((_, place)) => Err(format!(
            "This OpenClaw config uses $include{} for its MCP servers, which Toolport can't edit safely. Add the server with `openclaw mcp add` or edit the included file by hand.",
            if place.is_empty() {
                String::new()
            } else {
                format!(" in {place}")
            }
        )),
        None => Ok(()),
    }
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
        Err(error) => Err(format!("Could not read the OpenClaw config: {error}")),
    }
}

fn server_map(root: &Value) -> Result<Map<String, Value>, String> {
    let Some(mcp) = root.get("mcp") else {
        return Ok(Map::new());
    };
    let mcp = mcp.as_object().ok_or("OpenClaw 'mcp' must be an object")?;
    match mcp.get("servers") {
        None => Ok(Map::new()),
        Some(servers) => servers
            .as_object()
            .cloned()
            .ok_or_else(|| "OpenClaw 'mcp.servers' must be an object".into()),
    }
}

fn disabled(definition: &Value) -> bool {
    definition.get("enabled") == Some(&Value::Bool(false))
}

/// The transport OpenClaw uses for a server: a command means stdio, otherwise
/// `transport` (or a CLI-style `type` its config migration turns into one),
/// SSE when neither is set.
fn transport(definition: &Value) -> Option<&'static str> {
    let text = |key: &str| {
        definition
            .get(key)
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
    };
    if text("command").is_some_and(|command| !command.is_empty()) {
        return Some("stdio");
    }
    text("url").filter(|url| !url.is_empty())?;
    let kind = text("transport").or_else(|| text("type"));
    match kind.as_deref() {
        None | Some("sse") => Some("sse"),
        Some("streamable-http" | "http") => Some("http"),
        Some(_) => None,
    }
}

/// The definition in the shape the shared parsers read, with `type` set.
fn normalized_server(name: &str, definition: &Value) -> Result<Value, String> {
    let mut value = definition
        .as_object()
        .cloned()
        .ok_or_else(|| format!("Malformed OpenClaw server '{name}': expected an object"))?;
    let kind = transport(definition).ok_or_else(|| {
        format!("Malformed OpenClaw server '{name}': it needs a command or a URL with a supported transport")
    })?;
    value.remove("transport");
    value.insert("type".into(), Value::String(kind.into()));
    Ok(Value::Object(value))
}

/// The servers OpenClaw starts, plus a Toolport entry the user turned off, so
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

fn is_gateway(name: &str, definition: &Value) -> bool {
    gateway_edit_matches(name, definition.get("command").and_then(Value::as_str))
}

fn has_include(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.contains_key("$include") || map.values().any(has_include),
        Value::Array(list) => list.iter().any(has_include),
        _ => false,
    }
}

fn importable(name: &str, definition: &Value, reject_disabled: bool) -> Result<(), String> {
    normalized_server(name, definition)?;
    if has_include(definition) {
        return Err(format!(
            "Cannot import OpenClaw server '{name}': part of it comes from another file through $include. Keep this server in OpenClaw."
        ));
    }
    if let Some(field) = NOT_IMPORTED
        .iter()
        .find(|field| definition.get(**field).is_some())
    {
        return Err(format!(
            "Cannot import OpenClaw server '{name}': Toolport can't keep its '{field}' setting. Keep this server in OpenClaw, or set up the same behavior in Toolport by hand first."
        ));
    }
    if reject_disabled && disabled(definition) {
        return Err(format!(
            "Cannot move turned-off OpenClaw server '{name}': this path can't keep it turned off. Turn it on in OpenClaw first, or keep it there."
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
            format!("OpenClaw server '{name}' changed since detection; refresh the client list and retry.")
        })?;
        importable(name, definition, migration)?;
    }
    Ok(())
}

/// A Toolport server as an OpenClaw entry: the shared JSON shape, with
/// `transport` in place of the `type` hint for a URL and `cwd` for a command.
pub(super) fn entry_value(entry: &ServerEntry) -> Result<Value, String> {
    if entry.command.is_none() && entry.url.is_none() {
        return Err(format!(
            "'{}' has no command or URL; leaving the OpenClaw config untouched.",
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
            "streamable-http"
        };
        map.insert("transport".into(), Value::String(kind.into()));
    }
    Ok(value)
}

/// OpenClaw settings a user may add to the Toolport entry, kept when Toolport
/// rewrites it.
const GATEWAY_SETTINGS: &[&str] = &[
    "connectionTimeoutMs",
    "requestTimeoutMs",
    "supportsParallelToolCalls",
    "toolFilter",
    "codex",
];

/// A gateway the user turned off stays off when Toolport repairs it, and keeps
/// the OpenClaw settings they gave it.
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
    if root.get("mcp").is_none() {
        root["mcp"] = serde_json::json!({});
    }
    root["mcp"]["servers"] = Value::Object(servers);
    atomic_write_json_config(path, original, &root, "mcp")
}

pub(super) fn edit_gateway(path: &Path, entry: Option<&ServerEntry>) -> Result<(), String> {
    let (root, original) = read_root(path)?;
    let mut servers = server_map(&root)?;
    let mut value = entry.map(entry_value).transpose()?;
    if let Some(value) = &mut value {
        retain_gateway_state(&servers, value);
    }
    let count = servers.len();
    servers.retain(|name, definition| {
        !gateway_edit_matches(name, definition.get("command").and_then(Value::as_str))
    });
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

/// Where OpenClaw keeps its files, resolved as it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dirs {
    /// `OPENCLAW_HOME`, else the user's home.
    pub home: PathBuf,
    /// `OPENCLAW_STATE_DIR`, else `<home>/.openclaw`, or the legacy
    /// `<home>/.clawdbot` when only that one exists.
    pub state: PathBuf,
    /// `OPENCLAW_CONFIG_PATH`, else `<state>/openclaw.json`.
    pub config: PathBuf,
    /// The selected default agent's workspace. Configured agent/default paths
    /// precede OPENCLAW_WORKSPACE_DIR and the state workspace fallback.
    pub workspace: PathBuf,
    /// The default agent and its workspace can be resolved without the CLI cwd.
    pub workspace_known: bool,
    /// Whether the state dir is the default one; OpenClaw reads personal
    /// `~/.agents/skills` only then.
    pub default_state: bool,
    /// A path variable is relative. OpenClaw resolves it against its own
    /// working directory, which Toolport can't know, so the config is unknown.
    pub relative: bool,
}

/// `env` returns a variable trimmed, or `None` when it is unset or blank.
pub(crate) fn dirs(user_home: &Path, env: &dyn Fn(&str) -> Option<String>) -> Dirs {
    let expand_from = |base: &Path, raw: &str| -> Option<PathBuf> {
        let rest = if raw == "~" {
            Some("")
        } else {
            raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        };
        match rest {
            Some(rest) => Some(base.join(rest.trim_start_matches(['/', '\\']))),
            None => {
                let path = PathBuf::from(raw);
                path.is_absolute().then_some(path)
            }
        }
    };
    let relative = [
        "OPENCLAW_HOME",
        "OPENCLAW_STATE_DIR",
        "OPENCLAW_CONFIG_PATH",
    ]
    .iter()
    .filter_map(|key| env(key))
    .any(|raw| expand_from(user_home, &raw).is_none());
    let home = env("OPENCLAW_HOME")
        .and_then(|raw| expand_from(user_home, &raw))
        .unwrap_or_else(|| user_home.to_path_buf());
    let expand = |raw: String| expand_from(&home, &raw);
    let primary = home.join(".openclaw");
    // `OPENCLAW_PROFILE` selects `<home>/.openclaw-<profile>` when no state dir is
    // set. OpenClaw ignores an invalid name and the literal `default`.
    let profile = env("OPENCLAW_PROFILE").filter(|name| openclaw_profile_name(name));
    let profiled = env("OPENCLAW_STATE_DIR").is_none() && profile.is_some();
    let (state, default_state) = match env("OPENCLAW_STATE_DIR").and_then(expand) {
        Some(dir) => {
            let default = dir == primary;
            (dir, default)
        }
        None if profiled => (
            home.join(format!(
                ".openclaw-{}",
                profile.as_deref().unwrap_or_default()
            )),
            false,
        ),
        None => {
            let legacy = home.join(".clawdbot");
            let dir = if !primary.exists() && legacy.exists() {
                legacy
            } else {
                primary
            };
            (dir, true)
        }
    };
    let config = env("OPENCLAW_CONFIG_PATH")
        .and_then(expand)
        .unwrap_or_else(|| {
            // A profile's config is only ever the one in its own state dir.
            if profiled {
                return state.join("openclaw.json");
            }
            // The active config is independent of the selected state directory.
            // Search every existing candidate before creating a canonical file.
            env("OPENCLAW_STATE_DIR")
                .and_then(expand)
                .into_iter()
                .chain([home.join(".openclaw"), home.join(".clawdbot")])
                .flat_map(|dir| [dir.join("openclaw.json"), dir.join("clawdbot.json")])
                .find(|path| path.exists())
                .unwrap_or_else(|| state.join("openclaw.json"))
        });
    let root = match std::fs::read_to_string(&config) {
        Ok(text) => parse_json_value(&text).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(serde_json::json!({})),
        Err(_) => None,
    };
    let resolved_workspace = default_workspace(&home, &state, root.as_ref(), env);
    let workspace_known = resolved_workspace.is_some();
    let workspace = resolved_workspace.unwrap_or_else(|| state.join("workspace"));
    Dirs {
        home,
        state,
        config,
        workspace,
        workspace_known,
        default_state,
        relative,
    }
}

/// OpenClaw's profile name rule, `^[a-z0-9][a-z0-9_-]{0,63}$` (any case), with
/// `default` meaning no profile.
fn openclaw_profile_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !name.eq_ignore_ascii_case("default")
        && (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// Match the packaged agents-list default selection. A fleet without a default,
/// an empty roster, includes or a cwd-dependent path has no writable rules target.
fn default_workspace(
    home: &Path,
    state: &Path,
    root: Option<&Value>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let root = root?;
    if !root.is_object() || root.get("$include").is_some() {
        return None;
    }
    let empty = serde_json::json!({});
    let agents = root.get("agents").unwrap_or(&empty);
    if !agents.is_object() || agents.get("$include").is_some() {
        return None;
    }
    let defaults = agents.get("defaults").unwrap_or(&empty);
    if !defaults.is_object() || defaults.get("$include").is_some() {
        return None;
    }
    let explicit = agents.get("ownership").and_then(Value::as_str) == Some("explicit");
    let mut roster: Vec<(String, &Value)> = Vec::new();
    let has_roster = if let Some(entries) = agents.get("entries") {
        for (id, entry) in entries.as_object()? {
            if !entry.is_object() || entry.get("$include").is_some() {
                return None;
            }
            roster.push((id.trim().to_ascii_lowercase(), entry));
        }
        true
    } else if let Some(entries) = agents.get("list") {
        for entry in entries.as_array()? {
            if entry.get("$include").is_some() {
                return None;
            }
            roster.push((
                entry.get("id")?.as_str()?.trim().to_ascii_lowercase(),
                entry,
            ));
        }
        true
    } else {
        false
    };
    let marked: Vec<_> = roster
        .iter()
        .filter(|(_, entry)| entry.get("default") == Some(&Value::Bool(true)))
        .collect();
    let inherited = roster.len() == 1 || (!explicit && marked.len() == 1) || !has_roster;
    let selected = if explicit {
        let id = defaults
            .get("systemAgent")?
            .get("agentId")?
            .as_str()?
            .trim()
            .to_ascii_lowercase();
        if !has_roster && id == "main" {
            return workspace_path(home, state, defaults, env, None, true);
        }
        roster.iter().find(|(candidate, _)| *candidate == id)?
    } else if marked.len() == 1 {
        marked[0]
    } else if roster.len() == 1 {
        &roster[0]
    } else if !has_roster {
        // The unconfigured legacy/default agent inherits the default workspace.
        return workspace_path(home, state, defaults, env, None, true);
    } else {
        return None;
    };
    if selected.0.is_empty()
        || !selected
            .0
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    if let Some(raw) = selected
        .1
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        return expand_workspace(home, raw);
    }
    workspace_path(home, state, defaults, env, Some(&selected.0), inherited)
}

fn expand_workspace(home: &Path, raw: &str) -> Option<PathBuf> {
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home.join(rest.trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(raw)
    };
    path.is_absolute().then_some(path)
}

fn workspace_path(
    home: &Path,
    state: &Path,
    defaults: &Value,
    env: &dyn Fn(&str) -> Option<String>,
    agent: Option<&str>,
    inherited: bool,
) -> Option<PathBuf> {
    if let Some(raw) = defaults
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        let path = expand_workspace(home, raw)?;
        return Some(if inherited { path } else { path.join(agent?) });
    }
    if !inherited {
        return Some(state.join(format!("workspace-{}", agent?)));
    }
    if let Some(raw) = env("OPENCLAW_WORKSPACE_DIR") {
        let path = PathBuf::from(raw);
        return path.is_absolute().then_some(path);
    }
    Some(if env("OPENCLAW_STATE_DIR").is_some() {
        state.join("workspace")
    } else {
        home.join(".openclaw/workspace")
    })
}

#[cfg(test)]
#[path = "openclaw_tests.rs"]
mod tests;
