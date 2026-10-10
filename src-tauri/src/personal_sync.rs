//! Owner-only configuration sync. Values and approvals remain machine-local unless
//! a nonsecret value is explicitly marked portable. Network work never holds the
//! registry lock; acknowledgements compare the exact queued mutation again.
use crate::registry::{Registry, ServerEntry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

const STATE: &str = "personalSyncState";
thread_local! { static APPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) fn remote_update<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            APPLYING.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(APPLYING.with(|v| v.replace(true)));
    f()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Mutation {
    pub local_id: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub at: i64,
    /// First sign-in found two different definitions without a common ancestor.
    pub initial_conflict: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncState {
    pub baseline: BTreeMap<String, Value>,
    pub pending: BTreeMap<String, Mutation>,
    pub conflicts: BTreeMap<String, Value>,
    pub conflict_versions: BTreeMap<String, String>,
    pub publishing: BTreeMap<String, Mutation>,
    pub publish_errors: BTreeMap<String, String>,
    pub warnings: BTreeMap<String, String>,
    pub choose_local_servers: bool,
    #[serde(skip_serializing)]
    pub last_synced_at: Option<i64>,
    pub error: Option<String>,
    pub sign_in_required: bool,
    pub initialized: bool,
}
pub fn is_personal(reg: &Registry) -> bool {
    reg.team.as_ref().is_some_and(|t| {
        t.role == "admin"
            && t.unknown_fields
                .get("accountStatus")
                .is_some_and(|s| s["personalSync"] == true)
    })
}
pub fn state(reg: &Registry) -> Result<SyncState, String> {
    let mut state: SyncState = reg
        .team
        .as_ref()
        .and_then(|t| t.unknown_fields.get(STATE))
        .map(|v| {
            serde_json::from_value(v.clone()).map_err(|e| format!("Could not read sync state: {e}"))
        })
        .unwrap_or_else(|| Ok(SyncState::default()))?;
    if let Some(team) = &reg.team {
        if let Some(path) = status_path() {
            if let Ok(value) = std::fs::read(&path).and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes).map_err(std::io::Error::other)
            }) {
                if value["connection"] == connection_key(team) {
                    state.last_synced_at = value["lastSyncedAt"].as_i64().or(state.last_synced_at);
                }
            }
        }
    }
    Ok(state)
}
fn status_path() -> Option<std::path::PathBuf> {
    crate::registry::resolved_path().map(|p| p.with_extension("personal-sync-status.json"))
}
fn connection_key(team: &crate::registry::TeamConnection) -> Value {
    json!([team.server_url, team.team_id, team.reporting_device_id])
}
fn mark_synced(team: &crate::registry::TeamConnection, at: i64) -> Result<(), String> {
    let path = status_path().ok_or("Could not resolve sync status path")?;
    crate::registry::atomic_write(
        &path,
        &json!({"connection":connection_key(team),"lastSyncedAt":at}).to_string(),
    )
}
fn save(reg: &mut Registry, state: &SyncState) -> Result<(), String> {
    let mut state = state.clone();
    state.conflict_versions = state
        .conflicts
        .iter()
        .map(|(id, v)| (id.clone(), conflict_version(v)))
        .collect();
    if let Some(team) = &mut reg.team {
        team.unknown_fields.insert(
            STATE.into(),
            serde_json::to_value(&state).map_err(|e| e.to_string())?,
        );
    }
    Ok(())
}
pub fn attach_status(reg: &mut Registry) {
    let at = state(reg).ok().and_then(|st| st.last_synced_at);
    if let Some(st) = reg
        .team
        .as_mut()
        .and_then(|t| t.unknown_fields.get_mut(STATE))
        .and_then(Value::as_object_mut)
    {
        if let Some(at) = at {
            st.insert("lastSyncedAt".into(), json!(at));
        }
    }
}
pub fn conflict_version(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON conflict"))
    )
}

/// Account status is the only mode authority. Private existing definitions are
/// opt-in when a governed team becomes personal, including a later transition.
pub(crate) fn mode_changed(reg: &mut Registry, was_personal: bool) -> Result<(), String> {
    if is_personal(reg) && !was_personal {
        let mut st = state(reg)?;
        if st.initialized {
            if let Some(saved) = reg.unknown_fields.remove("personalSyncSuspendedRows") {
                if let Some(rows) = saved.as_array() {
                    for row in rows {
                        let entry: ServerEntry = serde_json::from_value(row["server"].clone())
                            .map_err(|e| e.to_string())?;
                        if !st.pending.values().any(|m| m.local_id == entry.id) {
                            continue;
                        }
                        reg.servers.retain(|s| s.id != entry.id);
                        for profile in &mut reg.profiles {
                            profile.enabled_server_ids.retain(|id| id != &entry.id);
                            if row["profiles"]
                                .as_array()
                                .is_some_and(|ids| ids.contains(&json!(profile.id)))
                            {
                                profile.enabled_server_ids.push(entry.id.clone());
                            }
                        }
                        reg.servers.push(entry);
                    }
                }
            }
            return Ok(());
        }
        for s in &mut reg.servers {
            if !s.source.as_deref().unwrap_or("").starts_with("team:")
                && !crate::clients::is_gateway_server(s)
            {
                s.unknown_fields.insert("syncLocalOnly".into(), json!(true));
                st.choose_local_servers = true;
            }
        }
        save(reg, &st)?;
    } else if was_personal && !is_personal(reg) {
        // The governed pull may replace rows, but cannot erase unpublished
        // personal edits if the status endpoint later returns to personal mode.
        let st = state(reg)?;
        let rows: Vec<_> = reg.servers.iter().filter(|s| st.pending.values().any(|m| m.local_id == s.id))
            .map(|s| json!({"server":s,"profiles":reg.profiles.iter().filter(|p| p.enabled_server_ids.contains(&s.id)).map(|p| &p.id).collect::<Vec<_>>()})).collect();
        reg.unknown_fields
            .insert("personalSyncSuspendedRows".into(), json!(rows));
        // Force a governed pull even when the cloud version did not change.
        if let Some(t) = &mut reg.team {
            t.last_etag = Some("\"mode-transition\"".into());
            t.unknown_fields
                .insert("personalSyncGovernanceTransition".into(), json!(true));
            t.unknown_fields.remove("memberReview");
        }
    }
    Ok(())
}

pub fn finish_local_selection() -> Result<Registry, String> {
    crate::registry::update(|r| {
        let mut st = state(r)?;
        st.choose_local_servers = false;
        save(r, &st)
    })
    .map(|(r, ())| r)
}

/// Remote execution overrides are refused regardless of value, portability or
/// secret-reference source. Local launchers retain the normal spawn screening.
pub fn risky_sync_env(key: &str) -> bool {
    let key = key.trim().to_ascii_uppercase();
    key.starts_with("GIT_CONFIG_")
        || key.starts_with("DYLD_")
        || key.starts_with("NPM_CONFIG_")
        || key.starts_with("UV_INDEX")
        || key.starts_with("UV_PYTHON")
        || [
            "PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "NODE_OPTIONS",
            "NODE_PATH",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "PERL5OPT",
            "PERL5LIB",
            "RUBYOPT",
            "RUBYLIB",
            "PYTHONHOME",
            "UV_INDEX",
            "PIP_CONFIG_FILE",
            "NODE_EXTRA_CA_CERTS",
            "LD_DEBUG",
            "LD_PROFILE",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_COUNT",
            "GIT_EXEC_PATH",
            "PYTHONUSERBASE",
            "PYTHONINSPECT",
            "PIP_INDEX_URL",
            "PIP_EXTRA_INDEX_URL",
            "UV_INDEX_URL",
            "UV_EXTRA_INDEX_URL",
            "UV_DEFAULT_INDEX",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_ASKPASS",
            "GIT_PROXY_COMMAND",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "PIP_FIND_LINKS",
            "PIP_TRUSTED_HOST",
            "UV_FIND_LINKS",
            "UV_INSECURE_HOST",
            "DOCKER_HOST",
            "RUSTC_WRAPPER",
            "BASH_ENV",
            "ENV",
            "ZDOTDIR",
            "GCONV_PATH",
        ]
        .contains(&key.as_str())
}
fn publish_error(value: &Value) -> Option<String> {
    if value["url"].as_str().is_some_and(credential_url) {
        return Some("This URL contains credentials. Use local authentication or keep this server on this machine only. The URL has not been changed.".into());
    }
    let args: Vec<String> = value["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let known = credential_arg_mask(&args);
    if let Some(index) = args.iter().enumerate().find_map(|(i, arg)| {
        let text = arg
            .split_once('=')
            .filter(|(name, _)| {
                *name == "-H"
                    || matches!(name.to_ascii_lowercase().as_str(), "--header" | "--headers")
            })
            .map_or(arg.as_str(), |(_, value)| value);
        (!known[i] && crate::registry::arg_looks_secret(text)).then_some(i)
    }) {
        return Some(format!("Argument {} may contain a credential. Review it or keep this server on this machine only. Its value has not been changed.", index + 1));
    }
    if env_references(value) {
        return Some("env: references cannot sync. Choose a password manager reference or keep this server on this machine only.".into());
    }
    value["env"].as_array().into_iter().flatten().find_map(|e| {
        e["key"]
            .as_str()
            .filter(|key| risky_sync_env(key))
            .map(|key| {
                format!(
                    "Execution environment {} cannot sync. Keep this server on this machine only.",
                    visible_text(key)
                )
            })
    })
}

pub fn visible_text(text: &str) -> String {
    text.chars().map(|c| {
        if c.is_control() || matches!(c as u32, 0x00ad | 0x034f | 0x061c | 0x115f..=0x1160 | 0x17b4..=0x17b5 | 0x180b..=0x180f | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x206f | 0x3164 | 0xfe00..=0xfe0f | 0xfeff | 0xffa0 | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xfff9..=0xfffb | 0xe0000..=0xe0fff) {
            format!("\\u{{{:04X}}}", c as u32)
        } else { c.to_string() }
    }).collect()
}
fn argument_values(bindings: &[crate::registry::ArgBinding]) -> String {
    bindings
        .iter()
        .map(|binding| {
            let parts = binding
                .parts
                .iter()
                .map(|part| match part {
                    crate::registry::ArgPart::Literal { value, .. } => visible_text(value),
                    crate::registry::ArgPart::Input { key, .. } => {
                        format!("{{{}}}", visible_text(key))
                    }
                })
                .collect::<String>();
            format!("Argument {} = {parts}", binding.index + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
// Read earlier display snapshots using today's labels without changing consent.
fn review_baseline(previous: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<String, String> = previous
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect();
    if !fields.contains_key("Transport") && !fields.contains_key("inheritEnv") {
        return fields;
    }
    let stdio = fields.remove("Transport").as_deref() == Some("stdio")
        || fields
            .get("Command")
            .is_some_and(|v| !matches!(v.as_str(), "null" | "[]" | ""));
    for key in ["Command", "URL", "Arguments"] {
        if fields
            .get(key)
            .is_some_and(|v| matches!(v.as_str(), "null" | "[]" | ""))
        {
            fields.remove(key);
        }
    }
    if let Some(args) = fields
        .get("Arguments")
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
    {
        fields.insert(
            "Arguments".into(),
            args.iter()
                .enumerate()
                .map(|(i, arg)| format!("\n  {}. {}", i + 1, visible_text(arg)))
                .collect(),
        );
    }
    if let Some(cwd) = fields
        .remove("Working directory")
        .filter(|v| !v.is_empty() && v != "Client default")
    {
        fields.insert("Working folder".into(), cwd);
    }
    let inherited = fields.remove("inheritEnv").as_deref() == Some("true");
    if stdio {
        fields.insert(
            "Uses this machine's environment".into(),
            if inherited { "yes" } else { "no" }.into(),
        );
    }
    if let Some(bindings) = fields
        .remove("Launch bindings")
        .filter(|v| v != "null" && v != "[]" && !v.is_empty())
    {
        fields.insert(
            "Argument values".into(),
            serde_json::from_str::<Vec<crate::registry::ArgBinding>>(&bindings)
                .map(|b| argument_values(&b))
                .unwrap_or(bindings),
        );
    }
    for (key, value) in fields.clone() {
        let label = if key.starts_with("Environment [") {
            "Environment"
        } else if key.starts_with("Launch input [") {
            "Input"
        } else {
            continue;
        };
        let Some((_, name)) = key.split_once("] ") else {
            continue;
        };
        let (local, reference) = value
            .rsplit_once("; reference: ")
            .unwrap_or((&value, "null"));
        let value = if reference != "null" {
            format!("Password manager: {reference}")
        } else if local == "null" {
            "Set on this machine".into()
        } else {
            local.to_string()
        };
        fields.insert(format!("{label}: {name}"), value);
        fields.remove(&key);
    }
    fields
}
/// Plain text only, for both native review entry points. Secret values are
/// masked, but their names and references always remain visible.
pub fn execution_review_fields(server: &ServerEntry) -> BTreeMap<String, String> {
    let v = json!(server);
    let show = |v: &Value| {
        if let Some(s) = v.as_str() {
            visible_text(s)
        } else {
            visible_text(&v.to_string())
        }
    };
    let mut fields = BTreeMap::new();
    // The gateway launches any row with a command, whatever its transport says.
    if server.transport == "stdio" || server.command.is_some() {
        if let Some(command) = &server.command {
            fields.insert("Command".into(), visible_text(command));
        }
        if !server.args.is_empty() {
            fields.insert(
                "Arguments".into(),
                server
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, arg)| format!("\n  {}. {}", i + 1, visible_text(arg)))
                    .collect(),
            );
        }
        if let Some(cwd) = &server.cwd {
            fields.insert("Working folder".into(), visible_text(cwd));
        }
        fields.insert(
            "Uses this machine's environment".into(),
            if server.inherit_env { "yes" } else { "no" }.into(),
        );
        if let Some(launch) = &server.launch {
            if !launch.bindings.is_empty() {
                fields.insert("Argument values".into(), argument_values(&launch.bindings));
            }
        }
    }
    if let Some(url) = &server.url {
        fields.insert("URL".into(), visible_text(url));
    }
    for (label, path) in [
        ("Environment", "/env"),
        ("Input", "/launch/inputs"),
        ("Header", "/headerKeys"),
    ] {
        for row in v
            .pointer(path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let reference = row["source"]["ref"].as_str();
            let value = if label == "Header" && row["env"].is_string() {
                format!("Uses environment: {}", show(&row["env"]))
            } else if let Some(reference) = reference {
                format!("Password manager: {}", visible_text(reference))
            } else if row["secret"] == true {
                "<masked secret>".into()
            } else if let Some(value) = row.get("value").filter(|v| !v.is_null()) {
                show(value)
            } else {
                "Set on this machine".into()
            };
            fields.insert(format!("{label}: {}", show(&row["key"])), value);
        }
    }
    fields
}
pub fn review_field_line(key: &str, value: &str) -> String {
    if key.starts_with("Environment:") || key.starts_with("Input:") || key.starts_with("Header:") {
        format!("{key} = {value}")
    } else {
        format!("{key}: {value}")
    }
}
pub fn execution_review_lines(server: &ServerEntry) -> Vec<String> {
    let previous = server
        .unknown_fields
        .get("syncExecutionReview")
        .and_then(Value::as_object)
        .map(review_baseline);
    let fields = execution_review_fields(server);
    let mut lines = Vec::new();
    if previous.is_none() {
        lines.push("New server".into());
    }
    for (key, value) in &fields {
        if previous.as_ref().is_none_or(|p| p.get(key) != Some(value)) {
            lines.push(review_field_line(key, value));
        }
    }
    if let Some(previous) = &previous {
        for key in previous.keys().filter(|key| !fields.contains_key(*key)) {
            lines.push(review_field_line(key, "Removed"));
        }
    }
    if server.unknown_fields.get("personalSyncArgsReview") == Some(&json!(true)) {
        lines.push(ARGS_REVIEW_LINE.into());
    }
    for n in missing_secret_args(server) {
        lines.push(format!("Argument {n} is a secret that does not sync. Edit this server and enter it on this machine before enabling."));
    }
    // Consent is to a reference reaching a destination, so this stays visible
    // even when neither the reference nor the destination changed.
    let references: Vec<String> = crate::secret_refs::destination_lines(server)
        .iter()
        .map(|line| visible_text(line))
        .collect();
    if previous.is_some() && lines.is_empty() {
        lines.push(
            if references.is_empty() {
                "Nothing in this definition changed. Confirm it to run it on this machine."
            } else {
                "Approve these password manager entries for this machine."
            }
            .into(),
        );
    }
    lines.extend(references);
    lines
}
pub const ARGS_REVIEW_LINE: &str = "Arguments changed on another machine and could not be matched to the secret values saved here. Toolport kept this machine's arguments. Check them before enabling.";
/// One-based argument positions whose secret value never synced to this machine.
pub fn missing_secret_args(server: &ServerEntry) -> Vec<usize> {
    if server.unknown_fields.get("personalSyncEntry") != Some(&json!(true)) {
        return Vec::new();
    }
    let mut missing: Vec<usize> = server
        .args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.contains("<redacted>"))
        .map(|(i, _)| i + 1)
        .collect();
    for binding in server.launch.iter().flat_map(|l| &l.bindings) {
        if binding.parts.iter().any(|part| {
            matches!(part, crate::registry::ArgPart::Literal { value, .. } if value.contains("<redacted>"))
        }) {
            missing.push(binding.index + 1);
        }
    }
    missing.sort_unstable();
    missing.dedup();
    missing
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub fn keep_local(s: &ServerEntry) -> bool {
    s.unknown_fields.get("syncLocalOnly") == Some(&json!(true))
}
fn original(s: &ServerEntry) -> &str {
    s.unknown_fields
        .get("teamOriginalId")
        .and_then(Value::as_str)
        .unwrap_or(&s.id)
}
fn eligible(s: &ServerEntry) -> bool {
    !keep_local(s) && !crate::clients::is_gateway_server(s)
}
fn reference(value: &Value) -> Option<&str> {
    value.get("source")?.get("ref")?.as_str()
}
fn env_references(value: &Value) -> bool {
    match value {
        Value::Object(m) => {
            reference(value).is_some_and(|r| r.starts_with("env:"))
                || m.values().any(env_references)
        }
        Value::Array(a) => a.iter().any(env_references),
        _ => false,
    }
}
fn credential_url(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|u| {
        !u.username().is_empty()
            || u.password().is_some()
            || u.query_pairs().any(|(key, _)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "token"
                        | "access_token"
                        | "api_key"
                        | "apikey"
                        | "api-key"
                        | "password"
                        | "secret"
                        | "key"
                        | "auth"
                        | "authorization"
                        | "signature"
                )
            })
    })
}
// Personal sync cannot use the deliberately broad sharing mask: its false
// positives would become installed values on another machine.
fn credential_arg_mask(args: &[String]) -> Vec<bool> {
    let credential_name = |name: &str| {
        matches!(
            name.trim_start_matches('-').to_ascii_lowercase().as_str(),
            "api-key"
                | "apikey"
                | "api_key"
                | "token"
                | "auth"
                | "auth-token"
                | "auth_token"
                | "access-token"
                | "access_token"
                | "password"
                | "pwd"
                | "secret"
                | "bearer"
                | "client-secret"
                | "client_secret"
                | "credential"
                | "accountkey"
        )
    };
    let header = |value: &str| {
        value.split_once(':').is_some_and(|(name, value)| {
            !value.trim().is_empty()
                && matches!(
                    name.trim().to_ascii_lowercase().as_str(),
                    "authorization"
                        | "proxy-authorization"
                        | "x-api-key"
                        | "x-auth-token"
                        | "x-goog-api-key"
                        | "api-key"
                        | "cookie"
                        | "private-token"
                )
        })
    };
    let header_flag = |name: &str| {
        name == "-H" || matches!(name.to_ascii_lowercase().as_str(), "--header" | "--headers")
    };
    let mut mask = vec![false; args.len()];
    for (index, arg) in args.iter().enumerate() {
        let arg = arg.trim();
        let (name, value) = arg
            .split_once('=')
            .map_or((arg, None), |(name, value)| (name, Some(value)));
        if header(arg) || credential_url(arg) {
            mask[index] = true;
        }
        if credential_name(name) {
            if value.is_some_and(|v| !v.is_empty()) {
                mask[index] = true;
            } else if arg.starts_with('-') && index + 1 < args.len() {
                mask[index + 1] = true;
            }
        } else if header_flag(name) {
            if let Some(value) = value {
                mask[index] |= header(value);
            } else if let Some(value) = args.get(index + 1) {
                mask[index + 1] |= header(value);
            }
        }
    }
    mask
}
fn portable(value: &Value) -> bool {
    value["secret"] == false && value["portable"] == true && reference(value).is_none()
}
fn input_export(mut input: Value) -> Value {
    let value = if portable(&input) {
        input.get("value").cloned()
    } else {
        None
    };
    if let Some(map) = input.as_object_mut() {
        // Approval and local override metadata can never leave this machine.
        map.retain(|key, _| {
            ["key", "label", "required", "secret", "portable", "source"].contains(&key.as_str())
        });
        if let Some(r) = map
            .get("source")
            .and_then(|s| s["ref"].as_str())
            .map(str::to_string)
        {
            map.insert("source".into(), json!({"ref": r}));
        } else {
            map.remove("source");
        }
        if map.get("secret") == Some(&json!(true)) || map.contains_key("source") {
            map.remove("secret");
        }
        if let Some(value) = value {
            map.insert("value".into(), value);
        }
    }
    input
}
pub fn export(s: &ServerEntry) -> Value {
    let mut v = json!(s);
    let args: Vec<_> = s
        .args
        .iter()
        .zip(credential_arg_mask(&s.args))
        .map(|(arg, secret)| {
            if secret && arg != "<launch-input>" {
                arg.split_once('=')
                    .filter(|(name, _)| name.starts_with('-'))
                    .map_or_else(
                        || "<redacted>".to_string(),
                        |(name, _)| format!("{name}=<redacted>"),
                    )
            } else {
                arg.clone()
            }
        })
        .collect();
    let map = v.as_object_mut().unwrap();
    map.retain(|k, _| {
        [
            "id",
            "name",
            "transport",
            "command",
            "args",
            "launch",
            "env",
            "url",
            "cwd",
            "disabledTools",
            "clientCredentials",
            "requestTimeoutMs",
            "initializeTimeoutMs",
            "headerKeys",
        ]
        .contains(&k.as_str())
    });
    v["id"] = json!(original(s));
    let intended_enabled = if s.needs_team_enable_review() {
        s.unknown_fields
            .get("personalSyncDesiredEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(s.enabled)
    } else {
        s.enabled
    };
    v["disabled"] = json!(!intended_enabled);
    v["args"] = json!(args);
    v["env"] = json!(s
        .env
        .iter()
        .map(|e| input_export(json!(e)))
        .collect::<Vec<_>>());
    if let Some(c) = v.get_mut("clientCredentials") {
        if let Ok(mut cc) = serde_json::from_value::<crate::registry::ClientCredentials>(c.clone())
        {
            cc.strip_secret_fields();
            *c = json!(cc);
        }
    }
    if let Some(inputs) = v
        .pointer_mut("/launch/inputs")
        .and_then(Value::as_array_mut)
    {
        for input in inputs {
            *input = input_export(input.clone());
        }
    }
    if let Some(bindings) = v
        .pointer_mut("/launch/bindings")
        .and_then(Value::as_array_mut)
    {
        let rendered: Vec<String> = bindings
            .iter()
            .map(|b| {
                b["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        if p["kind"] == "literal" {
                            p["value"].as_str().unwrap_or("").to_string()
                        } else {
                            "<launch-input>".into()
                        }
                    })
                    .collect::<String>()
            })
            .collect();
        let mut effective = s.args.clone();
        for (b, text) in bindings.iter().zip(&rendered) {
            if let Some(i) = b["index"]
                .as_u64()
                .and_then(|i| effective.get_mut(i as usize))
            {
                *i = text.clone();
            }
        }
        let mask = credential_arg_mask(&effective);
        for b in bindings {
            let only_literals = b["parts"]
                .as_array()
                .is_some_and(|parts| parts.iter().all(|p| p["kind"] == "literal"));
            if only_literals
                && b["index"]
                    .as_u64()
                    .is_some_and(|i| mask.get(i as usize) == Some(&true))
            {
                for part in b["parts"].as_array_mut().into_iter().flatten() {
                    part["value"] = json!("<redacted>");
                }
            }
        }
    }
    if let Some(headers) = v.get_mut("headerKeys").and_then(Value::as_array_mut) {
        for h in headers {
            if let Some(m) = h.as_object_mut() {
                m.retain(|k, _| ["key", "env", "source"].contains(&k.as_str()));
                if let Some(r) = m
                    .get("source")
                    .and_then(|s| s["ref"].as_str())
                    .map(str::to_string)
                {
                    m.insert("source".into(), json!({"ref": r}));
                } else {
                    m.remove("source");
                }
            }
        }
    }
    // A local password-manager override must never replace the synced reference.
    if let Some(overrides) = s
        .unknown_fields
        .get("memberSecretRefs")
        .and_then(Value::as_object)
    {
        for location in overrides.keys() {
            let saved = s
                .unknown_fields
                .get("personalSyncRemoteRefs")
                .and_then(|m| m.get(location))
                .cloned();
            if let Some((field, key)) = location.split_once(':') {
                let path = match field {
                    "env" => "/env",
                    "input" => "/launch/inputs",
                    "header" => "/headerKeys",
                    _ => continue,
                };
                for input in v
                    .pointer_mut(path)
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                    .filter(|i| i["key"] == key)
                {
                    if let Some(source) = saved.clone().filter(|v| !v.is_null()) {
                        input["source"] = source;
                    } else if let Some(m) = input.as_object_mut() {
                        m.remove("source");
                    }
                }
            }
        }
    }
    v
}
fn definition(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut value = value.clone();
    if let Some(m) = value.as_object_mut() {
        m.retain(|k, _| {
            [
                "id",
                "name",
                "transport",
                "command",
                "args",
                "launch",
                "env",
                "url",
                "cwd",
                "disabledTools",
                "clientCredentials",
                "requestTimeoutMs",
                "initializeTimeoutMs",
                "headerKeys",
                "disabled",
            ]
            .contains(&k.as_str())
        });
        m.retain(|_, v| !v.is_null());
        m.entry("disabled").or_insert(json!(false));
        for k in ["args", "env"] {
            m.entry(k).or_insert(json!([]));
        }
    }
    value
}
fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    definition(a.unwrap_or(&Value::Null)) == definition(b.unwrap_or(&Value::Null))
}
fn definitions(reg: &Registry) -> BTreeMap<String, (String, Value)> {
    reg.servers
        .iter()
        .filter(|s| eligible(s))
        .map(|s| (original(s).to_string(), (s.id.clone(), export(s))))
        .collect()
}
/// Called inside the registry's cross-process mutation lock. This journals both
/// shells, imports and edits without doing network I/O on a UI thread.
pub(crate) fn record(before: &Registry, reg: &mut Registry) -> Result<(), String> {
    if APPLYING.with(|v| v.get()) || !is_personal(before) || !is_personal(reg) {
        return Ok(());
    }
    let mut st = state(reg)?;
    for server in &mut reg.servers {
        if server.enabled
            && before
                .servers
                .iter()
                .any(|s| s.id == server.id && !s.enabled)
        {
            if let Some(ids) = server
                .unknown_fields
                .remove("personalSyncProfiles")
                .and_then(|v| v.as_array().cloned())
            {
                for profile in &mut reg.profiles {
                    if ids.contains(&json!(profile.id))
                        && !profile.enabled_server_ids.contains(&server.id)
                    {
                        profile.enabled_server_ids.push(server.id.clone());
                    }
                }
            }
        }
    }
    let mut linked: HashSet<String> = reg
        .servers
        .iter()
        .filter(|s| before.servers.iter().any(|old| old.id == s.id))
        .map(|s| original(s).to_string())
        .collect();
    for server in reg
        .servers
        .iter_mut()
        .filter(|s| eligible(s) && !before.servers.iter().any(|old| old.id == s.id))
    {
        let matches: Vec<_> = st
            .baseline
            .iter()
            .filter(|(_, v)| {
                v["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&server.name))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if matches.len() == 1 && !linked.contains(&matches[0]) {
            server
                .unknown_fields
                .insert("teamOriginalId".into(), json!(matches[0]));
        }
        if linked.contains(original(server)) {
            let id = crate::registry::unique_id(
                &server.id,
                &st.baseline
                    .keys()
                    .chain(linked.iter())
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            server
                .unknown_fields
                .insert("teamOriginalId".into(), json!(id));
        }
        linked.insert(original(server).to_string());
    }
    let a = definitions(before);
    let b = definitions(reg);
    for id in a.keys().chain(b.keys()).collect::<HashSet<_>>() {
        if a.get(id).map(|(_, v)| v) == b.get(id).map(|(_, v)| v) {
            continue;
        }
        // Opting out leaves the cloud definition intact for other machines.
        if reg
            .servers
            .iter()
            .any(|s| original(s) == id.as_str() && keep_local(s))
        {
            st.pending.remove(id);
            st.conflicts.remove(id);
            st.publish_errors.remove(id);
            continue;
        }
        let after = b.get(id).map(|(_, v)| v.clone());
        let base = st
            .pending
            .get(id)
            .map(|m| m.before.clone())
            .unwrap_or_else(|| st.baseline.get(id).cloned());
        let opted_in = before
            .servers
            .iter()
            .any(|s| original(s) == id.as_str() && keep_local(s));
        let initial_conflict = st.pending.get(id).is_some_and(|m| m.initial_conflict)
            || (opted_in
                && st.baseline.contains_key(id)
                && !same(st.baseline.get(id), after.as_ref()));
        if initial_conflict {
            st.conflicts.insert(
                id.clone(),
                st.baseline.get(id).cloned().unwrap_or(Value::Null),
            );
        }
        let local_id = b.get(id).or_else(|| a.get(id)).unwrap().0.clone();
        if same(base.as_ref(), after.as_ref()) && !st.publishing.contains_key(id) {
            st.pending.remove(id);
            st.conflicts.remove(id);
        } else {
            st.pending.insert(
                id.clone(),
                Mutation {
                    local_id,
                    before: base,
                    after,
                    at: now(),
                    initial_conflict,
                },
            );
        }
    }
    save(reg, &st)
}
fn index(config: &Value) -> Result<BTreeMap<String, Value>, String> {
    let mut entries = BTreeMap::new();
    for v in config["servers"]
        .as_array()
        .ok_or("Sync response has no server list")?
    {
        let id = v["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("Sync response has an unnamed server")?;
        if entries.insert(id.into(), v.clone()).is_some() {
            return Err("Sync response contains duplicate server identities".into());
        }
    }
    Ok(entries)
}
/// Three-way, per-server merge: unrelated remote edits are preserved. Same-server
/// edits are explicit conflicts, including edit/delete. Equality acknowledges a
/// lost successful PUT response without replaying or prompting.
pub fn merge(
    remote: &Value,
    pending: &BTreeMap<String, Mutation>,
) -> Result<(Value, BTreeMap<String, Value>), String> {
    let mut servers = index(remote)?;
    let mut conflicts = BTreeMap::new();
    for (id, m) in pending {
        let current = servers.get(id).cloned();
        if (m.initial_conflict || !same(current.as_ref(), m.before.as_ref()))
            && !same(current.as_ref(), m.after.as_ref())
        {
            conflicts.insert(id.clone(), current.unwrap_or(Value::Null));
            continue;
        }
        match &m.after {
            Some(after) => {
                let mut updated = servers.get(id).cloned().unwrap_or(json!({}));
                let keys = definition(&updated)
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys())
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(m) = updated.as_object_mut() {
                    for key in keys {
                        m.remove(&key);
                    }
                }
                if let (Some(m), Some(a)) = (updated.as_object_mut(), after.as_object()) {
                    m.extend(a.clone());
                }
                servers.insert(id.clone(), updated);
            }
            None => {
                servers.remove(id);
            }
        }
    }
    let mut config = remote.clone();
    config["servers"] = json!(servers.into_values().collect::<Vec<_>>());
    Ok((config, conflicts))
}
pub(crate) fn command_identity(v: &Value) -> Value {
    // Labels and timeout/tool metadata do not change what executes. Portable
    // values, references and launch bindings do, and remain in exact consent.
    let inputs = |values: &Value| {
        values
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|i| json!({"key": i["key"], "value": i["value"], "source": i["source"]}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    json!({"command":v["command"], "args":v["args"], "cwd":v["cwd"],
        "transport":v["transport"], "bindings":v["launch"]["bindings"],
        "inputs":inputs(&v["launch"]["inputs"]), "env":inputs(&v["env"])})
}
fn execution_changed(before: Option<&Value>, after: &Value) -> bool {
    let command = after["transport"] == "stdio" || after["command"].is_string();
    command && before.is_none_or(|b| command_identity(b) != command_identity(after))
}
pub(crate) fn restore_local(entry: &mut ServerEntry, old: &ServerEntry) {
    entry.inherit_env = old.inherit_env; // Never import ambient-env consent.
                                         // Preserve masked arguments on their originating machine; masking is a wire
                                         // boundary, not permission to erase the owner's installed setup.
    let old_wire = export(old);
    // When the layout moved, a masked value is identified by its flag or name
    // prefix, never by a shifted position. Ambiguous edits retain the complete
    // installed invocation.
    let incoming = entry.args.clone();
    let wire: Vec<String> = serde_json::from_value(old_wire["args"].clone()).unwrap_or_default();
    // Same layout as this machine's own wire form: every unmasked token equal
    // and in place. Restore by position, so repeated flags or prefixes are not
    // ambiguous. Older peers masked a whole token, including `--token=X`, as a
    // bare `<redacted>`; that slot still holds this machine's own value.
    let aligned = incoming.len() == wire.len()
        && incoming
            .iter()
            .zip(&wire)
            .zip(&old.args)
            .all(|((arg, masked), local)| {
                if !arg.contains("<redacted>") {
                    return arg == masked;
                }
                arg == "<redacted>"
                    || arg == masked
                    || arg.split_once('=').is_some_and(|(key, _)| {
                        local
                            .split_once('=')
                            .is_some_and(|(local_key, _)| key == local_key)
                    })
            });
    let mut ambiguous = false;
    if aligned {
        for (arg, local) in entry.args.iter_mut().zip(&old.args) {
            if arg.contains("<redacted>") {
                *arg = local.clone();
            }
        }
    }
    for (index, arg) in entry.args.iter_mut().enumerate() {
        if aligned || !arg.contains("<redacted>") {
            continue;
        }
        let flag = incoming
            .get(index.wrapping_sub(1))
            .filter(|v| v.starts_with('-') && !v.contains('='));
        let prefix = arg.split_once('=').map(|(key, _)| key);
        let candidates: Vec<_> = wire
            .iter()
            .enumerate()
            .filter(|(i, masked)| {
                if !masked.contains("<redacted>") {
                    return false;
                }
                if let Some(prefix) = prefix {
                    return old.args[*i]
                        .split_once('=')
                        .is_some_and(|(key, _)| key == prefix);
                }
                if let Some(flag) = flag {
                    return old.args.get(i.wrapping_sub(1)) == Some(flag);
                }
                incoming == wire && *i == index
            })
            .map(|(i, _)| i)
            .collect();
        if candidates.len() == 1 {
            *arg = old.args[candidates[0]].clone();
        } else {
            ambiguous = true;
        }
    }
    if ambiguous {
        entry.args = old.args.clone();
        entry.launch = old.launch.clone();
        // A reviewed layout stays approved until the synced arguments change again.
        if old.unknown_fields.get("personalSyncArgsApproved") != Some(&json!(incoming)) {
            entry
                .unknown_fields
                .insert("personalSyncArgsReview".into(), json!(true));
            entry
                .unknown_fields
                .insert("personalSyncArgsIncoming".into(), json!(incoming));
        }
    }
    if let (Some(installed), Some(previous)) = (&mut entry.launch, &old.launch) {
        for binding in &mut installed.bindings {
            if let Some(old_binding) = previous.bindings.iter().find(|b| b.index == binding.index) {
                for (part, old_part) in binding.parts.iter_mut().zip(&old_binding.parts) {
                    if let (
                        crate::registry::ArgPart::Literal { value, .. },
                        crate::registry::ArgPart::Literal {
                            value: old_value, ..
                        },
                    ) = (part, old_part)
                    {
                        if value == "<redacted>" {
                            *value = old_value.clone();
                        }
                    }
                }
            }
        }
    }
    // Keep the cloud references for export before upstream restores local overrides.
    if let Some(overrides) = old
        .unknown_fields
        .get("memberSecretRefs")
        .and_then(Value::as_object)
    {
        let wire = json!(entry);
        let mut remote_refs = serde_json::Map::new();
        for location in overrides.keys() {
            let Some((field, key)) = location.split_once(':') else {
                continue;
            };
            let path = match field {
                "env" => "/env",
                "input" => "/launch/inputs",
                "header" => "/headerKeys",
                _ => continue,
            };
            if let Some(input) = wire
                .pointer(path)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|i| i["key"] == key)
            {
                remote_refs.insert(
                    location.clone(),
                    input.get("source").cloned().unwrap_or(Value::Null),
                );
            }
        }
        entry
            .unknown_fields
            .insert("personalSyncRemoteRefs".into(), json!(remote_refs));
    }
    crate::teams::restore_local_references(entry, old);
    for input in &mut entry.env {
        if !portable(&json!(input)) && !input.secret && reference(&json!(input)).is_none() {
            input.value = old
                .env
                .iter()
                .find(|i| i.key == input.key && !i.secret)
                .and_then(|i| i.value.clone());
        }
    }
    if old_wire["launch"]["bindings"] == json!(entry.launch.as_ref().map(|l| &l.bindings)) {
        if let (Some(installed), Some(previous)) = (&mut entry.launch, &old.launch) {
            installed.bindings = previous.bindings.clone();
        }
    }
    for input in entry.launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if !portable(&json!(input)) && !input.secret && reference(&json!(input)).is_none() {
            input.value = old
                .launch
                .iter()
                .flat_map(|l| &l.inputs)
                .find(|i| i.key == input.key && !i.secret)
                .and_then(|i| i.value.clone());
        }
    }
}
fn stop_blocked(reg: &mut Registry, tag: &str, original_id: &str) {
    for s in reg
        .servers
        .iter_mut()
        .filter(|s| s.source.as_deref() == Some(tag) && original(s) == original_id)
    {
        s.enabled = false;
        s.require_team_enable_review();
        for p in &mut reg.profiles {
            p.enabled_server_ids.retain(|id| id != &s.id);
        }
    }
}
/// Merge personal definitions directly while retaining machine-local command and
/// credential consent. No new command or reference is resolved during this step.
pub fn apply(
    reg: &mut Registry,
    config: &Value,
    version: i64,
) -> Result<crate::teams::MergeOutcome, String> {
    let remote = index(config)?;
    let mut st = state(reg)?;
    let team_id = reg
        .team
        .as_ref()
        .ok_or("Sign in to sync first")?
        .team_id
        .clone();
    let tag = format!("team:{team_id}");
    let mut outcome = crate::teams::MergeOutcome::default();
    let own_approved: HashSet<String> = st
        .publishing
        .iter()
        .filter(|(id, sent)| {
            same(remote.get(*id), sent.after.as_ref())
                && st.pending.get(*id) == Some(*sent)
                && reg.servers.iter().any(|s| {
                    s.id == sent.local_id
                        && s.unknown_fields.get("teamEnableReview") != Some(&json!(true))
                        && same(Some(&export(s)), sent.after.as_ref())
                        && crate::secret_refs::check_approval(s).is_ok()
                })
        })
        .map(|(id, _)| id.clone())
        .collect();
    acknowledge(reg, &mut st, &remote);
    if !st.initialized {
        crate::local_auth::adopt_personal_sync_routes(reg)?;
        // Bind by explicit identity first, then by a unique display name. Recreating
        // the same named server does not produce a second cloud definition.
        let mut linked = HashSet::new();
        // Reserve explicit identities before display-name matching, regardless
        // of local row order. A sibling must never steal an existing binding.
        let mut owners = BTreeMap::new();
        for local in reg.servers.iter().filter(|s| eligible(s)) {
            if remote.contains_key(original(local)) {
                owners
                    .entry(original(local).to_string())
                    .or_insert_with(|| local.id.clone());
            }
        }
        for local in reg.servers.iter_mut().filter(|s| {
            eligible(s) && s.unknown_fields.get("teamRouteRemoved") != Some(&json!(true))
        }) {
            let exact = remote
                .get_key_value(original(local))
                .filter(|(id, _)| owners.get(*id) == Some(&local.id));
            let matches: Vec<_> = if let Some(exact) = exact {
                vec![exact]
            } else {
                remote
                    .iter()
                    .filter(|(id, _)| owners.get(*id).is_none_or(|owner| owner == &local.id))
                    .filter(|(_, v)| {
                        v["name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&local.name))
                    })
                    .collect()
            };
            if matches.len() == 1 && !linked.contains(matches[0].0) {
                if matches[0].1["disabled"] == true {
                    local.enabled = false;
                }
                local
                    .unknown_fields
                    .insert("teamOriginalId".into(), json!(matches[0].0));
            }
            // A name already adopted by a sibling cannot share its cloud id.
            if linked.contains(original(local))
                || (remote.contains_key(original(local)) && matches.len() != 1)
            {
                local.unknown_fields.insert(
                    "teamOriginalId".into(),
                    json!(crate::registry::unique_id(
                        &local.id,
                        &remote
                            .keys()
                            .chain(linked.iter())
                            .cloned()
                            .collect::<Vec<_>>()
                    )),
                );
            }
            let id = original(local).to_string();
            linked.insert(id.clone());
            let after = export(local);
            if !same(remote.get(&id), Some(&after))
                && !local.source.as_deref().unwrap_or("").starts_with("team:")
            {
                st.pending.insert(
                    id.clone(),
                    Mutation {
                        local_id: local.id.clone(),
                        // With no common baseline, different existing local and
                        // remote definitions require an explicit conflict choice.
                        before: remote.get(&id).cloned(),
                        initial_conflict: remote.contains_key(&id),
                        after: Some(after),
                        at: now(),
                    },
                );
            }
        }
        st.initialized = true;
    }
    // A conflict preserves the edited definition, not permission to keep a
    // remotely revoked route running. An explicit conflict choice may restore it.
    for (id, mutation) in &st.pending {
        let revoked = remote
            .get(id)
            .map_or(mutation.before.is_some(), |v| v["disabled"] == true);
        if revoked
            && !same(remote.get(id), mutation.before.as_ref())
            && reg
                .servers
                .iter()
                .any(|s| s.id == mutation.local_id && !keep_local(s))
        {
            crate::local_auth::revoke_personal_route(reg, &team_id, &mutation.local_id);
            if let Some(server) = reg.servers.iter_mut().find(|s| s.id == mutation.local_id) {
                let memberships: Vec<_> = reg
                    .profiles
                    .iter()
                    .filter(|p| p.enabled_server_ids.contains(&server.id))
                    .map(|p| p.id.clone())
                    .collect();
                if !memberships.is_empty() {
                    server
                        .unknown_fields
                        .insert("personalSyncProfiles".into(), json!(memberships));
                }
                server.enabled = false;
            }
            for profile in &mut reg.profiles {
                profile
                    .enabled_server_ids
                    .retain(|id| id != &mutation.local_id);
            }
        }
    }
    let deleted: Vec<_> = st
        .baseline
        .keys()
        .filter(|id| !remote.contains_key(*id) && !st.pending.contains_key(*id))
        .cloned()
        .collect();
    for id in deleted {
        let ids: Vec<_> = reg
            .servers
            .iter()
            .filter(|s| !keep_local(s) && s.source.as_deref() == Some(&tag) && original(s) == id)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            crate::local_auth::revoke_personal_route(reg, &team_id, &id);
            reg.servers.retain(|s| s.id != id);
            for p in &mut reg.profiles {
                p.enabled_server_ids.retain(|s| s != &id);
                p.tool_scope.remove(&id);
            }
        }
    }
    for (id, value) in &remote {
        let risky: Vec<_> = value["env"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["key"].as_str())
            .filter(|key| risky_sync_env(key))
            .map(visible_text)
            .collect();
        if risky.is_empty() {
            st.warnings.remove(id);
        } else {
            let name = visible_text(value["name"].as_str().unwrap_or(id));
            let warning = format!("A synced change tried to set {} on {name}. Toolport ignored it. If you didn't make this change, sign out of other devices and change your password.", risky.join(", "));
            if st.warnings.get(id) != Some(&warning) {
                crate::audit::record_timed(
                    id,
                    "sync_environment_refused",
                    false,
                    None,
                    Some(&warning),
                    Some("personal-sync"),
                );
                st.warnings.insert(id.clone(), warning);
            }
        }
        if st.pending.contains_key(id)
            || reg
                .servers
                .iter()
                .any(|s| keep_local(s) && original(s) == id)
        {
            continue;
        }
        if publish_error(value).is_some() {
            stop_blocked(reg, &tag, id);
            outcome.blocked += 1;
            continue;
        }
        let mut runtime = value.clone();
        runtime.as_object_mut().unwrap().remove("disabled");
        let (mut entry, classified_review) =
            match crate::teams::classify_team_server(&runtime, &tag) {
                crate::teams::TeamClass::Ready(e) => (e, false),
                crate::teams::TeamClass::Review(e) => (e, true),
                _ => {
                    stop_blocked(reg, &tag, id);
                    outcome.blocked += 1;
                    continue;
                }
            };
        // The shared classifier deliberately strips setup values for governed
        // teams. Restore only the explicitly portable personal fields here.
        for (field, path) in [("env", "/env"), ("inputs", "/launch/inputs")] {
            if let Some(inputs) = runtime.pointer(path).and_then(Value::as_array) {
                let imported: Vec<Value> = inputs
                    .iter()
                    .map(|i| {
                        let mut i = input_export(i.clone());
                        if i["secret"] != false {
                            i["secret"] = json!(true);
                        }
                        i
                    })
                    .collect();
                if field == "env" {
                    entry.env =
                        serde_json::from_value(json!(imported)).map_err(|e| e.to_string())?;
                } else if let Some(l) = &mut entry.launch {
                    l.inputs =
                        serde_json::from_value(json!(imported)).map_err(|e| e.to_string())?;
                }
            }
        }
        if let Some(headers) = runtime.get("headerKeys") {
            entry
                .unknown_fields
                .insert("headerKeys".into(), headers.clone());
        }
        let old = reg
            .servers
            .iter()
            .find(|s| eligible(s) && original(s) == id)
            .cloned();
        if let Some(old) = &old {
            entry.id = old.id.clone();
            let classified = entry.unknown_fields.clone();
            entry.unknown_fields = old.unknown_fields.clone();
            // These are derived from the received definition, never retained
            // from a prior route. All other extensions are machine-local.
            for key in [
                "headerKeys",
                "teamEnableReview",
                "teamRouteRemoved",
                "teamHeldChange",
                "personalSyncArgsReview",
                "personalSyncArgsIncoming",
                "personalSyncRemoteRefs",
            ] {
                entry.unknown_fields.remove(key);
            }
            if old.url != entry.url {
                entry.unknown_fields.remove("importedUrlKey");
            }
            entry.unknown_fields.extend(classified);
            restore_local(&mut entry, old);
        } else {
            // A secret argument never syncs. The server still appears here, off,
            // and asks for that value on this machine before it can be enabled.
            entry.id = crate::registry::unique_id(
                id,
                &reg.servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            );
        }
        if crate::secret_refs::validate_server(&entry).is_err() {
            stop_blocked(reg, &tag, id);
            outcome.blocked += 1;
            continue;
        }
        // Preserve existing sign-in only for its exact HTTP destination. New
        // destinations get an empty local vault namespace, never copied tokens.
        let credential_owner = reg
            .unknown_fields
            .get("personalSyncCredentialOwners")
            .and_then(|owners| owners.get(&entry.id))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                old.as_ref()
                    .filter(|s| {
                        !s.unknown_fields
                            .contains_key("personalSyncCredentialDestination")
                            && !s.needs_team_enable_review()
                    })
                    .and_then(|s| crate::local_auth::owner_in(reg, &s.id).ok())
            });
        let credential_owner = if let Some(owner) = credential_owner {
            owner
        } else {
            let mut bytes = [0u8; 32];
            getrandom::getrandom(&mut bytes)
                .map_err(|_| "Could not create local sync credentials")?;
            format!(
                "sync-credential-{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            )
        };
        reg.unknown_fields
            .entry("personalSyncCredentialOwners")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("Local credential ownership is unreadable")?
            .insert(entry.id.clone(), json!(credential_owner));
        let credential_destination = reg
            .unknown_fields
            .get("personalSyncCredentialDestinations")
            .and_then(|destinations| destinations.get(&entry.id))
            .cloned()
            .or_else(|| {
                old.as_ref()
                    .and_then(|s| s.unknown_fields.get("personalSyncCredentialDestination"))
                    .cloned()
            })
            .unwrap_or_else(|| {
                json!(crate::local_auth::personal_credential_destination(
                    old.as_ref().unwrap_or(&entry)
                ))
            });
        reg.unknown_fields
            .entry("personalSyncCredentialDestinations")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("Local credential destinations are unreadable")?
            .insert(entry.id.clone(), credential_destination.clone());
        entry.unknown_fields.insert(
            "personalSyncCredentialDestination".into(),
            credential_destination,
        );
        if value["disabled"] == true {
            if let Some(old) = &old {
                crate::local_auth::revoke_personal_route(reg, &team_id, &old.id);
            }
        }
        let installed = old.as_ref().map(export);
        // Consent covers what runs here: the arguments after restoring this
        // machine's masked values, so an older peer's masking is not a change.
        let mut effective = value.clone();
        effective["args"] = export(&entry)["args"].clone();
        let changed = execution_changed(installed.as_ref(), &effective);
        // Use the upstream approval sidecar, bound to the exact reference and
        // destination. Wire metadata is never evidence of local approval.
        if own_approved.contains(id) {
            crate::secret_refs::approve_server(&entry).map_err(|e| e.to_string())?;
        }
        let references_need_approval = crate::secret_refs::check_approval(&entry).is_err();
        let command_approved = old
            .as_ref()
            .and_then(|s| s.unknown_fields.get("syncCommandConsent"))
            == Some(&command_identity(&effective));
        let private_url_review = classified_review
            && entry.command.is_none()
            && entry
                .url
                .as_deref()
                .and_then(crate::oauth::host_of_url)
                .is_some_and(|host| crate::teams::team_host_is_private(&host))
            && old.as_ref().is_none_or(|s| {
                s.url != entry.url || s.unknown_fields.get("teamEnableReview") == Some(&json!(true))
            });
        entry
            .unknown_fields
            .insert("personalSyncEntry".into(), json!(true));
        let review = entry.unknown_fields.get("personalSyncArgsReview") == Some(&json!(true))
            || !missing_secret_args(&entry).is_empty()
            || private_url_review
            || (changed && !command_approved)
            || references_need_approval
            || old
                .as_ref()
                .is_some_and(|s| s.unknown_fields.get("teamEnableReview") == Some(&json!(true)));
        if let Some(consent) = old
            .as_ref()
            .and_then(|s| s.unknown_fields.get("syncCommandConsent"))
        {
            entry
                .unknown_fields
                .insert("syncCommandConsent".into(), consent.clone());
        }
        entry.unknown_fields.insert(
            "personalSyncDesiredEnabled".into(),
            json!(value["disabled"] != true),
        );
        entry.enabled = value["disabled"] != true && !review;
        if review {
            entry.require_team_enable_review();
            outcome.review += 1;
        } else {
            entry.unknown_fields.remove("teamEnableReview");
            outcome.applied += 1;
        }
        // Profile membership is local. Save memberships while a remote disable
        // or review holds a row off, and restore those exact profiles later.
        let memberships: Vec<String> = reg
            .profiles
            .iter()
            .filter(|p| p.enabled_server_ids.contains(&entry.id))
            .map(|p| p.id.clone())
            .collect();
        if !entry.enabled && !memberships.is_empty() {
            entry
                .unknown_fields
                .insert("personalSyncProfiles".into(), json!(memberships));
        }
        reg.servers.retain(|s| s.id != entry.id);
        let restore_profiles = entry
            .unknown_fields
            .get("personalSyncProfiles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let active = reg.active_profile_id();
        for profile in &mut reg.profiles {
            if !entry.enabled {
                profile.enabled_server_ids.retain(|s| s != &entry.id);
            } else if (old.is_none() && profile.id == active)
                || restore_profiles.contains(&json!(profile.id))
            {
                if !profile.enabled_server_ids.contains(&entry.id) {
                    profile.enabled_server_ids.push(entry.id.clone());
                }
            }
        }
        if entry.enabled {
            entry.unknown_fields.remove("personalSyncProfiles");
        }
        if let Some(t) = &mut reg.team {
            t.managed_server_ids.insert(entry.id.clone(), id.clone());
        }
        reg.servers.push(entry);
    }
    st.warnings.retain(|id, _| remote.contains_key(id));
    st.baseline = remote;
    if let Some(t) = &mut reg.team {
        t.last_version = version;
    }
    save(reg, &st)?;
    Ok(outcome)
}

pub(crate) fn check_review(
    reg: &Registry,
    current: &ServerEntry,
    reviewed: Option<&ServerEntry>,
) -> Result<(), String> {
    crate::secret_refs::check_reviewed_definition(current, reviewed)?;
    if is_personal(reg) && current.needs_team_enable_review() {
        if reviewed.is_none_or(|s| {
            s.id != current.id
                || command_identity(&json!(s)) != command_identity(&json!(current))
                || execution_review_fields(s) != execution_review_fields(current)
        }) {
            return Err(
                "The command, reference or destination changed. Review this server again.".into(),
            );
        }
    }
    Ok(())
}
pub fn enable_reviewed(profile: &str, reviewed: &ServerEntry) -> Result<Registry, String> {
    crate::registry_controller::set_server_enabled_after_reference_review(profile, reviewed)
}

pub fn set_local_only(server_id: &str, local_only: bool) -> Result<Registry, String> {
    crate::registry::update(|r| {
        if !is_personal(r) {
            return Err("This option requires personal sync".into());
        }
        let server = r
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or("Server no longer exists")?;
        server
            .unknown_fields
            .insert("syncLocalOnly".into(), json!(local_only));
        if local_only
            && server
                .source
                .as_deref()
                .is_some_and(|s| s.starts_with("team:"))
        {
            // Preserve shared provenance: opting out must never launder a
            // received command or key reference into trusted local input.
            server.source = Some("shared".into());
        }
        Ok(())
    })
    .map(|(r, ())| r)
}
pub fn set_portable(
    server_id: &str,
    kind: &str,
    key: &str,
    enabled: bool,
) -> Result<Registry, String> {
    crate::registry::update(|r| {
        let s = r
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or("Server no longer exists")?;
        if enabled && kind == "env" && risky_sync_env(key) { return Err("Execution environment overrides cannot be portable. Keep this server on this machine only.".into()); }
        let (secret, fields) = match kind {
            "env" => {
                let i = s
                    .env
                    .iter_mut()
                    .find(|i| i.key == key)
                    .ok_or("Variable no longer exists")?;
                (i.secret, &mut i.unknown_fields)
            }
            "input" => {
                let i = s
                    .launch
                    .iter_mut()
                    .flat_map(|l| &mut l.inputs)
                    .find(|i| i.key == key)
                    .ok_or("Input no longer exists")?;
                (i.secret, &mut i.unknown_fields)
            }
            _ => return Err("Unknown portable value type".into()),
        };
        if secret || fields.contains_key("source") {
            return Err("Secret values always stay on this machine".into());
        }
        fields.insert("portable".into(), json!(enabled));
        Ok(())
    })
    .map(|(r, ())| r)
}
pub fn resolve_conflict(
    id: &str,
    expected_version: &str,
    keep_mine: bool,
) -> Result<Registry, String> {
    remote_update(|| {
        crate::registry::update(|r| {
            let mut st = state(r)?;
            let expected = st
                .conflicts
                .get(id)
                .cloned()
                .ok_or("The conflict is no longer pending")?;
            if conflict_version(&expected) != expected_version {
                return Err("The conflict changed. Refresh Sync and review it again.".into());
            }
            let m = st
                .pending
                .get_mut(id)
                .ok_or("The conflict is no longer pending")?;
            if keep_mine {
                m.before = (!expected.is_null()).then(|| expected.clone());
                m.initial_conflict = false;
                m.at = 0;
            } else {
                if let Some(m) = st.pending.remove(id) {
                    if expected.is_null() {
                        if let Some(team) = r.team.as_ref().map(|t| t.team_id.clone()) {
                            crate::local_auth::revoke_personal_route(r, &team, &m.local_id);
                        }
                        r.servers.retain(|s| s.id != m.local_id);
                        for profile in &mut r.profiles {
                            profile.enabled_server_ids.retain(|sid| sid != &m.local_id);
                            profile.tool_scope.remove(&m.local_id);
                        }
                    }
                }
            }
            st.conflicts.remove(id);
            save(r, &st)
        })
    })
    .map(|(r, ())| r)
}

pub(crate) fn sync(
    conn: &crate::registry::TeamConnection,
    token: &str,
) -> Result<crate::teams::SyncResult, String> {
    let mut latest = crate::teams::fetch_personal_config(&conn.server_url, &conn.team_id, token)?;
    let (reg, _) = remote_update(|| {
        crate::registry::update(|r| {
            if !current(r, conn) {
                return Ok(());
            }
            apply(r, &latest.1, latest.0)?;
            Ok(())
        })
    })?;
    if !current(&reg, conn) {
        return Ok(crate::teams::SyncResult::Ok {
            role: conn.role.clone(),
            role_changed: false,
            applied: None,
        });
    }
    for attempt in 0..2 {
        // Snapshot the journal and mark the exact publication under the same
        // mutation lock. An undo after this point is recorded as a newer edit.
        let (_, prepared) = remote_update(|| {
            crate::registry::update(|r| {
                if !current(r, conn) {
                    return Ok(None);
                }
                let mut st = state(r)?;
                let merged = prepare_publication(&mut st, &latest.1)?;
                save(r, &st)?;
                Ok(Some(merged))
            })
        })?;
        let Some(merged) = prepared else {
            break;
        };
        if merged == latest.1 {
            break;
        }
        match crate::teams::push_personal_config(
            &conn.server_url,
            &conn.team_id,
            token,
            &merged,
            latest.0,
        ) {
            Ok(crate::teams::PushOutcome::Published(_)) => {
                latest =
                    crate::teams::fetch_personal_config(&conn.server_url, &conn.team_id, token)?;
                break;
            }
            Ok(_) => return Err("Finish the sync approval in Your account.".into()),
            Err(e) if e == crate::teams::STALE_PUSH_MESSAGE && attempt == 0 => {
                latest =
                    crate::teams::fetch_personal_config(&conn.server_url, &conn.team_id, token)?;
            }
            Err(e) if e == crate::teams::STALE_PUSH_MESSAGE => {
                return Err("Your setup changed again while syncing. Changes are saved on this machine and will retry.".into());
            }
            Err(e) => return Err(e),
        }
    }
    let (_, outcome) = remote_update(|| {
        crate::registry::update(|r| {
            if !current(r, conn) {
                return Ok(None);
            }
            let out = apply(r, &latest.1, latest.0)?;
            let mut st = state(r)?;
            mark_synced(r.team.as_ref().ok_or("Sign in to sync first")?, now())?;
            st.error = None;
            st.sign_in_required = false;
            save(r, &st)?;
            Ok(Some((latest.0, out)))
        })
    })?;
    Ok(crate::teams::SyncResult::Ok {
        role: conn.role.clone(),
        role_changed: false,
        applied: outcome,
    })
}
fn prepare_publication(st: &mut SyncState, remote: &Value) -> Result<Value, String> {
    let mut errors = BTreeMap::new();
    let ready: BTreeMap<_, _> = st
        .pending
        .iter()
        .filter(|(_, m)| now() - m.at >= 750)
        .filter(|(id, m)| {
            if let Some(error) = m.after.as_ref().and_then(publish_error) {
                errors.insert((*id).clone(), error);
                false
            } else {
                true
            }
        })
        .map(|(id, m)| (id.clone(), m.clone()))
        .collect();
    let (merged, conflicts) = merge(remote, &ready)?;
    st.publish_errors = errors;
    st.conflicts = conflicts.clone();
    // Retain unacknowledged publications from a lost reply until apply repairs
    // them. New ready mutations supersede only their own in-flight entry.
    for (id, m) in ready {
        if !conflicts.contains_key(&id) {
            st.publishing.insert(id, m);
        }
    }
    Ok(merged)
}
/// A successful older publication becomes the ancestor of newer edits. This
/// also repairs a lost PUT response on the next pull without self-conflicting.
fn acknowledge(reg: &mut Registry, st: &mut SyncState, remote: &BTreeMap<String, Value>) {
    for (id, sent) in st.publishing.clone() {
        if !same(remote.get(&id), sent.after.as_ref()) {
            continue;
        }
        if st.pending.get(&id) == Some(&sent) {
            st.pending.remove(&id);
            st.conflicts.remove(&id);
            if let Some(after) = &sent.after {
                if let Some(s) = reg
                    .servers
                    .iter_mut()
                    .find(|s| s.id == sent.local_id && !keep_local(s))
                {
                    let fields = execution_review_fields(s);
                    s.unknown_fields
                        .insert("syncCommandConsent".into(), command_identity(after));
                    s.unknown_fields
                        .insert("syncExecutionReview".into(), json!(fields));
                }
            }
        } else if let Some(newer) = st.pending.get_mut(&id) {
            // Do not approve the newer definition or discard its journal entry.
            newer.before = sent.after.clone();
            newer.initial_conflict = false;
            st.conflicts.remove(&id);
        }
        st.publishing.remove(&id);
    }
}
fn current(r: &Registry, c: &crate::registry::TeamConnection) -> bool {
    is_personal(r)
        && r.team.as_ref().is_some_and(|t| {
            t.team_id == c.team_id
                && t.server_url == c.server_url
                && t.reporting_device_id == c.reporting_device_id
        })
}
pub fn pending_review_count(reg: &Registry) -> usize {
    reg.servers
        .iter()
        .filter(|s| !reg.server_enabled(&s.id) && s.needs_team_enable_review() && !keep_local(s))
        .count()
}
pub fn banner(reg: &Registry) -> (String, bool) {
    let st = state(reg).unwrap_or_default();
    if reg
        .team
        .as_ref()
        .is_some_and(|t| t.unknown_fields["accountStatus"]["canReceiveConfig"] == false)
    {
        return (
            "Sync paused. Review this device in Your account.".into(),
            false,
        );
    }
    if let Some(error) = &st.error {
        return (error.clone(), false);
    }
    if let Some(error) = reg
        .team
        .as_ref()
        .and_then(|t| t.unknown_fields.get("accountStatusError"))
        .and_then(Value::as_str)
    {
        return (error.into(), false);
    }
    if !st.conflicts.is_empty() {
        return (
            "Changes need your choice. Resolve the conflicts below.".into(),
            false,
        );
    }
    if !st.publish_errors.is_empty() {
        return (
            "Some servers could not sync. Review the details below.".into(),
            false,
        );
    }
    let review = pending_review_count(reg);
    if review > 0 {
        return (
            format!(
                "{review} {} waiting for review on this machine.",
                if review == 1 {
                    "server is"
                } else {
                    "servers are"
                }
            ),
            false,
        );
    }
    if !st.initialized || st.last_synced_at.is_none() {
        return ("Waiting for first sync.".into(), false);
    }
    if !st.pending.is_empty() {
        return ("Changes waiting to sync.".into(), false);
    }
    ("Sync is up to date.".into(), true)
}
pub fn conflict_fields(value: Option<&Value>) -> BTreeMap<String, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return BTreeMap::from([("Server".into(), "Removed".into())]);
    };
    let text = |v: &Value| {
        v.as_str()
            .map(visible_text)
            .unwrap_or_else(|| visible_text(&v.to_string()))
    };
    let mut fields = BTreeMap::new();
    for (key, label) in [
        ("name", "Name"),
        ("transport", "Transport"),
        ("command", "Command"),
        ("cwd", "Working directory"),
        ("url", "URL"),
        ("disabled", "Disabled"),
        ("requestTimeoutMs", "Request timeout"),
        ("initializeTimeoutMs", "Startup timeout"),
        ("disabledTools", "Disabled tools"),
    ] {
        if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
            fields.insert(label.into(), text(v));
        }
    }
    for (i, v) in value["args"].as_array().into_iter().flatten().enumerate() {
        fields.insert(format!("Argument {}", i + 1), text(v));
    }
    // Same plain labels and masking as the execution review.
    for (path, label) in [
        ("/env", "Environment"),
        ("/launch/inputs", "Input"),
        ("/headerKeys", "Header"),
    ] {
        for v in value
            .pointer(path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            fields.insert(
                format!("{label}: {}", text(&v["key"])),
                if label == "Header" && v["env"].is_string() {
                    format!("Uses environment: {}", text(&v["env"]))
                } else if let Some(r) = reference(v) {
                    format!("Password manager: {}", visible_text(r))
                } else if v["secret"] == true {
                    "<masked secret>".into()
                } else if let Some(v) = v.get("value").filter(|v| !v.is_null()) {
                    text(v)
                } else {
                    "Set on this machine".into()
                },
            );
        }
    }
    if let Some(bindings) = value
        .pointer("/launch/bindings")
        .and_then(|b| serde_json::from_value::<Vec<crate::registry::ArgBinding>>(b.clone()).ok())
        .filter(|b| !b.is_empty())
    {
        fields.insert("Argument values".into(), argument_values(&bindings));
    }
    fields
}
fn plan_name(plan: Option<&str>) -> String {
    match plan.map(str::to_ascii_lowercase).as_deref() {
        Some("pro") => "Pro".into(),
        Some("team") => "Team".into(),
        Some("free") => "Free".into(),
        Some(other) if !other.is_empty() => visible_text(plan.unwrap_or_default()),
        _ => "unknown".into(),
    }
}
pub fn account_display_lines(reg: &Registry) -> Vec<String> {
    let st = state(reg).unwrap_or_default();
    let status = reg
        .team
        .as_ref()
        .and_then(|t| t.unknown_fields.get("accountStatus"))
        .cloned()
        .unwrap_or(Value::Null);
    if st.sign_in_required {
        return vec![format!(
            "Saved account plan: {}. Sign in to confirm your account and resume sync.",
            plan_name(status["plan"].as_str())
        )];
    }
    status_lines(&status, st.last_synced_at)
}
/// Account buttons on the Sync page, shared by both shells. Without sign-in a
/// sync cannot succeed, so the primary action is Sign in. Sign out stays
/// because it clears the saved account and its token.
pub fn account_actions(st: &SyncState) -> [&'static str; 3] {
    [
        if st.sign_in_required {
            "Sign in"
        } else {
            "Sync now"
        },
        "Your account",
        "Sign out",
    ]
}
pub fn status_lines(status: &Value, last_synced: Option<i64>) -> Vec<String> {
    status_lines_at(status, last_synced, now())
}
fn status_lines_at(status: &Value, last_synced: Option<i64>, now: i64) -> Vec<String> {
    let mut lines = vec![match status["plan"].as_str().unwrap_or("Unknown plan") {
        "free" => "Free · 1 person, 1 device".into(),
        "pro" => "Pro · unlimited devices".into(),
        plan => plan.to_string(),
    }];
    let days = |end: i64| ((end - now).max(0) as u64).div_ceil(86_400_000);
    if status["trialActive"] == true {
        if let Some(end) = status["trialEndsAt"].as_i64() {
            lines.push(format!("{} trial days left", days(end)));
        }
    }
    if let Some(end) = status["freeSyncGraceEndsAt"].as_i64() {
        lines.push(if end > now {
            format!("Every device keeps syncing for {} more days", days(end))
        } else {
            "Sync grace period ended.".into()
        });
    }
    if status["canReceiveConfig"] == false {
        lines.push(status["reason"].as_str().unwrap_or("This device cannot receive your setup. Choose your active device in Your account.").into());
    }
    lines.push(match last_synced {
        Some(t) if now - t < 60_000 => "Last synced just now".into(),
        Some(t) => {
            let minutes = (now - t).max(0) / 60_000;
            format!(
                "Last synced {minutes} {} ago",
                if minutes == 1 { "minute" } else { "minutes" }
            )
        }
        None => "Waiting for first sync".into(),
    });
    lines
}

pub fn record_error(error: Option<&str>) {
    let _ = remote_update(|| {
        crate::registry::update(|r| {
            if r.team.is_some() {
                let mut st = state(r)?;
                st.error = error.map(str::to_string);
                st.sign_in_required = error
                    .is_some_and(|e| e.starts_with("Sync sign-in is missing from this machine."));
                save(r, &st)?;
            }
            Ok(())
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_local_extensions_survive_polls_and_destination_changes() {
        let _data = crate::registry::DataDirTestEnv::new("sync-imported-fields");
        let mut r = machine();
        let mut row = local(http("imported"));
        row.enabled = true;
        row.source = Some("imported:cursor".into());
        row.unknown_fields.extend(
            serde_json::from_value::<serde_json::Map<String, Value>>(json!({
                "importedUrlKey":"url-token", "futureLocalField":{"keep":true},
                "memberSecretRefs":{}, "syncExecutionReview":{}, "localCredentialHint":"keep"
            }))
            .unwrap(),
        );
        let local_fields = row.unknown_fields.clone();
        r.servers.push(row);
        let wire = config(r.servers.iter().map(export).collect());
        for version in 1..=3 {
            apply(&mut r, &wire, version).unwrap();
        }
        for (key, value) in local_fields {
            assert_eq!(r.servers[0].unknown_fields[&key], value, "{key}");
        }
        assert!(!export(&r.servers[0]).to_string().contains("url-token"));
        let mut changed = wire;
        changed["servers"][0]["url"] = json!("https://example.org/mcp");
        apply(&mut r, &changed, 4).unwrap();
        assert!(!r.servers[0].unknown_fields.contains_key("importedUrlKey"));
        assert_eq!(
            r.servers[0].unknown_fields["futureLocalField"],
            json!({"keep":true})
        );
    }
    #[test]
    fn own_publication_carries_reference_approval_without_approving_received_changes() {
        let _data = crate::registry::DataDirTestEnv::new("sync-own-reference");
        for private_http in [false, true] {
            let mut r = machine();
            apply(&mut r, &config(vec![]), 0).unwrap();
            let before = r.clone();
            let mut row = if private_http {
                http("ref")
            } else {
                command("ref")
            };
            if private_http {
                row["url"] = json!("http://127.0.0.1/mcp");
            }
            row["env"] =
                json!([{"key":"TOKEN","secret":true,"source":{"ref":"op://Private/Item/key"}}]);
            let mut server = local(row);
            server.enabled = true;
            server.source = Some("manual".into());
            crate::secret_refs::approve_server(&server).unwrap();
            r.servers.push(server);
            record(&before, &mut r).unwrap();
            let mut st = state(&r).unwrap();
            st.pending.get_mut("ref").unwrap().at = 0;
            let wire = prepare_publication(&mut st, &config(vec![])).unwrap();
            save(&mut r, &st).unwrap();
            for version in 1..=3 {
                apply(&mut r, &wire, version).unwrap();
            }
            assert!(r.servers[0].enabled, "private HTTP: {private_http}");
            assert_ne!(
                r.servers[0].unknown_fields.get("teamEnableReview"),
                Some(&json!(true))
            );
            assert!(crate::secret_refs::check_approval(&r.servers[0]).is_ok());
            let mut changed = wire;
            changed["servers"][0]["env"][0]["source"]["ref"] = json!("op://Other/Item/key");
            apply(&mut r, &changed, 4).unwrap();
            assert!(r.servers[0].needs_team_enable_review());
            assert!(crate::secret_refs::check_approval(&r.servers[0]).is_err());
        }
    }
    #[test]
    fn shifted_masked_arguments_follow_flags_and_ambiguous_edits_stay_local() {
        let old = local(
            json!({"id":"args","name":"Args","transport":"stdio","command":"fixture","args":["--x","--token","secret"],"env":[]}),
        );
        for args in [
            json!(["--token", "<redacted>"]),
            json!(["--new", "value", "--x", "--token", "<redacted>"]),
        ] {
            let mut received = old.clone();
            received.args = serde_json::from_value(args).unwrap();
            restore_local(&mut received, &old);
            assert_eq!(received.args.last().unwrap(), "secret");
            assert!(!received.args.iter().any(|a| a.contains("<redacted>")));
        }
        let mut ambiguous = old.clone();
        ambiguous.args = vec!["--unknown".into(), "<redacted>".into()];
        restore_local(&mut ambiguous, &old);
        assert_eq!(ambiguous.args, old.args);
        assert_eq!(ambiguous.unknown_fields["personalSyncArgsReview"], true);
        let old = local(
            json!({"id":"prefix","name":"Prefix","transport":"stdio","command":"fixture","args":["--token=secret"],"env":[]}),
        );
        let mut received = old.clone();
        assert_eq!(export(&old)["args"], json!(["--token=<redacted>"]));
        received.args = vec!["--new".into(), "--token=<redacted>".into()];
        restore_local(&mut received, &old);
        assert_eq!(received.args, vec!["--new", "--token=secret"]);
    }
    #[test]
    fn repeated_masked_flags_restore_by_position_across_polls() {
        let _data = crate::registry::DataDirTestEnv::new("sync-repeated-flags");
        for args in [
            json!([
                "--header",
                "Authorization: Bearer a",
                "--header",
                "X-Api-Key: b"
            ]),
            json!(["--token=first", "--verbose", "--token=second"]),
        ] {
            let mut row = command("repeat");
            row["args"] = args.clone();
            let mut server = local(row);
            server.enabled = true;
            let mut r = machine();
            r.servers.push(server);
            r.profiles[0].enabled_server_ids.push("repeat".into());
            let wire = config(r.servers.iter().map(export).collect());
            assert!(wire.to_string().contains("<redacted>"));
            for version in 1..=3 {
                apply(&mut r, &wire, version).unwrap();
                let s = &r.servers[0];
                assert_eq!(json!(s.args), args, "poll {version}");
                assert!(!s.unknown_fields.contains_key("personalSyncArgsReview"));
                assert!(s.enabled && !s.needs_team_enable_review(), "poll {version}");
                assert!(r.profiles[0].enabled_server_ids.contains(&"repeat".into()));
            }
        }
    }
    #[test]
    fn approved_ambiguous_arguments_stay_approved_until_they_change() {
        let _data = crate::registry::DataDirTestEnv::new("sync-args-approval");
        let mut row = command("amb");
        row["args"] = json!(["--x", "--token", "secret"]);
        let mut r = machine();
        let mut server = local(row);
        server.enabled = true;
        r.servers.push(server);
        let mut wire = config(r.servers.iter().map(export).collect());
        apply(&mut r, &wire, 1).unwrap();
        wire["servers"][0]["args"] = json!(["--unknown", "<redacted>"]);
        apply(&mut r, &wire, 2).unwrap();
        assert!(r.servers[0].needs_team_enable_review());
        assert_eq!(
            execution_review_lines(&r.servers[0]).last().unwrap(),
            ARGS_REVIEW_LINE
        );
        let profile = r.active_profile_id();
        crate::registry_controller::apply_server_enabled(&mut r, &profile, "amb", true, true)
            .unwrap();
        for version in 3..=5 {
            apply(&mut r, &wire, version).unwrap();
            let s = &r.servers[0];
            assert_eq!(s.args, vec!["--x", "--token", "secret"]);
            assert!(s.enabled && !s.needs_team_enable_review(), "poll {version}");
        }
        wire["servers"][0]["args"] = json!(["--other", "<redacted>"]);
        apply(&mut r, &wire, 6).unwrap();
        assert!(r.servers[0].needs_team_enable_review());
    }
    #[test]
    fn new_server_with_secret_argument_waits_for_setup_on_this_machine() {
        let _data = crate::registry::DataDirTestEnv::new("sync-new-masked");
        let mut row = command("masked");
        row["args"] = json!(["--token", "actual-secret"]);
        let wire = config(vec![export(&local(row))]);
        assert!(!wire.to_string().contains("actual-secret"));
        let mut r = machine();
        for version in 1..=2 {
            let outcome = apply(&mut r, &wire, version).unwrap();
            assert_eq!((outcome.blocked, outcome.review), (0, 1));
        }
        let s = r.servers[0].clone();
        assert!(!s.enabled && s.needs_team_enable_review());
        assert_eq!(missing_secret_args(&s), vec![2]);
        assert!(execution_review_lines(&s).contains(&"Argument 2 is a secret that does not sync. Edit this server and enter it on this machine before enabling.".to_string()));
        assert_eq!(pending_review_count(&r), 1);
        let profile = r.active_profile_id();
        let id = s.id.clone();
        assert!(crate::registry_controller::apply_server_enabled(
            &mut r, &profile, &id, true, true
        )
        .unwrap_err()
        .contains("secret argument"));
        // Entered on this machine, kept across polls and never sent back.
        r.servers[0].args[1] = "typed-here".into();
        apply(&mut r, &wire, 3).unwrap();
        assert_eq!(r.servers[0].args[1], "typed-here");
        assert!(missing_secret_args(&r.servers[0]).is_empty());
        assert!(!export(&r.servers[0]).to_string().contains("typed-here"));
    }
    #[test]
    fn opt_in_requires_choice_when_remote_advanced_while_local_only() {
        let _data = crate::registry::DataDirTestEnv::new("sync-opt-in-conflict");
        let mut r = machine();
        let wire = config(vec![http("a")]);
        apply(&mut r, &wire, 0).unwrap();
        let before = r.clone();
        r.servers[0]
            .unknown_fields
            .insert("syncLocalOnly".into(), json!(true));
        record(&before, &mut r).unwrap();
        let mut newer = wire.clone();
        newer["servers"][0]["name"] = json!("Remote edit");
        apply(&mut r, &newer, 1).unwrap();
        crate::registry::save(&r).unwrap();
        let id = r.servers[0].id.clone();
        r = set_local_only(&id, false).unwrap();
        let st = state(&r).unwrap();
        assert!(st.pending["a"].initial_conflict);
        assert_eq!(st.conflicts["a"]["name"], "Remote edit");
        assert_eq!(merge(&newer, &st.pending).unwrap().0, newer);
    }
    #[test]
    fn publication_lock_captures_undo_before_or_after_snapshot() {
        let _data = crate::registry::DataDirTestEnv::new("sync-publication-snapshot");
        let mut r = machine();
        let wire = config(vec![http("a")]);
        apply(&mut r, &wire, 0).unwrap();
        let original = r.clone();
        r.servers[0].name = "Edit".into();
        record(&original, &mut r).unwrap();
        let before = r.clone();
        r.servers[0].name = "a".into();
        record(&before, &mut r).unwrap();
        let mut st = state(&r).unwrap();
        assert_eq!(prepare_publication(&mut st, &wire).unwrap(), wire);
        assert!(st.publishing.is_empty());
        r.servers[0].name = "Edit".into();
        record(&original, &mut r).unwrap();
        let mut st = state(&r).unwrap();
        st.pending.get_mut("a").unwrap().at = 0;
        let published = prepare_publication(&mut st, &wire).unwrap();
        save(&mut r, &st).unwrap();
        let before = r.clone();
        r.servers[0].name = "a".into();
        record(&before, &mut r).unwrap();
        assert!(state(&r).unwrap().pending.contains_key("a"));
        apply(&mut r, &published, 1).unwrap();
        let st = state(&r).unwrap();
        assert_eq!(st.pending["a"].after.as_ref().unwrap()["name"], "a");
        assert_eq!(st.pending["a"].before.as_ref().unwrap()["name"], "Edit");
    }
    #[test]
    fn polls_respect_profile_removal_and_restore_every_profile_after_remote_disable() {
        let _data = crate::registry::DataDirTestEnv::new("sync-profiles");
        let mut r = machine();
        let wire = config(vec![http("a")]);
        apply(&mut r, &wire, 0).unwrap();
        let id = r.servers[0].id.clone();
        r.profiles[0].enabled_server_ids.clear();
        apply(&mut r, &wire, 1).unwrap();
        assert!(r.profiles[0].enabled_server_ids.is_empty());
        r.profiles[0].enabled_server_ids.push(id.clone());
        let mut other = r.profiles[0].clone();
        other.id = "other".into();
        r.profiles.push(other);
        let mut disabled = wire.clone();
        disabled["servers"][0]["disabled"] = json!(true);
        apply(&mut r, &disabled, 2).unwrap();
        assert!(r.profiles.iter().all(|p| p.enabled_server_ids.is_empty()));
        apply(&mut r, &wire, 3).unwrap();
        assert!(r
            .profiles
            .iter()
            .all(|p| p.enabled_server_ids.contains(&id)));
    }
    #[test]
    fn refused_environment_warns_once_without_logging_values() {
        let _data = crate::registry::DataDirTestEnv::new("sync-warning");
        let mut r = machine();
        let mut attack = command("mock");
        attack["name"] = json!("Mock Tools");
        attack["env"] =
            json!([{"key":"npm_config_registry","value":"DO_NOT_LOG_THIS","portable":true}]);
        for version in 1..=2 {
            assert_eq!(
                apply(&mut r, &config(vec![attack.clone()]), version)
                    .unwrap()
                    .blocked,
                1
            );
        }
        let warning = state(&r).unwrap().warnings["mock"].clone();
        assert!(warning.contains("npm_config_registry on Mock Tools"));
        assert!(warning.contains("change your password"));
        crate::telemetry::flush();
        let log = std::fs::read_to_string(crate::audit::audit_path().unwrap()).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(log.contains("sync_environment_refused"));
        assert!(!log.contains("DO_NOT_LOG_THIS"));
        for key in [
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "PERL5OPT",
            "RUBYOPT",
            "PYTHONHOME",
            "UV_INDEX",
            "PIP_CONFIG_FILE",
            "NODE_EXTRA_CA_CERTS",
            "GIT_SSH",
            "git_ssh_command",
            "GIT_ASKPASS",
            "GIT_PROXY_COMMAND",
            "GIT_EXEC_PATH",
            "GIT_CONFIG_KEY_0",
            "SSH_ASKPASS",
            "PIP_FIND_LINKS",
            "PIP_TRUSTED_HOST",
            "PIP_EXTRA_INDEX_URL",
            "UV_INDEX_STRATEGY",
            "uv_extra_index_url",
            "UV_FIND_LINKS",
            "UV_PYTHON",
            "UV_PYTHON_INSTALL_MIRROR",
            "UV_DEFAULT_INDEX",
        ] {
            assert!(risky_sync_env(key), "{key}");
        }
        assert!(!risky_sync_env("UV_CACHE_DIR"));
        // The warning clears when the cloud definition drops the name, and
        // when the server is gone.
        let mut clean = attack.clone();
        clean["env"] = json!([]);
        apply(&mut r, &config(vec![clean]), 3).unwrap();
        assert!(state(&r).unwrap().warnings.is_empty());
        apply(&mut r, &config(vec![attack.clone()]), 4).unwrap();
        assert!(state(&r).unwrap().warnings.contains_key("mock"));
        apply(&mut r, &config(vec![]), 5).unwrap();
        assert!(state(&r).unwrap().warnings.is_empty());
    }
    #[test]
    fn review_omits_irrelevant_empty_fields_and_labels_new_servers() {
        let server = local(http("http"));
        let fields = execution_review_fields(&server);
        assert_eq!(fields.len(), 1);
        assert!(fields.contains_key("URL"));
        assert_eq!(execution_review_lines(&server)[0], "New server");
        let server = local(command("stdio"));
        let text = execution_review_lines(&server).join("\n");
        assert!(!text.contains("null"));
        assert!(!text.contains("CHANGED"));
        assert!(!text.contains("URL:"));
        assert!(text.contains("Uses this machine's environment: no"));
    }
    #[test]
    fn earlier_review_snapshots_show_only_real_changes_with_plain_fields() {
        let mut server = local(
            json!({"id":"review","name":"Review","transport":"stdio","command":"echo","args":["old"],"cwd":"/work","env":[{"key":"REGION","secret":false,"value":"west"},{"key":"TOKEN","secret":true,"source":{"ref":"op://Private/Item/key"}}],"launch":{"inputs":[],"bindings":[{"index":0,"parts":[{"kind":"literal","value":"old"}]}]}}),
        );
        for args in ["\n  1. old", "[\"old\"]"] {
            server.unknown_fields.insert("syncExecutionReview".into(), json!({
                "Command":"echo","Arguments":args,"Working directory":"/work",
                "Transport":"stdio","URL":"null","inheritEnv":"false",
                "Launch bindings":"[{\"index\":0,\"parts\":[{\"kind\":\"literal\",\"value\":\"old\"}]}]",
                "Environment [0] REGION":"west; reference: null",
                "Environment [1] TOKEN":"<masked secret>; reference: op://Private/Item/key"
            }));
            let reference =
                r#"1Password entry "op://Private/Item/key" will be sent to echo (env:TOKEN)"#;
            assert_eq!(
                execution_review_lines(&server),
                vec![
                    "Approve these password manager entries for this machine.",
                    reference
                ]
            );
            server.command = Some("node".into());
            assert_eq!(
                execution_review_lines(&server),
                vec![
                    "Command: node",
                    r#"1Password entry "op://Private/Item/key" will be sent to node (env:TOKEN)"#
                ]
            );
            server.command = Some("echo".into());
        }
        let mut server = local(http("http"));
        server.unknown_fields.insert(
            "syncExecutionReview".into(),
            json!({
                "Command":"null","Arguments":"","Working directory":"Client default",
                "Transport":"http","URL":server.url,"inheritEnv":"false","Launch bindings":"null"
            }),
        );
        assert_eq!(
            execution_review_lines(&server),
            vec!["Nothing in this definition changed. Confirm it to run it on this machine."]
        );
        server
            .unknown_fields
            .insert("personalSyncArgsReview".into(), json!(true));
        assert_eq!(execution_review_lines(&server), vec![ARGS_REVIEW_LINE]);
    }
    #[test]
    fn review_shows_commands_on_any_transport_and_reference_destinations() {
        let mut row = http("mixed");
        row["command"] = json!("curl-wrapper");
        row["args"] = json!(["--token", "actual-secret"]);
        row["env"] =
            json!([{"key":"TOKEN","secret":true,"source":{"ref":"op://Private/Item/key"}}]);
        row["headerKeys"] =
            json!([{"key":"Authorization","source":{"ref":"op://Private/Header/key"}}]);
        let mut server = local(row);
        server.source = Some("team:solo".into());
        let fields = execution_review_fields(&server);
        assert_eq!(fields["Command"], "curl-wrapper");
        assert!(fields["Arguments"].contains("--token"));
        assert_eq!(fields["URL"], "https://example.com/mcp");
        assert!(fields.contains_key("Uses this machine's environment"));
        server
            .unknown_fields
            .insert("syncExecutionReview".into(), json!(fields));
        server.url = Some("https://attacker.example/mcp".into());
        let lines = execution_review_lines(&server);
        assert_eq!(lines[0], "URL: https://attacker.example/mcp");
        assert!(lines.contains(&r#"1Password entry "op://Private/Header/key" will be sent to https://attacker.example/mcp (header:Authorization)"#.to_string()));
        assert!(lines.contains(
            &r#"1Password entry "op://Private/Item/key" will be sent to curl-wrapper (env:TOKEN)"#
                .to_string()
        ));
        assert!(!lines.join("\n").contains("actual-secret"));
    }
    #[test]
    fn broad_argument_hints_warn_without_rewriting_values_or_legacy_local_args() {
        let _data = crate::registry::DataDirTestEnv::new("solo-argument-hints");
        let args = vec![
            "--header",
            "Content-Type: application/json",
            "--label",
            "access-key-documentation",
            "--header=X-Version:Toolport2026",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let server: ServerEntry = serde_json::from_value(json!({"id":"docs","name":"Docs","transport":"stdio","command":"echo","args":args,"env":[]})).unwrap();
        let wire = export(&server);
        assert_eq!(wire["args"], json!(args));
        assert!(publish_error(&wire)
            .unwrap()
            .contains("may contain a credential"));
        let mut safe = wire.clone();
        safe["args"] = json!([
            "--header",
            "Content-Type: application/json",
            "--header=X-Version:Toolport2026"
        ]);
        assert_eq!(publish_error(&safe), None);
        let mut reg = machine();
        reg.servers.push(server.clone());
        apply(&mut reg, &config(vec![wire.clone()]), 1).unwrap();
        let mut legacy = wire;
        legacy["args"][3] = json!("<redacted>");
        for version in 2..=4 {
            apply(&mut reg, &config(vec![legacy.clone()]), version).unwrap();
            assert!(!reg.servers[0]
                .unknown_fields
                .contains_key("personalSyncArgsReview"));
            assert!(!reg.servers[0].needs_team_enable_review());
        }
        crate::registry::save(&reg).unwrap();
        assert_eq!(crate::registry::load().unwrap().servers[0].args, args);
        // Older peers masked `--token=X` as a bare token. Same layout, no hold.
        let mut token: ServerEntry = serde_json::from_value(json!({"id":"token","name":"Token","transport":"stdio","command":"echo","args":["--verbose","--token=actual-secret"],"env":[]})).unwrap();
        token.enabled = true;
        let mut reg = machine();
        reg.servers.push(token);
        let mut legacy = config(reg.servers.iter().map(export).collect());
        legacy["servers"][0]["args"][1] = json!("<redacted>");
        for version in 1..=3 {
            apply(&mut reg, &legacy, version).unwrap();
            assert_eq!(
                reg.servers[0].args,
                vec!["--verbose", "--token=actual-secret"]
            );
            assert!(!reg.servers[0]
                .unknown_fields
                .contains_key("personalSyncArgsReview"));
            assert!(!reg.servers[0].needs_team_enable_review());
        }
        let known: ServerEntry = serde_json::from_value(json!({"id":"auth","name":"Auth","transport":"stdio","command":"echo","args":["--token","actual-secret","--header","Authorization: Bearer actual-secret"],"env":[]})).unwrap();
        assert!(!export(&known).to_string().contains("actual-secret"));
    }
    #[test]
    fn missing_sync_sign_in_gives_account_guidance_without_claiming_success() {
        crate::secrets::tests::with_isolated_vault(|| {
            crate::registry::save(&machine()).unwrap();
            let error = crate::teams::sync_now()
                .err()
                .expect("missing sign-in must fail");
            assert!(error.contains("Sign in again"));
            assert!(!error.contains("team token"));
            let reg = crate::registry::load().unwrap();
            let (message, healthy) = banner(&reg);
            assert_eq!(message, error);
            assert!(state(&reg).unwrap().sign_in_required);
            assert!(account_display_lines(&reg)[0].contains("Saved account plan: Pro."));
            assert_eq!(
                account_actions(&state(&reg).unwrap()),
                ["Sign in", "Your account", "Sign out"]
            );
            assert_eq!(
                account_actions(&SyncState::default()),
                ["Sync now", "Your account", "Sign out"]
            );
            assert!(!healthy);
        });
    }
    #[test]
    fn ordinary_user_values_survive_export_and_two_machine_roundtrips() {
        let _data = crate::registry::DataDirTestEnv::new("solo-values");
        let mut a = machine();
        let mut s: ServerEntry = serde_json::from_value(json!({"id":"label","name":"Label","transport":"stdio","command":"echo","args":["--label","machine A v2"],"env":[]})).unwrap();
        s.enabled = true;
        let url = "https://gitmcp.io/btsouth/Toolport2026";
        let h: ServerEntry = serde_json::from_value(
            json!({"id":"docs-http","name":"Toolport docs","transport":"http","url":url,"env":[]}),
        )
        .unwrap();
        a.servers = vec![s.clone(), h];
        let wire = config(a.servers.iter().map(export).collect());
        assert_eq!(
            wire["servers"][0]["args"],
            json!(["--label", "machine A v2"])
        );
        assert_eq!(wire["servers"][1]["url"], url);
        apply(&mut a, &wire, 1).unwrap();
        crate::registry::save(&a).unwrap();
        let mut b = machine();
        apply(&mut b, &wire, 1).unwrap();
        let from_b = config(b.servers.iter().map(export).collect());
        apply(&mut a, &from_b, 2).unwrap();
        crate::registry::save(&a).unwrap();
        let a = crate::registry::load().unwrap();
        for r in [&a, &b] {
            assert_eq!(
                r.servers.iter().find(|s| s.name == "Label").unwrap().args,
                s.args
            );
            assert_eq!(
                r.servers
                    .iter()
                    .find(|s| s.name == "Toolport docs")
                    .unwrap()
                    .url
                    .as_deref(),
                Some(url)
            );
            assert!(!json!(r).to_string().contains("YOUR_API_KEY"));
        }
    }
    #[test]
    fn credential_url_is_refused_without_modification_and_named_args_stay_local() {
        let url = "https://service.example/mcp?api_key=realSecret2026";
        let mut s: ServerEntry = serde_json::from_value(json!({"id":"a","name":"A","transport":"http","url":url,"args":["--token","realSecret2026"],"env":[]})).unwrap();
        let exported = export(&s);
        assert_eq!(exported["url"], url);
        assert!(publish_error(&exported)
            .unwrap()
            .contains("has not been changed"));
        let old = s.clone();
        s.args = serde_json::from_value(exported["args"].clone()).unwrap();
        restore_local(&mut s, &old);
        assert_eq!(s.args, old.args);
        assert_eq!(s.url, old.url);
    }
    #[test]
    fn idle_polls_keep_registry_bytes_mtime_and_backup_journal() {
        let _data = crate::registry::DataDirTestEnv::new("solo-idle");
        let mut r = machine();
        let wire = config(vec![http("docs")]);
        apply(&mut r, &wire, 1).unwrap();
        crate::registry::save(&r).unwrap();
        let path = crate::registry::resolved_path().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let backups = || {
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
                .map(|e| (e.file_name(), std::fs::read(e.path()).unwrap()))
                .collect::<BTreeMap<_, _>>()
        };
        let before = backups();
        for at in 1..=10 {
            remote_update(|| {
                crate::registry::update(|r| {
                    apply(r, &wire, 1)?;
                    mark_synced(r.team.as_ref().unwrap(), at)?;
                    Ok(())
                })
            })
            .unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(backups(), before);
        assert_eq!(
            state(&crate::registry::load().unwrap())
                .unwrap()
                .last_synced_at,
            Some(10)
        );
        assert!(!String::from_utf8(bytes).unwrap().contains("lastSyncedAt"));
        let mut other = machine();
        other.team.as_mut().unwrap().reporting_device_id = "other".into();
        assert_eq!(state(&other).unwrap().last_synced_at, None);
    }
    #[test]
    fn status_never_claims_green_while_first_sync_review_or_conflict_is_pending() {
        let _data = crate::registry::DataDirTestEnv::new("solo-banner");
        let mut r = machine();
        assert_eq!(banner(&r), ("Waiting for first sync.".into(), false));
        let mut st = SyncState {
            initialized: true,
            ..SyncState::default()
        };
        save(&mut r, &st).unwrap();
        mark_synced(r.team.as_ref().unwrap(), 1).unwrap();
        assert!(banner(&r).1);
        st.conflicts.insert("docs".into(), http("docs"));
        save(&mut r, &st).unwrap();
        assert!(!banner(&r).1);
        assert!(banner(&r).0.contains("choice"));
        st.conflicts.clear();
        save(&mut r, &st).unwrap();
        let mut s: ServerEntry = serde_json::from_value(http("docs")).unwrap();
        s.require_team_enable_review();
        s.unknown_fields
            .insert("personalSyncEntry".into(), json!(true));
        r.servers.push(s);
        assert_eq!(
            banner(&r),
            (
                "1 server is waiting for review on this machine.".into(),
                false
            )
        );
        let status = json!({"plan":"free","freeSyncGraceEndsAt":0,"canReceiveConfig":false,"reason":"Choose your device in Your account."});
        let lines = status_lines_at(&status, Some(0), 60_000);
        assert_eq!(lines.iter().filter(|s| s.contains("Choose")).count(), 1);
        assert!(lines.contains(&"Last synced 1 minute ago".into()));
        assert_eq!(crate::teams::sync_retry_seconds(&r, 4), 60);
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["canReceiveConfig"] = json!(false);
        assert_eq!(crate::teams::sync_retry_seconds(&r, 4), 480);
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["canReceiveConfig"] = json!(true);
        r.servers.clear();
        assert!(banner(&r).1);
    }
    #[test]
    fn new_local_only_server_never_enters_the_sync_journal() {
        let _data = crate::registry::DataDirTestEnv::new(
            "new_local_only_server_never_enters_the_sync_journal",
        );
        let mut before = machine();
        apply(&mut before, &config(vec![]), 1).unwrap();
        let mut after = before.clone();
        crate::registry_controller::apply_add_server(
            &mut after,
            crate::registry_controller::ServerFields {
                sync_local_only: true,
                name: "Private".into(),
                transport: "stdio".into(),
                command: Some("echo".into()),
                args: vec!["machine A v2".into()],
                url: None,
                cwd: None,
            },
        )
        .unwrap();
        record(&before, &mut after).unwrap();
        assert!(keep_local(&after.servers[0]));
        assert!(state(&after).unwrap().pending.is_empty());
    }
    #[test]
    fn conflicts_show_names_both_values_and_visible_controls() {
        let mut a = http("docs-http");
        a["name"] = json!("Toolport docs");
        a["url"] = json!("https://example.com/this");
        let mut b = a.clone();
        b["url"] = json!("https://example.com/other");
        b["args"] = json!(["line\nnext"]);
        b["headerKeys"] = json!([{"key":"Authorization","env":"TOKEN"}]);
        b["env"] = json!([{"key":"TOKEN","secret":true},{"key":"REGION","secret":false}]);
        b["launch"] = json!({"inputs":[{"key":"project","secret":false}],"bindings":[{"index":0,"parts":[{"kind":"input","key":"project"}]}]});
        let left = conflict_fields(Some(&a));
        let right = conflict_fields(Some(&b));
        assert_eq!(left["Name"], "Toolport docs");
        assert_ne!(left["URL"], right["URL"]);
        assert_eq!(right["Argument 1"], r"line\u{000A}next");
        assert_eq!(right["Header: Authorization"], "Uses environment: TOKEN");
        assert_eq!(right["Environment: TOKEN"], "<masked secret>");
        assert_eq!(right["Environment: REGION"], "Set on this machine");
        assert_eq!(right["Input: project"], "Set on this machine");
        assert_eq!(right["Argument values"], "Argument 1 = {project}");
        let text = format!("{right:?}");
        assert!(!text.contains("null"));
        assert!(!text.contains("Launch bindings"));
    }
    #[test]
    fn personal_pairing_copy_and_custom_origin_are_accurate() {
        let fresh = crate::teams::pairing_confirm_copy("https://sync.example.com", false);
        assert!(fresh.contains("Sync service:"));
        assert!(!fresh.contains("replaces"));
        assert!(!fresh.contains("Control plane"));
        assert!(
            crate::teams::pairing_confirm_copy("https://sync.example.com", true)
                .contains("replaces")
        );
        assert_eq!(
            crate::teams::sync_sign_in_url("http://127.0.0.1:18787").unwrap(),
            "http://127.0.0.1:18787/?intent=pro&from=app-sync"
        );
    }
    fn machine() -> Registry {
        let mut r = Registry::default();
        r.team=Some(serde_json::from_value(json!({"serverUrl":"https://example.com","teamId":"solo","role":"admin","reportingDeviceId":"fixture","accountStatus":{"personalSync":true,"plan":"pro","canReceiveConfig":true}})).unwrap());
        r
    }
    fn http(id: &str) -> Value {
        json!({"id":id,"name":id,"transport":"http","url":"https://example.com/mcp","args":[],"env":[],"disabled":false})
    }
    fn command(id: &str) -> Value {
        json!({"id":id,"name":id,"transport":"stdio","command":"npx","args":["-y","fixture"],"env":[],"disabled":false})
    }
    fn local(value: Value) -> ServerEntry {
        serde_json::from_value(value).unwrap()
    }
    fn config(rows: Vec<Value>) -> Value {
        json!({"servers":rows,"futureMetadata":{"keep":true}})
    }
    #[test]
    fn synced_package_registry_and_execution_overrides_fail_closed() {
        let _data = crate::registry::DataDirTestEnv::new("sync-env-attack");
        for key in [
            "PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_FRAMEWORK_PATH",
            "NODE_OPTIONS",
            "NODE_PATH",
            "npm_config_registry",
            "NPM_CONFIG_REGISTRY",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "PERL5OPT",
            "PERL5LIB",
            "RUBYOPT",
            "RUBYLIB",
            "PYTHONHOME",
            "UV_INDEX",
            "PIP_CONFIG_FILE",
            "NODE_EXTRA_CA_CERTS",
            "LD_DEBUG",
            "LD_PROFILE",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_COUNT",
            "GIT_EXEC_PATH",
            "PYTHONUSERBASE",
            "PYTHONINSPECT",
            "PIP_INDEX_URL",
            "PIP_EXTRA_INDEX_URL",
            "UV_INDEX_URL",
            "UV_EXTRA_INDEX_URL",
            "UV_DEFAULT_INDEX",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_ASKPASS",
            "GIT_PROXY_COMMAND",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "PIP_FIND_LINKS",
            "PIP_TRUSTED_HOST",
            "UV_FIND_LINKS",
            "UV_INSECURE_HOST",
            "DOCKER_HOST",
            "RUSTC_WRAPPER",
        ] {
            let mut r = machine();
            let trusted = command("package");
            apply(&mut r, &config(vec![trusted.clone()]), 1).unwrap();
            crate::registry::save(&r).unwrap();
            let reviewed = r.servers[0].clone();
            r = enable_reviewed(&r.active_profile_id(), &reviewed).unwrap();
            let mut attack = trusted.clone();
            attack["env"] =
                json!([{"key":key,"secret":false,"portable":true,"value":"https://evil/"}]);
            assert!(publish_error(&attack).is_some(), "{key}");
            assert_eq!(
                apply(&mut r, &config(vec![attack]), 2).unwrap().blocked,
                1,
                "{key}"
            );
            assert!(!r.servers[0].enabled, "{key}");
        }
    }
    #[test]
    fn execution_review_shows_values_bindings_cwd_inheritance_and_changes() {
        let mut row = command("review");
        row["command"] = json!("npx\u{202e}");
        row["cwd"] = json!("/work\n\u{200b}");
        row["env"] = json!([{"key":"REGION","secret":false,"value":"west\t"},{"key":"TOKEN","secret":true,"value":"secret-value"}]);
        row["launch"] = json!({"inputs":[{"key":"project","label":"Project","secret":false,"value":"/work"},{"key":"auth","label":"Auth","secret":true,"value":"input-secret"}],"bindings":[{"index":1,"parts":[{"kind":"input","key":"project"}]}]});
        let mut server = local(row);
        server.inherit_env = true;
        let fields = execution_review_fields(&server);
        server
            .unknown_fields
            .insert("syncExecutionReview".into(), json!(fields));
        server.env[0].value = Some("east".into());
        let text = execution_review_lines(&server).join("\n");
        assert_eq!(text, "Environment: REGION = east");
        let full = execution_review_fields(&server)
            .into_iter()
            .map(|(k, v)| review_field_line(&k, &v))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full.contains("Environment: TOKEN = <masked secret>"));
        assert!(full.contains("Input: project = /work"));
        assert!(full.contains("Input: auth = <masked secret>"));
        assert!(full.contains("Argument values:"));
        assert!(full.contains("Uses this machine's environment: yes"));
        assert!(full.contains("npx\\u{202E}"));
        assert!(full.contains("/work\\u{000A}\\u{200B}"));
        assert!(!full.contains("secret-value"));
        assert!(!full.contains("input-secret"));
        server.env.clear();
        assert!(execution_review_lines(&server)
            .iter()
            .any(|line| line == "Environment: REGION = Removed"));
    }
    #[test]
    fn acknowledgement_rebases_newer_edits_including_disable_and_lost_response() {
        let _data = crate::registry::DataDirTestEnv::new("sync-ack-rebase");
        for (disable, undo) in [(false, false), (true, false), (false, true)] {
            let mut r = machine();
            apply(&mut r, &config(vec![http("a")]), 0).unwrap();
            let before = r.clone();
            if disable {
                r.servers[0].enabled = false;
            } else {
                r.servers[0].name = "First".into();
            }
            record(&before, &mut r).unwrap();
            let mut st = state(&r).unwrap();
            let sent = st.pending["a"].clone();
            st.publishing.insert("a".into(), sent.clone());
            save(&mut r, &st).unwrap();
            let before = r.clone();
            r.servers[0].name = if undo { "a" } else { "Second" }.into();
            r.servers[0].enabled = true;
            record(&before, &mut r).unwrap();
            // No explicit HTTP acknowledgement: the pull repairs a lost response.
            let cloud = config(vec![sent.after.clone().unwrap()]);
            apply(&mut r, &cloud, 1).unwrap();
            let st = state(&r).unwrap();
            assert!(st.publishing.is_empty());
            assert_eq!(st.pending["a"].before, sent.after);
            assert_eq!(r.servers[0].name, if undo { "a" } else { "Second" });
            assert!(r.servers[0].enabled);
            assert!(merge(&cloud, &st.pending).unwrap().1.is_empty());
        }
    }
    #[test]
    fn private_destinations_require_exact_review_and_metadata_keeps_consent() {
        let _data = crate::registry::DataDirTestEnv::new("sync-private-url");
        for url in [
            "http://127.0.0.1/mcp",
            "http://localhost/mcp",
            "http://10.1.2.3/mcp",
            "http://192.168.1.2/mcp",
            "http://172.16.2.3/mcp",
            "http://[::1]/mcp",
        ] {
            let mut r = machine();
            let mut row = http("a");
            apply(&mut r, &config(vec![row.clone()]), 0).unwrap();
            row["url"] = json!(url);
            assert_eq!(
                apply(&mut r, &config(vec![row.clone()]), 1).unwrap().review,
                1,
                "{url}"
            );
            assert!(!r.servers[0].enabled);
            crate::registry::save(&r).unwrap();
            let reviewed = r.servers[0].clone();
            r = enable_reviewed(&r.active_profile_id(), &reviewed).unwrap();
            row["name"] = json!("Metadata");
            assert_eq!(
                apply(&mut r, &config(vec![row.clone()]), 2).unwrap().review,
                0
            );
            assert!(r.servers[0].enabled);
            row["url"] = json!("http://10.9.8.7/mcp");
            assert_eq!(apply(&mut r, &config(vec![row]), 3).unwrap().review, 1);
        }
        let mut r = machine();
        let mut row = http("blocked");
        row["url"] = json!("http://169.254.169.254/mcp");
        assert_eq!(apply(&mut r, &config(vec![row]), 1).unwrap().blocked, 1);
        assert!(r.servers.is_empty());
    }
    #[test]
    fn opting_out_does_not_delete_the_cloud_copy() {
        let _data = crate::registry::DataDirTestEnv::new("sync-opt-out");
        let mut r = machine();
        let cloud = config(vec![http("a")]);
        apply(&mut r, &cloud, 1).unwrap();
        crate::registry::save(&r).unwrap();
        r = set_local_only(&r.servers[0].id, true).unwrap();
        assert!(state(&r).unwrap().pending.is_empty());
        assert_eq!(merge(&cloud, &state(&r).unwrap().pending).unwrap().0, cloud);
        apply(&mut r, &config(vec![]), 2).unwrap();
        assert_eq!(r.servers.len(), 1);
    }
    #[test]
    fn becoming_personal_asks_once_and_defaults_private_locals_to_none() {
        let _data = crate::registry::DataDirTestEnv::new("sync-mode-shrink");
        let mut r = machine();
        let mut private = local(http("private"));
        private.enabled = true;
        r.servers.push(private);
        mode_changed(&mut r, false).unwrap();
        apply(&mut r, &config(vec![http("shared")]), 1).unwrap();
        assert!(state(&r).unwrap().choose_local_servers);
        assert!(state(&r).unwrap().pending.is_empty());
        assert!(keep_local(
            r.servers.iter().find(|s| s.id == "private").unwrap()
        ));
        mode_changed(&mut r, true).unwrap();
        crate::registry::save(&r).unwrap();
        r = finish_local_selection().unwrap();
        assert!(!state(&r).unwrap().choose_local_servers);
        r = set_local_only("private", false).unwrap();
        assert!(state(&r).unwrap().pending.contains_key("private"));
    }
    #[test]
    fn becoming_governed_keeps_ids_masked_args_values_and_credential_owners() {
        let _data = crate::registry::DataDirTestEnv::new("sync-mode-grow");
        let mut r = machine();
        let mut row = command("exec");
        row["args"] = json!(["--token", "<redacted>"]);
        row["env"] = json!([{"key":"REGION","secret":false}]);
        let mut installed = row.clone();
        installed["args"][1] = json!("local-argument-secret");
        let mut installed = local(installed);
        installed.enabled = true;
        r.servers.push(installed);
        apply(&mut r, &config(vec![row.clone(), http("remote")]), 1).unwrap();
        let s = r.servers.iter_mut().find(|s| s.command.is_some()).unwrap();
        s.args[1] = "local-argument-secret".into();
        s.env[0].value = Some("local-region".into());
        let ids: Vec<_> = r.servers.iter().map(|s| s.id.clone()).collect();
        let remote_id = r
            .servers
            .iter()
            .find(|s| s.url.is_some())
            .unwrap()
            .id
            .clone();
        let owner = crate::local_auth::owner_in(&r, &remote_id).unwrap();
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["personalSync"] = json!(false);
        mode_changed(&mut r, true).unwrap();
        crate::teams::apply_team_config(&mut r, "solo", &config(vec![row.clone(), http("remote")]));
        assert_eq!(
            r.servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            ids
        );
        let s = r.servers.iter().find(|s| s.command.is_some()).unwrap();
        assert_eq!(s.args[1], "local-argument-secret");
        assert_eq!(s.env[0].value.as_deref(), Some("local-region"));
        assert_eq!(crate::local_auth::owner_in(&r, &remote_id).unwrap(), owner);
        // The staged governed path also retains the same installed identity.
        crate::teams::stage_team_config(&mut r, "solo", &config(vec![row, http("remote")]), 2, &[])
            .unwrap();
        assert_eq!(crate::local_auth::owner_in(&r, &remote_id).unwrap(), owner);
        assert!(r
            .servers
            .iter()
            .any(|s| s.id == ids[0] && s.args[1] == "local-argument-secret"));
    }
    #[test]
    fn mode_transitions_never_export_local_reference_overrides() {
        let _data = crate::registry::DataDirTestEnv::new("sync-mode-reference-boundary");
        let mut r = machine();
        let mut row = http("references");
        row["headerKeys"] = json!([{"key":"X-Key","source":{"ref":"op://Team/service/key"}}]);
        let cloud = config(vec![row]);
        apply(&mut r, &cloud, 1).unwrap();
        r.servers[0].unknown_fields.insert(
            "memberSecretRefs".into(),
            json!({"header:X-Key":"op://Local/service/key"}),
        );
        r.servers[0].unknown_fields.insert(
            "personalSyncRemoteRefs".into(),
            json!({"header:X-Key":{"ref":"op://Team/service/key"}}),
        );
        r.servers[0].unknown_fields.get_mut("headerKeys").unwrap()[0]["source"] =
            json!({"ref":"op://Local/service/key"});
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["personalSync"] = json!(false);
        mode_changed(&mut r, true).unwrap();
        crate::teams::apply_team_config(&mut r, "solo", &cloud);
        assert_eq!(
            r.servers[0].unknown_fields["headerKeys"][0]["source"]["ref"],
            "op://Local/service/key"
        );
        assert_eq!(
            export(&r.servers[0])["headerKeys"][0]["source"]["ref"],
            "op://Team/service/key"
        );
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["personalSync"] = json!(true);
        mode_changed(&mut r, false).unwrap();
        apply(&mut r, &cloud, 2).unwrap();
        assert_eq!(
            export(&r.servers[0])["headerKeys"][0]["source"]["ref"],
            "op://Team/service/key"
        );
        assert!(state(&r).unwrap().pending.is_empty());
    }
    #[test]
    fn governance_transition_retains_disabled_personal_definitions_locally() {
        let _data = crate::registry::DataDirTestEnv::new("sync-mode-disabled");
        let mut r = machine();
        let mut row = http("off");
        row["disabled"] = json!(true);
        apply(&mut r, &config(vec![row]), 1).unwrap();
        let id = r.servers[0].id.clone();
        let owner = crate::local_auth::owner_in(&r, &id).unwrap();
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["personalSync"] = json!(false);
        mode_changed(&mut r, true).unwrap();
        crate::teams::stage_team_config(&mut r, "solo", &config(vec![]), 2, &[]).unwrap();
        assert_eq!(r.servers[0].id, id);
        assert!(!r.servers[0].enabled);
        assert!(keep_local(&r.servers[0]));
        assert_eq!(crate::local_auth::owner_in(&r, &id).unwrap(), owner);
    }
    #[test]
    fn name_matching_never_gives_two_local_servers_one_cloud_id() {
        let _data = crate::registry::DataDirTestEnv::new("sync-name-identity");
        let cloud = config(vec![http("cloud")]);
        let mut r = machine();
        for id in ["first", "second"] {
            let mut s = local(http(id));
            s.name = "cloud".into();
            r.servers.push(s);
        }
        apply(&mut r, &cloud, 1).unwrap();
        assert_eq!(definitions(&r).len(), 2);
        assert_eq!(
            r.servers.iter().filter(|s| original(s) == "cloud").count(),
            1
        );
        let before = r.clone();
        let mut s = local(http("third"));
        s.name = "cloud".into();
        r.servers.push(s);
        record(&before, &mut r).unwrap();
        assert_eq!(definitions(&r).len(), 3);
        assert_eq!(
            r.servers.iter().filter(|s| original(s) == "cloud").count(),
            1
        );
    }
    #[test]
    fn first_sync_reserves_existing_cloud_identity_before_name_matching() {
        let _data = crate::registry::DataDirTestEnv::new("sync-reserved-identity");
        let mut r = machine();
        let mut sibling = local(http("private"));
        sibling.name = "cloud".into();
        let mut bound = local(http("bound"));
        bound.name = "cloud".into();
        bound
            .unknown_fields
            .insert("teamOriginalId".into(), json!("cloud"));
        r.servers = vec![sibling, bound];
        apply(&mut r, &config(vec![http("cloud")]), 1).unwrap();
        assert_eq!(
            original(r.servers.iter().find(|s| s.id == "bound").unwrap()),
            "cloud"
        );
        assert_eq!(
            original(r.servers.iter().find(|s| s.id == "private").unwrap()),
            "private"
        );
        assert_eq!(definitions(&r).len(), 2);
        let mut explicit = machine();
        let mut bound = local(http("cloud"));
        bound.name = "Same name".into();
        explicit.servers.push(bound);
        let mut a = http("cloud");
        a["name"] = json!("Same name");
        let mut b = http("other");
        b["name"] = json!("Same name");
        apply(&mut explicit, &config(vec![a, b]), 1).unwrap();
        assert_eq!(
            original(explicit.servers.iter().find(|s| s.id == "cloud").unwrap()),
            "cloud"
        );
    }
    #[test]
    fn full_review_rejects_inputs_or_inheritance_changed_while_open() {
        let _data = crate::registry::DataDirTestEnv::new("sync-reviewed-inputs");
        let mut r = machine();
        let mut row = command("review");
        row["env"] = json!([{"key":"REGION","secret":false,"portable":true,"value":"west"}]);
        apply(&mut r, &config(vec![row]), 1).unwrap();
        let reviewed = r.servers[0].clone();
        assert!(check_review(&r, &r.servers[0], Some(&reviewed)).is_ok());
        r.servers[0].env[0].value = Some("east".into());
        assert!(check_review(&r, &r.servers[0], Some(&reviewed)).is_err());
        r.servers[0] = reviewed.clone();
        r.servers[0].inherit_env = true;
        assert!(check_review(&r, &r.servers[0], Some(&reviewed)).is_err());
    }
    #[test]
    fn conflict_choice_uses_an_opaque_version_even_if_js_normalizes_numbers() {
        let _data = crate::registry::DataDirTestEnv::new("sync-conflict-numbers");
        let mut r = machine();
        let mut remote = http("a");
        remote["requestTimeoutMs"] = json!(1.0);
        let mut st = SyncState::default();
        st.conflicts.insert("a".into(), remote.clone());
        st.pending.insert(
            "a".into(),
            Mutation {
                after: Some(http("a")),
                ..Default::default()
            },
        );
        save(&mut r, &st).unwrap();
        crate::registry::save(&r).unwrap();
        let opaque = state(&r).unwrap().conflict_versions["a"].clone();
        let mut javascript_value = remote.clone();
        javascript_value["requestTimeoutMs"] = json!(1);
        assert_ne!(javascript_value, remote);
        assert!(resolve_conflict("a", &opaque, true).is_ok());
    }
    #[test]
    fn two_machines_http_add_edit_delete_without_review() {
        let _data = crate::registry::DataDirTestEnv::new("solo-http");
        let mut a = machine();
        let mut b = machine();
        apply(&mut a, &config(vec![]), 0).unwrap();
        apply(&mut b, &config(vec![]), 0).unwrap();
        let before = a.clone();
        let mut s = local(http("docs"));
        s.enabled = true;
        a.servers.push(s);
        record(&before, &mut a).unwrap();
        let (cloud, conflicts) = merge(&config(vec![]), &state(&a).unwrap().pending).unwrap();
        assert!(conflicts.is_empty());
        let out = apply(&mut b, &cloud, 1).unwrap();
        assert_eq!(out.review, 0);
        assert_eq!(b.servers.len(), 1);
        assert!(b.servers[0].enabled);
        let id = b.servers[0].id.clone();
        let mut edited = cloud.clone();
        edited["servers"][0]["name"] = json!("New name");
        apply(&mut b, &edited, 2).unwrap();
        assert_eq!(b.servers[0].id, id);
        assert_eq!(b.servers[0].name, "New name");
        assert!(b.servers[0].enabled);
        apply(&mut b, &config(vec![]), 3).unwrap();
        assert!(b.servers.is_empty());
    }
    #[test]
    fn command_requires_one_exact_local_confirmation_and_changed_command_reopens_it() {
        let _data = crate::registry::DataDirTestEnv::new("solo-command");
        let mut b = machine();
        let cloud = config(vec![command("tool")]);
        assert_eq!(apply(&mut b, &cloud, 1).unwrap().review, 1);
        let id = b.servers[0].id.clone();
        assert!(!b.servers[0].enabled);
        let reviewed = b.servers[0].clone();
        let profile = b.active_profile_id();
        check_review(&b, &b.servers[0], Some(&reviewed)).unwrap();
        crate::registry_controller::apply_server_enabled(&mut b, &profile, &id, true, true)
            .unwrap();
        assert_eq!(apply(&mut b, &cloud, 1).unwrap().review, 0);
        assert!(b.servers[0].enabled);
        let mut changed = cloud.clone();
        changed["servers"][0]["args"] = json!(["-y", "different"]);
        assert_eq!(apply(&mut b, &changed, 2).unwrap().review, 1);
        assert!(!b.servers[0].enabled);
        assert!(check_review(&b, &b.servers[0], Some(&reviewed)).is_err());
    }
    #[test]
    fn choosing_a_conflicting_remote_command_still_requires_local_review() {
        let _data = crate::registry::DataDirTestEnv::new("solo-conflict-command");
        let mut r = machine();
        let first = config(vec![command("tool")]);
        apply(&mut r, &first, 1).unwrap();
        let reviewed = r.servers[0].clone();
        let profile = r.active_profile_id();
        crate::registry_controller::apply_server_enabled(
            &mut r,
            &profile,
            &reviewed.id,
            true,
            true,
        )
        .unwrap();
        let before = r.clone();
        r.servers[0].args = vec!["-y".into(), "local-edit".into()];
        record(&before, &mut r).unwrap();
        let mut remote = first.clone();
        remote["servers"][0]["args"] = json!(["-y", "remote-edit"]);
        apply(&mut r, &remote, 2).unwrap(); // Advances fetched baseline while pending stays local.
        let (_, conflicts) = merge(&remote, &state(&r).unwrap().pending).unwrap();
        let mut st = state(&r).unwrap();
        st.conflicts = conflicts;
        save(&mut r, &st).unwrap();
        crate::registry::save(&r).unwrap();
        let mut r =
            resolve_conflict("tool", &conflict_version(&remote["servers"][0]), false).unwrap();
        assert_eq!(apply(&mut r, &remote, 2).unwrap().review, 1);
        assert!(!r.servers[0].enabled);
        assert_eq!(r.servers[0].args, ["-y", "remote-edit"]);
    }
    #[test]
    fn first_sign_in_conflicts_when_a_same_named_definition_differs() {
        let _data = crate::registry::DataDirTestEnv::new("solo-initial-conflict");
        let mut r = machine();
        let mut mine = local(http("local"));
        mine.name = "docs".into();
        mine.url = Some("https://local.example/mcp".into());
        mine.enabled = true;
        r.servers.push(mine);
        let cloud = config(vec![http("docs")]);
        apply(&mut r, &cloud, 1).unwrap();
        let (merged, conflicts) = merge(&cloud, &state(&r).unwrap().pending).unwrap();
        assert_eq!(merged, cloud);
        assert!(conflicts.contains_key("docs"));
        let (_, deleted_conflict) = merge(&config(vec![]), &state(&r).unwrap().pending).unwrap();
        assert_eq!(deleted_conflict.get("docs"), Some(&Value::Null));
        assert_eq!(r.servers.len(), 1);
        assert_eq!(
            r.servers[0].url.as_deref(),
            Some("https://local.example/mcp")
        );
    }
    #[test]
    fn accepting_remote_delete_after_conflict_removes_the_local_route() {
        let _data = crate::registry::DataDirTestEnv::new("solo-conflict-delete");
        let mut r = machine();
        let cloud = config(vec![http("docs")]);
        apply(&mut r, &cloud, 1).unwrap();
        let before = r.clone();
        r.servers[0].name = "Local edit".into();
        record(&before, &mut r).unwrap();
        let deleted = config(vec![]);
        apply(&mut r, &deleted, 2).unwrap();
        let (_, conflicts) = merge(&deleted, &state(&r).unwrap().pending).unwrap();
        let mut st = state(&r).unwrap();
        st.conflicts = conflicts;
        save(&mut r, &st).unwrap();
        crate::registry::save(&r).unwrap();
        let r = resolve_conflict("docs", &conflict_version(&Value::Null), false).unwrap();
        assert!(r.servers.is_empty());
        assert!(!r
            .profiles
            .iter()
            .any(|p| p.enabled_server_ids.iter().any(|id| id == "docs")));
        assert!(state(&r).unwrap().pending.is_empty());
    }
    #[test]
    fn reference_sources_export_only_the_reference_and_use_secret_wire_defaults() {
        let mut s = local(http("ref"));
        s.env = serde_json::from_value(json!([{"key":"TOKEN","secret":false,"source":{"ref":"op://Shared/Token/key","approval":"local-marker"}}])).unwrap();
        s.unknown_fields.insert("headerKeys".into(), json!([{"key":"X-Key","source":{"ref":"op://Shared/Token/key","approval":"local-marker","value":"synthetic-private-value"}}]));
        let wire = export(&s);
        assert!(wire["env"][0].get("secret").is_none());
        assert_eq!(
            wire["headerKeys"][0]["source"],
            json!({"ref":"op://Shared/Token/key"})
        );
        assert!(!wire.to_string().contains("local-marker"));
        assert!(!wire.to_string().contains("synthetic-private-value"));
    }
    #[test]
    fn literal_launch_credentials_are_not_exported() {
        let mut s = local(command("tool"));
        s.args = vec!["<launch-input>".into()];
        s.launch = Some(serde_json::from_value(json!({"inputs":[],"bindings":[{"index":0,"parts":[{"kind":"literal","value":"--token=synthetic-private-token"}]}]})).unwrap());
        let wire = export(&s).to_string();
        assert!(!wire.contains("synthetic-private-token"));
        assert!(wire.contains("<redacted>"));
    }
    #[test]
    fn native_status_copy_covers_trial_grace_and_blocked_delivery() {
        let status = json!({"plan":"pro","trialActive":true,"trialEndsAt":7*86_400_000,"freeSyncGraceEndsAt":2*86_400_000,"canReceiveConfig":false,"reason":"Choose this device"});
        let lines = status_lines_at(&status, Some(0), 0);
        assert!(lines.iter().any(|s| s == "7 trial days left"));
        assert!(lines.iter().any(|s| s.contains("2 more days")));
        assert!(lines.iter().any(|s| s == "Choose this device"));
        assert!(lines.iter().any(|s| s == "Last synced just now"));
    }
    #[test]
    fn review_hold_is_local_and_does_not_publish_disable() {
        let _data = crate::registry::DataDirTestEnv::new("solo-held-edit");
        let mut r = machine();
        let cloud = config(vec![command("tool")]);
        apply(&mut r, &cloud, 1).unwrap();
        let before = r.clone();
        r.servers[0].name = "Renamed".into();
        record(&before, &mut r).unwrap();
        assert_eq!(
            state(&r).unwrap().pending["tool"].after.as_ref().unwrap()["disabled"],
            false
        );
        assert!(!r.servers[0].enabled);
    }
    #[test]
    fn metadata_edits_do_not_reopen_command_review() {
        let _data = crate::registry::DataDirTestEnv::new("solo-metadata");
        let mut r = machine();
        let cloud = config(vec![command("tool")]);
        apply(&mut r, &cloud, 1).unwrap();
        let reviewed = r.servers[0].clone();
        let id = reviewed.id.clone();
        let profile = r.active_profile_id();
        check_review(&r, &r.servers[0], Some(&reviewed)).unwrap();
        crate::registry_controller::apply_server_enabled(&mut r, &profile, &id, true, true)
            .unwrap();
        let mut changed = cloud.clone();
        changed["servers"][0]["name"] = json!("New display name");
        changed["servers"][0]["requestTimeoutMs"] = json!(5000);
        assert_eq!(apply(&mut r, &changed, 2).unwrap().review, 0);
        assert!(r.servers[0].enabled);
    }
    #[test]
    fn http_url_changes_apply_without_reusing_the_previous_destinations_vault() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut b = machine();
            let original = config(vec![http("docs")]);
            // Cloud identities cannot select a leftover machine-local vault.
            crate::secrets::set_secret("docs", crate::secrets::HTTP_AUTH_KEY, "synthetic-orphan")
                .unwrap();
            apply(&mut b, &original, 1).unwrap();
            let id = b.servers[0].id.clone();
            let owner = crate::local_auth::owner_in(&b, &id).unwrap();
            assert_ne!(owner, id);
            crate::registry::save(&b).unwrap();
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY).unwrap(),
                None
            );
            crate::secrets::set_secret(&id, crate::secrets::HTTP_AUTH_KEY, "synthetic-old-token")
                .unwrap();
            assert_eq!(
                crate::remote::current_credential(&id).unwrap().as_deref(),
                Some("synthetic-old-token")
            );
            // The shared URL guard fails closed on unresolved DNS. This
            // credential test needs an explicitly public synthetic destination.
            let _public = crate::teams::PublicTeamHostOverride::set("changed.example");
            let mut changed = original.clone();
            changed["servers"][0]["url"] = json!("https://changed.example/mcp");
            assert_eq!(apply(&mut b, &changed, 2).unwrap().review, 0);
            assert_eq!(b.servers[0].id, id);
            assert!(b.servers[0].enabled);
            let new_owner = crate::local_auth::owner_in(&b, &id).unwrap();
            assert_ne!(new_owner, owner);
            // Remove/recreate must not make an old raw namespace available to
            // a different destination, even before the next sync round.
            let mut recreated = b.clone();
            let mut replacement = local(http("docs"));
            replacement.url = Some("https://changed.example/mcp".into());
            let replacement_destination =
                crate::local_auth::personal_credential_destination(&replacement);
            replacement.unknown_fields.insert(
                "personalSyncCredentialDestination".into(),
                json!(replacement_destination),
            );
            recreated.servers = vec![replacement];
            assert_eq!(
                crate::local_auth::owner_in(&recreated, &id).unwrap(),
                new_owner
            );
            crate::registry::save(&b).unwrap();
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY).unwrap(),
                None
            );
            assert_eq!(crate::remote::current_credential(&id).unwrap(), None);
            crate::secrets::set_secret(&id, crate::secrets::HTTP_AUTH_KEY, "synthetic-new-token")
                .unwrap();
            apply(&mut b, &changed, 2).unwrap();
            assert_eq!(crate::local_auth::owner_in(&b, &id).unwrap(), new_owner);
            crate::registry::save(&b).unwrap();
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY)
                    .unwrap()
                    .as_deref(),
                Some("synthetic-new-token")
            );
            assert_eq!(
                crate::remote::current_credential(&id).unwrap().as_deref(),
                Some("synthetic-new-token")
            );
            {
                let _old_request = crate::local_auth::pin_http_destination(
                    &id,
                    original["servers"][0]["url"].as_str().unwrap(),
                )
                .unwrap();
                assert_eq!(
                    crate::remote::current_credential(&id).unwrap().as_deref(),
                    Some("synthetic-old-token")
                );
                // An old refresh finishing late writes only to its old scope.
                crate::secrets::set_secret(
                    &id,
                    crate::secrets::HTTP_AUTH_KEY,
                    "synthetic-old-rotated",
                )
                .unwrap();
                assert_eq!(
                    crate::remote::current_credential(&id).unwrap().as_deref(),
                    Some("synthetic-old-rotated")
                );
            }
            assert_eq!(
                crate::remote::current_credential(&id).unwrap().as_deref(),
                Some("synthetic-new-token")
            );
            assert!(export(&b.servers[0])
                .get("personalSyncCredentialDestination")
                .is_none());
            apply(&mut b, &original, 3).unwrap();
            assert_eq!(crate::local_auth::owner_in(&b, &id).unwrap(), owner);
            crate::registry::save(&b).unwrap();
            assert_eq!(
                crate::secrets::get_secret_result(&id, crate::secrets::HTTP_AUTH_KEY)
                    .unwrap()
                    .as_deref(),
                Some("synthetic-old-rotated")
            );
        });
    }
    #[test]
    fn adopted_legacy_copy_uses_one_existing_credential_identity() {
        let _data = crate::registry::DataDirTestEnv::new("solo-adoption");
        let mut r = machine();
        let mut personal = local(http("docs"));
        personal.enabled = false;
        let mut managed = personal.clone();
        managed.id = "team_solo_docs".into();
        managed.source = Some("team:solo".into());
        managed.enabled = true;
        managed
            .unknown_fields
            .insert("teamOriginalId".into(), json!("docs"));
        r.team
            .as_mut()
            .unwrap()
            .managed_server_ids
            .insert(managed.id.clone(), personal.id.clone());
        r.servers = vec![personal.clone(), managed.clone()];
        crate::local_auth::bind(&mut r, &managed, &personal).unwrap();
        apply(&mut r, &config(vec![http("docs")]), 1).unwrap();
        assert_eq!(r.servers.len(), 1);
        assert_eq!(r.servers[0].id, "docs");
        assert!(r.servers[0].enabled);
        assert_eq!(crate::local_auth::owner_in(&r, "docs").unwrap(), "docs");
        assert!(state(&r).unwrap().pending.is_empty());
    }
    #[test]
    fn removed_bound_original_is_not_republished_on_first_sync() {
        let _data = crate::registry::DataDirTestEnv::new("solo-removed");
        let mut r = machine();
        let mut old = local(http("gone"));
        old.enabled = false;
        old.unknown_fields
            .insert("teamRouteRemoved".into(), json!(true));
        r.servers.push(old);
        apply(&mut r, &config(vec![]), 2).unwrap();
        assert!(state(&r).unwrap().pending.is_empty());
        assert!(!r.servers[0].enabled);
    }
    #[test]
    fn local_reference_override_is_retained_and_never_exported() {
        let _data = crate::registry::DataDirTestEnv::new("solo-local-ref");
        let mut r = machine();
        let mut row = http("docs");
        row["headerKeys"] = json!([{"key":"X-Key","source":{"ref":"op://Shared/Token/key"}}]);
        let cloud = config(vec![row]);
        apply(&mut r, &cloud, 1).unwrap();
        r.servers[0].unknown_fields.insert(
            "memberSecretRefs".into(),
            json!({"header:X-Key":"op://Private/Token/key"}),
        );
        apply(&mut r, &cloud, 1).unwrap();
        assert_eq!(
            r.servers[0].unknown_fields["headerKeys"][0]["source"]["ref"],
            "op://Private/Token/key"
        );
        let wire = export(&r.servers[0]);
        assert_eq!(
            wire["headerKeys"][0]["source"]["ref"],
            "op://Shared/Token/key"
        );
        assert!(!wire.to_string().contains("Private"));
        assert!(!wire.to_string().contains("memberSecretRefs"));
    }
    #[test]
    fn concurrent_edits_and_edit_delete_conflict_but_other_servers_merge() {
        let initial = http("a");
        let mut mine = initial.clone();
        mine["name"] = json!("Mine");
        let mut theirs = initial.clone();
        theirs["name"] = json!("Theirs");
        let pending = BTreeMap::from([(
            "a".into(),
            Mutation {
                before: Some(initial.clone()),
                after: Some(mine.clone()),
                ..Default::default()
            },
        )]);
        let (merged, c) = merge(&config(vec![theirs.clone(), http("b")]), &pending).unwrap();
        assert_eq!(c["a"], theirs);
        assert_eq!(merged["servers"].as_array().unwrap().len(), 2);
        assert!(merge(&config(vec![http("b")]), &pending)
            .unwrap()
            .1
            .contains_key("a"));
        let (merged, c) = merge(&config(vec![initial, http("b")]), &pending).unwrap();
        assert!(c.is_empty());
        assert_eq!(merged["servers"][0]["name"], "Mine");
        assert_eq!(merged["futureMetadata"]["keep"], true);
        assert!(merge(&config(vec![mine]), &pending).unwrap().1.is_empty());
    }
    #[test]
    fn keep_local_and_portable_values_never_export_secrets_or_approvals() {
        let _data = crate::registry::DataDirTestEnv::new(
            "keep_local_and_portable_values_never_export_secrets_or_approvals",
        );
        let mut a = machine();
        let before = a.clone();
        let mut s = local(command("local"));
        s.unknown_fields.insert("syncLocalOnly".into(), json!(true));
        a.servers.push(s);
        record(&before, &mut a).unwrap();
        assert!(state(&a).unwrap().pending.is_empty());
        let s = local(
            json!({"id":"portable","name":"portable","transport":"stdio","command":"fixture","args":[],"env":[{"key":"REGION","secret":false,"portable":true,"value":"west"},{"key":"LOCAL","secret":false,"value":"local-only"},{"key":"TOKEN","secret":true,"portable":true,"value":"synthetic-secret"}],"launch":{"inputs":[{"key":"project","label":"Project","secret":false,"portable":true,"value":"shared"},{"key":"private","label":"Private","secret":true,"portable":true,"value":"synthetic-secret"}],"bindings":[]},"memberSecretRefs":{"env:LOCAL":"private-reference"},"syncCommandConsent":"local-approval"}),
        );
        let out = export(&s);
        assert_eq!(out["env"][0]["value"], "west");
        assert!(out["env"][1].get("value").is_none());
        assert!(out["env"][2].get("value").is_none());
        assert_eq!(out["launch"]["inputs"][0]["value"], "shared");
        assert!(!out.to_string().contains("synthetic-secret"));
        assert!(!out.to_string().contains("approval"));
        assert!(!out.to_string().contains("private-reference"));
    }
    #[test]
    fn keep_local_does_not_receive_or_duplicate_its_cloud_definition() {
        let _data = crate::registry::DataDirTestEnv::new("solo-local-receive");
        let mut a = machine();
        let mut local = local(http("docs"));
        local.enabled = true;
        local.source = Some("team:solo".into());
        local
            .unknown_fields
            .insert("syncLocalOnly".into(), json!(true));
        local.url = Some("https://local.example/mcp".into());
        a.servers.push(local);
        apply(&mut a, &config(vec![http("docs")]), 1).unwrap();
        assert_eq!(a.servers.len(), 1);
        assert_eq!(
            a.servers[0].url.as_deref(),
            Some("https://local.example/mcp")
        );
        assert!(a.servers[0].enabled);
        apply(&mut a, &config(vec![]), 2).unwrap();
        assert_eq!(a.servers.len(), 1);
        assert!(state(&a).unwrap().pending.is_empty());
    }
    #[test]
    fn portable_values_apply_while_unmarked_values_remain_local() {
        let _data = crate::registry::DataDirTestEnv::new("solo-values");
        let mut b = machine();
        let mut value = http("values");
        value["env"] = json!([{"key":"REGION","secret":false,"portable":true,"value":"west"},{"key":"LOCAL","secret":false}]);
        apply(&mut b, &config(vec![value.clone()]), 1).unwrap();
        b.servers[0].env[1].value = Some("only-here".into());
        value["env"][0]["value"] = json!("east");
        apply(&mut b, &config(vec![value]), 2).unwrap();
        assert_eq!(b.servers[0].env[0].value.as_deref(), Some("east"));
        assert_eq!(b.servers[0].env[1].value.as_deref(), Some("only-here"));
        assert!(!b.servers[0].inherit_env);
    }
    #[test]
    fn key_references_are_held_and_env_references_refused() {
        let _data = crate::registry::DataDirTestEnv::new("solo-refs");
        let mut b = machine();
        let mut row = http("ref");
        row["env"] = json!([{"key":"TOKEN","source":{"ref":"op://Private/service/key"}}]);
        assert_eq!(
            apply(&mut b, &config(vec![row.clone()]), 1).unwrap().review,
            1
        );
        assert!(!b.servers[0].enabled);
        assert!(b.servers[0].needs_team_enable_review());
        row["env"][0]["source"]["ref"] = json!("env:OP_SERVICE_ACCOUNT_TOKEN");
        assert_eq!(apply(&mut b, &config(vec![row]), 2).unwrap().blocked, 1);
    }
    #[test]
    fn personal_references_use_upstream_destination_approval_and_exact_review() {
        let _data = crate::registry::DataDirTestEnv::new("solo-reference-approval");
        let mut r = machine();
        let mut row = http("reference");
        row["headerKeys"] = json!([{"key":"X-Key","source":{"ref":"op://Private/service/key"}}]);
        let first = config(vec![row.clone()]);
        assert_eq!(apply(&mut r, &first, 1).unwrap().review, 1);
        let reviewed = r.servers[0].clone();
        assert!(crate::secret_refs::check_approval(&reviewed).is_err());
        crate::registry::save(&r).unwrap();
        r = enable_reviewed(&r.active_profile_id(), &reviewed).unwrap();
        crate::secret_refs::check_approval(&r.servers[0]).unwrap();
        assert_eq!(apply(&mut r, &first, 1).unwrap().review, 0);
        assert!(r.servers[0].enabled);
        row["name"] = json!("Metadata only");
        assert_eq!(
            apply(&mut r, &config(vec![row.clone()]), 2).unwrap().review,
            0
        );
        row["headerKeys"][0]["key"] = json!("Authorization");
        assert_eq!(
            apply(&mut r, &config(vec![row.clone()]), 3).unwrap().review,
            1
        );
        assert!(check_review(&r, &r.servers[0], Some(&reviewed)).is_err());
        crate::registry::save(&r).unwrap();
        assert!(enable_reviewed(&r.active_profile_id(), &reviewed).is_err());
        let reviewed = r.servers[0].clone();
        r = enable_reviewed(&r.active_profile_id(), &reviewed).unwrap();
        row["url"] = json!("https://other.example/mcp");
        assert_eq!(
            apply(&mut r, &config(vec![row.clone()]), 4).unwrap().review,
            1
        );
        assert!(crate::secret_refs::check_approval(&r.servers[0]).is_err());
        assert!(check_review(&r, &r.servers[0], Some(&reviewed)).is_err());
        row["headerKeys"][0]["source"]["ref"] = json!("op://Private/changed/key");
        assert_eq!(apply(&mut r, &config(vec![row]), 5).unwrap().review, 1);
        assert!(!r.servers[0].enabled);
    }
    #[test]
    fn pro_sync_refuses_environment_references_and_local_override_injection() {
        let _data = crate::registry::DataDirTestEnv::new("solo-pro-reference-policy");
        let mut r = machine();
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["plan"] = json!("pro");
        let mut row = http("reference");
        row["headerKeys"] = json!([{"key":"X-Key","source":{"ref":"env:ACCESS_TOKEN"}}]);
        assert_eq!(
            apply(&mut r, &config(vec![row.clone()]), 1)
                .unwrap()
                .blocked,
            1
        );
        assert!(r.servers.is_empty());
        row["headerKeys"][0]["source"]["ref"] = json!("op://Private/service/key");
        assert_eq!(
            apply(&mut r, &config(vec![row.clone()]), 2).unwrap().review,
            1
        );
        r.servers[0].unknown_fields.insert(
            "memberSecretRefs".into(),
            json!({"header:X-Key":"env:ACCESS_TOKEN"}),
        );
        assert_eq!(apply(&mut r, &config(vec![row]), 3).unwrap().blocked, 1);
        assert!(!r.servers[0].enabled);
    }
    #[test]
    fn solo_requires_authenticated_owner_signal_and_teams_keep_review() {
        let _data = crate::registry::DataDirTestEnv::new("solo-governance");
        let mut r = machine();
        assert!(is_personal(&r));
        r.team.as_mut().unwrap().unknown_fields["accountStatus"]["personalSync"] = json!(false);
        assert!(!is_personal(&r));
        assert_eq!(
            crate::teams::stage_team_config(&mut r, "solo", &config(vec![http("review")]), 1, &[])
                .unwrap()
                .review,
            1
        );
        assert!(crate::teams::server_change_held(&r, &r.servers[0].id));
    }
    #[test]
    fn received_disable_preserves_visible_server_and_stops_it() {
        let _data = crate::registry::DataDirTestEnv::new("solo-disable");
        let mut b = machine();
        let mut v = http("off");
        apply(&mut b, &config(vec![v.clone()]), 1).unwrap();
        v["disabled"] = json!(true);
        apply(&mut b, &config(vec![v]), 2).unwrap();
        assert_eq!(b.servers.len(), 1);
        assert!(!b.servers[0].enabled);
    }
    #[test]
    fn remote_delete_or_disable_stops_a_route_even_with_a_pending_local_edit() {
        let _data = crate::registry::DataDirTestEnv::new("solo-conflict-revocation");
        for delete in [false, true] {
            let mut b = machine();
            let initial = config(vec![http("docs")]);
            apply(&mut b, &initial, 1).unwrap();
            let before = b.clone();
            b.servers[0].name = "Local edit".into();
            record(&before, &mut b).unwrap();
            let mut remote = if delete {
                config(vec![])
            } else {
                initial.clone()
            };
            if !delete {
                remote["servers"][0]["disabled"] = json!(true);
            }
            apply(&mut b, &remote, 2).unwrap();
            assert_eq!(b.servers[0].name, "Local edit");
            assert!(!b.servers[0].enabled);
            assert!(state(&b).unwrap().pending.contains_key("docs"));
            assert!(merge(&remote, &state(&b).unwrap().pending)
                .unwrap()
                .1
                .contains_key("docs"));
            assert!(!b
                .profiles
                .iter()
                .any(|p| p.enabled_server_ids.contains(&b.servers[0].id)));
        }
    }
    #[test]
    fn conflict_resolution_checks_the_exact_remote_generation() {
        let _data = crate::registry::DataDirTestEnv::new("solo-conflict");
        let mut r = machine();
        let remote = http("a");
        let mut st = SyncState::default();
        st.pending.insert(
            "a".into(),
            Mutation {
                initial_conflict: true,
                after: Some(http("mine")),
                ..Mutation::default()
            },
        );
        st.conflicts.insert("a".into(), remote.clone());
        save(&mut r, &st).unwrap();
        crate::registry::save(&r).unwrap();
        assert!(resolve_conflict("a", &conflict_version(&http("b")), true).is_err());
        let r = resolve_conflict("a", &conflict_version(&remote), true).unwrap();
        let pending = state(&r).unwrap().pending;
        assert_eq!(pending["a"].before, Some(remote.clone()));
        assert!(!pending["a"].initial_conflict);
        let (merged, conflicts) = merge(&config(vec![remote]), &pending).unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(merged["servers"][0], http("mine"));
    }
    #[test]
    fn local_journal_and_ack_do_not_drop_newer_edits() {
        let _data =
            crate::registry::DataDirTestEnv::new("local_journal_and_ack_do_not_drop_newer_edits");
        let mut r = machine();
        let mut st = SyncState::default();
        st.initialized = true;
        st.baseline.insert("a".into(), http("a"));
        save(&mut r, &st).unwrap();
        let mut s = local(http("a"));
        s.enabled = true;
        r.servers.push(s);
        let before = r.clone();
        r.servers[0].name = "First".into();
        record(&before, &mut r).unwrap();
        let original = state(&r).unwrap().pending["a"].clone();
        let before = r.clone();
        r.servers[0].name = "Second".into();
        record(&before, &mut r).unwrap();
        let current = state(&r).unwrap().pending["a"].clone();
        assert_ne!(original, current);
        assert_eq!(current.before, Some(http("a")));
    }
    #[test]
    fn server_metadata_survives_owner_edits() {
        let mut remote = http("a");
        remote["dashboardMetadata"] = json!({"keep":true});
        let mut mine = http("a");
        mine["name"] = json!("Renamed");
        let pending = BTreeMap::from([(
            "a".into(),
            Mutation {
                before: Some(remote.clone()),
                after: Some(mine),
                ..Default::default()
            },
        )]);
        let (merged, c) = merge(&config(vec![remote]), &pending).unwrap();
        assert!(c.is_empty());
        assert_eq!(merged["servers"][0]["dashboardMetadata"]["keep"], true);
    }
    #[test]
    fn two_machine_http_fixture_covers_delivery_retry_conflict_and_secret_boundary() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        let _data = crate::registry::DataDirTestEnv::new("solo-http-fixture");
        let fixture = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", fixture.server_addr());
        let cloud = Arc::new(Mutex::new((0i64, config(vec![]))));
        let shared = cloud.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let stale = Arc::new(AtomicBool::new(false));
        let retry = stale.clone();
        let handle = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let Some(mut req) = fixture
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .unwrap()
                else {
                    continue;
                };
                assert_eq!(
                    req.headers()
                        .iter()
                        .find(|h| h.field.equiv("authorization"))
                        .unwrap()
                        .value
                        .as_str(),
                    "Bearer fixture"
                );
                let mut state = shared.lock().unwrap();
                let (code, body) = if req.method() == &tiny_http::Method::Get {
                    assert!(req.url().contains("manage=1"));
                    (200, json!({"version":state.0,"config":state.1}))
                } else {
                    let body: Value = serde_json::from_reader(req.as_reader()).unwrap();
                    if retry.swap(false, Ordering::AcqRel) {
                        state.0 += 1;
                        state.1["servers"]
                            .as_array_mut()
                            .unwrap()
                            .push(http("unrelated"));
                    }
                    if body["base_version"] != state.0 {
                        (409, json!({"error":"stale"}))
                    } else {
                        state.0 += 1;
                        state.1 = body["config"].clone();
                        (200, json!({"version":state.0}))
                    }
                };
                req.respond(
                    tiny_http::Response::from_string(body.to_string())
                        .with_status_code(code)
                        .with_header(
                            tiny_http::Header::from_bytes("content-type", "application/json")
                                .unwrap(),
                        ),
                )
                .unwrap();
            }
        });
        fn flush(r: &mut Registry, url: &str) {
            r.team.as_mut().unwrap().server_url = url.into();
            let mut st = state(r).unwrap();
            for m in st.pending.values_mut() {
                m.at = 0;
            }
            save(r, &st).unwrap();
            crate::registry::save(r).unwrap();
            let conn = r.team.clone().unwrap();
            sync(&conn, "fixture").unwrap();
            *r = crate::registry::load().unwrap();
        }
        let mut a = machine();
        let mut b = machine();
        flush(&mut a, &url);
        flush(&mut b, &url);
        let before = a.clone();
        let mut server = local(http("docs"));
        server.enabled = true;
        server.env=serde_json::from_value(json!([{"key":"REGION","secret":false,"portable":true,"value":"west"},{"key":"LOCAL","secret":false,"value":"only-A"},{"key":"TOKEN","secret":true,"portable":true,"value":"synthetic-secret"}])).unwrap();
        a.servers.push(server);
        record(&before, &mut a).unwrap();
        stale.store(true, Ordering::Release);
        flush(&mut a, &url);
        assert!(state(&a).unwrap().pending.is_empty());
        flush(&mut b, &url);
        assert!(b.servers.iter().find(|s| s.id == "docs").unwrap().enabled);
        assert_eq!(
            b.servers.iter().find(|s| s.id == "docs").unwrap().env[0]
                .value
                .as_deref(),
            Some("west")
        );
        let payload = cloud.lock().unwrap().1.to_string();
        assert!(!payload.contains("synthetic-secret"));
        assert!(!payload.contains("only-A"));
        assert!(payload.contains("unrelated"));
        let before = a.clone();
        let mut refused = local(http("refused"));
        refused.env =
            serde_json::from_value(json!([{"key":"TOKEN","source":{"ref":"env:LOCAL_TOKEN"}}]))
                .unwrap();
        a.servers.push(refused);
        let mut safe = local(http("safe"));
        safe.enabled = true;
        a.servers.push(safe);
        record(&before, &mut a).unwrap();
        // A bad queued server cannot block a safe publish or the final apply.
        cloud.lock().unwrap().1["servers"]
            .as_array_mut()
            .unwrap()
            .push(http("received-during-publish"));
        flush(&mut a, &url);
        assert!(cloud.lock().unwrap().1["servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "safe"));
        assert!(a
            .servers
            .iter()
            .any(|s| original(s) == "received-during-publish"));
        assert!(state(&a).unwrap().pending.contains_key("refused"));
        assert!(state(&a).unwrap().publish_errors.contains_key("refused"));
        let before = a.clone();
        a.servers.retain(|s| s.id != "refused");
        record(&before, &mut a).unwrap();

        let before = a.clone();
        let mut command_entry = local(command("command"));
        command_entry.enabled = true;
        a.servers.push(command_entry);
        record(&before, &mut a).unwrap();
        flush(&mut a, &url);
        assert!(
            a.servers
                .iter()
                .find(|s| s.id == "command")
                .unwrap()
                .enabled
        );
        flush(&mut b, &url);
        let reviewed = b
            .servers
            .iter()
            .find(|s| s.id == "command")
            .unwrap()
            .clone();
        assert!(!reviewed.enabled);
        crate::registry::save(&b).unwrap();
        b = enable_reviewed(&b.active_profile_id(), &reviewed).unwrap();
        flush(&mut b, &url);
        assert!(
            b.servers
                .iter()
                .find(|s| s.id == "command")
                .unwrap()
                .enabled
        );
        let before = a.clone();
        a.servers.iter_mut().find(|s| s.id == "docs").unwrap().name = "Edited on A".into();
        record(&before, &mut a).unwrap();
        flush(&mut a, &url);
        flush(&mut b, &url);
        assert_eq!(
            b.servers.iter().find(|s| s.id == "docs").unwrap().name,
            "Edited on A"
        );
        let before = a.clone();
        a.servers.iter_mut().find(|s| s.id == "docs").unwrap().name = "A conflict".into();
        record(&before, &mut a).unwrap();
        let before = b.clone();
        b.servers.iter_mut().find(|s| s.id == "docs").unwrap().name = "B conflict".into();
        record(&before, &mut b).unwrap();
        flush(&mut b, &url);
        flush(&mut a, &url);
        assert!(state(&a).unwrap().conflicts.contains_key("docs"));
        let remote = state(&a).unwrap().conflicts["docs"].clone();
        crate::registry::save(&a).unwrap();
        a = resolve_conflict("docs", &conflict_version(&remote), false).unwrap();
        flush(&mut a, &url);
        assert_eq!(
            a.servers.iter().find(|s| s.id == "docs").unwrap().name,
            "B conflict"
        );
        let before = a.clone();
        a.servers.retain(|s| s.id != "docs");
        record(&before, &mut a).unwrap();
        flush(&mut a, &url);
        flush(&mut b, &url);
        assert!(!b.servers.iter().any(|s| s.id == "docs"));
        stop.store(true, Ordering::Release);
        handle.join().unwrap();
    }
}
