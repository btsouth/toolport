use conduit_lib::rate_limits::{bind_data_dir, check_and_count, Cap};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn caps() -> Vec<Cap> {
    vec![Cap {
        id: "shared-day".into(),
        window: "day".into(),
        max_calls: 10_000,
        tool: None,
        unknown_fields: Default::default(),
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

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn signal(stream: &mut TcpStream, expected: u8) {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    assert_eq!(byte[0], expected);
}

#[test]
fn concurrent_gateway_processes_must_not_lose_rate_limit_increments() {
    if std::env::var("TOOLPORT_RL_CHILD").ok().as_deref() == Some("1") {
        let dir = PathBuf::from(std::env::var("TOOLPORT_RL_DIR").expect("TOOLPORT_RL_DIR"));
        let id = std::env::var("TOOLPORT_RL_CHILD_ID").expect("TOOLPORT_RL_CHILD_ID");
        bind_data_dir(&dir);
        let mut parent = TcpStream::connect(std::env::var("TOOLPORT_RL_PARENT").unwrap()).unwrap();
        parent.write_all(b"r").unwrap();
        signal(&mut parent, b'd');
        // A held lock is an intentional fail-closed denial, even below the cap.
        // Shorten this injected contention deadline, never the production one.
        std::env::set_var("TOOLPORT_LOCK_TIMEOUT_MS", "1");
        let error = check_and_count(&caps(), "srv", "echo").unwrap_err();
        assert!(
            error.starts_with("Toolport rate-limit counters are busy"),
            "{error}"
        );
        parent.write_all(b"d").unwrap();
        signal(&mut parent, b'g');
        std::env::remove_var("TOOLPORT_LOCK_TIMEOUT_MS");
        std::env::remove_var("CONDUIT_LOCK_TIMEOUT_MS");
        let mut accepted = 0;
        for _ in 0..25 {
            match check_and_count(&caps(), "srv", "echo") {
                Ok(()) => accepted += 1,
                Err(error) => assert!(
                    error.starts_with("Toolport rate-limit counters are busy"),
                    "unexpected denial: {error}"
                ),
            }
        }
        fs::write(dir.join(format!("accepted-{id}")), accepted.to_string()).unwrap();
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
    fs::write(dir.join("rate_limit_counters.json"), r#"{"counts":{}}"#).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let (connected_tx, connected_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..4 {
            if connected_tx.send(listener.accept()).is_err() {
                return;
            }
        }
    });
    let executable = std::env::current_exe().expect("current test executable");
    let mut children = Vec::new();
    let mut streams = Vec::new();
    for id in ["a", "b", "c", "d"] {
        children.push(Worker(
            Command::new(&executable)
                .env("TOOLPORT_RL_CHILD", "1")
                .env("TOOLPORT_RL_CHILD_ID", id)
                .env("TOOLPORT_RL_DIR", &dir)
                .env("TOOLPORT_RL_PARENT", &endpoint)
                .args([
                    "--exact",
                    "concurrent_gateway_processes_must_not_lose_rate_limit_increments",
                    "--nocapture",
                ])
                .spawn()
                .expect("spawn rate-limit child"),
        ));
    }
    for _ in 0..4 {
        let (mut stream, _) = connected_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("child did not rendezvous")
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        signal(&mut stream, b'r');
        streams.push(stream);
    }
    let lock = conduit_lib::registry::lock_at(&dir.join("rate_limit_counters.json")).unwrap();
    for stream in &mut streams {
        stream.write_all(b"d").unwrap();
    }
    for stream in &mut streams {
        signal(stream, b'd');
    }
    assert_eq!(counter_total(&dir), 0, "denied calls consumed budget");
    drop(lock);
    for stream in &mut streams {
        stream.write_all(b"g").unwrap();
    }
    for child in &mut children {
        let status = child.0.wait().expect("wait for rate-limit child");
        assert!(status.success(), "rate-limit child failed: {status}");
    }
    let accepted: u64 = ["a", "b", "c", "d"]
        .iter()
        .map(|id| {
            fs::read_to_string(dir.join(format!("accepted-{id}")))
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .sum();
    assert!(accepted > 0, "no concurrent writer made progress");
    assert_eq!(
        counter_total(&dir),
        accepted,
        "an accepted increment was lost"
    );
    let _ = fs::remove_dir_all(dir);
}
