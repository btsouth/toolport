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
            "model = \"gpt-5\"\n\n[mcp_servers.toolport]\ncommand = \"{current}\"\n\
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
            "[mcp_servers.toolport]\ncommand = \"{current}\"\n\
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
