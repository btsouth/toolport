//! A Run on the Tools tab, through the real gateway against `mock-mcp-server`:
//! a server that is slow to start still gets its call, the Run says while it
//! waits for the start, and Cancel stops a Run that is still waiting.

#![cfg(all(unix, feature = "test-support"))]

mod chaos_support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chaos_support::{mock_entry, write_registry, Scratch, GATEWAY};
use conduit_lib::playground::{
    call_tool_with_gateway, run_time_limit, start_wait, RunPhase, ToolRun,
};
use serde_json::json;

fn scratch(tag: &str, env: &[(&str, &str)]) -> (Scratch, conduit_lib::registry::DataDirOverride) {
    let scratch = Scratch::new(tag);
    write_registry(scratch.path(), &[mock_entry("slow", env)], &["slow"]);
    let data = conduit_lib::registry::DataDirOverride::set(scratch.path());
    (scratch, data)
}

fn recorder() -> (
    Arc<Mutex<Vec<RunPhase>>>,
    impl Fn(RunPhase) + Send + Sync + 'static,
) {
    let phases = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&phases);
    (phases, move |phase| sink.lock().unwrap().push(phase))
}

#[test]
fn the_limit_covers_a_cold_start_an_approval_and_the_call() {
    let limit = run_time_limit(None);
    assert!(limit > conduit_lib::downstream::SETUP_CATALOG_WAIT_BUDGET * 2);
    assert!(limit >= Duration::from_secs(25 + 120 + 30));
    let server = |command: &str, args: &[&str]| -> conduit_lib::registry::ServerEntry {
        serde_json::from_value(serde_json::json!({
            "id": "s", "name": "S", "transport": "stdio", "command": command,
            "args": args, "env": [],
        }))
        .unwrap()
    };
    let local = server("/usr/bin/local-server", &[]);
    assert_eq!(run_time_limit(Some(&local)), limit);
    // A server's own longer call limit is added, not capped at the default.
    let mut long_calls = local.clone();
    long_calls.request_timeout_ms = Some(600_000);
    assert_eq!(
        run_time_limit(Some(&long_calls)) - limit,
        Duration::from_secs(600) - conduit_lib::downstream::STDIO_READ_TIMEOUT
    );
    // So is the time a server may take to start: two minutes for a command that
    // downloads its package first, or the server's own startup limit.
    let download = server("npx", &["-y", "@example/server"]);
    assert_eq!(start_wait(&download), Duration::from_secs(120));
    assert_eq!(
        run_time_limit(Some(&download)) - limit,
        Duration::from_secs(120) - conduit_lib::downstream::SETUP_CATALOG_WAIT_BUDGET
    );
    let mut slow = local.clone();
    slow.initialize_timeout_ms = Some(300_000);
    assert_eq!(start_wait(&slow), Duration::from_secs(300));
}

#[test]
fn a_slow_start_then_a_normal_call_succeeds_and_says_it_was_starting() {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    let (_scratch, _data) = scratch("slow-run", &[("MOCK_MCP_START_DELAY_MS", "3000")]);
    let (phases, on_phase) = recorder();
    let run = ToolRun::new();
    let started = Instant::now();
    let result = call_tool_with_gateway(
        GATEWAY.as_ref(),
        "slow",
        "echo",
        json!({"text": "after a slow start"}),
        &run,
        on_phase,
    )
    .expect("the call succeeds after the slow start");
    assert!(
        result.to_string().contains("after a slow start"),
        "unexpected result: {result}"
    );
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert_eq!(
        *phases.lock().unwrap(),
        vec![RunPhase::Starting, RunPhase::Started]
    );
    assert!(!run.is_starting());
}

/// The Tools tab's handshake does not wait for servers, so the gateway itself
/// says the call waits for the start, and Cancel ends that wait.
#[test]
fn cancel_while_the_server_starts_ends_the_run_at_once() {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    // Starts slower than the gateway waits, so only Cancel ends the Run early.
    let (_scratch, _data) = scratch("cancel-run", &[("MOCK_MCP_START_DELAY_MS", "60000")]);
    let (phases, on_phase) = recorder();
    let run = ToolRun::new();
    let watcher = run.clone();
    let canceller = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !watcher.is_starting() {
            assert!(
                Instant::now() < deadline,
                "the Run never said it was starting"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        watcher.cancel();
        Instant::now()
    });
    let result = call_tool_with_gateway(
        GATEWAY.as_ref(),
        "slow",
        "echo",
        json!({"text": "never"}),
        &run,
        on_phase,
    );
    let returned = Instant::now();
    let cancelled_at = canceller.join().unwrap();
    let error = result.expect_err("a cancelled Run fails");
    assert!(
        error.contains("cancel"),
        "the error says it was cancelled: {error}"
    );
    assert!(
        returned.duration_since(cancelled_at) < Duration::from_secs(3),
        "Cancel took {:?} to end the Run",
        returned.duration_since(cancelled_at)
    );
    assert_eq!(*phases.lock().unwrap(), vec![RunPhase::Starting]);
    assert!(!run.is_starting());
}

/// The usual case: the gateway already knows the server's tools from an
/// earlier start, so it answers the handshake at once and waits for the start
/// only when the call comes in. That wait is reported too.
#[test]
fn a_known_server_that_is_not_running_yet_says_it_is_starting() {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    let (scratch, _data) = scratch("warm-cache", &[("MOCK_MCP_START_DELAY_MS", "3000")]);
    let first = call_tool_with_gateway(
        GATEWAY.as_ref(),
        "slow",
        "echo",
        json!({"text": "first"}),
        &ToolRun::new(),
        |_| {},
    );
    assert!(first.is_ok(), "{first:?}");
    // Stop the shared gateway, so the server is down but its tools are cached.
    chaos_support::kill_daemon(scratch.path());
    let (phases, on_phase) = recorder();
    let started = Instant::now();
    let result = call_tool_with_gateway(
        GATEWAY.as_ref(),
        "slow",
        "echo",
        json!({"text": "second"}),
        &ToolRun::new(),
        on_phase,
    )
    .expect("the call succeeds after the start");
    assert!(result.to_string().contains("second"), "{result}");
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert_eq!(
        *phases.lock().unwrap(),
        vec![RunPhase::Starting, RunPhase::Started]
    );
}

/// Start notices are progress on the client's own token, so only a request
/// that asks for them gets them. Another client's progress stays the server's.
#[test]
fn start_notices_go_only_to_a_request_that_asks() {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    let (scratch, _data) = scratch("notices", &[("MOCK_MCP_START_DELAY_MS", "2000")]);
    let notices = |asks: bool| {
        let mut meta = json!({"progressToken": "p1"});
        if asks {
            meta[conduit_lib::downstream::START_NOTICES_META_KEY] = json!(true);
        }
        let mut client = chaos_support::Client::start(scratch.path(), "other");
        // `echo_meta` returns the `_meta` the server received.
        let id = client.send_request(
            "tools/call",
            json!({
                "name": "toolport_call_tool",
                "arguments": {
                    "_toolportTarget": {"serverId": "slow", "tool": "echo_meta"},
                    "arguments": {},
                },
                "_meta": meta,
            }),
        );
        let reply = client.wait_for_id(id, Duration::from_secs(60)).to_string();
        assert!(reply.contains("progressToken"), "{reply}");
        assert!(
            !reply.contains("startNotices"),
            "the ask reached the server: {reply}"
        );
        let phases: Vec<_> = client
            .notes
            .iter()
            .filter_map(|note| note["params"]["_meta"]["toolport/phase"].as_str())
            .map(str::to_string)
            .collect();
        // Stop the shared gateway, so the next call starts the server again.
        chaos_support::kill_daemon(scratch.path());
        phases
    };
    assert_eq!(notices(false), Vec::<String>::new());
    assert_eq!(notices(true), vec!["starting", "started"]);
}
