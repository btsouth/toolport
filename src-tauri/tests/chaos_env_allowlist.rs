//! P1.7 chaos: a spawned MCP server must not inherit the gateway's ambient
//! credentials. Tracked by `#1013`, which clears the child environment and
//! passes only an allowlist plus the server's own configured env.
//!
//! Unix only: the fixture is a `/bin/sh` wrapper, which needs no mock knob.

#![cfg(unix)]

mod chaos_support;

use std::time::Duration;

use chaos_support::{start_daemon_with_env, write_registry, Client, Scratch, MOCK};
use serde_json::json;

#[test]
#[ignore = "needs #1013"]
fn a_spawned_server_does_not_inherit_ambient_credentials() {
    let scratch = Scratch::new("env-allowlist");
    let dump = scratch.join("child-env.txt");
    // Snapshot this server process's own environment, then become the mock so
    // the gateway still completes its handshake.
    let script = format!("env > '{}'; exec '{}'", dump.display(), MOCK);
    let server = json!({
        "id": "wrapped",
        "name": "wrapped",
        "transport": "stdio",
        "command": "/bin/sh",
        "args": ["-c", script],
        "env": [{ "key": "CONFIGURED_KEY", "value": "present", "secret": false }],
        "source": "manual",
        "disabledTools": []
    });
    write_registry(scratch.path(), &[server], &["wrapped"], false);

    // The launching gateway itself carries an ambient credential.
    let _daemon = start_daemon_with_env(scratch.path(), &[("AMBIENT_SECRET", "leak")]);
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("wrapped__echo", Duration::from_secs(60)),
        "{}",
        client.diagnostics()
    );

    let dumped = scratch.read("child-env.txt");
    assert!(
        dumped.contains("CONFIGURED_KEY=present"),
        "the server's own configured env must still be passed"
    );
    assert!(
        !dumped.contains("AMBIENT_SECRET"),
        "the spawned server inherited an ambient credential"
    );
}
