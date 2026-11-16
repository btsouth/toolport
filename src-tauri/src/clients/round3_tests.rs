use super::*;

fn entry(command: &Path) -> ServerEntry {
    serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport",
        "transport":"stdio", "command":command.to_str().unwrap()}))
    .unwrap()
}

#[cfg(unix)]
#[test]
fn round3_legacy_linked_and_backup_profiles_keep_references_and_repair() {
    let data = crate::registry::DataDirTestEnv::new("round3-legacy-profiles");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let root = data.dir.join("home");
    std::fs::create_dir(&root).unwrap();
    let _home = TestHome::set(&root);
    let outside = data.dir.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".claude-work")).unwrap();
    std::fs::create_dir(root.join(".claude-old")).unwrap();
    std::fs::create_dir(root.join(".claude-backup-2026")).unwrap();
    let old = data.dir.join("bin/toolport-gateway-1.24.0");
    let current = data.dir.join("Toolport/bin/toolport-gateway-2.0.0");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    std::fs::write(&old, "old").unwrap();
    std::fs::write(&current, "new").unwrap();
    let raw = serde_json::json!({"mcpServers":{"my-toolport":{"command":old,"env":{"TOOLPORT_PROFILE":"work"}}}}).to_string();
    let paths: Vec<_> = [".claude-work", ".claude-old", ".claude-backup-2026"]
        .iter()
        .map(|folder| root.join(folder).join(".claude.json"))
        .collect();
    for path in &paths {
        std::fs::write(path, &raw).unwrap();
    }
    assert!(locations::all().is_empty());
    let references = referenced_gateway_paths().unwrap();
    assert!(references.contains(&old));
    cleanup_legacy_gateway_copies(&current, old.parent().unwrap(), Some(&references));
    assert!(old.exists());
    let mut outcome = RepointOutcome::default();
    repoint_other_claude_configs(current.to_str().unwrap(), &mut outcome);
    assert_eq!(outcome.extra_claude_repointed.len(), 3);
    for path in &paths {
        let value: serde_json::Value =
            serde_json::from_str(&read_config_file(path).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["toolport"]["command"],
            current.to_str().unwrap()
        );
        assert_eq!(
            value["mcpServers"]["toolport"]["env"]["TOOLPORT_PROFILE"],
            "work"
        );
        assert!(value["mcpServers"].get("my-toolport").is_none());
    }
    std::fs::write(&paths[0], "{broken").unwrap();
    assert!(referenced_gateway_paths().is_none());
    assert!(detect_clients()
        .iter()
        .any(|row| row.config_path == paths[0].to_string_lossy() && row.error.is_some()));
}

#[test]
fn round3_main_client_recognizes_repoints_and_deduplicates_custom_gateway_names() {
    let data = crate::registry::DataDirTestEnv::new("round3-main-dedupe");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let def = find_def("cursor").unwrap();
    let path = resolved_definition_path(&def).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"mcpServers":{"my-toolport":{"command":"/missing/toolport-gateway-1.24.0"},"another":{"command":"conduit-gateway"},"notes":{"command":"notes"}}}"#).unwrap();
    let mut rows = vec![read_client(&def)];
    apply_entry_states(&mut rows, &HashMap::new());
    assert!(rows[0].gateway_installed);
    assert_eq!(rows[0].entry_state, GatewayEntryState::Managed);
    let current = data.dir.join("toolport-gateway");
    std::fs::write(&current, "fixture").unwrap();
    let outcome = repoint_stale_gateways_in(current.to_str().unwrap(), rows, &HashMap::new());
    assert_eq!(outcome.repointed.len(), 1);
    let value: serde_json::Value = serde_json::from_str(&read_config_file(&path).unwrap()).unwrap();
    assert_eq!(value["mcpServers"].as_object().unwrap().len(), 2);
    assert_eq!(
        value["mcpServers"]["toolport"]["command"],
        current.to_str().unwrap()
    );
    uninstall_gateway("cursor").unwrap();
    assert!(!read_client(&def).gateway_installed);
}

#[test]
fn round3_missing_automatic_config_and_folder_are_absent_not_errors() {
    for remove_folder in [false, true] {
        let data = crate::registry::DataDirTestEnv::new("round3-missing-auto");
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&data.dir);
        let _home = TestHome::set(&data.dir);
        let folder = data.dir.join(".claude2");
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join(".claude.json");
        std::fs::write(&path, "{}").unwrap();
        detect_clients_for_display_in(
            machine_defs(true),
            crate::registry::Registry::default(),
            None,
        )
        .unwrap();
        let def = find_def("claude-code@claude2").unwrap();
        std::fs::remove_file(&path).unwrap();
        if remove_folder {
            std::fs::remove_dir(&folder).unwrap();
        }
        let row = read_client(&def);
        assert!(row.error.is_none(), "{:?}", row.error);
        assert!(!row.config_exists);
        assert!(row.location_removable);
        assert!(referenced_gateway_paths().is_some());
        remove_client_location("claude-code@claude2").unwrap();
        assert!(locations::saved().unwrap().is_empty());
    }
}

#[test]
fn round3_unreadable_store_blocks_pruning_and_bulk_disconnect_reports_error() {
    let data = crate::registry::DataDirTestEnv::new("round3-bad-store");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    crate::registry::save(&crate::registry::Registry::default()).unwrap();
    let folder = data.dir.join("external");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join(".claude.json"),
        r#"{"mcpServers":{"toolport":{"command":"/old/toolport-gateway"}}}"#,
    )
    .unwrap();
    add_client_location("claude-code", folder.to_str().unwrap()).unwrap();
    std::fs::write(data.dir.join("client-locations.json"), "{broken").unwrap();
    assert!(referenced_gateway_paths().is_none());
    assert!(detect_clients().iter().any(|row| row.error.is_some()));
    for dry_run in [true, false] {
        let rows = disconnect_all(dry_run).unwrap();
        assert!(rows.iter().any(|row| row.error.is_some()));
    }
}

#[test]
fn round3_reserved_duplicates_removed_in_all_formats_and_foreign_slots_preserved() {
    let data = crate::registry::DataDirTestEnv::new("round3-duplicates");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let gateway = entry(&data.dir.join("toolport-gateway"));
    for (index, (format, raw)) in [
        (Format::JsonMcpServers, r#"{"mcpServers":{"toolport":{"command":"toolport-gateway"},"conduit":{"command":"conduit-gateway"},"foreign":{"command":"toolport-gateway"}}}"#),
        (Format::JsonZCodeMcp, r#"{"mcp":{"servers":{"toolport":{"type":"stdio","command":"toolport-gateway"},"conduit":{"type":"stdio","command":"conduit-gateway"},"foreign":{"type":"stdio","command":"toolport-gateway"}}}}"#),
        (Format::JsonOpenClawMcp, r#"{"mcp":{"servers":{"toolport":{"command":"toolport-gateway"},"conduit":{"command":"conduit-gateway"},"foreign":{"command":"toolport-gateway"}}}}"#),
        (Format::JsonNanobotMcp, r#"{"tools":{"mcpServers":{"toolport":{"command":"toolport-gateway"},"conduit":{"command":"conduit-gateway"},"foreign":{"command":"toolport-gateway"}}}}"#),
        (Format::JsonOpenHandsMcpServers, r#"{"mcpServers":{"toolport":{"command":"toolport-gateway"},"conduit":{"command":"conduit-gateway"},"foreign":{"command":"toolport-gateway"}}}"#),
        (Format::JsonOpenCodeMcp, r#"{"mcp":{"toolport":{"type":"local","command":["toolport-gateway"]},"conduit":{"type":"local","command":["conduit-gateway"]},"foreign":{"type":"local","command":["toolport-gateway"]}}}"#),
        (Format::TomlMcpServers, "[mcp_servers.toolport]\ncommand='toolport-gateway'\n[mcp_servers.conduit]\ncommand='conduit-gateway'\n[mcp_servers.foreign]\ncommand='toolport-gateway'\n"),
        (Format::YamlExtensions, "extensions:\n  toolport: {type: stdio, cmd: toolport-gateway, enabled: true}\n  conduit: {type: stdio, cmd: conduit-gateway, enabled: true}\n  foreign: {type: stdio, cmd: toolport-gateway, enabled: true}\n"),
        (Format::YamlMcpServers, "mcp_servers:\n  toolport: {command: toolport-gateway}\n  conduit: {command: conduit-gateway}\n  foreign: {command: toolport-gateway}\n"),
        (Format::YamlMcpServersList, "mcpServers:\n  - {name: toolport, command: toolport-gateway}\n  - {name: conduit, command: conduit-gateway}\n  - {name: foreign, command: toolport-gateway}\n"),
        (Format::JsonCodewhaleServers, r#"{"servers":{"toolport":{"command":"toolport-gateway"},"conduit":{"command":"conduit-gateway"},"foreign":{"command":"toolport-gateway"}}}"#),
        (Format::TomlReasonixPlugins, "[[plugins]]\nname = 'toolport'\ncommand = 'toolport-gateway'\n[[plugins]]\nname = 'conduit'\ncommand = 'conduit-gateway'\n[[plugins]]\nname = 'foreign'\ncommand = 'toolport-gateway'\n"),
        (Format::YamlDshPatch, "- insert:\n    - id: mcp-toolport\n      name: '@deepseek-ai/dsh-mcp-client'\n      config: {serverName: toolport, transport: stdio, command: toolport-gateway}\n    - id: mcp-conduit\n      name: '@deepseek-ai/dsh-mcp-client'\n      config: {serverName: conduit, transport: stdio, command: conduit-gateway}\n    - id: mcp-foreign\n      name: '@deepseek-ai/dsh-mcp-client'\n      config: {serverName: foreign, transport: stdio, command: toolport-gateway}\n"),
    ].into_iter().enumerate() {
        let path = data.dir.join(format!("format-{index}/config/native.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, raw).unwrap();
        edit_format(format, &path, Some(&gateway), true).unwrap();
        let rows = parse_client_content(format, &read_config_file(&path).unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.name == "foreign"));
        edit_format(format, &path, None, true).unwrap();
        let rows = parse_client_content(format, &read_config_file(&path).unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "foreign");
        assert!(owned_gateway_entry(&rows).is_none());
        std::fs::write(&path, raw).unwrap();
        mutation::run("fixture", &path, format, || edit_format(format, &path, Some(&gateway), true)).unwrap();
        let rows = parse_client_content(format, &read_config_file(&path).unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "toolport");
    }
}

#[test]
fn round3_configured_connected_location_reclaims_main_identity() {
    let data = crate::registry::DataDirTestEnv::new("round3-main-adoption");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let folder = data.dir.join(".claude2");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join(".claude.json"), "{}").unwrap();
    let location = machine_defs(true)
        .into_iter()
        .find(|def| def.client_id() == "claude-code@claude2")
        .unwrap();
    locations::remember(location.location.as_ref().unwrap()).unwrap();
    let current = data.dir.join("toolport-gateway");
    std::fs::write(&current, "fixture").unwrap();
    let mut gateway = entry(&current);
    gateway.env.push(crate::registry::EnvVar {
        key: crate::brand::CLIENT_ID.into(),
        value: Some("claude-code@claude2".into()),
        secret: false,
        unknown_fields: Default::default(),
    });
    install_or_remove("claude-code@claude2", Some(&gateway)).unwrap();
    let _override = EnvRestore::set("CLAUDE_CONFIG_DIR", &folder);
    let rows = detect_clients_for_display_in(
        machine_defs(true),
        crate::registry::Registry::default(),
        Some(current.clone()),
    )
    .unwrap();
    let main = rows.iter().find(|row| row.id == "claude-code").unwrap();
    assert!(main.error.is_none());
    let outcome = repoint_stale_gateways_in(
        current.to_str().unwrap(),
        vec![main.clone()],
        &HashMap::new(),
    );
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let value: serde_json::Value =
        serde_json::from_str(&read_config_file(&folder.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["toolport"]["env"][crate::brand::CLIENT_ID],
        "claude-code"
    );
    assert_eq!(
        value["mcpServers"]["toolport"]["env"][crate::brand::ATTRIBUTION_ID],
        "claude-code"
    );
    assert!(!rows.iter().any(|row| row.id == "claude-code@claude2"));
}

#[cfg(windows)]
#[test]
fn round3_windows_config_write_survives_native_canonicalization_failure() {
    let data = crate::registry::DataDirTestEnv::new("round3-windows-config");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let path = data.dir.join("config.json");
    std::fs::write(&path, "{}").unwrap();
    crate::registry::client_file::with_native_canonicalize_failure(|| {
        let revision = crate::registry::client_file::read(&path).unwrap();
        crate::registry::client_file::commit(&path, &revision, Some("{\"saved\":true}")).unwrap();
        let missing = data.dir.join("new.json");
        let revision = crate::registry::client_file::read(&missing).unwrap();
        crate::registry::client_file::commit(&missing, &revision, Some("{}")).unwrap();
        assert_eq!(std::fs::read_to_string(&missing).unwrap(), "{}");
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"saved\":true}");
}

#[cfg(windows)]
#[test]
fn round3_windows_backup_acl_survives_native_canonicalization_failure() {
    let data = crate::registry::DataDirTestEnv::new("round3-windows-acl");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    crate::registry::client_file::with_native_canonicalize_failure(|| {
        backup_permissions::secure(&data.dir).unwrap();
    });
}

#[test]
fn round3_unreadable_config_and_home_scan_fail_closed() {
    let data = crate::registry::DataDirTestEnv::new("round3-unreadable");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let root = data.dir.join("home");
    std::fs::create_dir(&root).unwrap();
    let _home = TestHome::set(&root);
    let config = root.join(".claude.json");
    std::fs::create_dir(&config).unwrap();
    let row = read_client(&find_def("claude-code").unwrap());
    assert!(row.error.is_some());
    assert!(referenced_gateway_paths().is_none());
    std::fs::remove_dir(&config).unwrap();
    std::fs::remove_dir(&root).unwrap();
    std::fs::write(&root, "not a directory").unwrap();
    assert!(referenced_gateway_paths().is_none());
}

#[cfg(windows)]
#[test]
fn round3_windows_long_native_path_falls_back_when_canonicalization_fails() {
    use std::os::windows::ffi::OsStrExt;
    let data = crate::registry::DataDirTestEnv::new("round3-windows-long-fallback");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let path = data
        .dir
        .join("x".repeat(200))
        .join("y".repeat(100))
        .join("config.json");
    crate::registry::client_file::with_native_canonicalize_failure(|| {
        let native = crate::registry::client_file::native_path(&path).unwrap();
        assert_eq!(
            native,
            path.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        );
    });
}

#[test]
fn round3_location_disconnect_removes_both_reserved_names_after_adoption() {
    let data = crate::registry::DataDirTestEnv::new("round3-reserved-disconnect");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let folder = data.dir.join("profile");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join(".claude.json");
    let original = serde_json::json!({"mcpServers": {
        "toolport":{"command":"/missing/toolport-gateway"},
        "conduit":{"command":"/missing/conduit-gateway"},
        "foreign":{"command":"/missing/toolport-gateway", "args":["--foreign"]}
    }});
    std::fs::write(&path, original.to_string()).unwrap();
    let id = add_client_location("claude-code", folder.to_str().unwrap()).unwrap();
    let gateway = data.dir.join("toolport-gateway");
    std::fs::write(&gateway, "fixture").unwrap();
    let rows = detect_clients_for_display_in(
        vec![find_def(&id).unwrap()],
        crate::registry::Registry::default(),
        Some(gateway),
    )
    .unwrap();
    assert!(rows[0].error.is_none());
    let value: serde_json::Value = serde_json::from_str(&read_config_file(&path).unwrap()).unwrap();
    assert_eq!(value["mcpServers"].as_object().unwrap().len(), 2);
    assert_eq!(
        value["mcpServers"]["foreign"],
        original["mcpServers"]["foreign"]
    );
    uninstall_gateway(&id).unwrap();
    assert!(!read_client(&find_def(&id).unwrap()).gateway_installed);
    let value: serde_json::Value = serde_json::from_str(&read_config_file(&path).unwrap()).unwrap();
    assert_eq!(value["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(
        value["mcpServers"]["foreign"],
        original["mcpServers"]["foreign"]
    );
}

/// CodeRabbit: a secondary Claude Code config whose repair needs a backup the
/// locked keyring refuses waits for the keyring instead of failing, so the
/// start-up waiter retries it.
#[test]
fn a_locked_secondary_claude_repair_waits_for_the_keyring() {
    let data = crate::registry::DataDirTestEnv::new("round3-locked-secondary");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let root = data.dir.join("home");
    std::fs::create_dir(&root).unwrap();
    let _home = TestHome::set(&root);
    let old = data.dir.join("bin/toolport-gateway-1.24.0");
    let current = data.dir.join("Toolport/bin/toolport-gateway-2.0.0");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    std::fs::write(&old, "old").unwrap();
    std::fs::write(&current, "new").unwrap();
    // A Toolport server stores the inline value, so the backup needs the vault.
    let registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_KEY","secret":true}]}]})).unwrap();
    crate::registry::save(&registry).unwrap();
    let path = root.join(".claude-work").join(".claude.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let raw = serde_json::json!({"mcpServers":{
        "toolport":{"command":old,"env":{"TOOLPORT_PROFILE":"work"}},
        "native":{"command":"native","env":{"API_KEY":"sk-secondary-locked-fixture"}}
    }})
    .to_string();
    std::fs::write(&path, &raw).unwrap();
    let mut outcome = RepointOutcome::default();
    backup_vault::fake::locked_store(backup_vault::Unlock::StillLocked, || {
        backup_vault::noninteractive(|| {
            repoint_other_claude_configs_in(
                current.to_str().unwrap(),
                std::slice::from_ref(&path),
                &mut outcome,
            )
        });
    });
    assert!(
        outcome.extra_claude_failed.is_empty(),
        "{:?}",
        outcome.extra_claude_failed
    );
    assert_eq!(outcome.deferred.len(), 1, "{:?}", outcome.deferred);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        raw,
        "nothing written"
    );
}
