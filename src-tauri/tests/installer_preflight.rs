//! Real process inventory: client sessions survive deferral, foreign paths survive
//! idle installation, and authenticated idle daemons can exit without a kill.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct TempDir(PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "toolport-installer-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Gateway(Child);
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(binary: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env("TOOLPORT_DATA_DIR", data)
        .env("TOOLPORT_NO_KEYRING", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

fn copied_gateway(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("toolport-gateway{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_toolport-gateway"), &path).unwrap();
    path
}

#[test]
fn busy_client_defers_and_foreign_gateway_is_never_stopped() {
    let temp = TempDir::new("busy");
    let installed = copied_gateway(temp.path(), "install");
    let foreign = copied_gateway(temp.path(), "foreign");
    let data = temp.path().join("data");
    let mut client = Gateway(
        command(&installed, &data)
            .arg("fixture-direct-stdio")
            .spawn()
            .unwrap(),
    );
    let mut other = Gateway(
        command(&foreign, &temp.path().join("other-data"))
            .arg("fixture-direct-stdio")
            .spawn()
            .unwrap(),
    );
    // A bare positional fixture argument selects direct stdio, avoiding the
    // default shared-host adapter role without changing normal CLI behavior.
    // The OS process handle exists before preflight inventories it. There is no
    // need to wait for gateway startup: an open stdio process is already a veto.
    let output = command(Path::new(env!("CARGO_BIN_EXE_toolport-gateway")), &data)
        .args([
            "--installer-preflight",
            installed.parent().unwrap().to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("cancel to defer"));
    assert!(client.0.try_wait().unwrap().is_none());
    assert!(other.0.try_wait().unwrap().is_none());
    client.0.kill().unwrap();
    client.0.wait().unwrap();
    let status = command(Path::new(env!("CARGO_BIN_EXE_toolport-gateway")), &data)
        .args([
            "--installer-preflight",
            installed.parent().unwrap().to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
    assert!(other.0.try_wait().unwrap().is_none());
}

#[test]
fn idle_daemon_exits_gracefully_before_installation() {
    let temp = TempDir::new("idle");
    let installed = copied_gateway(temp.path(), "install");
    let data = temp.path().join("data");
    let mut daemon = Gateway(command(&installed, &data).arg("--daemon").spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(15);
    let descriptor = loop {
        let descriptor = std::fs::read_dir(&data)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .find_map(|entry| {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("daemon-") {
                    return None;
                }
                conduit_lib::daemon::read_descriptor(&entry.path())
            });
        if let Some(descriptor) = descriptor {
            break descriptor;
        }
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited before advertising"
        );
        assert!(Instant::now() < deadline, "daemon did not advertise");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(conduit_lib::daemon::probe_identity(&descriptor).is_ok());
    let status = command(Path::new(env!("CARGO_BIN_EXE_toolport-gateway")), &data)
        .args([
            "--installer-preflight",
            installed.parent().unwrap().to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "idle shutdown must not terminate by signal"
            );
            break;
        }
        assert!(Instant::now() < deadline, "daemon remained alive");
        std::thread::sleep(Duration::from_millis(20));
    }
}
