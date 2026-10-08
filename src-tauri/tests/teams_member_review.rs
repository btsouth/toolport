//! Held team definitions must stay absent from discovery and dispatch in the real gateway.
#![cfg(unix)]
mod chaos_support;

use chaos_support::{Client, Scratch, MOCK};
use conduit_lib::{registry, teams};
use serde_json::json;

#[test]
fn held_team_servers_never_start_or_dispatch_via_search_and_call() {
    let _lock = registry::data_dir_test_lock();
    let scratch = Scratch::new("teams-member-review");
    let _data = registry::DataDirOverride::set(scratch.path());
    let marker = scratch.join("held-server-started");
    let mut reg = registry::Registry::default();
    reg.team = Some(serde_json::from_value(json!({"teamId":"test-review", "serverUrl":"https://teams.toolport.app", "role":"member"})).unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = json!({"servers":[
        {"id":"held", "name":"Held echo", "transport":"stdio", "command":"sh", "args":["-c", "touch \"$1\"; exec \"$2\"", "fixture", marker, MOCK], "env":[]},
        {"id":"remote", "name":"Held remote", "transport":"http", "url":format!("http://{}/mcp", listener.local_addr().unwrap()), "env":[]}
    ]});
    teams::stage_team_config(&mut reg, "test-review", &config, 1, &[]).unwrap();
    let ids: Vec<_> = reg.servers.iter().map(|s| s.id.clone()).collect();
    registry::save_to(&scratch.join("registry.json"), &reg).unwrap();
    let _daemon = chaos_support::start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "member");
    let names = client.tool_names();
    for id in &ids {
        let target = format!("{id}__echo");
        assert!(!names.iter().any(|name| name.starts_with(id)));
        let direct = client.call(&target, json!({"text":"must stay held"}));
        assert!(
            direct.get("error").is_some() || direct["result"]["isError"] == true,
            "direct dispatch succeeded: {direct}"
        );
        let routed = client.call("toolport_call_tool", json!({"_toolportTarget":{"serverId":id,"tool":"echo"},"arguments":{"text":"must stay held"}}));
        assert!(
            routed.get("error").is_some() || routed["result"]["isError"] == true,
            "gateway dispatch succeeded: {routed}"
        );
    }
    let search = client.call("toolport_search_tools", json!({"query":"echo"}));
    assert!(!search.to_string().contains("Held echo"));
    assert!(!search.to_string().contains("Held remote"));
    assert!(!marker.exists(), "held stdio process started");
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "held remote connected"
    );
}
