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
fn assert_fixture_cleanup(condition: bool, message: &str) {
    if !std::thread::panicking() {
        assert!(condition, "{message}");
    }
}

#[test]
fn reviewed_fixture_cleanup_preserves_original_panic() {
    struct Probe;
    impl Drop for Probe {
        fn drop(&mut self) {
            assert_fixture_cleanup(false, "cleanup failed");
        }
    }
    let panic = std::panic::catch_unwind(|| {
        let _probe = Probe;
        panic!("original verification failure");
    })
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original verification failure")
    );
}

impl Fixture {
    fn stop_daemons(&self) {
        if let Ok(files) = std::fs::read_dir(self.dir.join("data")) {
            for file in files.flatten() {
                let name = file.file_name();
                if name.to_string_lossy().starts_with("daemon-")
                    && file.path().extension().is_some_and(|e| e == "json")
                {
                    if let Some(descriptor) = conduit_lib::daemon::read_descriptor(&file.path()) {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        let running = || {
                            #[cfg(target_os = "linux")]
                            return std::fs::read_link(format!("/proc/{}/exe", descriptor.pid))
                                .is_ok();
                            #[cfg(not(target_os = "linux"))]
                            file.path().exists()
                        };
                        // The adapter deletes its session asynchronously after EOF.
                        // Keep the private data and descriptor until shutdown is accepted.
                        while running() && std::time::Instant::now() < deadline {
                            if file.path().exists() {
                                let _ = conduit_lib::daemon::request_shutdown_if_idle(&descriptor);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        #[cfg(target_os = "linux")]
                        if running() {
                            let image = std::fs::read_link(format!("/proc/{}/exe", descriptor.pid));
                            let private_image = std::env::current_exe()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .join("toolport-gateway");
                            let owned = image.is_ok_and(|image| image == private_image);
                            assert_fixture_cleanup(owned, "fixture daemon image changed");
                            if !owned {
                                continue;
                            }
                            // This daemon belongs to this disposable fixture. An active
                            // adapter listener can outlive its caller, so reap it before
                            // removing the executable or its private data.
                            let killed =
                                unsafe { libc::kill(descriptor.pid as i32, libc::SIGKILL) };
                            assert_fixture_cleanup(killed == 0, "could not reap fixture daemon");
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(10);
                            while std::fs::read_link(format!("/proc/{}/exe", descriptor.pid))
                                .is_ok()
                                && std::time::Instant::now() < deadline
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            if !std::thread::panicking() {
                                assert!(std::fs::read_link(format!(
                                    "/proc/{}/exe",
                                    descriptor.pid
                                ))
                                .is_err());
                            }
                        }
                        conduit_lib::daemon::clear_descriptor(&file.path());
                        assert_fixture_cleanup(
                            !running() && !file.path().exists(),
                            "private fixture daemon did not shut down",
                        );
                    }
                }
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_daemons();
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
fn run_private_fixture(test: &str) -> bool {
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
    let mock = std::env::var_os("TOOLPORT_REVIEWED_TEST_MOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mock-mcp-server")));
    std::fs::copy(
        mock,
        dir.join(format!("mock-mcp-server{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let status = std::process::Command::new(&child)
        .env("TOOLPORT_REVIEWED_CHILD", "1")
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .status()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(status.success(), "private cold-connect fixture failed");
    true
}

#[test]
fn reviewed_setup_real_gateway_and_failed_launch() {
    if run_private_fixture("reviewed_setup_real_gateway_and_failed_launch") {
        return;
    }
    let _lock = registry::data_dir_test_lock();
    let mut fixture = Fixture::new();
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
    // Imported subprocess values are available to both the app probe and gateway.
    registry::save(&registry::Registry::default()).unwrap();
    fixture.set(
        "TOOLPORT_SECRET_KEY",
        Some("synthetic-import-integration".into()),
    );
    let credential_config =
        json!({"mcpServers":{"secured":{"command":mock,"env":{"PAT":"synthetic-setup-pat"}}}})
            .to_string();
    std::fs::write(fixture.config(), &credential_config).unwrap();
    let result = controller::migrate_client("claude-code", None, false).unwrap();
    let saved = registry::load().unwrap();
    let entry = &saved.servers[0];
    assert_eq!(
        conduit_lib::secrets::get_vault_secret_result(&entry.id, "PAT")
            .unwrap()
            .as_deref(),
        Some("synthetic-setup-pat")
    );
    assert!(!serde_json::to_string(&saved)
        .unwrap()
        .contains("synthetic-setup-pat"));
    assert_eq!(result.moved, ["secured"]);
    controller::disconnect_client("claude-code").unwrap();

    // A credential-bearing URL is resolved only for the real HTTP transport.
    let http = HttpFixture::new();
    registry::save(&registry::Registry::default()).unwrap();
    std::fs::write(fixture.config(),json!({"mcpServers":{"remote":{"url":format!("{}?token=synthetic-url-key",http.url),"headers":{"Authorization":"Bearer synthetic-setup-pat"}}}}).to_string()).unwrap();
    let result = controller::migrate_client("claude-code", None, false).unwrap();
    assert_eq!(result.moved, ["remote"]);
    let saved = registry::load().unwrap();
    let entry = &saved.servers[0];
    assert_eq!(
        conduit_lib::secrets::get_vault_secret_result(
            &entry.id,
            conduit_lib::secrets::HTTP_AUTH_KEY
        )
        .unwrap()
        .as_deref(),
        Some("synthetic-setup-pat")
    );
    assert!(!serde_json::to_string(&saved)
        .unwrap()
        .contains("synthetic-url-key"));
    let mut connection = conduit_lib::remote::connect_remote(entry).unwrap();
    assert!(connection
        .call("echo", json!({"text":"verified"}))
        .unwrap()
        .to_string()
        .contains("verified"));
}

struct HttpFixture {
    child: std::process::Child,
    url: String,
}
impl HttpFixture {
    fn new() -> Self {
        use std::io::BufRead;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mock-mcp-server"))
            .env("MOCK_MCP_HTTP", "1")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .map(|_| line);
            let _ = sender.send(result);
        });
        let url = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .trim()
            .strip_prefix("MOCK_MCP_URL=")
            .unwrap()
            .to_string();
        Self { child, url }
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn reviewed_setup_waits_for_slow_first_catalog() {
    if run_private_fixture("reviewed_setup_waits_for_slow_first_catalog") {
        return;
    }
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let mock = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("mock-mcp-server");
    // A warm visible catalog keeps the client's initial lazy list instant. The
    // new server still has to publish its own first catalog during scoped search.
    std::fs::write(
        fixture.config(),
        json!({"mcpServers":{"warm":{"command":mock}}}).to_string(),
    )
    .unwrap();
    let warm = controller::preview_client_setup("claude-code").unwrap();
    migrate_fixture(&fixture, &["warm".into()], &warm.revision);
    fixture.stop_daemons();
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.config()).unwrap()).unwrap();
    config["mcpServers"]["slow"] = json!({"command":mock,"args":["--start-delay-ms=3000"]});
    std::fs::write(fixture.config(), config.to_string()).unwrap();
    let review = controller::preview_client_setup("claude-code").unwrap();
    let outcome = migrate_fixture(&fixture, &["slow".into()], &review.revision);
    assert!(outcome.servers[0].tool_count > 0);
    assert_eq!(
        conduit_lib::clients::discovery_capabilities("claude-code").cold_full_list_wait_ms,
        2_000
    );
}
