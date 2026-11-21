//! T3 Code's Antigravity instances as rows: the one file in T3 Code's private profile
//! that Toolport writes, and everything else in that profile left as it was.
use super::*;

struct Fixture {
    home: PathBuf,
    profile: PathBuf,
    _data: crate::registry::DataDirTestEnv,
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
                "agy_work": {"driver": "antigravity", "displayName": "Antigravity Work", "enabled": true}}}"#,
        )
        .unwrap();
        // SHA-256 of "agy_work", as T3 Code names the instance's private profile.
        let profile = home.join(
            ".t3/userdata/providers/antigravity/c1a9e44a482971b672b7f9a0ef35dad8f943f7d5bc73859a9de21eb7db7dc810",
        );
        Self {
            profile,
            home,
            _data: data,
            _env: env,
            _env_lock: env_lock,
            _home: guard,
        }
    }

    /// Lay the profile out as T3 Code and `agy` leave it after a first run.
    fn run_once(&self) {
        let config = self.profile.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(self.profile.join("antigravity-acp/tmp")).unwrap();
        std::fs::write(
            self.profile.join("antigravity-acp/settings.json"),
            "{\"t3\":true}\n",
        )
        .unwrap();
        std::fs::write(config.join("config.json"), "{}\n").unwrap();
        std::fs::create_dir_all(self.home.join(".gemini/skills")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(self.home.join(".gemini/skills"), config.join("skills"))
            .unwrap();
    }

    fn row(&self) -> locations::ClientLocation {
        locations::all()
            .into_iter()
            .find(|row| row.client_type == "antigravity" && locations::is_t3_home(row))
            .expect("an Antigravity row")
    }
}

/// Every file and link in the profile with its bytes or target, `mcp_config.json` aside.
fn snapshot(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if path
                .file_name()
                .is_some_and(|name| name == "mcp_config.json")
            {
                continue;
            }
            if meta.file_type().is_symlink() {
                out.push((
                    path.clone(),
                    format!("-> {:?}", std::fs::read_link(&path).unwrap()),
                ));
            } else if meta.is_dir() {
                out.push((path.clone(), "dir".into()));
                stack.push(path);
            } else {
                out.push((path.clone(), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_profile_path_is_the_sha256_of_the_instance_id() {
    let fx = Fixture::new("t3-agy-hash");
    let expected = fx.home.join(format!(
        ".t3/userdata/providers/antigravity/{}/config/mcp_config.json",
        crate::registry::sha256_hex("agy_work")
    ));
    assert_eq!(
        antigravity_config_path(&fx.home, "agy_work"),
        Some(expected.clone())
    );
    assert_eq!(
        expected.parent().unwrap().parent().unwrap(),
        fx.profile.as_path()
    );
}

#[test]
fn an_antigravity_instance_becomes_a_row_once_t3_code_has_run_it() {
    let fx = Fixture::new("t3-agy-row");
    // Before its first run the card says to start it once; there is no row.
    assert!(automatic(&[]).is_empty());
    let others = other_instances();
    assert_eq!(others.len(), 1, "{others:?}");
    assert_eq!(others[0].gap, T3CodeGap::MissingHome);
    assert_eq!(others[0].display_name, "Antigravity Work");
    fx.run_once();
    let rows = automatic(&[]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert!(row.id.starts_with("antigravity@t3-") && is_location_id(&row.id));
    assert_eq!(
        row.config_path,
        std::fs::canonicalize(fx.profile.join("config"))
            .unwrap()
            .join("mcp_config.json")
    );
    assert_eq!(
        row.name,
        "Antigravity (~/.t3/userdata/providers/antigravity/c1a9e44a/config)"
    );
    assert!(other_instances().is_empty(), "{:?}", other_instances());
    let client = detect_clients_for_skills()
        .into_iter()
        .find(|client| client.id == row.id)
        .unwrap();
    assert_eq!(client.t3_code.len(), 1);
    assert_eq!(client.t3_code[0].display_name, "Antigravity Work");
    // The user's own Antigravity row is not labelled with this instance.
    let main = detect_clients_for_skills()
        .into_iter()
        .find(|client| client.id == "antigravity");
    assert!(main.is_none_or(|main| main.t3_code.is_empty()));
}

#[test]
fn connect_and_disconnect_touch_only_mcp_config_json() {
    let fx = Fixture::new("t3-agy-write");
    fx.run_once();
    let config = fx.profile.join("config/mcp_config.json");
    let original = "{\n  \"mcpServers\": {\n    \"notes\": {\"command\": \"notes\", \"disabled\": true}\n  }\n}\n";
    std::fs::write(&config, original).unwrap();
    let before = snapshot(&fx.profile);
    let row = fx.row();
    install_gateway_for(&row.id, None, None).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(doc["mcpServers"]["notes"]["disabled"], true);
    let entry = &doc["mcpServers"]["toolport"];
    let expected_bin = format!("toolport-gateway{}", std::env::consts::EXE_SUFFIX);
    assert!(entry["command"].as_str().unwrap().ends_with(&expected_bin));
    assert!(entry["env"]["TOOLPORT_CLIENT_ID"]
        .as_str()
        .unwrap()
        .starts_with("antigravity@t3-"));
    assert_eq!(
        entry["env"].get("TOOLPORT_DATA_DIR").is_some(),
        cfg!(all(unix, not(target_os = "macos")))
    );
    assert_eq!(
        snapshot(&fx.profile),
        before,
        "nothing else in the profile changed"
    );
    assert!(
        !fx.home.join(".gemini/config").exists() && !fx.home.join(".gemini/GEMINI.md").exists()
    );

    // Disconnect with no edits since puts the bytes back.
    uninstall_gateway(&row.id).unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    install_gateway_for(&row.id, None, None).unwrap();
    // `agy` turns the entry off in place: a rewrite keeps that, and it is still ours.
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    doc["mcpServers"]["toolport"]["disabled"] = serde_json::json!(true);
    std::fs::write(&config, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let client = detect_client(&row.id).unwrap();
    assert_ne!(client.entry_state, GatewayEntryState::Customized);
    install_gateway_for(&row.id, Some("work"), None).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(doc["mcpServers"]["toolport"]["disabled"], true);

    // The file was edited since Toolport wrote it (as agy would), so Disconnect puts its
    // content back rather than its bytes: Toolport's entry, `disabled` and all, is gone.
    uninstall_gateway(&row.id).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        after,
        serde_json::from_str::<serde_json::Value>(original).unwrap()
    );
    assert_eq!(snapshot(&fx.profile), before);
}

#[test]
fn a_linked_config_folder_or_a_removed_profile_is_left_alone() {
    let fx = Fixture::new("t3-agy-safety");
    // A `config` folder that is a link makes no row, and the card says why.
    #[cfg(unix)]
    {
        std::fs::create_dir_all(&fx.profile).unwrap();
        std::fs::create_dir_all(fx.home.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(fx.home.join("elsewhere"), fx.profile.join("config")).unwrap();
        assert!(automatic(&[]).is_empty());
        assert_eq!(other_instances()[0].gap, T3CodeGap::LinkedHome);
        std::fs::remove_file(fx.profile.join("config")).unwrap();
    }
    fx.run_once();
    let row = fx.row();
    install_gateway_for(&row.id, None, None).unwrap();
    // T3 Code removed the instance's profile: nothing recreates it.
    std::fs::remove_dir_all(&fx.profile).unwrap();
    assert!(install_gateway_for(&row.id, None, None).is_err());
    assert!(!fx.profile.exists());
}
