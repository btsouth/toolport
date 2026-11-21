use super::*;

const T3_HOME: &str = "opencode@t3-0123456789abcdef";

fn env_of(entry: &ServerEntry) -> HashMap<String, String> {
    entry
        .env
        .iter()
        .filter_map(|var| Some((var.key.clone(), var.value.clone()?)))
        .collect()
}

/// Detect `entry` the way a refresh does: written into a client config, parsed back.
fn detected(entry: &ServerEntry) -> Vec<McpServer> {
    let text = serde_json::json!({ "mcpServers": { "toolport": entry_to_json(entry) } });
    parse_client_content(Format::JsonMcpServers, &text.to_string()).unwrap()
}

#[test]
fn a_location_row_names_this_data_folder_and_a_main_row_does_not() {
    let data = crate::registry::DataDirTestEnv::new("pin-location");
    let pinned = env_of(&gateway_entry(None, T3_HOME).unwrap());
    let main = env_of(&gateway_entry(None, "opencode").unwrap());
    assert!(!main.contains_key(DATA_DIR_ENV), "{main:?}");
    if cfg!(all(unix, not(target_os = "macos"))) {
        assert_eq!(
            pinned.get(DATA_DIR_ENV).map(String::as_str),
            data.dir.to_str()
        );
        assert_eq!(pinned.get(crate::brand::CLIENT_ID).unwrap(), T3_HOME);
    } else {
        assert!(!pinned.contains_key(DATA_DIR_ENV), "{pinned:?}");
    }
}

#[test]
fn a_pinned_entry_is_still_toolports_own() {
    let _data = crate::registry::DataDirTestEnv::new("pin-ownership");
    let entry = gateway_entry(None, T3_HOME).unwrap();
    let record = ManagedEntry::from_gateway_entry(&entry);
    assert_eq!(
        resolve_entry_state(&detected(&entry), Some(&record)),
        GatewayEntryState::Managed
    );
    // An entry an earlier Toolport wrote without the pin, with its record.
    let mut earlier = entry.clone();
    earlier.env.retain(|var| var.key != DATA_DIR_ENV);
    let earlier_record = ManagedEntry::from_gateway_entry(&earlier);
    assert_eq!(
        resolve_entry_state(&detected(&earlier), Some(&earlier_record)),
        GatewayEntryState::Managed
    );
    // And without any record, as before ownership records existed.
    assert_eq!(
        resolve_entry_state(&detected(&entry), None),
        GatewayEntryState::Managed
    );
    // A data folder the user added by hand to an entry Toolport wrote is a change.
    if cfg!(all(unix, not(target_os = "macos"))) {
        assert_eq!(
            resolve_entry_state(&detected(&entry), Some(&earlier_record)),
            GatewayEntryState::Customized
        );
    }
}

#[test]
fn only_a_pin_this_toolport_wrote_is_rewritten() {
    let data = crate::registry::DataDirTestEnv::new("pin-stale");
    let gateway = "/usr/bin/toolport-gateway";
    let current = data.dir.to_str().unwrap().to_string();
    let json = |dir: &str| {
        serde_json::json!({
            "notes": "TOOLPORT_DATA_DIR is mentioned here, outside the entry",
            "mcpServers": { "toolport": { "command": gateway, "env": {
                "TOOLPORT_CLIENT_ID": T3_HOME, DATA_DIR_ENV: dir } } }
        })
        .to_string()
    };
    let unpinned = serde_json::json!({
        "notes": "TOOLPORT_DATA_DIR=/elsewhere",
        "mcpServers": { "toolport": { "command": gateway, "env": { "TOOLPORT_CLIENT_ID": T3_HOME } } }
    })
    .to_string();
    let wrote = |dir: &str| ManagedEntry {
        command: gateway.into(),
        env: [
            ("TOOLPORT_CLIENT_ID".to_string(), T3_HOME.to_string()),
            (DATA_DIR_ENV.to_string(), dir.to_string()),
        ]
        .into(),
        args: Vec::new(),
        transport: "stdio".into(),
        url: None,
        updated_at: 0,
        codex_settings: Vec::new(),
        unknown_fields: Default::default(),
    };
    let old = wrote("/old/Conduit");
    let format = || Some(Format::JsonMcpServers);
    let stale = |text: &str, id: &str, command: &str, record: Option<&ManagedEntry>| {
        data_dir_pin_is_stale(id, format, "toolport", command, Some(text), record)
    };
    let linux = cfg!(all(unix, not(target_os = "macos")));
    // This Toolport wrote the old folder, which was renamed since.
    assert_eq!(
        stale(&json("/old/Conduit"), T3_HOME, gateway, Some(&old)),
        linux
    );
    assert!(!stale(&json(&current), T3_HOME, gateway, Some(&old)));
    // Another Toolport on its own data folder wrote it, with no record here.
    assert!(!stale(&json("/old/Conduit"), T3_HOME, gateway, None));
    // The user changed the folder this Toolport wrote.
    assert!(!stale(
        &json("/home/me/other-toolport"),
        T3_HOME,
        gateway,
        Some(&wrote(&current))
    ));
    // A pin kept for the user is recorded as an empty value, never stale.
    assert!(!stale(
        &json("/old/Conduit"),
        T3_HOME,
        gateway,
        Some(&wrote(""))
    ));
    // Entries without a pin, main rows and entries that are not ours stay as they are.
    assert!(!stale(&unpinned, T3_HOME, gateway, Some(&old)));
    assert!(!stale(
        &json("/old/Conduit"),
        "opencode",
        gateway,
        Some(&old)
    ));
    assert!(!stale(
        &json("/old/Conduit"),
        T3_HOME,
        "/usr/local/bin/wrapper",
        Some(&old)
    ));
    let toml = format!(
        "[mcp_servers.toolport]\ncommand = \"{gateway}\"\n[mcp_servers.toolport.env]\nTOOLPORT_DATA_DIR = \"/old/Conduit\"\n"
    );
    assert_eq!(
        data_dir_pin_is_stale(
            "codex@t3-0123456789abcdef",
            || Some(Format::TomlMcpServers),
            "toolport",
            gateway,
            Some(&toml),
            Some(&old),
        ),
        linux
    );
}

#[test]
fn a_relative_data_folder_is_never_pinned() {
    let data = crate::registry::DataDirTestEnv::new("pin-relative");
    let relative = crate::registry::DataDirOverride::set("portable-data");
    let pinned = pinned_data_dir(T3_HOME);
    drop(relative);
    let _back = crate::registry::DataDirOverride::set(&data.dir);
    assert_eq!(pinned, None);
}

/// A location row connected by this Toolport, as a scan sees it with the gateway
/// installed where the entry points.
struct Connected {
    id: String,
    file: PathBuf,
    gateway: PathBuf,
    data: crate::registry::DataDirTestEnv,
    _env: Vec<EnvRestore>,
    _env_lock: std::sync::MutexGuard<'static, ()>,
    _home: TestHome,
}

impl Connected {
    fn new(label: &str) -> Self {
        let data = crate::registry::DataDirTestEnv::new(label);
        let env_lock = env_test_lock();
        let env = isolated_client_env(&data.dir);
        let home = data.dir.join("home");
        let guard = TestHome::set(&home);
        let gateway = data.dir.join("toolport-gateway");
        std::fs::write(&gateway, "fixture").unwrap();
        let folder = home.join("codex-work");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("config.toml");
        std::fs::write(&file, "model = \"gpt-6\"\n").unwrap();
        let id = add_client_location("codex", folder.to_str().unwrap()).unwrap();
        let mut record = install_gateway_for(&id, None, None)
            .unwrap()
            .managed
            .unwrap();
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace(&record.command, gateway.to_str().unwrap());
        std::fs::write(&file, text).unwrap();
        record.command = gateway.to_str().unwrap().into();
        crate::registry::update(|registry| {
            registry.set_client_managed_entry(&id, record.clone());
            Ok(())
        })
        .unwrap();
        Self {
            id,
            file,
            gateway,
            data,
            _env: env,
            _env_lock: env_lock,
            _home: guard,
        }
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.file).unwrap()
    }

    /// Draw the Clients page for this row from the current data folder.
    fn scan(&self, registry: crate::registry::Registry) -> DetectedClient {
        detect_clients_for_display_in(
            vec![find_def(&self.id).unwrap()],
            registry,
            Some(self.gateway.clone()),
        )
        .unwrap()
        .remove(0)
    }
}

#[test]
fn a_second_toolport_on_another_data_folder_leaves_the_pin() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let row = Connected::new("pin-second");
    let before = row.read();
    assert!(before.contains(row.data.dir.to_str().unwrap()), "{before}");
    row.scan(crate::registry::load().unwrap());
    assert_eq!(row.read(), before, "this Toolport's own scan");
    // A second Toolport with its own data folder knows the same location but has no
    // record of writing the entry. Opening Clients there changes nothing.
    let other = row.data.dir.join("second-instance");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(
        row.data.dir.join("client-locations.json"),
        other.join("client-locations.json"),
    )
    .unwrap();
    let second = crate::registry::DataDirOverride::set(&other);
    let shown = row.scan(crate::registry::Registry::default());
    let after = row.read();
    drop(second);
    let _back = crate::registry::DataDirOverride::set(&row.data.dir);
    assert_eq!(shown.error, None);
    assert_eq!(after, before);
}

#[test]
fn a_data_folder_set_by_hand_is_kept() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let row = Connected::new("pin-by-hand");
    let mine = "/home/me/other-toolport";
    let edited = row.read().replace(
        &format!("\"{}\"", row.data.dir.display()),
        &format!("\"{mine}\""),
    );
    assert!(edited.contains(mine));
    std::fs::write(&row.file, &edited).unwrap();
    row.scan(crate::registry::load().unwrap());
    assert_eq!(row.read(), edited, "a scan left it alone");
    // A gateway update repoints the entry and still keeps the user's folder.
    let moved = row.data.dir.join("bin/toolport-gateway");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::write(&moved, "fixture").unwrap();
    let shown = detect_clients_for_display_in(
        vec![find_def(&row.id).unwrap()],
        crate::registry::load().unwrap(),
        Some(moved.clone()),
    )
    .unwrap()
    .remove(0);
    let after = row.read();
    assert!(after.contains(moved.to_str().unwrap()), "{after}");
    assert!(after.contains(mine), "{after}");
    assert_eq!(shown.error, None);
    // And the next scan neither moves it back nor calls the entry the user's.
    let shown = detect_clients_for_display_in(
        vec![find_def(&row.id).unwrap()],
        crate::registry::load().unwrap(),
        Some(moved),
    )
    .unwrap()
    .remove(0);
    assert_eq!(row.read(), after);
    assert_eq!(shown.entry_state, GatewayEntryState::Managed);
}

#[test]
fn a_deleted_data_folder_pin_stays_deleted_across_repoint() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let row = Connected::new("pin-deleted");
    let before = row.read();
    assert!(before.contains("TOOLPORT_DATA_DIR"), "{before}");
    let edited = before
        .lines()
        .filter(|line| !line.contains("TOOLPORT_DATA_DIR"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!edited.contains("TOOLPORT_DATA_DIR"));
    std::fs::write(&row.file, &edited).unwrap();
    let moved = row.data.dir.join("bin/toolport-gateway");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::write(&moved, "fixture").unwrap();
    let shown = detect_clients_for_display_in(
        vec![find_def(&row.id).unwrap()],
        crate::registry::load().unwrap(),
        Some(moved.clone()),
    )
    .unwrap()
    .remove(0);
    let after = row.read();
    assert_eq!(after, edited, "a deleted pin leaves the entry untouched");
    assert!(!after.contains("TOOLPORT_DATA_DIR"), "{after}");
    assert_eq!(shown.error, None);
}

#[test]
fn an_access_change_keeps_a_hand_set_data_folder() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let row = Connected::new("pin-access-change");
    let mine = "/home/me/other-toolport";
    let edited = row.read().replace(
        &format!("\"{}\"", row.data.dir.display()),
        &format!("\"{mine}\""),
    );
    assert!(edited.contains(mine));
    std::fs::write(&row.file, &edited).unwrap();
    let write = install_gateway_for(&row.id, Some("Work"), Some(&[])).unwrap();
    let after = row.read();
    assert!(after.contains("Work"), "{after}");
    assert!(after.contains(mine), "{after}");
    assert_eq!(
        write
            .managed
            .as_ref()
            .and_then(|record| record.env.get(DATA_DIR_ENV))
            .map(String::as_str),
        Some("")
    );
}
