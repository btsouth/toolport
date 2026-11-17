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
fn only_a_pin_naming_another_folder_is_rewritten() {
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
    let format = || Some(Format::JsonMcpServers);
    let stale = |text: &str, id: &str, command: &str| {
        data_dir_pin_is_stale(id, format, "toolport", command, Some(text))
    };
    let linux = cfg!(all(unix, not(target_os = "macos")));
    assert_eq!(stale(&json("/old/Conduit"), T3_HOME, gateway), linux);
    assert!(!stale(&json(&current), T3_HOME, gateway));
    // Entries without a pin, main rows and entries that are not ours stay as they are.
    assert!(!stale(&unpinned, T3_HOME, gateway));
    assert!(!stale(&json("/old/Conduit"), "opencode", gateway));
    assert!(!stale(
        &json("/old/Conduit"),
        T3_HOME,
        "/usr/local/bin/wrapper"
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
        ),
        linux
    );
}
