use super::*;

/// One Toolport launch: detect Codex, run the startup repair, then persist the
/// ownership records it returns the way the desktop and GTK shells do.
fn launch(current: &str, managed: &mut HashMap<String, ManagedEntry>) -> RepointOutcome {
    let def = find_def("codex").unwrap();
    let mut rows = vec![read_client(&def)];
    apply_entry_states(&mut rows, managed);
    let outcome = repoint_stale_gateways_in(current, rows, managed);
    for (id, entry) in &outcome.repointed {
        managed.insert(id.clone(), entry.clone());
    }
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    outcome
}

fn codex_entry(path: &Path) -> toml::Table {
    let doc: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["mcp_servers"]["toolport"].as_table().unwrap().clone()
}

fn without_line(path: &Path, key: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with(key))
        .collect();
    std::fs::write(path, kept.join("\n") + "\n").unwrap();
}

fn codex_config(data: &crate::registry::DataDirTestEnv) -> PathBuf {
    let path = resolved_definition_path(&find_def("codex").unwrap()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let current = data.dir.join("toolport-gateway");
    std::fs::write(&current, "fixture").unwrap();
    path
}

#[test]
fn codex_settings_come_once_and_a_deleted_key_stays_deleted() {
    let data = crate::registry::DataDirTestEnv::new("codex-settings-once");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let path = codex_config(&data);
    let current = data.dir.join("toolport-gateway");
    let current = current.to_str().unwrap();

    // An install from before Toolport wrote these settings gets all three, once.
    std::fs::write(
        &path,
        format!(
            "model = \"gpt-5\"\n\n[mcp_servers.toolport]\ncommand = '{current}'\n\
             enabled_tools = [\"a\"] # mine\n\n\
             [mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = \"codex\"\n\
             TOOLPORT_ATTRIBUTION_ID = \"codex\"\n"
        ),
    )
    .unwrap();
    let mut managed = HashMap::new();
    launch(current, &mut managed);
    let entry = codex_entry(&path);
    assert_eq!(entry["supports_parallel_tool_calls"].as_bool(), Some(true));
    assert_eq!(entry["startup_timeout_sec"].as_integer(), Some(75));
    assert_eq!(entry["tool_timeout_sec"].as_integer(), Some(600));
    assert_eq!(entry["enabled_tools"][0].as_str(), Some("a"));
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("enabled_tools = [\"a\"] # mine"));
    let settled = std::fs::read_to_string(&path).unwrap();
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), settled);

    // The user turns parallel calls off by deleting the key: it stays deleted on
    // every later launch, and so does a timeout they remove.
    without_line(&path, "supports_parallel_tool_calls");
    without_line(&path, "tool_timeout_sec");
    let edited = std::fs::read_to_string(&path).unwrap();
    for _ in 0..2 {
        launch(current, &mut managed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    }
}

#[test]
fn a_fresh_codex_install_gets_every_setting_and_keeps_deletions() {
    let data = crate::registry::DataDirTestEnv::new("codex-settings-fresh");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let path = codex_config(&data);
    let current = data.dir.join("toolport-gateway");
    let current = current.to_str().unwrap();
    std::fs::write(&path, "model = \"gpt-5\"\n").unwrap();

    let mut fresh = gateway_entry(None, "codex").unwrap();
    fresh.command = Some(current.to_string());
    let write = install_or_remove("codex", Some(&fresh)).unwrap();
    let mut managed = HashMap::from([("codex".to_string(), write.managed.unwrap())]);
    let entry = codex_entry(&path);
    for key in [
        "supports_parallel_tool_calls",
        "startup_timeout_sec",
        "tool_timeout_sec",
    ] {
        assert!(entry.contains_key(key), "fresh install lacks {key}");
    }
    // The first launch backfills the attribution env; the settings are untouched.
    launch(current, &mut managed);
    assert_eq!(
        codex_entry(&path)["startup_timeout_sec"].as_integer(),
        Some(75)
    );
    without_line(&path, "startup_timeout_sec");
    let edited = std::fs::read_to_string(&path).unwrap();
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn settings_the_user_set_before_count_as_handled() {
    let data = crate::registry::DataDirTestEnv::new("codex-settings-user-set");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let path = codex_config(&data);
    let current = data.dir.join("toolport-gateway");
    let current = current.to_str().unwrap();
    std::fs::write(
        &path,
        format!(
            "[mcp_servers.toolport]\ncommand = '{current}'\n\
             supports_parallel_tool_calls = false\nstartup_timeout_ms = 3000\n\
             tool_timeout_sec = 90\n\n\
             [mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = \"codex\"\n\
             TOOLPORT_ATTRIBUTION_ID = \"codex\"\n"
        ),
    )
    .unwrap();
    // The record an earlier release wrote, before Toolport tracked these settings.
    let def = find_def("codex").unwrap();
    let server = read_client(&def).servers.remove(0);
    let record: ManagedEntry = serde_json::from_value(serde_json::json!({
        "command": current,
        "args": server.args,
        "env": server.env_keys.iter().map(|key| (key.clone(), "codex")).collect::<HashMap<_, _>>(),
        "updatedAt": 1
    }))
    .unwrap();
    let mut managed = HashMap::from([("codex".to_string(), record)]);
    let original = std::fs::read_to_string(&path).unwrap();
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    without_line(&path, "supports_parallel_tool_calls");
    let edited = std::fs::read_to_string(&path).unwrap();
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn a_shared_http_codex_entry_gets_its_absent_settings_in_place() {
    let data = crate::registry::DataDirTestEnv::new("codex-settings-http");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    let path = codex_config(&data);
    let current = data.dir.join("toolport-gateway");
    let current = current.to_str().unwrap();
    let url = "http://127.0.0.1:8765/mcp";
    let original = format!(
        "model = \"gpt-5\"\n\n[mcp_servers.toolport]\nurl = \"{url}\"\n\
         bearer_token_env_var = \"TOOLPORT_TOKEN\" # mine\nenabled_tools = [\"a\"]\n\n\
         [mcp_servers.other]\ncommand = \"npx\"\n"
    );
    std::fs::write(&path, &original).unwrap();
    let def = find_def("codex").unwrap();
    let server = read_client(&def)
        .servers
        .into_iter()
        .find(|server| server.name == "toolport")
        .unwrap();
    let record: ManagedEntry = serde_json::from_value(serde_json::json!({
        "command": server.command.clone().unwrap_or_default(),
        "args": server.args,
        "env": server.env_keys.iter().map(|key| (key.clone(), "")).collect::<HashMap<_, _>>(),
        "transport": "sharedHttp",
        "url": url,
        "updatedAt": 1
    }))
    .unwrap();
    let mut managed = HashMap::from([("codex".to_string(), record)]);
    launch(current, &mut managed);
    let after = std::fs::read_to_string(&path).unwrap();
    let entry = codex_entry(&path);
    assert_eq!(entry["supports_parallel_tool_calls"].as_bool(), Some(true));
    assert_eq!(entry["tool_timeout_sec"].as_integer(), Some(600));
    // The app serves the HTTP endpoint already, so no cold start to cover.
    assert!(!entry.contains_key("startup_timeout_sec"));
    // Transport and everything else stay as written.
    assert_eq!(entry["url"].as_str(), Some(url));
    assert!(!entry.contains_key("command"));
    assert!(after.contains("bearer_token_env_var = \"TOOLPORT_TOKEN\" # mine"));
    assert_eq!(entry["enabled_tools"][0].as_str(), Some("a"));
    assert!(after.starts_with("model = \"gpt-5\"\n"));
    assert!(after.contains("[mcp_servers.other]\ncommand = \"npx\"\n"));
    assert_eq!(managed["codex"].transport, "sharedHttp");
    assert_eq!(managed["codex"].url.as_deref(), Some(url));
    // Settled, and a deletion stays.
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    without_line(&path, "supports_parallel_tool_calls");
    let edited = std::fs::read_to_string(&path).unwrap();
    launch(current, &mut managed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn an_entry_with_some_settings_and_no_record_keeps_the_rest_removed() {
    // A data dir that was reset or restored has no record of what Toolport wrote. If
    // the entry still carries any Toolport setting, the missing ones were removed on
    // purpose, so nothing is added back.
    for with_record in [false, true] {
        let data = crate::registry::DataDirTestEnv::new("codex-settings-partial");
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&data.dir);
        let _home = TestHome::set(&data.dir);
        let path = codex_config(&data);
        let current = data.dir.join("toolport-gateway");
        let current = current.to_str().unwrap();
        std::fs::write(
            &path,
            format!(
                "[mcp_servers.toolport]\ncommand = '{current}'\ntool_timeout_sec = 600\n\n\
                 [mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = \"codex\"\n\
                 TOOLPORT_ATTRIBUTION_ID = \"codex\"\n"
            ),
        )
        .unwrap();
        let mut managed = HashMap::new();
        if with_record {
            let server = read_client(&find_def("codex").unwrap()).servers.remove(0);
            let record: ManagedEntry = serde_json::from_value(serde_json::json!({
                "command": current,
                "args": server.args,
                "env": server.env_keys.iter().map(|key| (key.clone(), "codex")).collect::<HashMap<_, _>>(),
                "updatedAt": 1
            }))
            .unwrap();
            managed.insert("codex".to_string(), record);
        }
        let original = std::fs::read_to_string(&path).unwrap();
        launch(current, &mut managed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "record={with_record}"
        );
        if with_record {
            assert_eq!(managed["codex"].codex_settings.len(), 3);
        }
    }
}

#[test]
fn a_stale_command_repair_keeps_the_no_record_safeguard() {
    // The same partial entry, now pointing at an older gateway binary. The startup
    // repair rewrites the command, and must not carry the two missing defaults in
    // with it: without a record, an entry that keeps one setting had the rest
    // removed on purpose.
    for with_record in [false, true] {
        let data = crate::registry::DataDirTestEnv::new("codex-settings-stale");
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&data.dir);
        let _home = TestHome::set(&data.dir);
        let path = codex_config(&data);
        let current = data.dir.join("toolport-gateway");
        let current = current.to_str().unwrap();
        // An older release's gateway, since removed. A gateway that still exists at
        // another path is current on Windows, so the stale one must be gone.
        let stale = data.dir.join("old-release").join("toolport-gateway");
        let stale = stale.to_str().unwrap();
        std::fs::write(
            &path,
            format!(
                "[mcp_servers.toolport]\ncommand = '{stale}'\ntool_timeout_sec = 90\n\n\
                 [mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = \"codex\"\n\
                 TOOLPORT_ATTRIBUTION_ID = \"codex\"\n"
            ),
        )
        .unwrap();
        let mut managed = HashMap::new();
        if with_record {
            let server = read_client(&find_def("codex").unwrap()).servers.remove(0);
            let record: ManagedEntry = serde_json::from_value(serde_json::json!({
                "command": stale,
                "args": server.args,
                "env": server.env_keys.iter().map(|key| (key.clone(), "codex")).collect::<HashMap<_, _>>(),
                "updatedAt": 1
            }))
            .unwrap();
            managed.insert("codex".to_string(), record);
        }
        launch(current, &mut managed);
        let entry = codex_entry(&path);
        if with_record {
            assert_eq!(
                entry["command"].as_str(),
                Some(current),
                "record={with_record}"
            );
        }
        assert_eq!(
            entry["tool_timeout_sec"].as_integer(),
            Some(90),
            "record={with_record}"
        );
        assert!(
            !entry.contains_key("supports_parallel_tool_calls")
                && !entry.contains_key("startup_timeout_sec"),
            "record={with_record}: {entry:?}"
        );
        // A later launch with the record now written changes nothing more.
        let after = std::fs::read_to_string(&path).unwrap();
        launch(current, &mut managed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    }
}

#[test]
fn changing_access_keeps_removed_codex_settings_removed() {
    // Applying an access scope rewrites the entry through install_gateway. It is not
    // a Connect, so settings the user removed stay removed, with or without a record;
    // an explicit reset (force) writes the defaults again, as the docs say.
    for with_record in [false, true] {
        let data = crate::registry::DataDirTestEnv::new("codex-settings-scope");
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&data.dir);
        let _home = TestHome::set(&data.dir);
        let path = codex_config(&data);
        std::fs::write(&path, "model = \"gpt-5\"\n").unwrap();
        let first = install_gateway_for("codex", None, None).unwrap();
        let record = first.managed.clone().unwrap();
        let entry = codex_entry(&path);
        assert_eq!(entry["supports_parallel_tool_calls"].as_bool(), Some(true));
        without_line(&path, "supports_parallel_tool_calls");
        without_line(&path, "startup_timeout_sec");
        let handled = if with_record {
            codex_settings_marked("codex", &record)
                .unwrap_or(record.clone())
                .codex_settings
        } else {
            Vec::new()
        };
        install_gateway_for("codex", Some("Work"), Some(&handled)).unwrap();
        let entry = codex_entry(&path);
        assert_eq!(
            entry["tool_timeout_sec"].as_integer(),
            Some(600),
            "record={with_record}"
        );
        assert!(
            !entry.contains_key("supports_parallel_tool_calls")
                && !entry.contains_key("startup_timeout_sec"),
            "record={with_record}: {entry:?}"
        );
        // An explicit reset writes them again.
        install_gateway_for("codex", Some("Work"), None).unwrap();
        let entry = codex_entry(&path);
        assert_eq!(entry["supports_parallel_tool_calls"].as_bool(), Some(true));
        assert_eq!(entry["startup_timeout_sec"].as_integer(), Some(75));
    }
}

#[test]
fn the_connect_preview_lists_the_codex_settings_it_will_add() {
    let all = codex_gateway_settings_preview("codex", None);
    assert_eq!(
        all,
        [
            "supports_parallel_tool_calls = true",
            "startup_timeout_sec = 75",
            "tool_timeout_sec = 600"
        ]
    );
    let text = "[mcp_servers.toolport]\ncommand = \"toolport-gateway\"\n\
                startup_timeout_ms = 3000\ntool_timeout_sec = 90\n";
    assert_eq!(
        codex_gateway_settings_preview("codex@work", Some(text)),
        ["supports_parallel_tool_calls = true"]
    );
    assert!(codex_gateway_settings_preview("grok", None).is_empty());
    assert!(codex_gateway_settings_preview("claude-code", None).is_empty());
}

/// F1: with the keyring locked, the start-up refresh of a Codex entry (its
/// backup must seal another server's inline key) neither asks to unlock nor
/// reports a failure. It is deferred, and the next pass with the keyring open
/// writes it.
#[test]
fn a_locked_keyring_defers_the_startup_refresh_without_a_prompt() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = crate::registry::conduit_dir().unwrap();
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&dir);
        let _home = TestHome::set(&dir);
        let path = resolved_definition_path(&find_def("codex").unwrap()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let current = dir.join("toolport-gateway");
        std::fs::write(&current, "fixture").unwrap();
        let current = current.to_str().unwrap();
        crate::registry::update(|registry| {
            registry.servers.push(
                serde_json::from_value(serde_json::json!({
                    "id": "weather", "name": "Weather", "command": "weather",
                    "transport": "stdio", "enabled": true,
                    "env": [{"key": "WEATHER_API_KEY", "secret": true}]
                }))
                .unwrap(),
            );
            Ok(())
        })
        .unwrap();
        std::fs::write(
            &path,
            format!(
                "[mcp_servers.toolport]\ncommand = '{current}'\n\n\
                 [mcp_servers.toolport.env]\nTOOLPORT_CLIENT_ID = \"codex\"\n\
                 TOOLPORT_ATTRIBUTION_ID = \"codex\"\n\n\
                 [mcp_servers.weather]\ncommand = \"weather\"\n\n\
                 [mcp_servers.weather.env]\nWEATHER_API_KEY = \"sk-weather-locked-fixture\"\n"
            ),
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        let rows = || {
            let mut rows = vec![read_client(&find_def("codex").unwrap())];
            apply_entry_states(&mut rows, &HashMap::new());
            rows
        };
        let mut outcome = None;
        let (calls, prompts) = backup_vault::fake::locked_everywhere(|| {
            outcome = Some(backup_vault::noninteractive(|| {
                repoint_stale_gateways_in(current, rows(), &HashMap::new())
            }));
        });
        assert_eq!(prompts, Vec::<String>::new());
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("prompting") || call == "unlock prompt"),
            "{calls:?}"
        );
        let outcome = outcome.unwrap();
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(outcome.deferred, vec!["codex".to_string()]);
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing written");

        // Keyring open again (the next start, or a Retry unlock): written.
        let outcome = repoint_stale_gateways_in(current, rows(), &HashMap::new());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(outcome.repointed.len(), 1);
        assert_eq!(
            codex_entry(&path)["startup_timeout_sec"].as_integer(),
            Some(75)
        );
    });
}

/// F1: legacy plaintext backups are sealed at start. With the keyring locked
/// that write would raise the unlock prompt; the start-up pass defers it.
#[test]
fn a_locked_keyring_defers_sealing_legacy_backups_at_start() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = crate::registry::conduit_dir().unwrap();
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&dir);
        let _home = TestHome::set(&dir);
        let legacy = backup_dir("cursor").unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        // A 2.3.5 timestamped backup: the client config as it was, key inline.
        let original = "{\"mcpServers\":{\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-legacy-locked-fixture\"}}}}\n".to_string();
        let file = legacy.join("1700000000000-mcp.json");
        std::fs::write(&file, &original).unwrap();
        let mut outcome = None;
        let (calls, prompts) = backup_vault::fake::locked_everywhere(|| {
            outcome = Some(repoint_stale_gateways(&HashMap::new()));
        });
        assert_eq!(prompts, Vec::<String>::new());
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("prompting") || call == "unlock prompt"),
            "{calls:?}"
        );
        let outcome = outcome.unwrap();
        assert!(
            !outcome
                .failed
                .iter()
                .any(|(what, _)| what == "client recovery backups"),
            "{:?}",
            outcome.failed
        );
        assert!(outcome
            .deferred
            .contains(&"client recovery backups".to_string()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    });
}

/// K10: a pass that runs again while the keyring would still ask starts no
/// second backup-upgrade waiter.
#[test]
fn repeated_start_up_passes_start_one_backup_upgrade_waiter() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = crate::registry::conduit_dir().unwrap();
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&dir);
        let _home = TestHome::set(&dir);
        let legacy = backup_dir("cursor").unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        let original = "{\"mcpServers\":{\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-legacy-waiter-fixture\"}}}}\n";
        std::fs::write(legacy.join("1700000000000-mcp.json"), original).unwrap();
        crate::secrets::locked_store::prompts_during(|| {
            for _ in 0..3 {
                let outcome = repoint_stale_gateways(&HashMap::new());
                assert!(outcome
                    .deferred
                    .contains(&"client recovery backups".to_string()));
            }
        });
        assert_eq!(UPGRADE_WAITERS.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

/// CodeRabbit (Major): with the registry unreadable, the ownership records are
/// unknown, so the post-unlock refresh rewrites nothing and keeps waiting.
#[test]
fn an_unreadable_registry_stops_the_unlock_refresh() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = crate::registry::conduit_dir().unwrap();
        let _env_lock = env_test_lock();
        let _client_env = isolated_client_env(&dir);
        let _home = TestHome::set(&dir);
        let registry = crate::registry::resolved_path().unwrap();
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        std::fs::write(&registry, "{ not a registry").unwrap();
        let outcome = refresh_stale_client_configs();
        assert!(outcome.repointed.is_empty() && outcome.failed.is_empty());
        assert!(!outcome.deferred.is_empty(), "{:?}", outcome.deferred);
        assert_eq!(
            std::fs::read_to_string(&registry).unwrap(),
            "{ not a registry"
        );
    });
}

/// F1: the log names what the pass was doing; the same path is a refresh.
#[test]
fn refresh_failures_do_not_claim_a_repoint_onto_the_same_path() {
    let same = repoint_failure_message(
        "codex",
        "/usr/bin/toolport-gateway",
        "/usr/bin/toolport-gateway",
        "disk full",
    );
    assert_eq!(
        same,
        "toolport: could not refresh codex's gateway entry (/usr/bin/toolport-gateway): disk full"
    );
    let moved = repoint_failure_message(
        "codex",
        "/opt/old/toolport-gateway",
        "/usr/bin/toolport-gateway",
        "disk full",
    );
    assert_eq!(
        moved,
        "toolport: could not re-point codex's gateway entry from /opt/old/toolport-gateway to /usr/bin/toolport-gateway: disk full"
    );
}
