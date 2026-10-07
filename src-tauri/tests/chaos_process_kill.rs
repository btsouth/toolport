//! P1.7 chaos: killing the shared host daemon, and killing a downstream child,
//! must not leave the client stuck or the server permanently gone.
//!
//! The daemon is killed by the pid in its descriptor, never a process-name
//! pattern, so a parallel test's daemon is never in range. Unix only.

#![cfg(unix)]

mod chaos_support;

use std::time::Duration;

use chaos_support::{
    daemon_pid, mock_entry, pid_alive, pid_running, signal, start_daemon, wait_for,
    write_registry, Client, Scratch,
};
use serde_json::json;

const CATALOG: Duration = Duration::from_secs(60);

#[test]
fn a_killed_daemon_is_replaced_and_the_client_recovers() {
    let scratch = Scratch::new("daemon-kill");
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
        "the server should answer before the kill: {before}"
    );

    let old = daemon_pid(scratch.path()).expect("daemon pid");
    signal(old, "-KILL");
    wait_for("the old daemon to stop running", Duration::from_secs(10), || {
        !pid_running(old)
    });

    // The adapter re-rendezvouses (a call against the dead daemon fails first),
    // and a later call succeeds without a gateway restart.
    match client.wait_for_call("x__echo", "chaos", CATALOG) {
        Ok(_) => {}
        Err(error) => panic!(
            "the adapter never recovered from the hidden daemon: {error}\n{}",
            client.diagnostics()
        ),
    }

    // A brand-new client is served as well: the death is contained to the one
    // daemon, and the system elects or starts whatever it needs for the next one.
    let mut fresh = Client::start(scratch.path(), "b");
    assert!(
        fresh.wait_for_tool("x__echo", CATALOG),
        "a new client was not served after the daemon died\n{}",
        fresh.diagnostics()
    );
    match fresh.wait_for_call("x__echo", "chaos", CATALOG) {
        Ok(_) => {}
        Err(error) => panic!(
            "a new client could not call after the daemon died: {error}\n{}",
            fresh.diagnostics()
        ),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_killed_downstream_child_is_respawned() {
    use chaos_support::find_child;

    let scratch = Scratch::new("child-kill");
    write_registry(scratch.path(), &[mock_entry("x", &[])], &["x"]);
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("x__echo", CATALOG),
        "{}",
        client.diagnostics()
    );

    let daemon = daemon_pid(scratch.path()).expect("daemon pid");
    let child = find_child(daemon, "mock-mcp-server").expect("the mock child");
    signal(child, "-KILL");

    // The next calls fail while the pipe is dead, then the server is respawned
    // (or served from the adapter's own gateway) and answers again without a
    // gateway restart.
    match client.wait_for_call("x__echo", "chaos", CATALOG) {
        Ok(_) => {}
        Err(error) => panic!(
            "the killed child was never respawned: {error}\n{}",
            client.diagnostics()
        ),
    }
}

/// REL-07: a killed daemon must not leave an EOF-immune downstream server
/// running. #1021 gives each spawned server a parent-death signal and a child
/// ledger.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_daemon_does_not_leave_its_servers_running() {
    let scratch = Scratch::new("orphan");
    let pid_file = scratch.join("child.pid");
    let pid_file_arg = pid_file.to_string_lossy().into_owned();
    write_registry(
        scratch.path(),
        &[mock_entry(
            "x",
            &[
                ("MOCK_MCP_PID_FILE", &pid_file_arg),
                ("MOCK_MCP_IGNORE_EOF", "1"),
            ],
        )],
        &["x"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("x__echo", CATALOG),
        "{}",
        client.diagnostics()
    );
    let up = client.call("x__echo", json!({ "text": "up" }));
    assert!(
        chaos_support::reply_ok(&up, "up"),
        "the server should answer before the daemon dies: {up}"
    );

    wait_for("the server pid", Duration::from_secs(30), || {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|raw| raw.lines().next().and_then(|l| l.trim().parse::<u64>().ok()))
            .is_some()
    });
    let child: u64 = std::fs::read_to_string(&pid_file)
        .expect("read pid file")
        .lines()
        .next()
        .and_then(|line| line.trim().parse().ok())
        .expect("parse child pid");
    assert!(pid_alive(child), "the recorded child should be running");

    let daemon = daemon_pid(scratch.path()).expect("daemon pid");
    signal(daemon, "-KILL");
    // A zombie awaiting its reaper is gone for this purpose.
    wait_for("the orphan to be reaped", Duration::from_secs(15), || {
        !chaos_support::pid_running(child)
    });
}
