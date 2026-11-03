//! Synthetic A0 release gate. Run only inside Omabox with the isolated local Teams
//! server on port 18788. Uses real gateway routing and required receipt transport.
use conduit_lib::http_client::{RequestHeaderExt as _, ResponseResultExt as _};
use conduit_lib::{registry, teams};
use serde_json::{json, Value};
use std::process::Command;

#[test]
#[ignore = "requires private Omabox HOME/keyring"]
fn managed_switch_preserves_existing_local_value() {
    assert_eq!(std::env::var("HOME").unwrap(), "/home/sbx");
    let _lock = registry::data_dir_test_lock();
    let dir = std::path::PathBuf::from("/home/sbx/managed-value-test");
    std::fs::create_dir_all(&dir).unwrap();
    let _override = registry::DataDirOverride::set(dir);
    registry::update(|r| {
        r.team = Some(serde_json::from_value(json!({"serverUrl":"http://127.0.0.1:18788","teamId":"synthetic","role":"admin","lastVersion":1,"managedServerIds":{"team_original":"original"}})).unwrap());
        r.servers = vec![
            serde_json::from_value(json!({"id":"original","name":"Original","transport":"stdio","command":"echo","args":[],"env":[{"key":"LOCAL_VALUE","secret":false,"value":"personal-value"}]})).unwrap(),
            serde_json::from_value(json!({"id":"team_original","name":"Managed","transport":"stdio","command":"echo","args":[],"source":"team:synthetic","env":[{"key":"LOCAL_VALUE","secret":false,"value":"managed-value"}]})).unwrap(),
        ];
        Ok(())
    }).unwrap();
    conduit_lib::secrets::delete_secret("team_original", "LOCAL_VALUE").unwrap();
    assert!(teams::use_managed_server("team_original").is_err());
    assert_eq!(
        registry::load().unwrap().servers[1].env[0].value.as_deref(),
        Some("managed-value")
    );
    assert_eq!(
        registry::load().unwrap().servers[0].env[0].value.as_deref(),
        Some("personal-value")
    );
    conduit_lib::secrets::delete_secret("team_original", "LOCAL_VALUE").unwrap();
}

#[test]
#[ignore = "requires private Omabox HOME/keyring and synthetic Teams on 18788"]
fn managed_call_reaches_teams_with_raw_identity() {
    assert_eq!(
        std::env::var("HOME").unwrap(),
        "/home/sbx",
        "run inside Omabox"
    );
    let _lock = registry::data_dir_test_lock();
    let dir = std::path::PathBuf::from("/home/sbx/activation-a0");
    std::fs::create_dir_all(&dir).unwrap();
    let _override = registry::DataDirOverride::set(&dir);
    let api = "http://127.0.0.1:18788";
    // A separately authenticated synthetic seat avoids the self-host global seat
    // limit when reusing this fixture for one real AI-client validation.
    let supplied: Option<Value> = std::env::var("ACTIVATION_SEAT_FILE")
        .ok()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap());
    let created: Value = if let Some(value) = supplied.as_ref() {
        value.clone()
    } else {
        conduit_lib::http_client::agent()
            .post(&format!("{api}/teams"))
            .set_header("authorization", "Bearer activation-synthetic-bootstrap")
            .send_json(json!({"name":"A0 synthetic identity"}))
            .retain_status_body()
            .unwrap()
            .into_body()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .unwrap()
    };
    let team = created["team_id"].as_str().unwrap();
    let auth = format!("Bearer {}", created["admin_token"].as_str().unwrap());
    let fixture = "/home/sbx/activation-mcp.py";
    std::fs::write(fixture, r#"import sys,json
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');i=r.get('id')
 if i is None:continue
 if m=='initialize':v={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'synthetic-echo','version':'1'}}
 elif m=='tools/list':v={'tools':[{'name':'echo','description':'Read-only synthetic greeting','inputSchema':{'type':'object','properties':{'fail':{'type':'boolean'}}}}]}
 elif m=='tools/call':v={'content':[{'type':'text','text':'Synthetic result'}],'isError':r.get('params',{}).get('arguments',{}).get('fail',False)}
 else:v={}
 print(json.dumps({'jsonrpc':'2.0','id':i,'result':v}),flush=True)
"#).unwrap();
    conduit_lib::http_client::agent().put(&format!("{api}/teams/{team}/config")).set_header("authorization", &auth)
        .send_json(json!({"base_version":0,"config":{"servers":[{"id":"audit-echo", "name":"Synthetic echo", "transport":"stdio", "command":"python3", "args":[fixture]}]}})).retain_status_body().unwrap();
    let invite: Value = if let Some(value) = supplied.as_ref() {
        json!({"invite_code":value["connectCode"]})
    } else {
        conduit_lib::http_client::agent()
            .post(&format!("{api}/teams/{team}/invites"))
            .set_header("authorization", &auth)
            .send_json(json!({"role":"member"}))
            .retain_status_body()
            .unwrap()
            .into_body()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .unwrap()
    };
    teams::connect(
        api,
        invite["invite_code"].as_str().unwrap(),
        Some("Synthetic member"),
    )
    .unwrap();
    let auth = if supplied.is_some() {
        format!("Bearer {}", teams::load_token().unwrap().unwrap())
    } else {
        auth
    };
    registry::update(|r| {
        let id = r
            .servers
            .iter()
            .find(|s| s.source.as_deref() == Some(format!("team:{team}").as_str()))
            .unwrap()
            .id
            .clone();
        assert!(id.starts_with("team_audit-echo-"));
        assert_eq!(
            r.team
                .as_ref()
                .unwrap()
                .managed_server_ids
                .get(&id)
                .map(String::as_str),
            Some("audit-echo")
        );
        // Explicit synthetic review. The final usability gate must use the actual UI.
        r.set_server_enabled("default", &id, true)?;
        Ok(())
    })
    .unwrap();
    let client = r#"import subprocess,json,select,time,sys
p=subprocess.Popen([sys.argv[1]],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open('/home/sbx/activation-gateway.log','w'),text=True,bufsize=1)
def rpc(i,m,params):
 p.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':m,'params':params})+'\n');p.stdin.flush();deadline=time.time()+40
 while time.time()<deadline:
  if select.select([p.stdout],[],[],1)[0]:
   line=p.stdout.readline()
   if not line:break
   r=json.loads(line)
   if r.get('id')==i:return r
 raise RuntimeError('timeout '+m)
try:
 rpc(1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'activation-protocol-test','version':'1'}})
 p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');p.stdin.flush()
 found=rpc(2,'tools/call',{'name':'toolport_search_tools','arguments':{'query':'echo'}})
 import re
 name=re.search(r'team_audit_echo_[a-z0-9_]+__echo',json.dumps(found)).group(0)
 for i,fail in [(3,False),(4,True)]:
  r=rpc(i,'tools/call',{'name':'toolport_call_tool','arguments':{'name':name,'arguments':{'fail':fail}}})
  print(json.dumps(r),flush=True)
  assert r['result'].get('isError',False)==fail,r
finally:p.terminate();p.wait(timeout=10)
"#;
    let output = Command::new("python3")
        .args(["-c", client, &std::env::var("ACTIVATION_GATEWAY").unwrap()])
        .env("TOOLPORT_DATA_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    teams::sync_now().unwrap();
    let evidence: Value = conduit_lib::http_client::agent()
        .get(&format!("{api}/teams/{team}/activation"))
        .set_header("authorization", &auth)
        .call()
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    let row = &evidence["devices"][0];
    assert!(row["firstSuccessAt"].is_i64(), "{evidence}");
    let counter = &row["receipt"]["counters"]["audit-echo"];
    assert_eq!(counter["successes"], 1);
    assert_eq!(counter["failures"], 1);
    teams::sync_now().unwrap();
    let retried: Value = conduit_lib::http_client::agent()
        .get(&format!("{api}/teams/{team}/activation"))
        .set_header("authorization", &auth)
        .call()
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    assert_eq!(
        retried["devices"][0]["firstSuccessAt"],
        row["firstSuccessAt"]
    );
    std::fs::write(
        "/home/sbx/a0-evidence.json",
        serde_json::to_string_pretty(&retried).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires synthetic authenticated portal fixture in private Omabox"]
fn portal_member_connect_keeps_the_authenticated_seat() {
    assert_eq!(std::env::var("HOME").unwrap(), "/home/sbx");
    let _lock = registry::data_dir_test_lock();
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string("/home/sbx/portal-connect.json").unwrap())
            .unwrap();
    let dir = std::path::PathBuf::from("/home/sbx/activation-member");
    std::fs::create_dir_all(&dir).unwrap();
    let _override = registry::DataDirOverride::set(dir);
    teams::connect(
        "http://127.0.0.1:18788",
        fixture["connectCode"].as_str().unwrap(),
        None,
    )
    .unwrap();
    teams::sync_now().unwrap();
    let connection = registry::load().unwrap().team.unwrap();
    assert_eq!(connection.team_id, fixture["teamId"].as_str().unwrap());
    assert_eq!(connection.role, "member");
    assert_eq!(connection.account_linked, Some(true));
    assert_eq!(connection.team_name.as_deref(), Some("Activation Acme"));
    let token = teams::load_token().unwrap().unwrap();
    let me: Value = conduit_lib::http_client::agent()
        .get(&format!(
            "http://127.0.0.1:18788/teams/{}/me",
            connection.team_id
        ))
        .set_header("authorization", &format!("Bearer {token}"))
        .call()
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    assert_eq!(me["member_id"], fixture["memberId"]);
}

#[test]
#[ignore = "requires private Omabox HOME/keyring and synthetic Teams on 18788"]
fn selected_share_is_additive_conflict_safe_and_locally_usable() {
    assert_eq!(std::env::var("HOME").unwrap(), "/home/sbx");
    let _lock = registry::data_dir_test_lock();
    let dir = std::path::PathBuf::from(format!(
        "/home/sbx/activation-a3-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let _override = registry::DataDirOverride::set(&dir);
    let api = "http://127.0.0.1:18788";
    let created: Value = conduit_lib::http_client::agent()
        .post(&format!("{api}/teams"))
        .set_header("authorization", "Bearer activation-synthetic-bootstrap")
        .send_json(json!({"name":"A3 selected share"}))
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    let team = created["team_id"].as_str().unwrap();
    let auth = format!("Bearer {}", created["admin_token"].as_str().unwrap());
    let remote = json!({"id":"unrelated", "name":"Other team server", "transport":"http", "url":"https://example.test/mcp"});
    conduit_lib::http_client::agent()
        .put(&format!("{api}/teams/{team}/config"))
        .set_header("authorization", &auth)
        .send_json(json!({"base_version":0,"config":{"servers":[remote],"denyDestructive":true}}))
        .retain_status_body()
        .unwrap();
    let invite: Value = conduit_lib::http_client::agent()
        .post(&format!("{api}/teams/{team}/invites"))
        .set_header("authorization", &auth)
        .send_json(json!({"role":"admin"}))
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    registry::update(|r| {
        r.servers.push(serde_json::from_value(json!({"id":"selected-one","name":"Selected one","enabled":true,"transport":"stdio","command":"python3","args":["/home/sbx/activation-mcp.py"],"cwd":"/home/sbx","env":[{"key":"SYNTHETIC_KEY","secret":true}]})).unwrap());
        r.servers.push(serde_json::from_value(json!({"id":"keep-personal","name":"Keep personal","enabled":true,"transport":"stdio","command":"python3","args":[],"env":[]})).unwrap());
        r.profiles[0].enabled_server_ids.extend(["selected-one".into(),"keep-personal".into()]);
        Ok(())
    }).unwrap();
    conduit_lib::secrets::set_secret("selected-one", "SYNTHETIC_KEY", "synthetic-only-secret")
        .unwrap();
    teams::connect(
        api,
        invite["invite_code"].as_str().unwrap(),
        Some("A3 admin"),
    )
    .unwrap();
    let ids = vec!["selected-one".into()];
    let preview = teams::preview_push_selected(&ids).unwrap();
    assert!(preview.removed.is_empty());
    assert_eq!(preview.added, vec!["Selected one"]);
    let definition = &preview.definitions[0];
    assert_eq!(definition.name, "Selected one");
    assert!(definition
        .fields
        .iter()
        .any(|field| field.label == "Command" && field.value == "python3"));
    assert!(definition
        .fields
        .iter()
        .any(|field| field.value == "SYNTHETIC_KEY"));
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("synthetic-only-secret"));
    let published =
        teams::push_selected(&ids, preview.base_version, &preview.local_fingerprint).unwrap();
    assert!(
        published.local_setup_error.is_none(),
        "local setup must complete"
    );
    assert!(
        teams::push_selected(&ids, preview.base_version, &preview.local_fingerprint)
            .unwrap_err()
            .contains("changed")
    );
    let config: Value = conduit_lib::http_client::agent()
        .get(&format!("{api}/teams/{team}/config"))
        .set_header("authorization", &auth)
        .call()
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    assert!(config["config"]["servers"]
        .as_array()
        .unwrap()
        .contains(&remote));
    assert_eq!(config["config"]["servers"].as_array().unwrap().len(), 2);
    assert_eq!(config["config"]["denyDestructive"], true);
    assert!(!config.to_string().contains("synthetic-only-secret"));
    teams::sync_now().unwrap();
    // Released managed identities can include a collision-safe suffix. Use the
    // recorded binding rather than assuming the old unsuffixed local id.
    let managed_id = registry::load()
        .unwrap()
        .team
        .unwrap()
        .managed_server_ids
        .into_iter()
        .find(|(_, raw)| raw == "selected-one")
        .unwrap()
        .0;
    // Publishing is the complete owner handoff, with no second enable/switch action.
    let r = registry::load().unwrap();
    assert!(r.is_enabled(&r.active_profile_id(), &managed_id));
    assert!(!r.is_enabled(&r.active_profile_id(), "selected-one"));
    assert!(r.is_enabled(&r.active_profile_id(), "keep-personal"));
    assert!(r.servers.iter().any(|s| s.id == "selected-one"));
    assert!(
        conduit_lib::secrets::get_secret_result(&managed_id, "SYNTHETIC_KEY")
            .unwrap()
            .as_deref()
            == Some("synthetic-only-secret"),
        "managed identity must retain the synthetic credential"
    );
    std::fs::write("/home/sbx/activation-mcp.py", r#"import sys,json,os
assert os.environ.get('SYNTHETIC_KEY') == 'synthetic-only-secret'
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');i=r.get('id')
 if i is None:continue
 if m=='initialize':v={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'publisher-test','version':'1'}}
 elif m=='tools/list':v={'tools':[{'name':'echo','description':'Synthetic read-only greeting','inputSchema':{'type':'object'}}]}
 elif m=='tools/call':v={'content':[{'type':'text','text':'Synthetic success'}]}
 else:v={}
 print(json.dumps({'jsonrpc':'2.0','id':i,'result':v}),flush=True)
"#).unwrap();
    let managed = r.servers.iter().find(|s| s.id == managed_id).unwrap();
    let mut downstream = conduit_lib::server_runtime::connect_server(managed).unwrap();
    assert!(downstream
        .call("echo", json!({}))
        .unwrap()
        .to_string()
        .contains("Synthetic success"));
    let client = r#"import subprocess,json,select,time,sys,re
p=subprocess.Popen([sys.argv[1]],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open('/home/sbx/publisher-gateway.log','w'),text=True,bufsize=1)
def rpc(i,m,params):
 p.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':m,'params':params})+'\n');p.stdin.flush();deadline=time.time()+40
 while time.time()<deadline:
  if select.select([p.stdout],[],[],1)[0]:
   line=p.stdout.readline()
   if not line:break
   r=json.loads(line)
   if r.get('id')==i:return r
 raise RuntimeError('timeout '+m)
try:
 rpc(1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'publisher-regression','version':'1'}})
 p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');p.stdin.flush()
 found=rpc(2,'tools/call',{'name':'toolport_search_tools','arguments':{'query':'echo'}})
 name=re.search(r'team_selected_one_[a-z0-9_]+__echo',json.dumps(found)).group(0)
 result=rpc(3,'tools/call',{'name':'toolport_call_tool','arguments':{'name':name,'arguments':{}}})
 assert 'Synthetic success' in json.dumps(result),result
finally:p.terminate();p.wait(timeout=10)
"#;
    let output = Command::new("python3")
        .args([
            "-c",
            client,
            &std::env::var("ACTIVATION_GATEWAY").expect("set the candidate gateway path"),
        ])
        .env("TOOLPORT_DATA_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    teams::sync_now().unwrap();
    let activation: Value = conduit_lib::http_client::agent()
        .get(&format!("{api}/teams/{team}/activation"))
        .set_header("authorization", &auth)
        .call()
        .retain_status_body()
        .unwrap()
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_json()
        .unwrap();
    assert!(
        activation["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["firstSuccessAt"].is_i64()
                && device["receipt"]["counters"]["selected-one"]["successes"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0),
        "the managed success must reach Teams"
    );
    std::fs::write(
        "/home/sbx/publisher-ready.json",
        serde_json::to_vec_pretty(&r).unwrap(),
    )
    .unwrap();
    registry::update(|r| {
        r.servers
            .iter_mut()
            .find(|s| s.id == managed_id)
            .unwrap()
            .args
            .push("--changed-target".into());
        Ok(())
    })
    .unwrap();
    assert!(teams::use_managed_server(&managed_id).is_err());
    teams::disconnect().unwrap();
    let disconnected = registry::load().unwrap();
    assert!(disconnected.team.is_none());
    assert!(disconnected.is_enabled(&disconnected.active_profile_id(), "selected-one"));
    assert!(
        conduit_lib::secrets::get_secret("selected-one", "SYNTHETIC_KEY").as_deref()
            == Some("synthetic-only-secret"),
        "personal credential must survive disconnect"
    );
    assert!(teams::load_token().unwrap().is_none());
}
