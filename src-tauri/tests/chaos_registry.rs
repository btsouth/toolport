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
    write_registry(
        scratch.path(),
        &[mock_entry("x", &[])],
        &["x"],
    );
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

    // The unreadable bytes are preserved for inspection before anything heals.
    wait_for("the corrupt copy to be quarantined", Duration::from_secs(30), || {
        !scratch.matching("registry.json.unreadable-").is_empty()
    });
    let quarantined = scratch
        .matching("registry.json.unreadable-")
        .into_iter()
        .map(|name| scratch.read(&name))
        .find(|content| content.contains("TRUNCATED"));
    assert!(
        quarantined.is_some(),
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
        failures, 0,
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
