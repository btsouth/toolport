//! P1.7 chaos: registry corruption and rapid enable/disable churn must not take
//! the running gateway or a stable server down.
//!
//! Unix only for the shared daemon harness.

#![cfg(unix)]

mod chaos_support;

use std::time::Duration;

use chaos_support::{
    live_daemon_pid, mock_entry, set_enabled, start_daemon, wait_for, write_registry, Client,
    Scratch,
};
use serde_json::json;

const CATALOG: Duration = Duration::from_secs(60);

#[test]
fn a_corrupt_registry_does_not_take_down_a_running_gateway() {
    let scratch = Scratch::new("registry-corrupt");
    write_registry(scratch.path(), &[mock_entry("x", &[])], &["x"]);
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("x__echo", CATALOG),
        "{}",
        client.diagnostics()
    );
    let before = client.call("x__echo", json!({ "text": "before" }));
    assert!(
        chaos_support::reply_ok(&before, "before"),
        "the server should answer before the corruption: {before}"
    );

    // Truncate the registry under the running gateway, exactly as a crashed
    // writer or a full disk would.
    let corrupt = "{ \"version\": 1, \"servers\": [ { \"id\": \"x\", TRUNCATED";
    std::fs::write(scratch.path().join("registry.json"), corrupt).expect("corrupt the registry");

    // A matching temporary filename is not evidence that quarantine finished.
    // Wait for the exact final copy, including every byte of the failed write.
    let quarantined = format!(
        "registry.json.unreadable-sha256-{}",
        conduit_lib::registry::sha256_hex(corrupt)
    );
    wait_for(
        "the corrupt copy to be quarantined",
        Duration::from_secs(30),
        || scratch.read(&quarantined) == corrupt,
    );
    assert_eq!(
        scratch.read(&quarantined),
        corrupt,
        "the corrupt bytes must survive in the quarantine copy"
    );

    // The gateway process is still alive and still answers its own requests.
    assert!(
        live_daemon_pid(scratch.path()).is_some(),
        "the daemon died on a corrupt registry\n{}",
        chaos_support::log_tail(scratch.path())
    );
    let _ = client.tool_names();

    // A brand-new client can still start against the surviving gateway.
    let mut fresh = Client::start(scratch.path(), "fresh");
    let _ = fresh.tool_names();
}

#[test]
fn rapid_registry_toggles_do_not_break_a_stable_server() {
    let scratch = Scratch::new("registry-toggle");
    write_registry(
        scratch.path(),
        &[mock_entry("x", &[]), mock_entry("y", &[])],
        &["x", "y"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("x__echo", CATALOG) && client.wait_for_tool("y__echo", CATALOG),
        "{}",
        client.diagnostics()
    );

    // Toggle the unrelated server `y` on and off while `x` keeps serving.
    let mut failures = 0;
    for round in 0..8 {
        if round % 2 == 0 {
            set_enabled(scratch.path(), &["x"]);
        } else {
            set_enabled(scratch.path(), &["x", "y"]);
        }
        let reply = client.call("x__echo", json!({ "text": "steady" }));
        if !chaos_support::reply_ok(&reply, "steady") {
            failures += 1;
            eprintln!("toggle round {round}: {reply}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(
        failures,
        0,
        "the stable server failed during registry churn\n{}",
        client.diagnostics()
    );

    // Once the churn settles with `y` enabled, its catalog entry returns.
    set_enabled(scratch.path(), &["x", "y"]);
    assert!(
        client.wait_for_tool("y__echo", Duration::from_secs(30)),
        "the re-enabled server never came back\n{}",
        client.diagnostics()
    );
    let back = client.call("y__echo", json!({ "text": "back" }));
    assert!(
        chaos_support::reply_ok(&back, "back"),
        "the re-enabled server answered wrong: {back}"
    );
}

#[test]
fn editing_one_server_preserves_the_other_pid_and_in_flight_call() {
    let scratch = Scratch::new("incremental-reload");
    let a_pids = scratch.join("a.pids");
    let b_pids = scratch.join("b.pids");
    let transcript = scratch.join("b.requests");
    let trace = scratch.join("b.wire.jsonl");
    let a_path = a_pids.to_string_lossy();
    let b_path = b_pids.to_string_lossy();
    let transcript_path = transcript.to_string_lossy();
    let trace_path = trace.to_string_lossy();
    let mut a = mock_entry("a", &[("MOCK_MCP_PID_FILE", &a_path)]);
    let b = mock_entry(
        "b",
        &[
            ("MOCK_MCP_PID_FILE", &b_path),
            ("MOCK_MCP_TRANSCRIPT", &transcript_path),
            ("MOCK_MCP_WIRE_TRACE", &trace_path),
            ("MOCK_MCP_CONCURRENT", "1"),
        ],
    );
    write_registry(scratch.path(), &[a.clone(), b.clone()], &["a", "b"]);
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "observer");
    assert!(client.wait_for_tool("a__echo", CATALOG));
    assert!(client.wait_for_tool("b__sleep", CATALOG));
    let old_a = std::fs::read_to_string(&a_pids).unwrap();
    let old_b = std::fs::read_to_string(&b_pids).unwrap();
    let mut worker = Client::start(scratch.path(), "worker");
    let call = worker.call_async("b__sleep", json!({"ms": 6000}));
    wait_for(
        "B's call to reach its child",
        Duration::from_secs(10),
        || {
            std::fs::read_to_string(&transcript)
                .unwrap_or_default()
                .contains("\"name\":\"sleep\"")
        },
    );
    a["env"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key":"RELOAD_REVISION", "value":"2", "secret":false}));
    write_registry(scratch.path(), &[a, b], &["a", "b"]);
    // Demand A after the edit; it must receive a new supervisor/child while B
    // is still sleeping. The stable catalog stays readable throughout.
    wait_for("A's replacement", Duration::from_secs(20), || {
        let _ = client.call("a__echo", json!({"text":"changed"}));
        std::fs::read_to_string(&a_pids)
            .unwrap_or_default()
            .lines()
            .count()
            > old_a.lines().count()
    });
    let list_started = std::time::Instant::now();
    assert!(client.tool_names().contains(&"b__sleep".to_string()));
    assert!(list_started.elapsed() < Duration::from_secs(2));
    let reply = worker.wait_for_id_with_diagnostics(call, Duration::from_secs(15), || {
        let mut state = format!("observer:\n{}\n", client.diagnostics());
        let mut pids: Vec<_> = live_daemon_pid(scratch.path())
            .into_iter()
            .map(|pid| pid.to_string())
            .collect();
        for name in [
            "a.pids",
            "b.pids",
            "b.requests",
            "b.wire.jsonl",
            "registry.json",
        ] {
            let raw = scratch.read(name);
            if name.ends_with(".pids") {
                pids.extend(
                    raw.lines()
                        .filter(|line| line.parse::<u32>().is_ok())
                        .map(str::to_string),
                );
            }
            let lines: Vec<_> = raw.lines().collect();
            state.push_str(&format!(
                "{name}:\n{}\n",
                lines[lines.len().saturating_sub(64)..].join("\n")
            ));
        }
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "pid,ppid,stat,etime,pcpu,comm", "-p", &pids.join(",")])
            .output()
        {
            state.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        state
    });
    assert!(
        !reply["result"]["isError"].as_bool().unwrap_or(false),
        "B's call failed: {reply}"
    );
    assert!(reply.get("error").is_none(), "B's call failed: {reply}");
    assert_eq!(
        std::fs::read_to_string(&b_pids).unwrap(),
        old_b,
        "editing A restarted B"
    );
    assert!(chaos_support::reply_ok(
        &client.call("b__echo", json!({"text":"steady"})),
        "steady"
    ));
}

#[test]
fn a_cached_server_stays_lazy_then_stops_when_idle_and_restarts_on_use() {
    let scratch = Scratch::new("lazy-idle");
    let pids = scratch.join("lazy.pids");
    let pid_path = pids.to_string_lossy();
    write_registry(
        scratch.path(),
        &[mock_entry("lazy", &[("MOCK_MCP_PID_FILE", &pid_path)])],
        &["lazy"],
    );
    // Seed a current, spec-bound cache through the real publication path.
    {
        let _warm_daemon = start_daemon(scratch.path());
        let mut warm_client = Client::start(scratch.path(), "cache-writer");
        assert!(warm_client.wait_for_tool("lazy__echo", CATALOG));
        wait_for("the catalog cache to be persisted", CATALOG, || {
            std::fs::read_to_string(scratch.join("tool-cache.servers.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .is_some_and(|cache| has_cached_echo(&cache))
        });
    }
    std::fs::remove_file(&pids).unwrap();
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "lazy-client");
    assert!(client.wait_for_tool("lazy__echo", CATALOG));
    // Exercise another full list after startup publication. Neither list may
    // demand a server whose persisted catalog already covers the tool.
    assert!(client.tool_names().contains(&"lazy__echo".to_string()));
    assert!(!pids.exists(), "cached discovery spawned a lazy server");
    assert!(chaos_support::reply_ok(
        &client.call("lazy__echo", json!({"text":"chaos"})),
        "chaos"
    ));
    let first = std::fs::read_to_string(&pids).unwrap();
    let pid = first.lines().last().unwrap().parse().unwrap();
    wait_for(
        "the idle server to stop",
        conduit_lib::router::SERVER_IDLE_TIMEOUT + Duration::from_secs(20),
        || !chaos_support::pid_running(pid),
    );
    assert!(client.tool_names().contains(&"lazy__echo".to_string()));
    assert_eq!(
        std::fs::read_to_string(&pids).unwrap(),
        first,
        "cached discovery restarted an idle server"
    );
    assert!(chaos_support::reply_ok(
        &client.call("lazy__echo", json!({"text":"chaos"})),
        "chaos"
    ));
    let second = std::fs::read_to_string(&pids).unwrap();
    assert_eq!(second.lines().count(), first.lines().count() + 1);
    assert_ne!(second.lines().last(), first.lines().last());
}

// Startup persists a spec before first-use discovery has populated its tools.
// A matching spec alone cannot prove that a restart will stay lazy.
fn has_cached_echo(cache: &serde_json::Value) -> bool {
    cache["version"] == 4
        && cache["servers"]["lazy"]["spec"].is_string()
        && cache["servers"]["lazy"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "echo"))
}

#[test]
fn an_empty_persisted_catalog_is_not_a_warm_cache() {
    let mut cache = json!({"version":4,"servers":{"lazy":{"spec":"current","tools":[]}}});
    assert!(!has_cached_echo(&cache));
    cache["servers"]["lazy"]["tools"] = json!([{"name":"echo"}]);
    assert!(has_cached_echo(&cache));
    cache["version"] = json!(3);
    assert!(!has_cached_echo(&cache));
}
