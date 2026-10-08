//! Real gateway results and failure guarantees using disposable client configs.
use conduit_lib::{registry, registry_controller as controller};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    env: Vec<(String, Option<std::ffi::OsString>)>,
    _data: registry::DataDirOverride,
}
impl Fixture {
    fn new() -> Self {
        let gateway = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("toolport-gateway{}", std::env::consts::EXE_SUFFIX));
        let dir = std::env::temp_dir().join(format!(
            "toolport-reviewed-setup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("client")).unwrap();
        std::fs::create_dir_all(dir.join("data/bin")).unwrap();
        let mut fixture = Self {
            _data: registry::DataDirOverride::set(dir.join("data")),
            dir,
            env: vec![],
        };
        let overrides = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| key.starts_with("TOOLPORT_") || key.starts_with("CONDUIT_"))
            .collect::<Vec<_>>();
        for key in overrides {
            fixture.set(&key, None);
        }
        fixture.set(
            "CLAUDE_CONFIG_DIR",
            Some(fixture.dir.join("client").into_os_string()),
        );
        fixture.set("APPIMAGE", None);
        fixture.set(
            "TOOLPORT_DATA_DIR",
            Some(fixture.dir.join("data").into_os_string()),
        );
        std::fs::copy(
            &gateway,
            fixture.dir.join(format!(
                "data/bin/toolport-gateway{}",
                std::env::consts::EXE_SUFFIX
            )),
        )
        .unwrap();
        let mut registry = registry::Registry::default();
        registry.set_client_discovery("claude-code", Some("lazy"));
        registry::save(&registry).unwrap();
        fixture
    }
    fn set(&mut self, key: &str, value: Option<std::ffi::OsString>) {
        self.env.push((key.into(), std::env::var_os(key)));
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    fn config(&self) -> PathBuf {
        self.dir.join("client/.claude.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(files) = std::fs::read_dir(self.dir.join("data")) {
            for file in files.flatten() {
                let name = file.file_name();
                if name.to_string_lossy().starts_with("daemon-")
                    && file.path().extension().is_some_and(|e| e == "json")
                {
                    if let Some(descriptor) = conduit_lib::daemon::read_descriptor(&file.path()) {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        // The adapter deletes its session asynchronously after EOF.
                        // Keep the private data and descriptor until shutdown is accepted.
                        while file.path().exists() && std::time::Instant::now() < deadline {
                            let _ = conduit_lib::daemon::request_shutdown_if_idle(&descriptor);
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        #[cfg(target_os = "linux")]
                        if file.path().exists() {
                            let image = std::fs::read_link(format!("/proc/{}/exe", descriptor.pid));
                            let private_image = std::env::current_exe()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .join("toolport-gateway");
                            assert_eq!(image.as_deref(), Ok(private_image.as_path()));
                            // This daemon belongs to this disposable fixture. An active
                            // adapter listener can outlive its caller, so reap it before
                            // removing the executable or its private data.
                            assert_eq!(
                                unsafe { libc::kill(descriptor.pid as i32, libc::SIGKILL) },
                                0
                            );
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(10);
                            while std::fs::read_link(format!("/proc/{}/exe", descriptor.pid))
                                .is_ok()
                                && std::time::Instant::now() < deadline
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            assert!(std::fs::read_link(format!("/proc/{}/exe", descriptor.pid))
                                .is_err());
                            conduit_lib::daemon::clear_descriptor(&file.path());
                        }
                        assert!(
                            !file.path().exists(),
                            "private fixture daemon {} did not shut down",
                            descriptor.pid
                        );
                    }
                }
            }
        }
        for (key, value) in self.env.iter().rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn migrate_fixture(
    fixture: &Fixture,
    names: &[String],
    revision: &str,
) -> controller::MigrateOutcome {
    controller::migrate_client_reviewed("claude-code", None, false, names, revision)
        .expect("the cold first connect must succeed")
}

// Run the acceptance test beside private gateway and mock images. Both read-only
// lookup and config publication then exercise the normal packaged resolver.
fn run_private_fixture() -> bool {
    if std::env::var_os("TOOLPORT_REVIEWED_CHILD").is_some() {
        return false;
    }
    let dir = std::env::temp_dir().join(format!(
        "toolport-reviewed-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let child = dir.join(format!("reviewed-setup{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe().unwrap(), &child).unwrap();
    let gateway = std::env::var_os("TOOLPORT_REVIEWED_TEST_GATEWAY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_toolport-gateway")));
    std::fs::copy(
        gateway,
        dir.join(format!("toolport-gateway{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_mock-mcp-server"),
        dir.join(format!("mock-mcp-server{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let status = std::process::Command::new(&child)
        .env("TOOLPORT_REVIEWED_CHILD", "1")
        .args([
            "--exact",
            "reviewed_setup_real_gateway_and_failed_launch",
            "--nocapture",
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(status.success(), "private cold-connect fixture failed");
    true
}

#[test]
fn reviewed_setup_real_gateway_and_failed_launch() {
    if run_private_fixture() {
        return;
    }
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let original =
        json!({"mcpServers":{"broken":{"command":"/not-a-real-toolport-setup-command"}}})
            .to_string();
    std::fs::write(fixture.config(), &original).unwrap();
    let review = controller::preview_client_setup("claude-code").unwrap();
    assert!(controller::migrate_client_reviewed(
        "claude-code",
        None,
        false,
        &["broken".into()],
        &review.revision
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(fixture.config()).unwrap(), original);

    // Start again with an empty registry so the failed import cannot affect this cutover.
    let mut empty = registry::Registry::default();
    empty.set_client_discovery("claude-code", Some("lazy"));
    registry::save(&empty).unwrap();
    let mock_path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("mock-mcp-server{}", std::env::consts::EXE_SUFFIX));
    let mock = mock_path.to_str().unwrap();
    std::fs::write(fixture.config(),json!({"mcpServers":{"alpha":{"command":mock},"beta":{"command":mock},"kept":{"command":"native-only","env":{"PAT":"synthetic-native-secret"},"custom":true}}}).to_string()).unwrap();
    registry::update(|reg| {
        let mut unrelated: registry::ServerEntry = serde_json::from_value(json!({"id":"unrelated","name":"Unrelated","enabled":true,"transport":"stdio","command":"/not-a-real-unrelated-command","args":[],"env":[]})).unwrap();
        unrelated.enabled = true;
        reg.add_server(unrelated);
        Ok(())
    }).unwrap();
    let review = controller::preview_client_setup("claude-code").unwrap();
    let result = migrate_fixture(&fixture, &["alpha".into(), "beta".into()], &review.revision);
    assert_eq!(result.moved, ["alpha", "beta"]);
    assert_eq!(result.servers.len(), 2);
    assert!(result.servers.iter().all(|s| s.tool_count > 0));
    assert!(result
        .result
        .outcome
        .warnings
        .iter()
        .any(|w| w.contains("Unrelated")));
    // Claude Code's lazy discovery actually exposes gateway meta-tools to the agent.
    assert_eq!(
        result
            .tools
            .iter()
            .filter(|t| t["name"] == "toolport_search_tools")
            .count(),
        1
    );
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.config()).unwrap()).unwrap();
    assert!(config["mcpServers"].get("alpha").is_none());
    assert!(config["mcpServers"].get("beta").is_none());
    assert_eq!(
        config["mcpServers"]["kept"]["env"]["PAT"],
        "synthetic-native-secret"
    );
    assert_eq!(config["mcpServers"].as_object().unwrap().len(), 2);
    assert!(result.result.outcome.backup.is_some());
    controller::disconnect_client("claude-code").unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.config()).unwrap()).unwrap();
    assert_eq!(restored["mcpServers"]["alpha"]["command"], mock);
}
