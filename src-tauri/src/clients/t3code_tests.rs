use super::*;

const SECRETS: [&str; 7] = [
    "sk-ant-api03-FIXTURE-SECRET-1",
    "sk-proj-FIXTURE-SECRET-2",
    "hunter2-FIXTURE-SERVER-PASSWORD",
    "FIXTURE-SENSITIVE-XDG",
    "opencode-FIXTURE-CONFIG-CONTENT",
    "xai-FIXTURE-GROK-KEY",
    "acp-FIXTURE-REGISTRY-KEY",
];

/// The shape T3 Code writes today (keys from a real file; every value invented).
fn fixture(home: &Path) -> String {
    let h = json_path(home);
    format!(
        r#"{{
  "favorites": [],
  "providerInstances": {{
    "claudeAgent": {{"driver": "claudeAgent", "displayName": "Claude Work", "enabled": true,
      "config": {{"binaryPath": "claude", "homePath": "", "customModels": [], "launchArgs": "--x"}}}},
    "claude_personal": {{"driver": "claudeAgent", "displayName": "Claude Personal", "enabled": true,
      "config": {{"homePath": "~/.claude_personal"}}}},
    "commandcode": {{"driver": "claudeAgent", "displayName": "CommandCode Claude", "enabled": false,
      "config": {{"homePath": "{h}/.t3/commandcode/claude"}},
      "environment": [
        {{"name": "ANTHROPIC_AUTH_TOKEN", "sensitive": true, "value": "{s0}"}},
        {{"name": "ANTHROPIC_BASE_URL", "sensitive": false, "value": "https://example.invalid"}}
      ]}},
    "codex": {{"driver": "codex", "enabled": true}},
    "codex_shadow": {{"driver": "codex", "displayName": "Codex Personal", "enabled": false,
      "config": {{"homePath": "~/.codex", "shadowHomePath": "~/.codex_personal"}}}},
    "codex_work": {{"driver": "codex", "displayName": "Codex-Work", "enabled": false,
      "config": {{"binaryPath": "codex", "homePath": "~/Codex-Work/.codex"}},
      "environment": [{{"name": "OPENAI_API_KEY", "sensitive": true, "value": "{s1}", "valueRedacted": true}}]}},
    "opencode": {{"driver": "opencode", "displayName": "OpenCode Go", "enabled": true,
      "config": {{"serverPassword": "{s2}", "serverUrl": "http://127.0.0.1:1"}},
      "environment": [{{"name": "OPENCODE_CONFIG_CONTENT", "sensitive": false, "value": "{{\"x\":\"{s4}\"}}"}}]}},
    "clinepass": {{"driver": "opencode", "displayName": "ClinePass", "enabled": false,
      "environment": [
        {{"name": "XDG_CONFIG_HOME", "sensitive": false, "value": "~/.t3/hermes/clinepass/config"}},
        {{"name": "XDG_DATA_HOME", "sensitive": false, "value": "~/.t3/hermes/clinepass/data"}}
      ]}},
    "hidden_xdg": {{"driver": "opencode", "displayName": "Hidden", "enabled": true,
      "environment": [{{"name": "XDG_CONFIG_HOME", "sensitive": true, "value": "{s3}"}}]}},
    "cursor": {{"driver": "cursor", "displayName": "Cursor", "enabled": true}},
    "kiro": {{"driver": "kiro"}},
    "antigravity": {{"driver": "antigravity", "displayName": "Antigravity", "enabled": false}},
    "muse": {{"driver": "muse", "displayName": "Muse Code", "enabled": false, "config": {{"customModels": []}}}},
    "future": {{"driver": "somethingNew", "config": {{"token": "{s0}"}}}},
    "grok_alt": {{"driver": "grok", "displayName": "Grok Alt", "enabled": true,
      "environment": [
        {{"name": "XAI_API_KEY", "sensitive": true, "value": "{s5}"}},
        {{"name": "GROK_HOME", "sensitive": false, "value": "~/.grok-alt"}}
      ]}},
    "grok_hidden": {{"driver": "grok", "enabled": true,
      "environment": [{{"name": "GROK_HOME", "sensitive": true, "value": "{s3}"}}]}},
    "pi": {{"driver": "pi", "enabled": true}},
    "pi_work": {{"driver": "pi", "displayName": "Pi Work", "enabled": true,
      "environment": [{{"name": "PI_CODING_AGENT_DIR", "sensitive": false, "value": "~/work/.pi-agent"}}]}},
    "fx": {{"driver": "fx", "enabled": true}},
    "acp_goose": {{"driver": "acpRegistry", "config": {{"agentId": "goose", "apiKey": "{s6}"}}}},
    "acp_odd": {{"driver": "acpRegistry", "displayName": "Odd agent", "config": {{"agentId": "rm -rf /"}}, "enabled": false}},
    "bad driver": {{"driver": "two words"}},
    "broken_env": {{"driver": "grok", "displayName": "Broken Env", "enabled": true,
      "environment": {{"GROK_HOME": "{s3}"}}}},
    "odd_name": {{"driver": "codex", "displayName": {{"v": "{s0}"}}, "config": ["{s1}"]}},
    "pi_hidden": {{"driver": "pi", "displayName": "Pi Hidden", "enabled": true,
      "environment": [{{"name": "PI_CODING_AGENT_DIR", "value": {{"k": "{s5}"}}}}]}},
    "array_config": {{"driver": "claudeAgent", "displayName": "Array Config", "config": ["{s6}"]}},
    "text_entry": "{s2}",
    "relative_codex": {{"driver": "codex", "displayName": "Relative", "enabled": true,
      "config": {{"homePath": "rel/.codex"}}}},
    "claude_env": {{"driver": "claudeAgent", "displayName": "Claude Env", "enabled": true,
      "environment": [{{"name": "CLAUDE_CONFIG_DIR", "sensitive": false, "value": " {h}/.claude-env "}}]}},
    "claude_hidden": {{"driver": "claudeAgent", "displayName": "Claude Hidden", "enabled": true,
      "environment": [{{"name": "CLAUDE_CONFIG_DIR", "sensitive": true, "value": "{s3}"}}]}},
    "claude_tilde": {{"driver": "claudeAgent", "displayName": "Claude Tilde", "enabled": true,
      "environment": [{{"name": "CLAUDE_CONFIG_DIR", "sensitive": false, "value": "~/.claude-tilde"}}]}},
    "claude_both": {{"driver": "claudeAgent", "displayName": "Claude Both", "enabled": true,
      "config": {{"homePath": "~/.claude-both"}},
      "environment": [{{"name": "CLAUDE_CONFIG_DIR", "sensitive": true, "value": "{s3}"}}]}},
    "codex_env": {{"driver": "codex", "displayName": "Codex Env", "enabled": true,
      "environment": [{{"name": "CODEX_HOME", "sensitive": false, "value": "{h}/.codex-env"}}]}},
    "codex_hidden": {{"driver": "codex", "displayName": "Codex Hidden", "enabled": true,
      "environment": [{{"name": "CODEX_HOME", "value": {{"k": "{s1}"}}}}]}},
    "codex_relative_env": {{"driver": "codex", "displayName": "Codex Relative", "enabled": true,
      "environment": [{{"name": "CODEX_HOME", "sensitive": false, "value": "codex-home"}}]}}
  }}
}}"#,
        s0 = SECRETS[0],
        s1 = SECRETS[1],
        s2 = SECRETS[2],
        s3 = SECRETS[3],
        s4 = SECRETS[4],
        s5 = SECRETS[5],
        s6 = SECRETS[6],
    )
}

/// A path as T3 Code's settings may hold it: forward slashes, so it needs no JSON escape.
fn json_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// An absolute home on every OS; forward slashes keep it valid inside JSON text.
fn fixture_home() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        "C:/home/fixture"
    } else {
        "/home/fixture"
    })
}

fn instances_of(reading: Reading) -> Vec<Instance> {
    match reading {
        Reading::Instances(instances) => instances,
        other => panic!("expected instances, got {other:?}"),
    }
}

fn by_id<'a>(instances: &'a [Instance], id: &str) -> &'a Instance {
    instances
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("{id}"))
}

#[test]
fn reads_only_allow_listed_fields_and_maps_each_provider() {
    let home = fixture_home();
    let instances = instances_of(parse(fixture(&home).into_bytes(), &home));
    let ids: Vec<&str> = instances.iter().map(|i| i.id.as_str()).collect();
    // Drivers Toolport cannot connect are not matched to rows. A home hidden behind a
    // sensitive variable is unknown, so that instance is not matched to any row either.
    assert_eq!(
        ids,
        vec![
            "claudeAgent",
            "claude_both",
            "claude_env",
            "claude_personal",
            "clinepass",
            "codex",
            "codex_env",
            "codex_shadow",
            "codex_work",
            "commandcode",
            "cursor",
            "grok_alt",
            "kiro",
            "opencode",
            "pi",
            "pi_work",
        ]
    );
    let claude = by_id(&instances, "claudeAgent");
    assert_eq!(
        (claude.display_name.as_str(), claude.enabled),
        ("Claude Work", true)
    );
    assert_eq!(
        claude.config_path, None,
        "empty homePath is the default config"
    );
    assert_eq!(
        by_id(&instances, "claude_personal").config_path,
        Some(home.join(".claude_personal/.claude.json"))
    );
    let commandcode = by_id(&instances, "commandcode");
    assert!(!commandcode.enabled);
    assert_eq!(
        commandcode.config_path,
        Some(home.join(".t3/commandcode/claude/.claude.json"))
    );
    // No display name: T3 Code's provider name.
    assert_eq!(by_id(&instances, "codex").display_name, "Codex");
    // A shadow home reads the shared home's config.toml.
    assert_eq!(
        by_id(&instances, "codex_shadow").config_path,
        Some(home.join(".codex/config.toml"))
    );
    assert_eq!(
        by_id(&instances, "codex_work").config_path,
        Some(home.join("Codex-Work/.codex/config.toml"))
    );
    assert_eq!(
        by_id(&instances, "clinepass").config_path,
        Some(home.join(".t3/hermes/clinepass/config/opencode/opencode.json"))
    );
    assert_eq!(by_id(&instances, "opencode").config_path, None);
    assert_eq!(
        by_id(&instances, "grok_alt").config_path,
        Some(home.join(".grok-alt/config.toml"))
    );
    assert_eq!(by_id(&instances, "pi").config_path, None);
    assert_eq!(
        by_id(&instances, "pi_work").config_path,
        Some(home.join("work/.pi-agent/mcp.json"))
    );
    // With no homePath, T3 Code's Claude reads CLAUDE_CONFIG_DIR from the instance
    // environment, trimmed, and Codex reads CODEX_HOME. A homePath wins over either.
    assert_eq!(
        by_id(&instances, "claude_env").config_path,
        Some(home.join(".claude-env/.claude.json"))
    );
    assert_eq!(
        by_id(&instances, "claude_both").config_path,
        Some(home.join(".claude-both/.claude.json"))
    );
    assert_eq!(
        by_id(&instances, "codex_env").config_path,
        Some(home.join(".codex-env/config.toml"))
    );
    // Kiro is off unless enabled, like T3 Code's default for its driver.
    let kiro = by_id(&instances, "kiro");
    assert!(!kiro.enabled);
    assert_eq!(kiro.display_name, "Kiro");
}

#[test]
fn a_key_in_the_settings_never_reaches_any_output() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-secrets-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    for folder in [
        ".claude_personal",
        ".t3/commandcode/claude",
        ".t3/userdata",
        "Codex-Work/.codex",
        ".t3/hermes/clinepass/config/opencode",
    ] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
    }
    std::fs::write(home.join(".t3/userdata/settings.json"), fixture(&home)).unwrap();
    std::fs::write(home.join(".t3/commandcode/claude/.claude.json"), "{}").unwrap();
    // The lock first: the data folder override is process-wide.
    let _lock = crate::registry::data_dir_test_lock();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);

    let reading = current();
    let rows = automatic(&[]);
    let mut clients: Vec<DetectedClient> = rows
        .iter()
        .map(|row| DetectedClient {
            id: row.id.clone(),
            name: row.name.clone(),
            uses_connectors: false,
            location_removable: false,
            discovery: discovery::capabilities(&row.client_type),
            config_path: row.config_path.display().to_string(),
            config_exists: true,
            app_present: true,
            servers: Vec::new(),
            plugin_servers: Vec::new(),
            gateway_installed: false,
            entry_state: GatewayEntryState::Absent,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
            shared: None,
        })
        .collect();
    // The default Claude Code and Codex rows: a home behind a hidden variable is never
    // taken for them.
    for (id, path) in [
        ("claude-code", home.join(".claude.json")),
        ("codex", home.join(".codex/config.toml")),
    ] {
        let mut row = config_error_row(id, id, path.display().to_string(), String::new());
        row.error = None;
        clients.push(row);
    }
    annotate(&mut clients);
    for client in &clients {
        for hidden in ["claude_hidden", "codex_hidden", "claude_tilde"] {
            assert!(
                client.t3_code.iter().all(|use_| use_.instance_id != hidden),
                "{hidden} on {}",
                client.id
            );
        }
    }
    let outputs = [
        format!("{reading:?}"),
        format!("{rows:?}"),
        serde_json::to_string(&clients).unwrap(),
        format!("{clients:?}"),
        UNREADABLE_NOTICE.to_string(),
        revision().unwrap_or_default(),
    ];
    for output in &outputs {
        for secret in SECRETS {
            assert!(!output.contains(secret), "{secret} leaked into {output}");
        }
    }
    assert!(clients.iter().any(|client| !client.t3_code.is_empty()));
    // A malformed file is noticed with the fixed line only.
    let broken = format!("{} trailing", fixture(&home));
    assert_eq!(parse(broken.into_bytes(), &home), Reading::Unreadable);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn enabled_follows_t3_codes_rules() {
    let home = PathBuf::from("/h");
    let text = r#"{"providerInstances": {
        "a": {"driver": "claudeAgent", "enabled": true, "config": {"enabled": false}},
        "b": {"driver": "claudeAgent", "config": {"enabled": true}},
        "c": {"driver": "cursor"},
        "d": {"driver": "opencode", "enabled": true},
        "e": {"driver": "codex"}
    }}"#;
    let instances = instances_of(parse(text.as_bytes().to_vec(), &home));
    let enabled = |id| by_id(&instances, id).enabled;
    assert!(!enabled("a"), "either flag off wins");
    assert!(enabled("b"));
    assert!(!enabled("c"), "Cursor is off by default");
    assert!(enabled("d"));
    assert!(enabled("e"), "Codex is on by default");
}

#[test]
fn default_instances_stand_in_only_when_absent() {
    let home = PathBuf::from("/h");
    // No providerInstances at all: T3 Code's default Claude and Codex.
    let instances = instances_of(parse(br#"{"favorites": []}"#.to_vec(), &home));
    assert_eq!(
        instances
            .iter()
            .map(|i| (i.id.as_str(), i.enabled, i.config_path.clone()))
            .collect::<Vec<_>>(),
        vec![("claudeAgent", true, None), ("codex", true, None)]
    );
    // A default id whose entry has a shape Toolport does not know is not replaced by a
    // default that might contradict it.
    let instances = instances_of(parse(
        br#"{"providerInstances": {"claudeAgent": {"driver": 7}, "codex": {"driver": "codex", "enabled": false}}}"#
            .to_vec(),
        &home,
    ));
    assert_eq!(instances.len(), 1);
    assert!(!instances[0].enabled);
}

#[test]
fn malformed_or_odd_files_fall_back_without_breaking() {
    let home = fixture_home();
    for text in [
        "",
        "not json",
        "[]",
        r#"{"providerInstances": []}"#,
        r#"{"providerInstances": "x"}"#,
    ] {
        assert_eq!(
            parse(text.as_bytes().to_vec(), &home),
            Reading::Unreadable,
            "{text}"
        );
    }
    assert_eq!(parse(vec![0xff, 0xfe], &home), Reading::Unreadable);
    // One bad instance hides only itself.
    let instances = instances_of(parse(
        br#"{"providerInstances": {
            "bad": {"driver": "claudeAgent", "config": {"homePath": 42}},
            "relative": {"driver": "claudeAgent", "config": {"homePath": "relative/dir"}},
            "good": {"driver": "claudeAgent", "displayName": "  Good\nname  ", "config": {"homePath": "~"}}
        }}"#
        .to_vec(),
        &home,
    ));
    let good = by_id(&instances, "good");
    assert_eq!(good.display_name, "Good name");
    assert_eq!(good.config_path, Some(home.join(".claude.json")));
    assert!(
        instances.iter().all(|i| i.id != "relative"),
        "relative homes are not guessed"
    );
    // "bad" keeps its id from the default but contributes no instance.
    assert!(instances.iter().all(|i| i.id != "bad"));
}

#[test]
fn absent_t3_code_changes_nothing() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-absent-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let _home = TestHome::set(&root);
    assert_eq!(*current(), Reading::Absent);
    assert!(automatic(&[]).is_empty());
    assert_eq!(revision(), None);
    let mut clients = vec![config_error_row(
        "claude-code",
        "Claude Code",
        String::new(),
        "x".into(),
    )];
    annotate(&mut clients);
    assert!(clients[0].t3_code.is_empty());
    let json = serde_json::to_value(&clients[0]).unwrap();
    assert!(json.get("t3Code").is_none(), "no new field without T3 Code");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn t3_home_variable_moves_the_data_folder() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-home-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let _home = TestHome::set(&root.join("home"));
    let previous = test_paths::set("T3CODE_HOME", Some("~/elsewhere".into()));
    assert_eq!(data_dir(), Some(root.join("home").join("elsewhere")));
    test_paths::set("T3CODE_HOME", Some("relative".into()));
    assert_eq!(data_dir(), None, "a relative T3CODE_HOME is not guessed");
    test_paths::set("T3CODE_HOME", Some("".into()));
    assert_eq!(data_dir(), Some(root.join("home").join(".t3")));
    test_paths::set("T3CODE_HOME", previous);
}

#[test]
fn annotate_labels_the_matching_rows() {
    // Labels look at the folders on disk (after the walk), so the homes exist here.
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-annotate-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    for folder in [".claude_personal", ".codex", ".cursor"] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
    }
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let row = |id: &str, path: PathBuf| {
        let mut row = config_error_row(id, id, path.display().to_string(), String::new());
        row.error = None;
        row
    };
    let mut clients = vec![
        row("claude-code", home.join(".claude.json")),
        row(
            "claude-code@claude_personal",
            home.join(".claude_personal/.claude.json"),
        ),
        row("codex", home.join(".codex/config.toml")),
        row("cursor", home.join(".cursor/mcp.json")),
    ];
    let instance = |id: &str, provider, path: Option<PathBuf>, enabled| Instance {
        id: id.into(),
        provider,
        display_name: id.to_uppercase(),
        enabled,
        config_path: path,
    };
    let instances = vec![
        instance(
            "personal",
            Provider::ClaudeCode,
            Some(home.join(".claude_personal/.claude.json")),
            true,
        ),
        instance(
            "work",
            Provider::Codex,
            Some(home.join(".codex/config.toml")),
            true,
        ),
        instance(
            "shadow",
            Provider::Codex,
            Some(home.join(".codex/config.toml")),
            false,
        ),
        // A Claude instance whose home has no row stays unlabelled rather than guessed.
        instance(
            "other",
            Provider::ClaudeCode,
            Some(home.join(".x/.claude.json")),
            true,
        ),
    ];
    annotate_with(&mut clients, &instances);
    let names = |index: usize| {
        clients[index]
            .t3_code
            .iter()
            .map(|use_| (use_.display_name.clone(), use_.enabled))
            .collect::<Vec<_>>()
    };
    assert!(names(0).is_empty());
    assert_eq!(names(1), vec![("PERSONAL".to_string(), true)]);
    assert_eq!(
        names(2),
        vec![("WORK".to_string(), true), ("SHADOW".to_string(), false)]
    );
    assert!(names(3).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn location_ids_are_stable_and_recognised() {
    let id = location_id(
        "claude-code",
        Path::new("/h/.t3/commandcode/claude/.claude.json"),
    );
    assert!(id.starts_with("claude-code@t3-") && id.len() == "claude-code@t3-".len() + 16);
    assert_eq!(
        id,
        location_id(
            "claude-code",
            Path::new("/h/.t3/commandcode/claude/.claude.json")
        )
    );
    assert!(is_location_id(&id));
    assert_eq!(locations::generated_id_type(&id), Some("claude-code"));
    for bad in [
        "claude-code@t3-xyz",
        "cursor@t3-0123456789abcdef",
        "claude-code@claude2",
        "codex@t3-0123456789ABCDEF",
    ] {
        assert!(!is_location_id(bad), "{bad}");
    }
}

#[test]
fn t3_homes_without_launchers_become_locations_once() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-rows-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/commandcode/claude")).unwrap();
    std::fs::create_dir_all(home.join(".t3/commandcode/codex")).unwrap();
    std::fs::create_dir_all(home.join(".claude2")).unwrap();
    std::fs::write(home.join(".claude2/.claude.json"), "{}").unwrap();
    let h = json_path(&home);
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        format!(
            r#"{{"providerInstances": {{
                "a": {{"driver": "claudeAgent", "displayName": "CC", "config": {{"homePath": "{h}/.t3/commandcode/claude"}}}},
                "b": {{"driver": "codex", "displayName": "CX", "enabled": false, "config": {{"homePath": "~/.t3/commandcode/codex"}}}},
                "c": {{"driver": "claudeAgent", "displayName": "Two", "config": {{"homePath": "~/.claude2"}}}},
                "d": {{"driver": "claudeAgent", "displayName": "Missing", "config": {{"homePath": "~/.not-there"}}}}
            }}}}"#
        ),
    )
    .unwrap();
    // `automatic` reads the saved locations, so the data folder must be a scratch one.
    let _lock = crate::registry::data_dir_test_lock();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let real = std::fs::canonicalize(&home).unwrap();
    let known = vec![locations::ClientLocation {
        id: "claude-code@claude2".into(),
        manual: false,
        client_type: "claude-code".into(),
        name: "Claude Code (claude2)".into(),
        config_path: home.join(".claude2/.claude.json"),
    }];
    let rows = automatic(&known);
    let found: Vec<(&str, PathBuf)> = rows
        .iter()
        .map(|row| (row.client_type.as_str(), row.config_path.clone()))
        .collect();
    // ~/.claude2 already has its own row, and a home that does not exist yet waits.
    assert_eq!(
        found,
        vec![
            (
                "claude-code",
                real.join(".t3/commandcode/claude/.claude.json")
            ),
            ("codex", real.join(".t3/commandcode/codex/config.toml")),
        ]
    );
    for row in &rows {
        assert!(is_location_id(&row.id));
        assert!(!row.manual);
        assert!(path_is_safe(&row.config_path));
        assert!(lists(&row.config_path));
    }
    assert_eq!(rows[0].name, "Claude Code (~/.t3/commandcode/claude)");
    // A config that is a link is not written through automatically.
    #[cfg(unix)]
    {
        std::fs::write(root.join("target.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            root.join("target.json"),
            home.join(".t3/commandcode/claude/.claude.json"),
        )
        .unwrap();
        assert!(automatic(&known)
            .iter()
            .all(|row| row.client_type != "claude-code"));
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn profile_folders_match_old_and_t3_code_forms() {
    for name in [
        ".claude2",
        ".claude-work",
        ".claude_personal",
        ".claude_openrouter",
    ] {
        assert!(claude_profile_name(name), "{name}");
    }
    for name in [".codex2", ".codex-work", ".codex_personal"] {
        assert!(profile_folder_name(name, ".codex"), "{name}");
    }
    for name in [
        ".claude_",
        ".claude-",
        ".claude_backup",
        ".claude_old-2",
        ".claude-bak",
        ".claudex",
        ".claude.json",
        ".codex_tmp",
        ".codexy",
    ] {
        let base = if name.starts_with(".codex") {
            ".codex"
        } else {
            ".claude"
        };
        assert!(!profile_folder_name(name, base), "{name}");
    }
    // The underscore form round-trips through its location id like the dash form.
    assert_eq!(
        locations::generated_id_type("claude-code@claude_personal"),
        Some("claude-code")
    );
    assert_eq!(
        locations::generated_id_type("codex@.codex_personal"),
        Some("codex")
    );
}

#[test]
fn an_unsaved_t3_home_can_be_previewed_and_connected() {
    // Connect previews a client by id before anything saves its location.
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-connect-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/commandcode/claude")).unwrap();
    std::fs::write(home.join(".t3/commandcode/claude/.claude.json"), "{}").unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {"cc": {"driver": "claudeAgent", "displayName": "CC", "enabled": false,
            "config": {"homePath": "~/.t3/commandcode/claude"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let row = automatic(&[]).pop().expect("the T3 Code home is listed");
    assert!(
        locations::saved().unwrap().is_empty(),
        "listing saves nothing"
    );
    let client = detect_client(&row.id).expect("found by id before it is saved");
    assert_eq!(client.id, row.id);
    assert!(client.config_exists && client.error.is_none());
    assert!(setup_revision(&row.id).is_ok());
    // Once T3 Code stops listing it, an unsaved home is gone.
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {}}"#,
    )
    .unwrap();
    // A rewrite inside the same second can keep the mtime; the length differs.
    assert!(detect_client(&row.id).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_saved_t3_home_whose_folder_is_gone_reads_as_not_installed() {
    // Removing T3 Code, or one instance's home, must not turn its saved row into a
    // config error: it reads like a Claude profile folder that was deleted.
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-gone-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/commandcode/claude")).unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {"cc": {"driver": "claudeAgent", "displayName": "CC",
            "config": {"homePath": "~/.t3/commandcode/claude"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let row = automatic(&[]).pop().expect("the T3 Code home is listed");
    locations::remember(&row).unwrap();
    std::fs::remove_dir_all(home.join(".t3")).unwrap();
    let client = detect_client(&row.id).expect("a saved home is still listed");
    assert_eq!(client.error, None);
    assert!(!client.app_present && !client.config_exists);
    assert!(client.location_removable, "it can be forgotten");
    // Nothing recreates the folder: writes still need a safe, existing folder.
    assert!(locations::validate_mutation(&row.id, &row.config_path).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// A network, device or traversal path is dropped by its text, so nothing ever opens,
/// resolves or mounts it (an SMB share on Windows, autofs `/net` on Linux).
#[test]
fn hostile_paths_are_dropped_before_any_file_system_call() {
    let home = fixture_home();
    let mut hostile = vec![
        "/net/host/share",
        "/dev/null",
        "/proc/self",
        "/home/other",
        "~/ok/../../other",
        "/home/fixture-other/x",
    ];
    if cfg!(windows) {
        hostile.extend([
            r"\\\\host\\share\\x",
            r"\\\\?\\UNC\\host\\share",
            r"\\\\.\\pipe\\x",
        ]);
    }
    let instances: Vec<String> = hostile
        .iter()
        .enumerate()
        .map(|(n, path)| {
            format!(r#""h{n}": {{"driver": "codex", "config": {{"homePath": "{path}"}}}}"#)
        })
        .chain([
            r#""env": {"driver": "claudeAgent", "environment": [{"name": "CLAUDE_CONFIG_DIR", "value": "/net/host/c"}]}"#.into(),
            r#""xdg": {"driver": "opencode", "environment": [{"name": "XDG_CONFIG_HOME", "value": "/net/host/o"}]}"#.into(),
            r#""ok": {"driver": "codex", "config": {"homePath": "~/ok"}}"#.into(),
        ])
        .collect();
    let text = format!(r#"{{"providerInstances": {{{}}}}}"#, instances.join(","));
    let found = instances_of(parse(text.into_bytes(), &home));
    // The defaults T3 Code adds for absent drivers have no folder of their own.
    let ids: Vec<&str> = found
        .iter()
        .filter(|i| {
            i.config_path.is_some() || i.id.starts_with('h') || i.id == "env" || i.id == "xdg"
        })
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(ids, vec!["ok"], "{found:?}");
    assert!(lexically_inside(&home.join("ok/.codex/config.toml"), &home));
}

/// Windows names are the same whatever their case, and its paths may use either slash.
/// The rule runs here on every OS; the Windows-shaped paths below run on Windows.
#[test]
fn a_home_matches_by_the_platforms_case_rule() {
    let home = fixture_home();
    let text = json_path(&home);
    let mixed = PathBuf::from(format!("{}/.Codex-Work/config.toml", text.to_uppercase()));
    assert!(inside_by_text(&mixed, &home, true));
    assert!(!inside_by_text(&mixed, &home, false));
    assert!(inside_by_text(
        &home.join(".codex-work/config.toml"),
        &home,
        false
    ));
    // By whole names, not by text: a sibling that starts the same is outside.
    assert!(!inside_by_text(
        Path::new(&format!("{text}2/x")),
        &home,
        true
    ));
    assert!(!inside_by_text(&home.join("../other/x"), &home, true));
    assert!(!inside_by_text(&home, &home, true));
    #[cfg(windows)]
    {
        let home = Path::new(r"C:\Users\Name");
        for good in [
            r"C:\Users\Name\.codex-work\config.toml",
            r"C:/Users/Name/.codex-work/config.toml",
            r"c:\users\name\.codex-work\config.toml",
        ] {
            assert!(lexically_inside(Path::new(good), home), "{good}");
        }
        for bad in [
            r"\\host\share\Users\Name\x",
            r"\\?\UNC\host\share\x",
            r"\\.\pipe\x",
            r"D:\Users\Name\x",
            r"C:\Users\Name2\x",
            r"C:\Users\Name\..\Other\x",
            r"Users\Name\x",
        ] {
            assert!(!lexically_inside(Path::new(bad), home), "{bad}");
        }
        // As T3 Code writes it, in either form, the home is kept.
        let text = r#"{"providerInstances": {
            "a": {"driver": "codex", "config": {"homePath": "C:\\Users\\Name\\.codex-work"}},
            "b": {"driver": "codex", "config": {"homePath": "c:/users/name/.codex-b"}},
            "c": {"driver": "codex", "config": {"homePath": "~/.codex-c"}}}}"#;
        let found = instances_of(parse(text.as_bytes().to_vec(), home));
        for id in ["a", "b", "c"] {
            assert!(by_id(&found, id).config_path.is_some(), "{id}");
        }
    }
}

/// Only Claude Code and Codex homes become rows. An OpenCode, Grok or Pi home T3 Code
/// names, even a plain existing folder inside home, gets none.
#[test]
fn only_claude_code_and_codex_homes_become_rows() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-kinds-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    for folder in ["oc/opencode", "grok", "pi", "cc"] {
        std::fs::create_dir_all(home.join(".t3/x").join(folder)).unwrap();
    }
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "oc": {"driver": "opencode", "environment": [{"name": "XDG_CONFIG_HOME", "value": "~/.t3/x/oc"}]},
            "grok": {"driver": "grok", "environment": [{"name": "GROK_HOME", "value": "~/.t3/x/grok"}]},
            "pi": {"driver": "pi", "environment": [{"name": "PI_CODING_AGENT_DIR", "value": "~/.t3/x/pi"}]},
            "cc": {"driver": "claudeAgent", "config": {"homePath": "~/.t3/x/cc"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    // The three are read as instances, so the test is not passing by accident.
    let found = instances();
    for id in ["oc", "grok", "pi"] {
        assert!(by_id(&found, id).config_path.is_some(), "{id}");
    }
    let types: Vec<String> = automatic(&[])
        .into_iter()
        .map(|row| row.client_type)
        .collect();
    assert_eq!(types, vec!["claude-code".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

/// The home text is checked as written, before `Path` drops `.` segments (S2).
#[test]
fn home_text_is_checked_segment_by_segment() {
    for (text, unix, windows) in [
        ("~", true, true),
        ("~/", true, true),
        ("~/.codex", true, true),
        ("~/.codex/", true, true),
        ("/home/n/work/codex", true, true),
        ("/home/n/work/./codex", false, false),
        ("/home/n/work/../codex", false, false),
        ("/home/n//codex", false, false),
        ("~/./codex", false, false),
        ("~user/codex", false, false),
        ("/home/n/a...", true, false),
        ("/home/n/codex.", true, false),
        ("/home/n/codex ", true, false),
        ("/home/n/codex:stream", true, false),
        (r"C:\Users\N\.codex", true, true),
        (r"C:\Users\N\.\codex", true, false),
        (r"C:\Users\N\\codex", true, false),
        (r"C:\Users\N\codex:ads", true, false),
        (r"C:\Users\N\codex.", true, false),
        (r"\\host\share\x", true, false),
        (r"~\.codex", false, true),
    ] {
        assert_eq!(plain_path_text(text, false), unix, "unix {text}");
        assert_eq!(plain_path_text(text, true), windows, "windows {text}");
    }
}

/// In settings, a home with `.` or empty segments drops the instance (S2).
#[test]
fn a_home_with_dot_or_empty_segments_drops_the_instance() {
    let home = fixture_home();
    let h = json_path(&home);
    let text = format!(
        r#"{{"providerInstances": {{
            "dot": {{"driver": "codex", "config": {{"homePath": "{h}/work/./codex"}}}},
            "double": {{"driver": "codex", "config": {{"homePath": "{h}/work//codex"}}}},
            "env": {{"driver": "claudeAgent", "environment": [{{"name": "CLAUDE_CONFIG_DIR", "value": "{h}/./c"}}]}},
            "ok": {{"driver": "codex", "config": {{"homePath": "{h}/work/codex"}}}}
        }}}}"#
    );
    let found = instances_of(parse(text.into_bytes(), &home));
    for id in ["dot", "double", "env"] {
        assert!(found.iter().all(|i| i.id != id), "{id}");
    }
    assert!(by_id(&found, "ok").config_path.is_some());
}

/// A link anywhere below home stops the walk there: as the home, in a middle folder,
/// or as the config file. Each target is a path that does not exist, so following the
/// link would read as missing, not as a link (S1).
#[cfg(unix)]
#[test]
fn the_walk_stops_at_the_first_link_without_following_it() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-walk-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    let nowhere = root.join("nowhere/at/all");
    std::fs::create_dir_all(home.join("mid/real")).unwrap();
    std::fs::create_dir_all(home.join("plain")).unwrap();
    std::os::unix::fs::symlink(&nowhere, home.join("as-home")).unwrap();
    std::os::unix::fs::symlink(&nowhere, home.join("mid/link")).unwrap();
    std::os::unix::fs::symlink(&nowhere, home.join("plain/.claude.json")).unwrap();
    for path in [
        home.join("as-home/.claude.json"),
        home.join("mid/link/deeper/.claude.json"),
        home.join("plain/.claude.json"),
    ] {
        assert_eq!(
            walk_below_home(&path, &home),
            HomeWalk::Link,
            "{}",
            path.display()
        );
    }
    assert_eq!(
        walk_below_home(&home.join("mid/real/.claude.json"), &home),
        HomeWalk::Plain
    );
    assert_eq!(
        walk_below_home(&home.join("gone/x/.claude.json"), &home),
        HomeWalk::Missing
    );
    assert_eq!(
        walk_below_home(&root.join("elsewhere/.claude.json"), &home),
        HomeWalk::NotPlain
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A linked home labels nothing and makes no row, wherever the link is, even when it
/// points at a folder that has a row of its own (S1).
#[cfg(unix)]
#[test]
fn a_linked_home_labels_nothing() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-linklabel-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".claude_personal")).unwrap();
    std::fs::create_dir_all(home.join(".t3/cc/claude")).unwrap();
    std::fs::write(home.join(".claude_personal/.claude.json"), "{}").unwrap();
    // As the home, in a middle folder (a link to home itself), and as the config file.
    std::os::unix::fs::symlink(home.join(".claude_personal"), home.join("linked")).unwrap();
    std::os::unix::fs::symlink(&home, home.join("mid")).unwrap();
    std::os::unix::fs::symlink(
        home.join(".claude_personal/.claude.json"),
        home.join(".t3/cc/claude/.claude.json"),
    )
    .unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "a": {"driver": "claudeAgent", "displayName": "Home", "config": {"homePath": "~/linked"}},
            "b": {"driver": "claudeAgent", "displayName": "Middle", "config": {"homePath": "~/mid/.claude_personal"}},
            "c": {"driver": "claudeAgent", "displayName": "File", "config": {"homePath": "~/.t3/cc/claude"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    assert!(automatic(&[]).is_empty());
    let mut clients = vec![{
        let mut row = config_error_row(
            "claude-code@claude_personal",
            "p",
            home.join(".claude_personal/.claude.json")
                .display()
                .to_string(),
            String::new(),
        );
        row.error = None;
        row
    }];
    annotate(&mut clients);
    assert!(clients[0].t3_code.is_empty(), "{:?}", clients[0].t3_code);
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder added by hand and the same folder named by T3 Code are one row, however each
/// is spelled, even before the config exists (S3). On Unix the hand-added spelling goes
/// through another name for home; on Windows it differs in case and has no `\\?\`.
#[test]
fn a_hand_added_folder_and_a_t3_home_are_one_row() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-samefolder-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/cx/codex")).unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {"cx": {"driver": "codex", "config": {"homePath": "~/.t3/cx/codex"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    #[cfg(unix)]
    let by_hand = {
        std::os::unix::fs::symlink(&home, root.join("alias")).unwrap();
        root.join("alias/.t3/cx/codex")
    };
    #[cfg(windows)]
    let by_hand = PathBuf::from(
        home.join(".t3/cx/codex")
            .display()
            .to_string()
            .to_uppercase(),
    );
    assert!(!home.join(".t3/cx/codex/config.toml").exists());
    // Saved earlier under its own spelling, as a hand-added folder is kept.
    locations::remember(&locations::ClientLocation {
        id: "codex@byhand".into(),
        manual: true,
        client_type: "codex".into(),
        name: "Codex (by hand)".into(),
        config_path: by_hand.join("config.toml"),
    })
    .unwrap();
    let rows: Vec<_> = locations::all()
        .into_iter()
        .filter(|row| {
            row.config_path.ends_with("config.toml")
                && row
                    .config_path
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("cx")
        })
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].manual);
    let _ = std::fs::remove_dir_all(&root);
}

/// Paths are compared as text with the platform's rules: on Windows the verbatim form
/// canonicalizing gives (`\\?\C:\`, `\\?\UNC\`) matches the ordinary form a Clients row
/// shows, and case does not matter. Nothing on disk is looked at (S4).
#[test]
fn verbatim_and_ordinary_windows_paths_compare_equal_as_text() {
    let key = |text: &str| path_key(text, true);
    for (a, b) in [
        (
            r"\\?\C:\Users\N\agents\cx\config.toml",
            r"C:\Users\N\agents\cx\config.toml",
        ),
        (
            r"\\?\C:\Users\N\agents\cx\config.toml",
            "c:/users/n/agents/cx/config.toml",
        ),
        (
            r"\\?\UNC\srv\share\N\.codex\config.toml",
            r"\\srv\share\N\.codex\config.toml",
        ),
        (r"C:\Users\N\cx\", r"C:\Users\N\cx"),
    ] {
        assert_eq!(key(a), key(b), "{a} and {b}");
    }
    for (a, b) in [
        (r"\\?\C:\Users\N\cx", r"D:\Users\N\cx"),
        (r"\\srv\share\x", r"\srv\share\x"),
        (r"\\?\UNC\srv\share\x", r"C:\srv\share\x"),
        (r"C:\Users\N\cx", r"C:\Users\N\cx2"),
    ] {
        assert_ne!(key(a), key(b), "{a} and {b}");
    }
    // Unix keeps case and is not changed by a doubled or trailing slash.
    assert_eq!(
        path_key("/home/n//cx/", false),
        path_key("/home/n/cx", false)
    );
    assert_ne!(path_key("/home/n/CX", false), path_key("/home/n/cx", false));
}

/// A newly found T3 Code row is labelled with its instance, whether its config is written
/// yet or not. On Windows this goes through a verbatim path on one side and the ordinary
/// path the row shows on the other (S4).
#[test]
fn a_new_t3_code_row_is_labelled_with_its_instance() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-rowlabel-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join("agents/cx")).unwrap();
    std::fs::create_dir_all(home.join("agents/cc")).unwrap();
    std::fs::write(home.join("agents/cc/.claude.json"), "{}").unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "cx": {"driver": "codex", "displayName": "Unwritten", "config": {"homePath": "~/agents/cx"}},
            "cc": {"driver": "claudeAgent", "displayName": "Written", "config": {"homePath": "~/agents/cc"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let clients = detect_clients_for_skills();
    for (kind, name) in [("codex@t3-", "Unwritten"), ("claude-code@t3-", "Written")] {
        let row = clients
            .iter()
            .find(|client| client.id.starts_with(kind))
            .unwrap_or_else(|| panic!("{kind} row"));
        assert_eq!(
            row.t3_code
                .iter()
                .map(|use_| use_.display_name.as_str())
                .collect::<Vec<_>>(),
            vec![name],
            "{}",
            row.config_path
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A link below home is not followed into a row, even to a folder inside home.
#[cfg(unix)]
#[test]
fn a_link_below_home_does_not_become_a_row() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-link-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join("real-claude")).unwrap();
    std::os::unix::fs::symlink(home.join("real-claude"), home.join("link")).unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "linked": {"driver": "claudeAgent", "config": {"homePath": "~/link"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    assert!(automatic(&[]).is_empty());
    // The folder itself, named plainly, is a row.
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "plain": {"driver": "claudeAgent", "config": {"homePath": "~/real-claude"}}}}"#,
    )
    .unwrap();
    let real = std::fs::canonicalize(&home).unwrap();
    let rows: Vec<_> = automatic(&[])
        .into_iter()
        .map(|row| row.config_path)
        .collect();
    assert_eq!(rows, vec![real.join("real-claude/.claude.json")]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Connect looks a client up by id; it must find exactly what the page lists, so a T3
/// Code home the page folds into a profile row is never a second way to that file.
#[test]
fn detect_client_finds_only_what_the_page_lists() {
    let _lock = crate::registry::data_dir_test_lock();
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-detect-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".claude_personal")).unwrap();
    std::fs::write(home.join(".claude_personal/.claude.json"), "{}").unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {"p": {"driver": "claudeAgent",
            "config": {"homePath": "~/.claude_personal"}}}}"#,
    )
    .unwrap();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let real = std::fs::canonicalize(&home).unwrap();
    let t3_id = location_id("claude-code", &real.join(".claude_personal/.claude.json"));
    let listed: Vec<String> = machine_defs(true)
        .iter()
        .map(|def| def.client_id().to_string())
        .collect();
    assert!(listed.iter().any(|id| id == "claude-code@claude_personal"));
    assert!(!listed.contains(&t3_id), "the profile row covers it");
    for id in [t3_id.as_str(), "claude-code@claude_personal", "claude-code"] {
        assert_eq!(
            detect_client(id).is_some(),
            listed.iter().any(|listed| listed == id),
            "{id}"
        );
    }
    assert!(crate::registry_controller::preview_client_setup(&t3_id).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hostile_settings_cannot_reach_outside_home_or_t3_codes_folder() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-hostile-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    // Stands in for /etc or another user's home: a real folder outside this home.
    let outside = root.join("someone-else");
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join("ok")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    // The lock first: an unreadable file is noted in Toolport's log, in a scratch folder.
    let _lock = crate::registry::data_dir_test_lock();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let o = json_path(&outside);
    let write = |text: &str| {
        let path = home.join(".t3/userdata/settings.json");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, text).unwrap();
    };
    write(&format!(
        r#"{{"providerInstances": {{
            "outside": {{"driver": "claudeAgent", "config": {{"homePath": "{o}"}}}},
            "etc": {{"driver": "claudeAgent", "config": {{"homePath": "/etc"}}}},
            "traversal": {{"driver": "claudeAgent", "config": {{"homePath": "~/ok/../../someone-else"}}}},
            "relative": {{"driver": "codex", "config": {{"homePath": "../someone-else"}}}},
            "inside": {{"driver": "claudeAgent", "config": {{"homePath": "~/ok"}}}}
        }}}}"#
    ));
    let rows = automatic(&[]);
    let real = std::fs::canonicalize(&home).unwrap();
    // Only the folder inside home becomes a row Toolport could write to.
    assert_eq!(
        rows.iter()
            .map(|row| row.config_path.clone())
            .collect::<Vec<_>>(),
        vec![real.join("ok/.claude.json")]
    );
    for path in [
        outside.join(".claude.json"),
        PathBuf::from("/etc/.claude.json"),
        home.join("ok/../../someone-else/.claude.json"),
    ] {
        assert!(!path_is_safe(&path), "{}", path.display());
        let row = locations::ClientLocation {
            id: location_id("claude-code", &path),
            manual: false,
            client_type: "claude-code".into(),
            name: "x".into(),
            config_path: path.clone(),
        };
        // A saved row with that path is refused before any write.
        assert!(locations::validate(&row).is_err(), "{}", path.display());
    }

    // Deep nesting is skipped without a crash: that instance is dropped and nothing of it
    // is kept. Nested past serde's limit outside an instance, the file is unreadable.
    write(&format!(
        r#"{{"providerInstances": {{"x": {}1{}}}}}"#,
        "[".repeat(200_000),
        "]".repeat(200_000)
    ));
    match &*current() {
        Reading::Instances(found) => assert!(found.iter().all(|i| i.id != "x")),
        other => panic!("expected instances, got {other:?}"),
    }
    write(&format!("{}1{}", "[".repeat(200_000), "]".repeat(200_000)));
    assert_eq!(*current(), Reading::Unreadable);
    write(&" ".repeat((SETTINGS_READ_LIMIT + 1) as usize));
    assert_eq!(*current(), Reading::Unreadable);

    #[cfg(unix)]
    {
        let settings = home.join(".t3/userdata/settings.json");
        // A link out of T3 Code's folder is not followed, even to a valid file.
        std::fs::write(
            outside.join("settings.json"),
            r#"{"providerInstances": {"p": {"driver": "claudeAgent"}}}"#,
        )
        .unwrap();
        let _ = std::fs::remove_file(&settings);
        std::os::unix::fs::symlink(outside.join("settings.json"), &settings).unwrap();
        assert_eq!(*current(), Reading::Unreadable);
        // A link that stays inside it is read.
        std::fs::write(
            home.join(".t3/userdata/real.json"),
            r#"{"providerInstances": {"p": {"driver": "claudeAgent", "displayName": "Inside"}}}"#,
        )
        .unwrap();
        std::fs::remove_file(&settings).unwrap();
        std::os::unix::fs::symlink(home.join(".t3/userdata/real.json"), &settings).unwrap();
        assert!(
            matches!(&*current(), Reading::Instances(found) if found.iter().any(|i| i.display_name == "Inside"))
        );
        // A dangling link is unreadable, not "no settings yet".
        std::fs::remove_file(&settings).unwrap();
        std::os::unix::fs::symlink(root.join("nothing.json"), &settings).unwrap();
        assert_eq!(*current(), Reading::Unreadable);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The settings file, or its folder, swapped for a link out of T3 Code's folder after
/// every check but before the open is not read: what is checked is the file actually
/// opened.
#[cfg(unix)]
#[test]
fn a_swap_between_the_check_and_the_open_is_not_read() {
    let root = std::env::temp_dir().join(format!(
        "toolport-t3-swap-{}-{}",
        std::process::id(),
        epoch_millis()
    ));
    let home = root.join("home");
    let outside = root.join("someone-else");
    let userdata = home.join(".t3/userdata");
    let settings = userdata.join("settings.json");
    std::fs::create_dir_all(&userdata).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("settings.json"),
        r#"{"providerInstances": {"p": {"driver": "claudeAgent", "displayName": "Outside"}}}"#,
    )
    .unwrap();
    let _lock = crate::registry::data_dir_test_lock();
    let _data = crate::registry::DataDirOverride::set(root.join("data"));
    let _home = TestHome::set(&home);
    let reads_outside = || match &*current() {
        Reading::Instances(found) => found.iter().any(|i| i.display_name == "Outside"),
        _ => false,
    };
    let inside = |padding: usize| {
        format!(
            r#"{{"providerInstances": {{"p": {{"driver": "claudeAgent", "displayName": "Inside"}}}}}}{}"#,
            " ".repeat(padding)
        )
    };
    let set_hook =
        |hook: Box<dyn FnMut()>| BEFORE_OPEN.with(|cell| *cell.borrow_mut() = Some(hook));
    let clear_hook = || BEFORE_OPEN.with(|cell| *cell.borrow_mut() = None);

    // The file itself becomes a link to a file outside.
    std::fs::write(&settings, inside(1)).unwrap();
    let (from, to) = (settings.clone(), outside.join("settings.json"));
    let mut once = true;
    set_hook(Box::new(move || {
        if std::mem::take(&mut once) {
            std::fs::remove_file(&from).unwrap();
            std::os::unix::fs::symlink(&to, &from).unwrap();
        }
    }));
    let file_swap = reads_outside();
    clear_hook();
    assert!(!file_swap, "a file swapped for an outside link was read");
    assert_eq!(*current(), Reading::Unreadable);

    // Its folder becomes a link to a folder outside.
    std::fs::remove_file(&settings).unwrap();
    std::fs::write(&settings, inside(2)).unwrap();
    let (folder, moved, target) = (userdata.clone(), home.join(".t3/moved"), outside.clone());
    let mut once = true;
    set_hook(Box::new(move || {
        if std::mem::take(&mut once) {
            std::fs::rename(&folder, &moved).unwrap();
            std::os::unix::fs::symlink(&target, &folder).unwrap();
        }
    }));
    let folder_swap = reads_outside();
    clear_hook();
    assert!(
        !folder_swap,
        "a folder swapped for an outside link was read"
    );
    std::fs::remove_file(&userdata).unwrap();
    std::fs::rename(home.join(".t3/moved"), &userdata).unwrap();

    // A file swapped for a FIFO neither hangs the scan nor counts as settings.
    std::fs::remove_file(&settings).unwrap();
    std::fs::write(&settings, inside(3)).unwrap();
    let fifo = settings.clone();
    let mut once = true;
    set_hook(Box::new(move || {
        if std::mem::take(&mut once) {
            std::fs::remove_file(&fifo).unwrap();
            let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: a valid NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }
    }));
    let after_fifo = current();
    clear_hook();
    assert_eq!(*after_fifo, Reading::Unreadable);

    // Without the handle's own path, the name is resolved again after the open and must
    // be the very file that was opened.
    std::fs::remove_file(&settings).unwrap();
    std::fs::write(&settings, inside(4)).unwrap();
    let real_root = std::fs::canonicalize(home.join(".t3")).unwrap();
    let opened = std::fs::File::open(&settings).unwrap();
    assert!(same_file_inside(&opened, &settings, &real_root));
    std::fs::remove_file(&settings).unwrap();
    std::os::unix::fs::symlink(outside.join("settings.json"), &settings).unwrap();
    assert!(!same_file_inside(&opened, &settings, &real_root));
    std::fs::remove_file(&settings).unwrap();
    std::fs::write(&settings, inside(5)).unwrap();
    assert!(
        !same_file_inside(&opened, &settings, &real_root),
        "a new file under the same name is not the one opened"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A Codex home T3 Code names, connected through its own row, gets the same Codex
/// settings as the main Codex config, and the main config is not touched.
#[test]
fn a_t3_codex_home_gets_the_codex_settings_when_connected() {
    let data = crate::registry::DataDirTestEnv::new("t3-codex-settings");
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let _home = TestHome::set(&data.dir);
    std::fs::write(data.dir.join("toolport-gateway"), "fixture").unwrap();
    let home = data.dir.clone();
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/cx/codex")).unwrap();
    std::fs::write(home.join(".t3/cx/codex/config.toml"), "model = \"gpt-5\"\n").unwrap();
    std::fs::write(
        home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {"cx": {"driver": "codex", "config": {"homePath": "~/.t3/cx/codex"}}}}"#,
    )
    .unwrap();
    let row = automatic(&[])
        .pop()
        .expect("the T3 Code Codex home is a row");
    assert!(row.id.starts_with("codex@t3-"), "{}", row.id);
    install_gateway_for(&row.id, None, None).unwrap();
    let text = std::fs::read_to_string(&row.config_path).unwrap();
    let doc: toml::Table = toml::from_str(&text).unwrap();
    let entry = doc["mcp_servers"]["toolport"].as_table().unwrap();
    assert_eq!(entry["supports_parallel_tool_calls"].as_bool(), Some(true));
    assert_eq!(entry["startup_timeout_sec"].as_integer(), Some(75));
    assert_eq!(entry["tool_timeout_sec"].as_integer(), Some(600));
    assert!(!home.join(".codex/config.toml").exists());
}
