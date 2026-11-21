//! T3 Code's OpenCode, Grok and Pi homes as rows: which file each row writes, in what
//! shape, and that connecting and disconnecting one leaves the rest of the file alone.
use super::*;

/// A scratch home with T3 Code settings naming one OpenCode, Grok and Pi home each,
/// and a fixture gateway binary. Holds the data-dir and environment locks.
struct Fixture {
    home: PathBuf,
    data: crate::registry::DataDirTestEnv,
    _env: Vec<EnvRestore>,
    _env_lock: std::sync::MutexGuard<'static, ()>,
    _home: TestHome,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let data = crate::registry::DataDirTestEnv::new(label);
        let env_lock = env_test_lock();
        let env = isolated_client_env(&data.dir);
        let home = data.dir.join("home");
        let guard = TestHome::set(&home);
        std::fs::write(data.dir.join("toolport-gateway"), "fixture").unwrap();
        std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
        std::fs::write(
            home.join(".t3/userdata/settings.json"),
            r#"{"providerInstances": {
                "oc": {"driver": "opencode", "displayName": "ClinePass",
                  "environment": [{"name": "XDG_CONFIG_HOME", "value": "~/.t3/hp/oc/config"}]},
                "grok": {"driver": "grok", "displayName": "Grok Alt", "enabled": true,
                  "environment": [{"name": "GROK_HOME", "value": "~/.t3/hp/grok"}]},
                "pi": {"driver": "pi", "displayName": "Pi Work", "enabled": true,
                  "environment": [{"name": "PI_CODING_AGENT_DIR", "value": "~/.t3/hp/pi"}]}}}"#,
        )
        .unwrap();
        Self {
            home,
            data,
            _env: env,
            _env_lock: env_lock,
            _home: guard,
        }
    }

    fn folder(&self, provider: &str) -> PathBuf {
        let folder = match provider {
            "opencode" => self.home.join(".t3/hp/oc/config/opencode"),
            "grok" => self.home.join(".t3/hp/grok"),
            _ => self.home.join(".t3/hp/pi"),
        };
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// The row for the provider's home, saved or not.
    fn row(&self, client_type: &str) -> locations::ClientLocation {
        locations::all()
            .into_iter()
            .filter(locations::is_t3_home)
            .find(|row| row.client_type == client_type)
            .unwrap_or_else(|| panic!("no {client_type} row"))
    }

    fn gaps(&self) -> Vec<(String, T3CodeGap)> {
        other_instances()
            .into_iter()
            .map(|other| (other.instance_id, other.gap))
            .collect()
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn json(path: &Path) -> serde_json::Value {
    mutation::value(Format::JsonMcpServers, Some(&read(path))).unwrap()
}

#[test]
fn opencode_grok_and_pi_homes_become_rows_once_their_folders_exist() {
    let fx = Fixture::new("t3-homes-rows");
    // No folder yet: no rows, and each home says it doesn't exist yet.
    assert!(automatic(&[]).is_empty());
    let mut gaps = fx.gaps();
    gaps.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        gaps,
        vec![
            ("grok".into(), T3CodeGap::MissingHome),
            ("oc".into(), T3CodeGap::MissingHome),
            ("pi".into(), T3CodeGap::MissingHome),
        ]
    );
    // An XDG_CONFIG_HOME without its `opencode` folder is not OpenCode's home yet.
    std::fs::create_dir_all(fx.home.join(".t3/hp/oc/config")).unwrap();
    assert!(automatic(&[]).is_empty());

    let real = std::fs::canonicalize(&fx.home).unwrap();
    for provider in ["opencode", "grok", "pi"] {
        fx.folder(provider);
    }
    let rows: Vec<(String, PathBuf, String)> = automatic(&[])
        .into_iter()
        .map(|row| {
            assert!(is_location_id(&row.id), "{}", row.id);
            assert!(row.id.starts_with(&format!("{}@t3-", row.client_type)));
            assert!(!row.manual);
            (row.client_type, row.config_path, row.name)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "grok".into(),
                real.join(".t3/hp/grok/config.toml"),
                "Grok Build (~/.t3/hp/grok)".into()
            ),
            (
                "opencode".into(),
                real.join(".t3/hp/oc/config/opencode/opencode.json"),
                "OpenCode (~/.t3/hp/oc/config/opencode)".into()
            ),
            (
                "pi".into(),
                real.join(".t3/hp/pi/mcp.json"),
                "Pi (~/.t3/hp/pi)".into()
            ),
        ]
    );
    // The card no longer lists them, and each row is labelled with its instance.
    assert_eq!(fx.gaps(), vec![]);
    for (client_type, name) in [
        ("grok", "Grok Alt"),
        ("opencode", "ClinePass"),
        ("pi", "Pi Work"),
    ] {
        let id = fx.row(client_type).id;
        let client = detect_clients_for_skills()
            .into_iter()
            .find(|client| client.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"));
        assert_eq!(
            client
                .t3_code
                .iter()
                .map(|u| u.display_name.as_str())
                .collect::<Vec<_>>(),
            vec![name],
            "{client_type}"
        );
        assert!(
            client.app_present && client.error.is_none(),
            "{client_type}"
        );
    }
    let _ = &fx.data;
}

#[test]
fn a_home_listed_another_way_is_one_row() {
    let fx = Fixture::new("t3-homes-dedupe");
    for provider in ["opencode", "grok", "pi"] {
        fx.folder(provider);
    }
    // Added by hand before T3 Code named it: the saved location stays the one row.
    let settings = fx.home.join(".t3/userdata/settings.json");
    let listed = read(&settings);
    std::fs::write(&settings, r#"{"providerInstances": {}}"#).unwrap();
    let manual = add_client_location("grok", fx.folder("grok").to_str().unwrap()).unwrap();
    assert!(!is_location_id(&manual), "{manual}");
    std::fs::write(&settings, listed).unwrap();
    let saved = locations::saved().unwrap();
    let rows = automatic(&saved);
    assert!(rows.iter().all(|row| row.client_type != "grok"), "{rows:?}");
    assert_eq!(
        locations::all()
            .iter()
            .filter(|row| row.client_type == "grok")
            .count(),
        1
    );
    assert!(fx.gaps().is_empty(), "{:?}", fx.gaps());
    // Listed by T3 Code twice (two instances, one home): one row.
    std::fs::write(
        fx.home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "a": {"driver": "pi", "environment": [{"name": "PI_CODING_AGENT_DIR", "value": "~/.t3/hp/pi"}]},
            "b": {"driver": "pi", "environment": [{"name": "PI_CODING_AGENT_DIR", "value": "~/.t3/hp/pi/"}]}}}"#,
    )
    .unwrap();
    assert_eq!(automatic(&[]).len(), 1);
    // The client's own default home is its main row, not a second one.
    std::fs::create_dir_all(fx.home.join(".pi/agent")).unwrap();
    std::fs::write(
        fx.home.join(".t3/userdata/settings.json"),
        r#"{"providerInstances": {
            "a": {"driver": "pi", "environment": [{"name": "PI_CODING_AGENT_DIR", "value": "~/.pi/agent"}]}}}"#,
    )
    .unwrap();
    if pi_path()
        .is_some_and(|main| locations::same_path(&main, &fx.home.join(".pi/agent/mcp.json")))
    {
        assert!(automatic(&[]).is_empty());
        assert_eq!(fx.gaps(), vec![]);
    }
}

#[test]
fn each_row_writes_its_providers_file_in_its_shape_and_names_the_data_folder() {
    let fx = Fixture::new("t3-homes-write");
    let pinned = cfg!(all(unix, not(target_os = "macos")));
    let data_dir = fx.data.dir.to_str().unwrap().to_string();

    // OpenCode: `opencode.json`, `mcp.toolport` local entry, other keys kept.
    let oc = fx.folder("opencode");
    std::fs::write(
        oc.join("opencode.json"),
        r#"{"$schema":"https://opencode.ai/config.json","model":"x/y","mcp":{"notes":{"type":"local","command":["notes"]}}}"#,
    )
    .unwrap();
    // Grok: `config.toml`, `[mcp_servers.toolport]`.
    let grok = fx.folder("grok");
    std::fs::write(grok.join("config.toml"), "# mine\nmodel = \"grok-5\"\n").unwrap();
    // Pi: `mcp.json`, `mcpServers.toolport`; the file does not exist yet.
    let pi = fx.folder("pi");

    for client_type in ["opencode", "grok", "pi"] {
        let row = fx.row(client_type);
        install_gateway_for(&row.id, None, None).unwrap();
        let client = detect_client(&row.id).unwrap();
        assert!(client.gateway_installed, "{client_type}");
        assert_ne!(
            client.entry_state,
            GatewayEntryState::Customized,
            "{client_type}"
        );
    }

    let expected_bin = format!("toolport-gateway{}", std::env::consts::EXE_SUFFIX);
    let oc_doc = json(&oc.join("opencode.json"));
    assert_eq!(oc_doc["model"], "x/y");
    assert_eq!(oc_doc["mcp"]["notes"]["command"][0], "notes");
    let entry = &oc_doc["mcp"]["toolport"];
    assert_eq!(entry["type"], "local");
    assert!(entry["command"][0]
        .as_str()
        .unwrap()
        .ends_with(&expected_bin));
    assert!(entry["environment"]["TOOLPORT_CLIENT_ID"]
        .as_str()
        .unwrap()
        .starts_with("opencode@t3-"));
    assert_eq!(
        entry["environment"].get("TOOLPORT_DATA_DIR").is_some(),
        pinned
    );
    if pinned {
        assert_eq!(entry["environment"]["TOOLPORT_DATA_DIR"], data_dir.as_str());
    }

    let grok_text = read(&grok.join("config.toml"));
    assert!(
        grok_text.starts_with("# mine\nmodel = \"grok-5\"\n"),
        "{grok_text}"
    );
    let grok_doc: toml::Table = toml::from_str(&grok_text).unwrap();
    let entry = grok_doc["mcp_servers"]["toolport"].as_table().unwrap();
    assert!(entry["command"].as_str().unwrap().ends_with(&expected_bin));
    assert_eq!(entry["env"].get("TOOLPORT_DATA_DIR").is_some(), pinned);

    let pi_doc = json(&pi.join("mcp.json"));
    let entry = &pi_doc["mcpServers"]["toolport"];
    assert!(entry["command"].as_str().unwrap().ends_with(&expected_bin));
    assert!(entry["env"]["TOOLPORT_CLIENT_ID"]
        .as_str()
        .unwrap()
        .starts_with("pi@t3-"));
    assert_eq!(entry["env"].get("TOOLPORT_DATA_DIR").is_some(), pinned);
    // Nothing went to the providers' default homes.
    assert!(!fx.home.join(".grok").exists() && !fx.home.join(".pi").exists());
}

#[test]
fn an_opencode_home_with_only_jsonc_is_written_there_with_its_comments() {
    let fx = Fixture::new("t3-homes-jsonc");
    let oc = fx.folder("opencode");
    let original = "{\n  // my model\n  \"model\": \"x/y\",\n}\n";
    // A link in place of the `.jsonc` makes no row, and the card says why.
    #[cfg(unix)]
    {
        std::fs::write(fx.home.join("real.jsonc"), original).unwrap();
        std::os::unix::fs::symlink(fx.home.join("real.jsonc"), oc.join("opencode.jsonc")).unwrap();
        assert!(automatic(&[]).iter().all(|r| r.client_type != "opencode"));
        assert!(fx
            .gaps()
            .contains(&("oc".to_string(), T3CodeGap::LinkedHome)));
        std::fs::remove_file(oc.join("opencode.jsonc")).unwrap();
    }
    std::fs::write(oc.join("opencode.jsonc"), original).unwrap();
    let row = fx.row("opencode");
    install_gateway_for(&row.id, None, None).unwrap();
    assert!(!oc.join("opencode.json").exists());
    let text = read(&oc.join("opencode.jsonc"));
    assert!(text.contains("// my model"), "{text}");
    assert!(text.contains("\"toolport\""), "{text}");
    uninstall_gateway(&row.id).unwrap();
    assert_eq!(read(&oc.join("opencode.jsonc")), original);
}

#[test]
fn disconnect_puts_each_file_back_as_it_was() {
    let fx = Fixture::new("t3-homes-restore");
    let files = [
        (
            "opencode",
            fx.folder("opencode").join("opencode.json"),
            "{\n  \"model\": \"x/y\",\n  \"mcp\": {}\n}\n",
        ),
        (
            "grok",
            fx.folder("grok").join("config.toml"),
            "# mine\nmodel = \"grok-5\"\n\n[mcp_servers.notes]\ncommand = \"notes\"\n",
        ),
        (
            "pi",
            fx.folder("pi").join("mcp.json"),
            "{\n  \"autoEnableCodemode\": true,\n  \"mcpServers\": {}\n}\n",
        ),
    ];
    for (client_type, path, original) in &files {
        std::fs::write(path, original).unwrap();
        let row = fx.row(client_type);
        install_gateway_for(&row.id, None, None).unwrap();
        assert_ne!(read(path), *original, "{client_type}");
        uninstall_gateway(&row.id).unwrap();
        assert_eq!(read(path), *original, "{client_type}");
    }
    // A file Toolport created is removed again.
    let pi = fx.folder("pi").join("mcp.json");
    std::fs::remove_file(&pi).unwrap();
    let row = fx.row("pi");
    install_gateway_for(&row.id, None, None).unwrap();
    uninstall_gateway(&row.id).unwrap();
    assert!(!pi.exists() || !read(&pi).contains("toolport"));
}

#[test]
fn a_config_toolport_cannot_parse_is_refused_and_left_alone() {
    let fx = Fixture::new("t3-homes-unparseable");
    for (client_type, path, broken) in [
        (
            "opencode",
            fx.folder("opencode").join("opencode.json"),
            "{ \"model\": ",
        ),
        (
            "grok",
            fx.folder("grok").join("config.toml"),
            "model = = \"x\"\n",
        ),
        (
            "pi",
            fx.folder("pi").join("mcp.json"),
            "{\"mcpServers\": [1,",
        ),
    ] {
        std::fs::write(&path, broken).unwrap();
        let row = fx.row(client_type);
        assert!(
            detect_client(&row.id).unwrap().error.is_some(),
            "{client_type}"
        );
        assert!(
            install_gateway_for(&row.id, None, None).is_err(),
            "{client_type}"
        );
        assert_eq!(read(&path), broken, "{client_type}");
    }
}

#[test]
fn pi_keeps_the_settings_it_saved_in_toolports_entry() {
    let fx = Fixture::new("t3-homes-pi-keys");
    let path = fx.folder("pi").join("mcp.json");
    let row = fx.row("pi");
    install_gateway_for(&row.id, Some("work"), None).unwrap();
    // Pi turns the entry off and changes how its tools show, in place.
    let mut doc = json(&path);
    doc["autoEnableCodemode"] = serde_json::json!(false);
    let entry = doc["mcpServers"]["toolport"].as_object_mut().unwrap();
    entry.insert("enabled".into(), serde_json::json!(false));
    entry.insert("exposure".into(), serde_json::json!("direct"));
    entry.insert(
        "toolExposure".into(),
        serde_json::json!({"toolport_search_tools": "direct"}),
    );
    entry.insert("lifecycle".into(), serde_json::json!("keep-alive"));
    entry.insert("idleTimeout".into(), serde_json::json!(30));
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    // Still Toolport's own entry.
    let def = find_def(&row.id).unwrap();
    let servers = read_client(&def).servers;
    let entry = gateway_entry(Some("work"), &row.id).unwrap();
    let record = ManagedEntry::from_gateway_entry(&entry);
    assert_eq!(
        resolve_entry_state(&servers, Some(&record)),
        GatewayEntryState::Managed
    );
    // A rewrite (an access change) keeps every one of them, and the rest of the file.
    install_gateway_for(&row.id, None, None).unwrap();
    let after = json(&path);
    let kept = &after["mcpServers"]["toolport"];
    assert_eq!(kept["enabled"], false);
    assert_eq!(kept["exposure"], "direct");
    assert_eq!(kept["toolExposure"]["toolport_search_tools"], "direct");
    assert_eq!(kept["lifecycle"], "keep-alive");
    assert_eq!(kept["idleTimeout"], 30);
    assert_eq!(after["autoEnableCodemode"], false);
    assert!(
        kept.get("TOOLPORT_PROFILE").is_none() && kept["env"].get("TOOLPORT_PROFILE").is_none()
    );
    // Disconnect takes the whole entry out, Pi's settings in it too, and keeps the
    // rest of the file as Pi left it.
    uninstall_gateway(&row.id).unwrap();
    let after = json(&path);
    assert!(after["mcpServers"].get("toolport").is_none(), "{after}");
    assert_eq!(after["autoEnableCodemode"], false);
    // Other clients' entries never get these keys from a Toolport rewrite.
    let fake = gateway_entry(None, "cursor").unwrap();
    assert!(
        with_client_settings("cursor", Format::JsonMcpServers, &path, &fake)
            .unknown_fields
            .is_empty()
    );
}

#[test]
fn a_deleted_grok_or_pi_home_stays_deleted() {
    let fx = Fixture::new("t3-homes-deleted");
    for client_type in ["grok", "pi", "opencode"] {
        fx.folder(client_type);
        let row = fx.row(client_type);
        install_gateway_for(&row.id, None, None).unwrap();
        let folder = row.config_path.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&folder).unwrap();
        let client = detect_client(&row.id).expect("a saved home is still listed");
        assert!(
            !client.app_present && client.error.is_none(),
            "{client_type}"
        );
        assert!(
            install_gateway_for(&row.id, None, None).is_err(),
            "{client_type}"
        );
        assert!(
            !folder.exists(),
            "{client_type}: nothing recreates the folder"
        );
        // Its saved row says it is not installed; the card does not list it twice.
        assert!(fx
            .gaps()
            .iter()
            .all(|(id, _)| !row.id.contains(id.as_str())));
    }
}

/// Connect `id`, let `edit` change Toolport's entry the way the provider does, then
/// Disconnect: the entry leaves and the file is as it was before Connect, byte for byte.
fn provider_edit_then_disconnect(id: &str, path: &Path, edit: impl FnOnce(&str) -> String) {
    let original = read(path);
    install_gateway_for(id, None, None).unwrap();
    let edited = edit(&read(path));
    assert_ne!(edited, read(path), "the edit changed nothing");
    std::fs::write(path, &edited).unwrap();
    assert_eq!(
        detect_client(id).unwrap().entry_state,
        GatewayEntryState::Managed
    );
    let outcome = uninstall_gateway(id).unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(read(path), original);
}

#[test]
fn a_setting_pi_saves_straight_after_connect_leaves_with_the_entry() {
    let fx = Fixture::new("t3-homes-pi-direct");
    let path = fx.folder("pi").join("mcp.json");
    std::fs::write(
        &path,
        "{\n  \"autoEnableCodemode\": true,\n  \"mcpServers\": {}\n}\n",
    )
    .unwrap();
    let row = fx.row("pi");
    // Connect, Pi turns Toolport off, Disconnect: no Toolport rewrite in between.
    provider_edit_then_disconnect(&row.id, &path, |text| {
        let mut doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["mcpServers"]["toolport"]["enabled"] = serde_json::json!(false);
        serde_json::to_string_pretty(&doc).unwrap()
    });
}

#[test]
fn a_nested_pi_setting_changed_after_a_rewrite_leaves_with_the_entry() {
    let fx = Fixture::new("t3-homes-pi-nested");
    let path = fx.folder("pi").join("mcp.json");
    let original = "{\n  \"autoEnableCodemode\": true,\n  \"mcpServers\": {}\n}\n";
    std::fs::write(&path, original).unwrap();
    let row = fx.row("pi");
    install_gateway_for(&row.id, None, None).unwrap();
    let mut doc = json(&path);
    doc["mcpServers"]["toolport"]["toolExposure"] =
        serde_json::json!({"toolport_search_tools": "direct"});
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    // An access change rewrites the entry and carries Pi's setting.
    install_gateway_for(&row.id, None, None).unwrap();
    let mut doc = json(&path);
    assert_eq!(
        doc["mcpServers"]["toolport"]["toolExposure"]["toolport_search_tools"],
        "direct"
    );
    // Pi changes one tool's exposure in place.
    doc["mcpServers"]["toolport"]["toolExposure"]["toolport_search_tools"] =
        serde_json::json!("codemode");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let outcome = uninstall_gateway(&row.id).unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(read(&path), original);
}

#[test]
fn the_ordinary_pi_row_drops_its_settings_with_the_entry_too() {
    let fx = Fixture::new("t3-homes-pi-main");
    let folder = fx.home.join(".pi/agent");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("mcp.json");
    std::fs::write(&path, "{\n  \"mcpServers\": {}\n}\n").unwrap();
    provider_edit_then_disconnect("pi", &path, |text| {
        let mut doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["mcpServers"]["toolport"]["enabled"] = serde_json::json!(false);
        doc["mcpServers"]["toolport"]["toolExposure"] =
            serde_json::json!({"toolport_search_tools": "direct"});
        serde_json::to_string_pretty(&doc).unwrap()
    });
}

#[test]
fn connecting_through_the_review_keeps_pis_settings() {
    let fx = Fixture::new("t3-homes-pi-review");
    let path = fx.folder("pi").join("mcp.json");
    let row = fx.row("pi");
    install_gateway_for(&row.id, None, None).unwrap();
    let mut doc = json(&path);
    doc["mcpServers"]["toolport"]["enabled"] = serde_json::json!(false);
    doc["mcpServers"]["toolport"]["exposure"] = serde_json::json!("direct");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let revision = setup_revision(&row.id).unwrap();
    migrate_reviewed(&row.id, None, &[], &revision, || Ok(())).unwrap();
    let after = json(&path);
    assert_eq!(after["mcpServers"]["toolport"]["enabled"], false);
    assert_eq!(after["mcpServers"]["toolport"]["exposure"], "direct");
}
