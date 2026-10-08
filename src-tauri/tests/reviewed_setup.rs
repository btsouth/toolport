//! Real gateway results and failure guarantees using disposable client configs.
use conduit_lib::{registry, registry_controller as controller};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    sidecar: Option<PathBuf>,
    env: Vec<(String, Option<std::ffi::OsString>)>,
    _data: registry::DataDirOverride,
}
impl Fixture {
    fn new() -> Self {
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
            sidecar: None,
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
        let sidecar = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("toolport-gateway");
        if !sidecar.exists() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_toolport-gateway"), &sidecar).unwrap();
            #[cfg(not(unix))]
            std::fs::copy(env!("CARGO_BIN_EXE_toolport-gateway"), &sidecar).unwrap();
            fixture.sidecar = Some(sidecar);
        }
        fixture.set(
            "TOOLPORT_DATA_DIR",
            Some(fixture.dir.join("data").into_os_string()),
        );
        std::fs::copy(
            env!("CARGO_BIN_EXE_toolport-gateway"),
            fixture.dir.join("data/bin/toolport-gateway"),
        )
        .unwrap();
        registry::save(&registry::Registry::default()).unwrap();
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
        for (key, value) in self.env.iter().rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if let Some(sidecar) = &self.sidecar {
            let _ = std::fs::remove_file(sidecar);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn migrate_fixture(
    fixture: &Fixture,
    names: &[String],
    revision: &str,
) -> controller::MigrateOutcome {
    let original = std::fs::read_to_string(fixture.config()).unwrap();
    let attempt = controller::migrate_client_reviewed("claude-code", None, false, names, revision);
    if let Err(error) = &attempt {
        assert!(error.contains("no verified gateway tools"), "{error}");
        assert_eq!(std::fs::read_to_string(fixture.config()).unwrap(), original);
        let keys = controller::preview_client_imports()
            .unwrap()
            .into_iter()
            .filter(|candidate| names.contains(&candidate.name))
            .map(|candidate| candidate.key)
            .collect();
        controller::import_client_servers(keys).unwrap();
        // Synchronize with real lazy discovery through a harmless fixture tool.
        // No timed sleeps or arbitrary retry loop is needed for the cold catalog.
        registry::update(|reg| {
            reg.safety_level = Some(registry::SafetyLevel::Off);
            reg.human_approval = false;
            Ok(())
        })
        .unwrap();
        let mut env = vec![(
            "TOOLPORT_DATA_DIR".into(),
            fixture.dir.join("data").display().to_string(),
        )];
        if let Ok(key) = std::env::var("TOOLPORT_SECRET_KEY") {
            env.push(("TOOLPORT_SECRET_KEY".into(), key));
        }
        let transport = conduit_lib::downstream::StdioTransport::spawn(
            env!("CARGO_BIN_EXE_toolport-gateway"),
            &[],
            &env,
            None,
            false,
        )
        .unwrap();
        let mut gateway = conduit_lib::downstream::DownstreamServer::connect(
            "fixture-agent".into(),
            Box::new(transport),
        )
        .unwrap();
        for name in names {
            let response = gateway
                .call(
                    "toolport_call_tool",
                    json!({"name":format!("{name}__echo"),"arguments":{"text":"verified"}}),
                )
                .unwrap();
            assert_ne!(response["isError"], true, "{response}");
            assert!(response.to_string().contains("verified"), "{response}");
        }
        return controller::migrate_client_reviewed("claude-code", None, false, names, revision)
            .unwrap();
    }
    attempt.unwrap()
}

#[test]
fn reviewed_setup_real_gateway_and_failed_launch() {
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let original =
        json!({"mcpServers":{"broken":{"command":"/not-a-real-toolport-setup-command"}}})
            .to_string();
    std::fs::write(fixture.config(), &original).unwrap();
    assert!(controller::migrate_client("claude-code", None, false).is_err());
    assert_eq!(std::fs::read_to_string(fixture.config()).unwrap(), original);

    // Start again with an empty registry so the failed import cannot affect this cutover.
    registry::save(&registry::Registry::default()).unwrap();
    let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
    std::fs::write(fixture.config(),json!({"mcpServers":{"alpha":{"command":mock},"beta":{"command":mock},"kept":{"command":"native-only","env":{"PAT":"synthetic-native-secret"},"custom":true}}}).to_string()).unwrap();
    let review = controller::preview_client_setup("claude-code").unwrap();
    let result = migrate_fixture(&fixture, &["alpha".into(), "beta".into()], &review.revision);
    assert_eq!(result.moved, ["alpha", "beta"]);
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
