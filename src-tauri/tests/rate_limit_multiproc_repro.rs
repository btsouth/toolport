use conduit_lib::rate_limits::{bind_data_dir, check_and_count, Cap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn caps() -> Vec<Cap> {
    vec![Cap {
        id: "shared-day".into(),
        window: "day".into(),
        max_calls: 10_000,
        tool: None,
    }]
}

fn counter_total(dir: &Path) -> u64 {
    let raw = fs::read_to_string(dir.join("rate_limit_counters.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value["counts"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(serde_json::Value::as_u64)
        .sum()
}

fn wait_until(mut predicate: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {label}");
}

#[test]
fn concurrent_gateway_processes_must_not_lose_rate_limit_increments() {
    if std::env::var("TOOLPORT_RL_CHILD").ok().as_deref() == Some("1") {
        let dir = PathBuf::from(std::env::var("TOOLPORT_RL_DIR").expect("TOOLPORT_RL_DIR"));
        let id = std::env::var("TOOLPORT_RL_CHILD_ID").expect("TOOLPORT_RL_CHILD_ID");
        fs::write(dir.join(format!("ready-{id}")), b"1").unwrap();
        wait_until(|| dir.join("go").exists(), "parent start signal");
        bind_data_dir(&dir);
        for _ in 0..25 {
            check_and_count(&caps(), "srv", "echo").expect("call must remain under cap");
        }
        return;
    }

    let sequence = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "toolport-rate-limit-multiproc-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();

    let executable = std::env::current_exe().expect("current test executable");
    let mut children = Vec::new();
    for id in ["a", "b", "c", "d"] {
        children.push(
            Command::new(&executable)
                .env("TOOLPORT_RL_CHILD", "1")
                .env("TOOLPORT_RL_CHILD_ID", id)
                .env("TOOLPORT_RL_DIR", &dir)
                // Four processes racing one counter file is the point of this test, and
                // what it asserts is that no increment is LOST — not that every child
                // wins the lock inside the production budget. On a loaded runner the 5s
                // default expires, a child correctly gives up, `check_and_count` returns
                // Err, and the child panics on its `expect`. That is the machine's
                // timing, not the invariant (SBS-895).
                .env("TOOLPORT_LOCK_TIMEOUT_MS", "60000")
                .args([
                    "--exact",
                    "concurrent_gateway_processes_must_not_lose_rate_limit_increments",
                    "--nocapture",
                ])
                .spawn()
                .expect("spawn rate-limit child"),
        );
    }

    wait_until(
        || {
            ["a", "b", "c", "d"]
                .iter()
                .all(|id| dir.join(format!("ready-{id}")).exists())
        },
        "all children",
    );
    fs::write(dir.join("go"), b"1").unwrap();

    for mut child in children {
        let status = child.wait().expect("wait for rate-limit child");
        assert!(status.success(), "rate-limit child failed: {status}");
    }

    assert_eq!(counter_total(&dir), 100);
    let _ = fs::remove_dir_all(dir);
}
