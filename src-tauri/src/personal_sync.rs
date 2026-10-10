//! Owner-only configuration sync. Values and approvals remain machine-local unless
//! a nonsecret value is explicitly marked portable. Network work never holds the
//! registry lock; acknowledgements compare the exact queued mutation again.
use std::collections::{BTreeMap, HashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::registry::{Registry, ServerEntry};

const STATE: &str = "personalSyncState";
thread_local! { static APPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) fn remote_update<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore { fn drop(&mut self) { APPLYING.with(|v| v.set(self.0)); } }
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
    reg.team.as_ref().is_some_and(|t| t.role == "admin" && t.unknown_fields.get("accountStatus").is_some_and(|s| s["personalSync"] == true))
}
pub fn state(reg: &Registry) -> Result<SyncState, String> {
    reg.team.as_ref().and_then(|t| t.unknown_fields.get(STATE)).map(|v| serde_json::from_value(v.clone()).map_err(|e| format!("Could not read sync state: {e}"))).unwrap_or_else(|| Ok(SyncState::default()))
}
fn save(reg: &mut Registry, state: &SyncState) -> Result<(), String> {
    if let Some(team) = &mut reg.team { team.unknown_fields.insert(STATE.into(), serde_json::to_value(state).map_err(|e| e.to_string())?); }
    Ok(())
}
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64 }
pub fn keep_local(s: &ServerEntry) -> bool { s.unknown_fields.get("syncLocalOnly") == Some(&json!(true)) }
fn original(s: &ServerEntry) -> &str { s.unknown_fields.get("teamOriginalId").and_then(Value::as_str).unwrap_or(&s.id) }
fn eligible(s: &ServerEntry) -> bool { !keep_local(s) && !crate::clients::is_gateway_server(s) }
fn reference(value: &Value) -> Option<&str> { value.get("source")?.get("ref")?.as_str() }
fn has_references(value: &Value) -> bool {
    match value { Value::Object(m) => reference(value).is_some() || m.values().any(has_references), Value::Array(a) => a.iter().any(has_references), _ => false }
}
fn env_references(value: &Value) -> bool {
    match value { Value::Object(m) => reference(value).is_some_and(|r| r.starts_with("env:")) || m.values().any(env_references), Value::Array(a) => a.iter().any(env_references), _ => false }
}
fn portable(value: &Value) -> bool { value["secret"] == false && value["portable"] == true && reference(value).is_none() }
fn input_export(mut input: Value) -> Value {
    let value = if portable(&input) { input.get("value").cloned() } else { None };
    if let Some(map) = input.as_object_mut() {
        // Approval and local override metadata can never leave this machine.
        map.retain(|key, _| ["key", "label", "required", "secret", "portable", "source"].contains(&key.as_str()));
        if let Some(r) = map.get("source").and_then(|s| s["ref"].as_str()).map(str::to_string) {
            map.insert("source".into(), json!({"ref": r}));
        } else { map.remove("source"); }
        if map.get("secret") == Some(&json!(true)) { map.remove("secret"); }
        if let Some(value) = value { map.insert("value".into(), value); }
    }
    input
}
pub fn export(s: &ServerEntry) -> Value {
    let mut v = json!(s);
    let args: Vec<_> = s.args.iter().zip(crate::registry::secret_arg_mask(&s.args)).map(|(arg, secret)| if secret && arg != "<launch-input>" { "<redacted>".to_string() } else { arg.clone() }).collect();
    let map = v.as_object_mut().unwrap();
    map.retain(|k, _| ["id", "name", "transport", "command", "args", "launch", "env", "url", "cwd", "disabledTools", "clientCredentials", "requestTimeoutMs", "initializeTimeoutMs", "headerKeys"].contains(&k.as_str()));
    v["id"] = json!(original(s));
    v["disabled"] = json!(!s.enabled);
    v["args"] = json!(args);
    if let Some(url) = &s.url { v["url"] = json!(crate::redact_url_userinfo(url)); }
    v["env"] = json!(s.env.iter().map(|e| input_export(json!(e))).collect::<Vec<_>>());
    if let Some(c) = v.get_mut("clientCredentials") { if let Ok(mut cc) = serde_json::from_value::<crate::registry::ClientCredentials>(c.clone()) { cc.strip_secret_fields(); *c = json!(cc); } }
    if let Some(inputs) = v.pointer_mut("/launch/inputs").and_then(Value::as_array_mut) { for input in inputs { *input = input_export(input.clone()); } }
    if let Some(headers) = v.get_mut("headerKeys").and_then(Value::as_array_mut) { for h in headers { if let Some(m) = h.as_object_mut() { m.retain(|k, _| ["key", "env", "source"].contains(&k.as_str())); } } }
    v
}
fn definitions(reg: &Registry) -> BTreeMap<String, (String, Value)> {
    reg.servers.iter().filter(|s| eligible(s)).map(|s| (original(s).to_string(), (s.id.clone(), export(s)))).collect()
}
/// Called inside the registry's cross-process mutation lock. This journals both
/// shells, imports and edits without doing network I/O on a UI thread.
pub(crate) fn record(before: &Registry, reg: &mut Registry) -> Result<(), String> {
    if APPLYING.with(|v| v.get()) || !is_personal(before) || !is_personal(reg) { return Ok(()); }
    let mut st = state(reg)?;
    let a = definitions(before); let b = definitions(reg);
    for id in a.keys().chain(b.keys()).collect::<HashSet<_>>() {
        if a.get(id).map(|(_,v)| v) == b.get(id).map(|(_,v)| v) { continue; }
        let after = b.get(id).map(|(_,v)| v.clone());
        let base = st.pending.get(id).map(|m| m.before.clone()).unwrap_or_else(|| st.baseline.get(id).cloned());
        let local_id = b.get(id).or_else(|| a.get(id)).unwrap().0.clone();
        if base == after { st.pending.remove(id); st.conflicts.remove(id); }
        else { st.pending.insert(id.clone(), Mutation { local_id, before: base, after, at: now() }); }
    }
    save(reg, &st)
}
fn index(config: &Value) -> Result<BTreeMap<String, Value>, String> {
    let mut entries = BTreeMap::new();
    for v in config["servers"].as_array().ok_or("Sync response has no server list")? {
        let id = v["id"].as_str().filter(|id| !id.is_empty()).ok_or("Sync response has an unnamed server")?;
        if entries.insert(id.into(), v.clone()).is_some() { return Err("Sync response contains duplicate server identities".into()); }
    }
    Ok(entries)
}
/// Three-way, per-server merge: unrelated remote edits are preserved. Same-server
/// edits are explicit conflicts, including edit/delete. Equality acknowledges a
/// lost successful PUT response without replaying or prompting.
pub fn merge(remote: &Value, pending: &BTreeMap<String, Mutation>) -> Result<(Value, BTreeMap<String, Value>), String> {
    let mut servers = index(remote)?; let mut conflicts = BTreeMap::new();
    for (id, m) in pending {
        let current = servers.get(id).cloned();
        if current != m.before && current != m.after { conflicts.insert(id.clone(), current.unwrap_or(Value::Null)); continue; }
        match &m.after { Some(after) => { servers.insert(id.clone(), after.clone()); }, None => { servers.remove(id); } }
    }
    let mut config = remote.clone(); config["servers"] = json!(servers.into_values().collect::<Vec<_>>());
    Ok((config, conflicts))
}
fn command_identity(v: &Value) -> Value {
    // Portable launch inputs can alter arguments; their exact values belong in
    // executable consent as well as command, args, cwd and launch bindings.
    json!({"command":v["command"], "args":v["args"], "cwd":v["cwd"], "transport":v["transport"], "launch":v["launch"], "env":v["env"]})
}
fn execution_changed(before: Option<&Value>, after: &Value) -> bool {
    let command = after["transport"] == "stdio" || after["command"].is_string();
    command && before.is_none_or(|b| command_identity(b) != command_identity(after))
}
fn restore_local(entry: &mut ServerEntry, old: &ServerEntry) {
    entry.inherit_env = old.inherit_env; // Never import ambient-env consent.
    for input in &mut entry.env {
        if !portable(&json!(input)) && !input.secret {
            input.value = old.env.iter().find(|i| i.key == input.key && !i.secret).and_then(|i| i.value.clone());
        }
    }
    for input in entry.launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if !portable(&json!(input)) && !input.secret { input.value = old.launch.iter().flat_map(|l| &l.inputs).find(|i| i.key == input.key && !i.secret).and_then(|i| i.value.clone()); }
    }
}
/// Merge personal definitions directly while retaining machine-local command and
/// credential consent. No new command or reference is resolved during this step.
pub fn apply(reg: &mut Registry, config: &Value, version: i64) -> Result<crate::teams::MergeOutcome, String> {
    let remote = index(config)?; let mut st = state(reg)?;
    let team_id = reg.team.as_ref().ok_or("Sign in to sync first")?.team_id.clone();
    let tag = format!("team:{team_id}");
    let mut outcome = crate::teams::MergeOutcome::default();
    if !st.initialized {
        // Bind by explicit identity first, then by a unique display name. Recreating
        // the same named server does not produce a second cloud definition.
        for local in reg.servers.iter_mut().filter(|s| eligible(s)) {
            let matches: Vec<_> = remote.iter().filter(|(id,v)| id.as_str() == original(local) || v["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(&local.name))).collect();
            if matches.len() == 1 { local.unknown_fields.insert("teamOriginalId".into(), json!(matches[0].0)); }
            let id = original(local).to_string(); let after = export(local);
            if remote.get(&id) != Some(&after) && !local.source.as_deref().unwrap_or("").starts_with("team:") {
                st.pending.insert(id.clone(), Mutation { local_id:local.id.clone(), before:remote.get(&id).cloned(), after:Some(after), at:now() });
            }
        }
        st.initialized = true;
    }
    let deleted: Vec<_> = st.baseline.keys().filter(|id| !remote.contains_key(*id) && !st.pending.contains_key(*id)).cloned().collect();
    for id in deleted {
        let ids: Vec<_> = reg.servers.iter().filter(|s| s.source.as_deref() == Some(&tag) && original(s) == id).map(|s| s.id.clone()).collect();
        for id in ids { crate::local_auth::revoke_personal_route(reg, &team_id, &id); reg.servers.retain(|s| s.id != id); for p in &mut reg.profiles { p.enabled_server_ids.retain(|s| s != &id); } }
    }
    for (id, value) in &remote {
        if st.pending.contains_key(id) { continue; }
        if env_references(value) { outcome.blocked += 1; continue; }
        let mut runtime = value.clone(); runtime.as_object_mut().unwrap().remove("disabled");
        let mut entry = match crate::teams::classify_team_server(&runtime, &tag) {
            crate::teams::TeamClass::Ready(e) | crate::teams::TeamClass::Review(e) => e,
            _ => { outcome.blocked += 1; continue; }
        };
        // The shared classifier deliberately strips setup values for governed
        // teams. Restore only the explicitly portable personal fields here.
        for (field, path) in [("env", "/env"), ("inputs", "/launch/inputs")] {
            if let Some(inputs) = runtime.pointer(path).and_then(Value::as_array) {
                let imported: Vec<Value> = inputs.iter().map(|i| { let mut i = input_export(i.clone()); if i["secret"] != false { i["secret"] = json!(true); } i }).collect();
                if field == "env" { entry.env = serde_json::from_value(json!(imported)).map_err(|e| e.to_string())?; }
                else if let Some(l) = &mut entry.launch { l.inputs = serde_json::from_value(json!(imported)).map_err(|e| e.to_string())?; }
            }
        }
        let old = reg.servers.iter().find(|s| eligible(s) && original(s) == id).cloned();
        if let Some(old) = &old { entry.id = old.id.clone(); restore_local(&mut entry, old); }
        else { entry.id = crate::registry::unique_id(id, &reg.servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>()); }
        let changed = execution_changed(st.baseline.get(id), value);
        let refs_changed = has_references(value) && old.as_ref().is_none_or(|o| crate::teams::consent_fingerprint(o) != crate::teams::consent_fingerprint(&entry));
        let review = changed || refs_changed || old.as_ref().is_some_and(|s| s.needs_team_enable_review());
        entry.enabled = value["disabled"] != true && !review;
        if review { entry.require_team_enable_review(); outcome.review += 1; } else { entry.unknown_fields.remove("teamEnableReview"); outcome.applied += 1; }
        // Preserve local approval/override markers. They are never exported.
        if let Some(old) = &old { for key in ["localSecretReferences"] { if let Some(v) = old.unknown_fields.get(key) { entry.unknown_fields.insert(key.into(),v.clone()); } } }
        reg.servers.retain(|s| s.id != entry.id);
        for p in &mut reg.profiles { if review || !entry.enabled { p.enabled_server_ids.retain(|s| s != &entry.id); } }
        if entry.enabled { let active = reg.active_profile_id(); if let Some(p) = reg.profiles.iter_mut().find(|p| p.id == active) { if !p.enabled_server_ids.contains(&entry.id) { p.enabled_server_ids.push(entry.id.clone()); } } }
        if let Some(t) = &mut reg.team { t.managed_server_ids.insert(entry.id.clone(), id.clone()); }
        reg.servers.push(entry);
    }
    st.baseline = remote;
    if let Some(t) = &mut reg.team { t.last_version = version; }
    save(reg, &st)?;
    Ok(outcome)
}

pub fn set_local_only(server_id: &str, local_only: bool) -> Result<Registry, String> {
    crate::registry::update(|r| {
        if !is_personal(r) { return Err("This option requires personal sync".into()); }
        let server = r.servers.iter_mut().find(|s| s.id == server_id).ok_or("Server no longer exists")?;
        server.unknown_fields.insert("syncLocalOnly".into(),json!(local_only));
        if local_only { server.source = Some("manual".into()); }
        Ok(())
    }).map(|(r,())|r)
}
pub fn set_portable(server_id: &str, kind: &str, key: &str, enabled: bool) -> Result<Registry, String> {
    crate::registry::update(|r| {
        let s = r.servers.iter_mut().find(|s| s.id == server_id).ok_or("Server no longer exists")?;
        let (secret, fields) = match kind {
            "env" => { let i=s.env.iter_mut().find(|i| i.key == key).ok_or("Variable no longer exists")?; (i.secret, &mut i.unknown_fields) },
            "input" => { let i=s.launch.iter_mut().flat_map(|l| &mut l.inputs).find(|i| i.key == key).ok_or("Input no longer exists")?; (i.secret, &mut i.unknown_fields) },
            _ => return Err("Unknown portable value type".into())
        };
        if secret || fields.contains_key("source") { return Err("Secret values always stay on this machine".into()); }
        fields.insert("portable".into(),json!(enabled)); Ok(())
    }).map(|(r,())|r)
}
pub fn resolve_conflict(id: &str, expected: &Value, keep_mine: bool) -> Result<Registry,String> {
    remote_update(|| crate::registry::update(|r| {
        let mut st=state(r)?;
        if st.conflicts.get(id) != Some(expected) { return Err("The conflict changed. Refresh Sync and review it again.".into()); }
        let m=st.pending.get_mut(id).ok_or("The conflict is no longer pending")?;
        if keep_mine { m.before = (!expected.is_null()).then(||expected.clone()); m.at=0; }
        else { st.pending.remove(id); }
        st.conflicts.remove(id); save(r,&st)
    })).map(|(r,())|r)
}

pub(crate) fn sync(conn: &crate::registry::TeamConnection, token: &str) -> Result<crate::teams::SyncResult,String> {
    let mut latest = crate::teams::fetch_config_for_update(&conn.server_url,&conn.team_id,token)?;
    let (reg, _) = remote_update(|| crate::registry::update(|r| {
        if !current(r,conn) { return Ok(()); }
        apply(r,&latest.1,latest.0)?; Ok(())
    }))?;
    let queued = state(&reg)?.pending;
    let ready: BTreeMap<_,_> = queued.into_iter().filter(|(_,m)| now()-m.at >= 750).collect();
    for attempt in 0..2 {
        let (merged,conflicts) = merge(&latest.1,&ready)?;
        remote_update(|| crate::registry::update(|r| { if current(r,conn) { let mut st=state(r)?; st.conflicts=conflicts.clone(); save(r,&st)?; } Ok(()) }))?;
        if merged == latest.1 { break; }
        match crate::teams::push_config(&conn.server_url,&conn.team_id,token,&merged,latest.0) {
            Ok(crate::teams::PushOutcome::Published(_)) => { latest=crate::teams::fetch_config_for_update(&conn.server_url,&conn.team_id,token)?; break; }
            Ok(_) => return Err("Finish the sync approval in Your account.".into()),
            Err(e) if e == crate::teams::STALE_PUSH_MESSAGE && attempt == 0 => { latest=crate::teams::fetch_config_for_update(&conn.server_url,&conn.team_id,token)?; }
            Err(e) => return Err(e)
        }
    }
    let remote=index(&latest.1)?;
    let (_,outcome)=remote_update(||crate::registry::update(|r| {
        if !current(r,conn) { return Ok(None); }
        let mut st=state(r)?;
        for (id,m) in &ready { if st.pending.get(id) == Some(m) && remote.get(id) == m.after.as_ref() { st.pending.remove(id); st.conflicts.remove(id); if let Some(after) = &m.after { st.baseline.insert(id.clone(),after.clone()); } } }
        save(r,&st)?;
        let out=apply(r,&latest.1,latest.0)?;
        let mut st=state(r)?; st.last_synced_at=Some(now()); st.error=None; save(r,&st)?;
        Ok(Some((latest.0,out)))
    }))?;
    Ok(crate::teams::SyncResult::Ok { role:conn.role.clone(),role_changed:false,applied:outcome })
}
fn current(r:&Registry,c:&crate::registry::TeamConnection)->bool { is_personal(r) && r.team.as_ref().is_some_and(|t|t.team_id==c.team_id && t.reporting_device_id==c.reporting_device_id) }
pub fn record_error(error: Option<&str>) {
    let _ = remote_update(||crate::registry::update(|r| { if r.team.is_some() { let mut st=state(r)?; st.error=error.map(str::to_string); save(r,&st)?; } Ok(()) }));
}
