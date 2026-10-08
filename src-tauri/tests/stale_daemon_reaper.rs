//! Real process/descriptor regression for an upgrade between 2.x previews.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use conduit_lib::{daemon, gateway_publish, registry, topology::CompatKey};

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// The integration test executable doubles as a fake daemon with the real
/// descriptor and authenticated shutdown contract. No Python/compiler needed.
#[test]
fn fake_daemon_fixture() {
    let Ok(version) = std::env::var("TOOLPORT_REAPER_FIXTURE_VERSION") else {
        return;
    };
    let dir = registry::conduit_dir().unwrap();
    let compat = CompatKey::new(&version, dir.display().to_string());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let descriptor = daemon::DaemonDescriptor::new(
        listener.local_addr().unwrap().to_string(),
        "fixture-token",
        &compat,
    );
    let path = daemon::descriptor_path(&dir, &compat);
    daemon::write_descriptor(&path, &descriptor).unwrap();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|chunk| chunk == b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.contains("Bearer fixture-token"));
        let shutdown = request.starts_with(&format!("POST {} ", daemon::SHUTDOWN_IF_IDLE_PATH));
        let body = serde_json::json!({"pid": descriptor.pid, "compat": descriptor.compat,
            "protocol": descriptor.protocol, "gatewayVersion": version})
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        if shutdown {
            std::fs::write(dir.join(format!("graceful-{}", descriptor.pid)), "shutdown").unwrap();
            daemon::clear_descriptor(&path);
            break;
        }
    }
}

fn spawn(exe: &Path, dir: &Path, version: &str) -> Child {
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(dir).unwrap();
    let mut child = Command::new(exe)
        .arg0("--daemon")
        .args(["--exact", "fake_daemon_fixture", "--nocapture"])
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REAPER_FIXTURE_VERSION", version)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let compat = CompatKey::new(version, dir.display().to_string());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(descriptor) = daemon::read_descriptor(&daemon::descriptor_path(dir, &compat)) {
            if daemon::probe_identity(&descriptor).is_ok() {
                return child;
            }
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "fixture exited before readiness"
        );
        assert!(Instant::now() < deadline, "fixture readiness deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn reaper_stops_older_preview_gracefully_and_keeps_current_and_foreign_data_dir() {
    let data = registry::DataDirTestEnv::new("stale-daemon-reaper");
    let dir = registry::conduit_dir().unwrap();
    let exe = dir.join("bin/toolport-gateway");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    let foreign = dir.join("foreign");
    let mut children = Children(vec![
        spawn(&exe, &dir, "2.0.0-preview.1"),
        spawn(&exe, &dir, env!("CARGO_PKG_VERSION")),
        spawn(&exe, &foreign, "1.24.0"),
    ]);
    let old = children.0[0].id();
    let report = gateway_publish::reap_stale(&[exe]);
    assert!(
        report.failed.is_empty() && report.remaining.is_empty(),
        "{report:?}"
    );
    assert_eq!(report.killed.len(), 1, "{report:?}");
    assert!(
        children.0[0].wait().unwrap().success(),
        "old daemon must exit gracefully"
    );
    assert!(dir.join(format!("graceful-{old}")).exists());
    assert!(
        children.0[1].try_wait().unwrap().is_none(),
        "current daemon stopped"
    );
    assert!(
        children.0[2].try_wait().unwrap().is_none(),
        "foreign data dir stopped"
    );
    drop(children);
    drop(data);
}
