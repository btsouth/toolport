//! Any process that learns a secret scrubs old logs, and two processes never
//! rewrite the same log at once.
//!
//! The desktop apps (Tauri and GTK) and the CLI read secrets through the same
//! library the gateway does, and none of them makes a scrub call of its own: the
//! library starts it whenever a shipped process learns a new secret. This test
//! is such a process (the library built without `cfg(test)`), with no gateway
//! anywhere. It also starts two copies of itself that learn the same secret at
//! the same moment, so their scrubs race on one data directory. Runs on every CI
//! OS (the headless integration groups on Linux, macOS and Windows).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const SECRET: &str = "app-process-secret-4Rt9-value";
const CHILD_ENV: &str = "TOOLPORT_SCRUB_TEST_CHILD_DIR";

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn seed(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let registry = json!({
        "version": 2,
        "servers": [{
            "id": "app-srv", "name": "app-srv", "transport": "stdio", "command": "unused",
            "args": [], "env": [{ "key": "APP_TOKEN", "secret": true }],
            "source": "manual", "disabledTools": []
        }],
        "profiles": [{ "id": "default", "name": "Default", "enabledServerIds": ["app-srv"] }],
        "activeProfileId": "default"
    });
    std::fs::write(dir.join("registry.json"), registry.to_string()).unwrap();
    let rows: String = (0..500)
        .map(|i| json!({"ts":i,"server":"app-srv","ok":false,"error":format!("old build leaked {SECRET}")}).to_string() + "\n")
        .collect();
    std::fs::write(dir.join("audit.jsonl"), &rows).unwrap();
    std::fs::write(dir.join("gateway.log"), format!("line {SECRET}\n")).unwrap();
}

fn scrubbed(dir: &Path) -> bool {
    ["audit.jsonl", "gateway.log"].iter().all(|name| {
        std::fs::read_to_string(dir.join(name)).is_ok_and(|text| !text.contains(SECRET))
    })
}

/// Learn the secret the way the apps do (an environment override here, so no
/// keychain is touched), then wait until the logs are clean.
fn learn_and_wait(dir: &Path) {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    let _data = conduit_lib::registry::DataDirOverride::set(dir);
    std::env::set_var("TOOLPORT_SECRET_APP_TOKEN", SECRET);
    let value = conduit_lib::secrets::get_secret_result("app-srv", "APP_TOKEN").unwrap();
    assert_eq!(value.as_deref(), Some(SECRET));
    // The marker is written after the rewrite: a process that exits once the
    // logs are clean may never write it, and the next one scrubs again.
    wait_for("the background scrub", Duration::from_secs(30), || {
        scrubbed(dir)
            && std::fs::read_to_string(dir.join("secret-scrub-v1.json"))
                .is_ok_and(|marker| marker.contains("app-srv/APP_TOKEN"))
    });
}

#[test]
fn scrub_child() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        learn_and_wait(&PathBuf::from(dir));
    }
}

#[test]
fn a_process_that_only_reads_a_secret_scrubs_old_logs() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let root = std::env::temp_dir().join(format!("toolport-scrub-any-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    // This process: no gateway, no explicit scrub call.
    let alone = root.join("alone");
    seed(&alone);
    learn_and_wait(&alone);
    let audit = std::fs::read_to_string(alone.join("audit.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 500);
    assert!(audit.contains("old build leaked [secret:APP_TOKEN]"));

    // Two processes learning the same secret at once, racing on one directory.
    let shared = root.join("shared");
    seed(&shared);
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..2)
        .map(|_| {
            std::process::Command::new(&exe)
                .args(["--exact", "scrub_child", "--test-threads=1"])
                .env(CHILD_ENV, &shared)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let audit = std::fs::read_to_string(shared.join("audit.jsonl")).unwrap();
    assert!(!audit.contains(SECRET));
    assert_eq!(audit.lines().count(), 500, "no line lost or duplicated");
    assert!(audit
        .lines()
        .all(|l| serde_json::from_str::<Value>(l).is_ok()));
    let marker = std::fs::read_to_string(shared.join("secret-scrub-v1.json")).unwrap();
    assert_eq!(marker.matches("app-srv/APP_TOKEN").count(), 1, "{marker}");
    assert!(!marker.contains(SECRET));
    let _ = std::fs::remove_dir_all(&root);
}
