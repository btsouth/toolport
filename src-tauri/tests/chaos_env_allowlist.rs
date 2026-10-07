//! P1.7 chaos: a spawned MCP server must not inherit the gateway's ambient
//! credentials. #1013 clears the child environment and
//! passes only an allowlist plus the server's own configured env.
//!
//! Unix only: the fixture is a shell launcher script, which needs no mock knob.

#![cfg(unix)]

mod chaos_support;

use std::time::Duration;

use chaos_support::{start_daemon_with_env, write_registry, Client, Scratch, MOCK};
use serde_json::json;

#[test]
fn a_spawned_server_does_not_inherit_ambient_credentials() {
    let scratch = Scratch::new("env-allowlist");
    let dump = scratch.join("child-env.txt");
    // Snapshot this server process's own environment, then become the mock so
    // the gateway still completes its handshake. A launcher file, not `sh -c`:
    // the spawn guard refuses inline eval.
    let launcher = scratch.join("launcher.sh");
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\nenv > '{}'\nexec '{}'\n", dump.display(), MOCK),
    )
    .expect("write launcher");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .expect("make launcher executable");
    }
    let server = json!({
        "id": "wrapped",
        "name": "wrapped",
        "transport": "stdio",
        "command": launcher.display().to_string(),
        "args": [],
        "env": [{ "key": "CONFIGURED_KEY", "value": "present", "secret": false }],
        "source": "manual",
        "disabledTools": []
    });
    write_registry(scratch.path(), &[server], &["wrapped"]);

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

#[test]
fn inherit_env_uses_one_login_snapshot_until_registry_reload() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("login-env");
    let shell = scratch.join("login-shell");
    let count = scratch.join("shell-count");
    let value = scratch.join("login-value");
    std::fs::write(&value, "first").unwrap();
    std::fs::write(&shell, format!(
        "#!/bin/sh\n[ \"$1\" = -lc ] || exit 1\necho run >> '{}'\nprintf 'LOGIN_KEY=%s\\000PATH={}:/usr/bin:/bin\\000' \"$(cat '{}')\"\n", count.display(), scratch.path().display(), value.display()
    )).unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entries: Vec<_> = ["one", "two", "three"].iter().map(|id| {
        let launcher = scratch.join(&format!("{id}.sh"));
        std::fs::write(&launcher, format!("#!/bin/sh\nenv > '{}'\nexec '{}'\n", scratch.join(&format!("{id}.env")).display(), MOCK)).unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        json!({ "id": id, "name": id, "transport": "stdio", "command": format!("{id}.sh"), "args": [], "env": [], "inheritEnv": true, "source": "manual", "disabledTools": [] })
    }).collect();
    write_registry(scratch.path(), &entries[..2], &["one", "two"]);
    let _daemon = start_daemon_with_env(
        scratch.path(),
        &[
            ("SHELL", shell.to_str().unwrap()),
            ("AMBIENT_SECRET", "daemon-only"),
        ],
    );
    let mut client = Client::start(scratch.path(), "login");
    for id in ["one", "two"] {
        assert!(
            client.wait_for_tool(&format!("{id}__echo"), Duration::from_secs(60)),
            "{}",
            client.diagnostics()
        );
        let env = scratch.read(&format!("{id}.env"));
        assert!(env.contains("LOGIN_KEY=first"));
        assert!(!env.contains("AMBIENT_SECRET"));
    }
    assert_eq!(scratch.read("shell-count").lines().count(), 1);
    std::fs::write(&value, "second").unwrap();
    write_registry(scratch.path(), &entries, &["one", "two", "three"]);
    assert!(
        client.wait_for_tool("three__echo", Duration::from_secs(60)),
        "{}",
        client.diagnostics()
    );
    assert!(scratch.read("three.env").contains("LOGIN_KEY=second"));
    assert_eq!(scratch.read("shell-count").lines().count(), 2);
}
