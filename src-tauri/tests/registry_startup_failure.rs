//! Registry failures must stop serving before a gateway publishes a default
//! configuration. All registry, backup and client paths are temporary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use conduit_lib::registry::{self, Registry};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolport-registry-startup-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_gateway_refuses(dir: &Path, role: &str, expected: &str) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("TOOLPORT_") || name.starts_with("CONDUIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .arg(role)
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("XDG_DATA_HOME", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "{role} did not refuse startup: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{role}: {error}");
    assert!(error.contains(expected), "{role}: {error}");
    assert!(
        error.contains("Refusing to start serving tools"),
        "{role}: {error}"
    );
    assert!(
        !error.contains("serving cached tools only"),
        "{role}: {error}"
    );
    assert!(output.stdout.is_empty(), "no MCP response may be served");
    assert!(!dir.join("daemon.json").exists());
}

#[test]
fn lock_creation_failure_is_visible_and_never_serves_defaults() {
    let scratch = Scratch::new("lock");
    let path = scratch.0.join("registry.json");
    registry::save_to(&path, &Registry::default()).unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::create_dir(scratch.0.join("registry.json.lock")).unwrap();
    for role in ["--daemon", "--private-gateway", "--http"] {
        assert_gateway_refuses(&scratch.0, role, "writable TOOLPORT_DATA_DIR");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

// Windows directory read-only attributes do not prohibit lock creation.
// The portable lock-failure injection above still runs there.
#[cfg(unix)]
#[test]
fn read_only_directory_refuses_load_update_and_gateway_startup() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("read-only");
    let path = scratch.0.join("registry.json");
    registry::save_to(&path, &Registry::default()).unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = registry::load_from(&path).unwrap_err();
    assert!(error.contains("writable TOOLPORT_DATA_DIR"), "{error}");
    let mut called = false;
    assert!(registry::update_at(&path, |_| {
        called = true;
        Ok(())
    })
    .is_err());
    assert!(!called);
    for role in ["--daemon", "--private-gateway", "--http"] {
        assert_gateway_refuses(&scratch.0, role, "writable TOOLPORT_DATA_DIR");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!scratch.0.join("registry.json.lock").exists());
}

#[test]
fn corrupt_and_future_registries_refuse_startup_without_losing_bytes() {
    let scratch = Scratch::new("bad-document");
    let path = scratch.0.join("registry.json");
    std::fs::write(&path, "{ corrupt registry").unwrap();
    let corrupt = std::fs::read(&path).unwrap();
    assert_gateway_refuses(&scratch.0, "--daemon", "Corrupt registry");
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    let preserved: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|file| {
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("registry.json.unreadable-")
        })
        .collect();
    assert_eq!(preserved.len(), 1);
    let mut future = serde_json::to_value(Registry::default()).unwrap();
    future["version"] = serde_json::json!(Registry::default().version + 1);
    let future = serde_json::to_vec(&future).unwrap();
    std::fs::write(&path, &future).unwrap();
    assert_gateway_refuses(&scratch.0, "--daemon", "Update Toolport");
    assert_eq!(std::fs::read(&path).unwrap(), future);
    assert_eq!(std::fs::read(&preserved[0]).unwrap(), corrupt);
    assert!(!scratch.0.join("registry.json.bak").exists());
    let after: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|file| {
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("registry.json.unreadable-")
        })
        .collect();
    assert_eq!(
        after, preserved,
        "future schemas never create quarantine copies"
    );
}

#[test]
fn unreadable_bytes_never_default_or_overwrite_from_last_good() {
    let scratch = Scratch::new("unreadable-bytes");
    let path = scratch.0.join("registry.json");
    let original = [0xff, 0xfe, 0x80];
    std::fs::write(&path, original).unwrap();
    let backup = serde_json::to_vec(&Registry::default()).unwrap();
    std::fs::write(scratch.0.join("registry.json.bak"), &backup).unwrap();
    assert!(registry::load_from(&path)
        .unwrap_err()
        .contains("Could not read registry"));
    assert_gateway_refuses(&scratch.0, "--daemon", "Could not read registry");
    let error = registry::save_to(&path, &Registry::default()).unwrap_err();
    assert!(
        error.contains("Refusing to replace unreadable bytes"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::read(scratch.0.join("registry.json.bak")).unwrap(),
        backup
    );
}
