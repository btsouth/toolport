//! Explicit early exits must leave their diagnostic evidence on disk.
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn newer_schema_exit_flushes_gateway_diagnostics() {
    let dir = std::env::temp_dir().join(format!("toolport-telemetry-exit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("registry.json"),
        r#"{"version":99,"servers":[],"profiles":[]}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .arg("--private-gateway")
        .env("TOOLPORT_DATA_DIR", &dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("gateway early exit exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(1));
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap();
    assert!(log.contains("load_resolved ERR (newer schema)"), "{log}");
    assert!(log.contains("role=private"), "{log}");
    std::fs::remove_dir_all(dir).unwrap();
}
