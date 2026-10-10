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
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncState {
    pub baseline: BTreeMap<String, Value>,
    pub pending: BTreeMap<String, Mutation>,
    pub conflicts: BTreeMap<String, Value>,
    pub last_synced_at: Option<i64>,
    pub error: Option<String>,
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
    reg.team
        .as_ref()
        .and_then(|t| t.unknown_fields.get(STATE))
        .map(|v| {
            serde_json::from_value(v.clone()).map_err(|e| format!("Could not read sync state: {e}"))
        })
        .unwrap_or_else(|| Ok(SyncState::default()))
}
fn save(reg: &mut Registry, state: &SyncState) -> Result<(), String> {
    if let Some(team) = &mut reg.team {
        team.unknown_fields.insert(
            STATE.into(),
            serde_json::to_value(state).map_err(|e| e.to_string())?,
        );
    }
    Ok(())
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
fn has_references(value: &Value) -> bool {
    match value {
        Value::Object(m) => reference(value).is_some() || m.values().any(has_references),
        Value::Array(a) => a.iter().any(has_references),
        _ => false,
    }
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
        if map.get("secret") == Some(&json!(true)) {
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
        .zip(crate::registry::secret_arg_mask(&s.args))
        .map(|(arg, secret)| {
            if secret && arg != "<launch-input>" {
                "<redacted>".to_string()
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
    v["disabled"] = json!(!s.enabled);
    v["args"] = json!(args);
    if let Some(url) = &s.url {
        v["url"] = json!(crate::redact_url_userinfo(url));
    }
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
    if let Some(headers) = v.get_mut("headerKeys").and_then(Value::as_array_mut) {
        for h in headers {
            if let Some(m) = h.as_object_mut() {
                m.retain(|k, _| ["key", "env", "source"].contains(&k.as_str()));
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
        if matches.len() == 1 {
            server
                .unknown_fields
                .insert("teamOriginalId".into(), json!(matches[0]));
        }
    }
    let a = definitions(before);
    let b = definitions(reg);
    for id in a.keys().chain(b.keys()).collect::<HashSet<_>>() {
        if a.get(id).map(|(_, v)| v) == b.get(id).map(|(_, v)| v) {
            continue;
        }
        let after = b.get(id).map(|(_, v)| v.clone());
        let base = st
            .pending
            .get(id)
            .map(|m| m.before.clone())
            .unwrap_or_else(|| st.baseline.get(id).cloned());
        let local_id = b.get(id).or_else(|| a.get(id)).unwrap().0.clone();
        if same(base.as_ref(), after.as_ref()) {
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
        if !same(current.as_ref(), m.before.as_ref()) && !same(current.as_ref(), m.after.as_ref()) {
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
    // Portable launch inputs can alter arguments; their exact values belong in
    // executable consent as well as command, args, cwd and launch bindings.
    json!({"command":v["command"], "args":v["args"], "cwd":v["cwd"], "transport":v["transport"], "launch":v["launch"], "env":v["env"]})
}
fn reference_identity(value: &Value) -> Value {
    json!({"url":value["url"],"command":command_identity(value),"headers":value["headerKeys"]})
}
fn execution_changed(before: Option<&Value>, after: &Value) -> bool {
    let command = after["transport"] == "stdio" || after["command"].is_string();
    command && before.is_none_or(|b| command_identity(b) != command_identity(after))
}
fn restore_local(entry: &mut ServerEntry, old: &ServerEntry) {
    entry.inherit_env = old.inherit_env; // Never import ambient-env consent.
    for input in &mut entry.env {
        if !portable(&json!(input)) && !input.secret {
            input.value = old
                .env
                .iter()
                .find(|i| i.key == input.key && !i.secret)
                .and_then(|i| i.value.clone());
        }
    }
    for input in entry.launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if !portable(&json!(input)) && !input.secret {
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
    if !st.initialized {
        // Bind by explicit identity first, then by a unique display name. Recreating
        // the same named server does not produce a second cloud definition.
        for local in reg.servers.iter_mut().filter(|s| eligible(s)) {
            let matches: Vec<_> = remote
                .iter()
                .filter(|(id, v)| {
                    id.as_str() == original(local)
                        || v["name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&local.name))
                })
                .collect();
            if matches.len() == 1 {
                if matches[0].1["disabled"] == true {
                    local.enabled = false;
                }
                local
                    .unknown_fields
                    .insert("teamOriginalId".into(), json!(matches[0].0));
            }
            let id = original(local).to_string();
            let after = export(local);
            if !same(remote.get(&id), Some(&after))
                && !local.source.as_deref().unwrap_or("").starts_with("team:")
            {
                st.pending.insert(
                    id.clone(),
                    Mutation {
                        local_id: local.id.clone(),
                        before: remote.get(&id).cloned(),
                        after: Some(after),
                        at: now(),
                    },
                );
            }
        }
        st.initialized = true;
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
            .filter(|s| s.source.as_deref() == Some(&tag) && original(s) == id)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            crate::local_auth::revoke_personal_route(reg, &team_id, &id);
            reg.servers.retain(|s| s.id != id);
            for p in &mut reg.profiles {
                p.enabled_server_ids.retain(|s| s != &id);
            }
        }
    }
    for (id, value) in &remote {
        if st.pending.contains_key(id) {
            continue;
        }
        if env_references(value) {
            stop_blocked(reg, &tag, id);
            outcome.blocked += 1;
            continue;
        }
        let mut runtime = value.clone();
        runtime.as_object_mut().unwrap().remove("disabled");
        let mut entry = match crate::teams::classify_team_server(&runtime, &tag) {
            crate::teams::TeamClass::Ready(e) | crate::teams::TeamClass::Review(e) => e,
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
            restore_local(&mut entry, old);
        } else {
            entry.id = crate::registry::unique_id(
                id,
                &reg.servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            );
        }
        if value["disabled"] == true {
            if let Some(old) = &old {
                crate::local_auth::revoke_personal_route(reg, &team_id, &old.id);
            }
        }
        let changed = execution_changed(st.baseline.get(id), value);
        let refs_changed = has_references(value)
            && old
                .as_ref()
                .is_none_or(|o| reference_identity(&export(o)) != reference_identity(value));
        let command_approved = old
            .as_ref()
            .and_then(|s| s.unknown_fields.get("syncCommandConsent"))
            == Some(&command_identity(value));
        let review = (changed && !command_approved)
            || refs_changed
            || old
                .as_ref()
                .is_some_and(|s| s.unknown_fields.get("teamEnableReview") == Some(&json!(true)));
        entry
            .unknown_fields
            .insert("personalSyncEntry".into(), json!(true));
        if let Some(consent) = old
            .as_ref()
            .and_then(|s| s.unknown_fields.get("syncCommandConsent"))
        {
            entry
                .unknown_fields
                .insert("syncCommandConsent".into(), consent.clone());
        }
        entry.enabled = value["disabled"] != true && !review;
        if review {
            entry.require_team_enable_review();
            outcome.review += 1;
        } else {
            entry.unknown_fields.remove("teamEnableReview");
            outcome.applied += 1;
        }
        // Preserve local approval/override markers. They are never exported.
        if let Some(old) = &old {
            for key in ["memberSecretRefs"] {
                if let Some(v) = old.unknown_fields.get(key) {
                    entry.unknown_fields.insert(key.into(), v.clone());
                }
            }
        }
        reg.servers.retain(|s| s.id != entry.id);
        for p in &mut reg.profiles {
            if review || !entry.enabled {
                p.enabled_server_ids.retain(|s| s != &entry.id);
            }
        }
        if entry.enabled {
            let active = reg.active_profile_id();
            if let Some(p) = reg.profiles.iter_mut().find(|p| p.id == active) {
                if !p.enabled_server_ids.contains(&entry.id) {
                    p.enabled_server_ids.push(entry.id.clone());
                }
            }
        }
        if let Some(t) = &mut reg.team {
            t.managed_server_ids.insert(entry.id.clone(), id.clone());
        }
        reg.servers.push(entry);
    }
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
    if is_personal(reg) && current.needs_team_enable_review() {
        if reviewed.is_none_or(|s| {
            s.id != current.id
                || reference_identity(&export(s)) != reference_identity(&export(current))
        }) {
            return Err(
                "The command, reference or destination changed. Review this server again.".into(),
            );
        }
    }
    Ok(())
}
pub fn enable_reviewed(profile: &str, reviewed: &ServerEntry) -> Result<Registry, String> {
    crate::registry::update(|reg| {
        let current = reg
            .servers
            .iter()
            .find(|s| s.id == reviewed.id)
            .ok_or("Server no longer exists")?;
        check_review(reg, current, Some(reviewed))?;
        crate::registry_controller::apply_server_enabled(reg, profile, &reviewed.id, true, true)
    })
    .map(|(r, ())| r)
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
        if local_only {
            server.source = Some("manual".into());
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
pub fn resolve_conflict(id: &str, expected: &Value, keep_mine: bool) -> Result<Registry, String> {
    remote_update(|| {
        crate::registry::update(|r| {
            let mut st = state(r)?;
            if st.conflicts.get(id) != Some(expected) {
                return Err("The conflict changed. Refresh Sync and review it again.".into());
            }
            let m = st
                .pending
                .get_mut(id)
                .ok_or("The conflict is no longer pending")?;
            if keep_mine {
                m.before = (!expected.is_null()).then(|| expected.clone());
                m.at = 0;
            } else {
                st.pending.remove(id);
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
    let mut latest = crate::teams::fetch_config_for_update(&conn.server_url, &conn.team_id, token)?;
    let (reg, _) = remote_update(|| {
        crate::registry::update(|r| {
            if !current(r, conn) {
                return Ok(());
            }
            apply(r, &latest.1, latest.0)?;
            Ok(())
        })
    })?;
    let queued = state(&reg)?.pending;
    let ready: BTreeMap<_, _> = queued
        .into_iter()
        .filter(|(_, m)| now() - m.at >= 750)
        .collect();
    for attempt in 0..2 {
        let (merged, conflicts) = merge(&latest.1, &ready)?;
        remote_update(|| {
            crate::registry::update(|r| {
                if current(r, conn) {
                    let mut st = state(r)?;
                    st.conflicts = conflicts.clone();
                    save(r, &st)?;
                }
                Ok(())
            })
        })?;
        if merged == latest.1 {
            break;
        }
        match crate::teams::push_config(&conn.server_url, &conn.team_id, token, &merged, latest.0) {
            Ok(crate::teams::PushOutcome::Published(_)) => {
                latest =
                    crate::teams::fetch_config_for_update(&conn.server_url, &conn.team_id, token)?;
                break;
            }
            Ok(_) => return Err("Finish the sync approval in Your account.".into()),
            Err(e) if e == crate::teams::STALE_PUSH_MESSAGE && attempt == 0 => {
                latest =
                    crate::teams::fetch_config_for_update(&conn.server_url, &conn.team_id, token)?;
            }
            Err(e) => return Err(e),
        }
    }
    let remote = index(&latest.1)?;
    let (_, outcome) = remote_update(|| {
        crate::registry::update(|r| {
            if !current(r, conn) {
                return Ok(None);
            }
            let mut st = state(r)?;
            for (id, m) in &ready {
                if st.pending.get(id) == Some(m) && same(remote.get(id), m.after.as_ref()) {
                    st.pending.remove(id);
                    st.conflicts.remove(id);
                    if let Some(after) = &m.after {
                        st.baseline.insert(id.clone(), after.clone());
                        if let Some(s) = r.servers.iter_mut().find(|s| s.id == m.local_id) {
                            s.unknown_fields
                                .insert("syncCommandConsent".into(), command_identity(after));
                        }
                    }
                }
            }
            save(r, &st)?;
            let out = apply(r, &latest.1, latest.0)?;
            let mut st = state(r)?;
            st.last_synced_at = Some(now());
            st.error = None;
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
fn current(r: &Registry, c: &crate::registry::TeamConnection) -> bool {
    is_personal(r)
        && r.team.as_ref().is_some_and(|t| {
            t.team_id == c.team_id && t.reporting_device_id == c.reporting_device_id
        })
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
            "Sync grace period ended. Choose your active Free device in Your account.".into()
        });
    }
    if status["canReceiveConfig"] == false {
        lines.push(status["reason"].as_str().unwrap_or("This device cannot receive your setup. Choose your active device in Your account.").into());
    }
    lines.push(match last_synced {
        Some(t) if now - t < 60_000 => "Last synced just now".into(),
        Some(t) => format!("Last synced {} minutes ago", (now - t).max(0) / 60_000),
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
                save(r, &st)?;
            }
            Ok(())
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn conflict_resolution_checks_the_exact_remote_generation() {
        let _data = crate::registry::DataDirTestEnv::new("solo-conflict");
        let mut r = machine();
        let remote = http("a");
        let mut st = SyncState::default();
        st.pending.insert("a".into(), Mutation::default());
        st.conflicts.insert("a".into(), remote.clone());
        save(&mut r, &st).unwrap();
        crate::registry::save(&r).unwrap();
        assert!(resolve_conflict("a", &http("b"), true).is_err());
        let r = resolve_conflict("a", &remote, true).unwrap();
        assert_eq!(state(&r).unwrap().pending["a"].before, Some(remote));
    }
    #[test]
    fn local_journal_and_ack_do_not_drop_newer_edits() {
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
        a = resolve_conflict("docs", &remote, false).unwrap();
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
