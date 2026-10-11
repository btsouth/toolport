//! Each serving mode sweeps an expired entry without another cache operation.
#![cfg(feature = "test-support")]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn check_mode(mode: &str) {
    let dir = Scratch(std::env::temp_dir().join(format!(
        "toolport-cache-maintenance-{}-{mode}",
        std::process::id()
    )));
    std::fs::create_dir_all(dir.path()).unwrap();
    let probe = dir.path().join("maintenance-complete");
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    command
        .env_clear()
        .envs(std::env::vars_os().filter(|(key, _)| {
            let key = key.to_string_lossy();
            !key.starts_with("TOOLPORT_") && !key.starts_with("CONDUIT_")
        }))
        .env("TOOLPORT_DATA_DIR", dir.path())
        .env("TOOLPORT_REGISTRY", dir.path().join("registry.json"))
        .env("TOOLPORT_TEST_CACHE_MAINTENANCE_PROBE", &probe)
        .env(
            "TOOLPORT_SECRET_KEY",
            "disposable-cache-maintenance-fixture",
        )
        .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", "60000")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::fs::write(
        dir.path().join("registry.json"),
        r#"{"version":3,"servers":[],"profiles":[]}"#,
    )
    .unwrap();
    match mode {
        "shared" => {
            command.arg("--daemon");
        }
        "private" => {
            command.arg("--private-gateway");
        }
        "http" => {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            command
                .args(["--http", &port.to_string()])
                .env("TOOLPORT_HTTP_TOKEN", "fixture-token");
        }
        // An explicit positional profile avoids the no-argument adapter path.
        "stdio" => {
            command.arg("fixture");
        }
        _ => unreachable!(),
    }
    let mut process = Process(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    while !probe.exists() {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "{mode} exited before maintenance"
        );
        assert!(
            Instant::now() < deadline,
            "{mode} never swept its expired entry"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(probe).unwrap(),
        "expired and trim consumed"
    );
    if mode == "private" || mode == "stdio" {
        drop(process.0.stdin.take());
        while process.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "{mode} did not exit after stdin closed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn shared_daemon_expires_without_cache_operations() {
    check_mode("shared");
}
#[test]
fn private_gateway_expires_without_cache_operations() {
    check_mode("private");
}
#[test]
fn direct_http_expires_without_cache_operations() {
    check_mode("http");
}
#[test]
fn standalone_stdio_expires_without_cache_operations() {
    check_mode("stdio");
}
