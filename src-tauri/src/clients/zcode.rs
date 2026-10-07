//! ZCode's user config is shared by its CLI and desktop. The desktop's `.agents`
//! fallback is inventory only: writing a native gateway would hide that map.

use super::*;
use serde_json::{Map, Value};

fn duplicate_keys(object: &jsonc_parser::cst::CstObject, path: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for property in object.properties() {
        let name = property
            .name()
            .and_then(|name| name.decoded_value().ok())
            .ok_or("Invalid ZCode config key")?;
        if !seen.insert(name.clone()) {
            return Err(format!(
                "Malformed ZCode config: duplicate '{path}{name}'; leaving it untouched."
            ));
        }
        if let Some(child) = property.object_value() {
            duplicate_keys(&child, &format!("{path}{name}."))?;
        }
    }
    Ok(())
}

fn root_from_content(content: &str) -> Result<Value, String> {
    let root = read_existing_json(content, true)?;
    if !root.is_object() {
        return Err("ZCode config root must be an object; leaving it untouched.".into());
    }
    if !content.trim().is_empty() {
        let cst =
            jsonc_parser::cst::CstRootNode::parse(content, &jsonc_parser::ParseOptions::default())
                .map_err(|error| format!("Could not parse ZCode config: {error}"))?;
        if let Some(object) = cst.object_value() {
            duplicate_keys(&object, "")?;
        }
    }
    Ok(root)
}

fn read_root(path: &Path) -> Result<(Value, Option<String>), String> {
    if let Some(staged) = mutation::read(path) {
        return match staged {
            Some(content) => Ok((root_from_content(&content)?, Some(content))),
            None => Ok((serde_json::json!({}), None)),
        };
    }
    match std::fs::metadata(path) {
        Ok(_) => {
            let content = read_config_file(path)?;
            Ok((root_from_content(&content)?, Some(content)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((serde_json::json!({}), None))
        }
        Err(error) => Err(format!("Could not read ZCode config: {error}")),
    }
}

fn server_map(root: &Value) -> Result<Map<String, Value>, String> {
    let Some(mcp) = root.get("mcp") else {
        return Ok(Map::new());
    };
    let mcp = mcp.as_object().ok_or("ZCode 'mcp' must be an object")?;
    match mcp.get("servers") {
        None => Ok(Map::new()),
        Some(servers) => servers
            .as_object()
            .cloned()
            .ok_or_else(|| "ZCode 'mcp.servers' must be an object".into()),
    }
}

fn fallback_path(path: &Path) -> Result<PathBuf, String> {
    let home = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("Could not resolve ZCode's user fallback path")?;
    Ok(home.join(".agents").join("mcp.json"))
}

fn fallback_map(path: &Path) -> Result<Map<String, Value>, String> {
    let fallback = fallback_path(path)?;
    let (root, _) = read_root(&fallback)?;
    match root.get("mcpServers") {
        None => Ok(Map::new()),
        Some(servers) => servers
            .as_object()
            .cloned()
            .ok_or_else(|| "ZCode fallback 'mcpServers' must be an object".into()),
    }
}

fn effective_map(path: &Path) -> Result<(Map<String, Value>, bool, bool), String> {
    let (root, original) = read_root(path)?;
    let native = server_map(&root)?;
    if !native.is_empty() {
        return Ok((native, false, true));
    }
    Ok((fallback_map(path)?, true, original.is_some()))
}

fn disabled(definition: &Value) -> bool {
    definition.get("enabled") == Some(&Value::Bool(false))
        || definition.get("enable") == Some(&Value::Bool(false))
}

/// Normalize only aliases ZCode itself accepts. Detection keeps values private.
fn normalized_server(name: &str, definition: &Value) -> Result<Value, String> {
    let invalid = |field: &str| format!("Malformed ZCode server '{name}': invalid '{field}'");
    let mut definition = definition
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("server object"))?;
    for (legacy, canonical) in [("environment", "env"), ("http_headers", "headers")] {
        if let Some(value) = definition.remove(legacy) {
            definition.entry(canonical).or_insert(value);
        }
    }
    let kind = match definition.get("type") {
        Some(Value::String(kind)) if kind == "remote" => "http".to_string(),
        Some(Value::String(kind)) if matches!(kind.as_str(), "stdio" | "http" | "sse") => {
            kind.clone()
        }
        None if definition.get("command").is_some_and(Value::is_string) => "stdio".into(),
        None if definition.get("url").is_some_and(Value::is_string) => "http".into(),
        _ => return Err(invalid("type")),
    };
    let required = if kind == "stdio" { "command" } else { "url" };
    if !definition
        .get(required)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(invalid(required));
    }
    for (field, value) in &definition {
        let valid = match field.as_str() {
            "type" => true,
            "command" | "cwd" => kind == "stdio" && value.as_str().is_some_and(|v| !v.is_empty()),
            "url" => kind != "stdio" && value.as_str().is_some_and(|v| !v.is_empty()),
            "args" => {
                kind == "stdio"
                    && value
                        .as_array()
                        .is_some_and(|a| a.iter().all(Value::is_string))
            }
            "env" | "headers" => {
                let correct_transport = (field == "env") == (kind == "stdio");
                correct_transport
                    && value
                        .as_object()
                        .is_some_and(|o| o.values().all(Value::is_string))
            }
            "enabled" | "enable" => value.is_boolean(),
            "timeoutMs" => value.as_f64().is_some_and(|v| v.is_finite() && v > 0.0),
            "protocolVersion" => matches!(value.as_str(), Some("auto" | "legacy" | "2026-07-28")),
            "oauth" => kind != "stdio" && valid_oauth(value),
            // The upstream reader strips these historical fields. Keep them on
            // disk, but guard imports rather than claiming to preserve a timeout.
            "timeout" | "startup_timeout_sec" => true,
            _ => false,
        };
        if !valid {
            return Err(invalid(field));
        }
    }
    definition.insert("type".into(), Value::String(kind));
    Ok(Value::Object(definition))
}

fn valid_oauth(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let credentials = match object.get("type").and_then(Value::as_str) {
        Some("client_credentials") => true,
        Some("authorization_code") => false,
        _ => return false,
    };
    if credentials
        && ["clientId", "clientSecret"].iter().any(|key| {
            !object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        })
    {
        return false;
    }
    object.iter().all(|(key, value)| match key.as_str() {
        "type" => true,
        "scope" => value.is_string(),
        "clientId" | "clientSecret" | "clientName" => value.as_str().is_some_and(|v| !v.is_empty()),
        "redirectPath" => !credentials && value.as_str().is_some_and(|v| !v.is_empty()),
        _ => false,
    })
}

fn inventory(servers: &Map<String, Value>) -> Result<Vec<McpServer>, String> {
    let mut inventory = servers
        .iter()
        .map(|(name, definition)| {
            normalized_server(name, definition).map(|value| json_server(name, &value))
        })
        .collect::<Result<Vec<_>, _>>()?;
    inventory.sort_by_key(|server| server.name.to_lowercase());
    Ok(inventory)
}

pub(super) fn parse(content: &str) -> Result<Vec<McpServer>, String> {
    inventory(&server_map(&root_from_content(content)?)?)
}

pub(super) fn detect(path: &Path) -> Result<(Vec<McpServer>, bool), String> {
    // A shared `.agents` directory belongs to many clients and is no install
    // marker for ZCode. Inventory its fallback only after ZCode has run here.
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let marker = path
                .parent()
                .and_then(Path::parent)
                .ok_or("Could not resolve ZCode's install marker")?;
            match std::fs::metadata(marker) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Ok((Vec::new(), false)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((Vec::new(), false))
                }
                Err(error) => {
                    return Err(format!("Could not read ZCode's install marker: {error}"))
                }
            }
        }
        Err(error) => return Err(format!("Could not read ZCode config: {error}")),
    }
    let (servers, _, exists) = effective_map(path)?;
    Ok((inventory(&servers)?, exists))
}

fn importable(name: &str, definition: &Value, reject_disabled: bool) -> Result<(), String> {
    normalized_server(name, definition)?;
    for field in [
        "cwd",
        "timeoutMs",
        "oauth",
        "protocolVersion",
        "timeout",
        "startup_timeout_sec",
    ] {
        if definition.get(field).is_some() {
            return Err(format!(
                "Cannot import ZCode server '{name}': Toolport's client import cannot preserve '{field}'. Keep this server in ZCode, or configure the equivalent behavior in Toolport manually before migrating."
            ));
        }
    }
    if reject_disabled && disabled(definition) {
        return Err(format!(
            "Cannot migrate or paste disabled ZCode server '{name}': this path cannot preserve its disabled state. Keep it in ZCode or import it through the client import screen, where new servers stay disabled."
        ));
    }
    Ok(())
}

fn refuse_fallback(path: &Path) -> Result<(), String> {
    let (servers, fallback, _) = effective_map(path)?;
    if fallback && !servers.is_empty() {
        return Err(format!(
            "Cannot install or migrate ZCode while it uses servers from {}. Adding native mcp.servers would hide those servers. Copy them into {} under mcp.servers using ZCode first, then retry. Toolport never edits the shared fallback file.",
            fallback_path(path)?.display(), path.display()
        ));
    }
    Ok(())
}

pub(super) fn validate_import(
    path: &Path,
    names: &[String],
    migration: bool,
) -> Result<(), String> {
    if migration {
        refuse_fallback(path)?;
    }
    let (servers, _, _) = effective_map(path)?;
    inventory(&servers)?;
    if migration {
        for (name, definition) in &servers {
            if gateway_definition(name, definition) {
                importable(name, definition, false)?;
            }
        }
    }
    for name in names {
        let definition = servers.get(name).ok_or_else(|| {
            format!("ZCode server '{name}' changed since detection; refresh the client inventory and retry.")
        })?;
        importable(name, definition, migration)?;
    }
    Ok(())
}

pub(super) fn parse_snippet(content: &str) -> Result<Vec<ParsedSnippetServer>, String> {
    let servers = server_map(&root_from_content(content)?)?;
    if servers.is_empty() {
        return Err("No ZCode servers found under 'mcp.servers'".into());
    }
    servers
        .iter()
        .map(|(name, definition)| {
            // Pasted snippets have no disabled-state field in the public shape.
            importable(name, definition, true)?;
            Ok(json_server_with_values(
                name,
                &normalized_server(name, definition)?,
            ))
        })
        .collect()
}

fn entry_value(entry: &ServerEntry) -> Result<Value, String> {
    // ServerEntry can carry behavior ZCode cannot express. Do not turn two
    // distinct Toolport timeouts into one ZCode timeout or omit launch/auth data.
    if entry.client_credentials.is_some()
        || entry.request_timeout_ms.is_some()
        || entry.initialize_timeout_ms.is_some()
        || entry.launch.is_some()
    {
        return Err("Cannot write ZCode server with Toolport launch, OAuth, or timeout settings; configure it in ZCode manually.".into());
    }
    let mut value = entry_to_json(entry);
    value["type"] = Value::String(entry.transport.clone());
    if let Some(cwd) = &entry.cwd {
        value["cwd"] = Value::String(cwd.clone());
    }
    normalized_server(&entry.name, &value)?;
    Ok(value)
}

fn gateway_definition(name: &str, definition: &Value) -> bool {
    gateway_identity_matches(
        name,
        name,
        definition.get("command").and_then(Value::as_str),
    )
}

fn retain_gateway_state(servers: &Map<String, Value>, value: &mut Value) -> Result<(), String> {
    for (name, definition) in servers {
        if gateway_definition(name, definition) {
            // Repair may replace the command, but must preserve an explicit off
            // switch. False wins when the current and legacy fields disagree.
            if disabled(definition) {
                value["enabled"] = Value::Bool(false);
            }
            // The ownership ABI cannot compare these settings; refuse a repair
            // that would otherwise erase a customized gateway's behavior.
            importable(name, definition, false)?;
        }
    }
    Ok(())
}

fn write_map(
    path: &Path,
    mut root: Value,
    original: Option<&str>,
    servers: Map<String, Value>,
) -> Result<(), String> {
    if root.get("mcp").is_none() {
        root["mcp"] = serde_json::json!({});
    }
    root["mcp"]["servers"] = Value::Object(servers);
    atomic_write_json_config(path, original, &root, "mcp")
}

pub(super) fn edit_gateway(path: &Path, entry: Option<&ServerEntry>) -> Result<(), String> {
    if entry.is_some() {
        refuse_fallback(path)?;
    }
    let (root, original) = read_root(path)?;
    let mut servers = server_map(&root)?;
    inventory(&servers)?;
    let mut value = entry.map(entry_value).transpose()?;
    if let Some(value) = &mut value {
        retain_gateway_state(&servers, value)?;
    }
    let count = servers.len();
    servers.retain(|name, definition| !gateway_definition(name, definition));
    if entry.is_none()
        && servers.is_empty()
        && fallback_map(path)?
            .iter()
            .any(|(name, definition)| gateway_definition(name, definition))
    {
        return Err(format!(
            "Cannot disconnect ZCode: {} contains a Toolport gateway that ZCode would still load. Remove that gateway from the shared config manually, then retry. Toolport never edits the shared fallback file.",
            fallback_path(path)?.display()
        ));
    }
    if let Some(value) = value {
        servers.insert(GATEWAY_ENTRY_NAME.into(), value);
    } else if servers.len() == count {
        // An uninstall with no native gateway never edits or materializes fallback.
        return Ok(());
    }
    write_map(path, root, original.as_deref(), servers)
}

pub(super) fn write_servers(path: &Path, entries: &[ServerEntry]) -> Result<(), String> {
    refuse_fallback(path)?;
    let (root, original) = read_root(path)?;
    let previous = server_map(&root)?;
    inventory(&previous)?;
    for (name, definition) in &previous {
        if !gateway_definition(name, definition) {
            importable(name, definition, true)?;
        }
    }
    let mut servers = Map::new();
    for entry in entries {
        let mut value = entry_value(entry)?;
        if is_gateway_server(entry) {
            retain_gateway_state(&previous, &mut value)?;
        }
        servers.insert(entry.name.clone(), value);
    }
    write_map(path, root, original.as_deref(), servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            loop {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("toolport-zcode-{}-{sequence}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("ZCode fixture: {error}"),
                }
            }
        }

        fn native(&self) -> PathBuf {
            self.0.join(".zcode/cli/config.json")
        }

        fn fallback(&self) -> PathBuf {
            self.0.join(".agents/mcp.json")
        }

        fn write(&self, path: &Path, content: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn fixtures_reserve_distinct_directories_in_parallel() {
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..32).map(|_| scope.spawn(Fixture::new)).collect();
            let fixtures: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let paths: std::collections::HashSet<_> =
                fixtures.iter().map(|fixture| &fixture.0).collect();
            assert_eq!(paths.len(), fixtures.len());
            assert!(fixtures.iter().all(|fixture| fixture.0.is_dir()));
        });
    }

    fn gateway() -> ServerEntry {
        crate::registry_controller::server_from_detected(
            &json_server(
                "toolport",
                &serde_json::json!({
                    "command": "/opt/toolport-gateway-current", "args": [],
                    "env": {"TOOLPORT_CLIENT_ID": "zcode", "TOOLPORT_PROFILE": "work"}
                }),
            ),
            "zcode",
        )
    }

    #[test]
    fn zcode_registration_and_user_paths_are_native_on_every_platform() {
        let definition = find_def("zcode").unwrap();
        assert_eq!(definition.name, "ZCode");
        assert!(matches!(definition.format, Format::JsonZCodeMcp));
        assert!(!definition.uses_connectors);
        for platform in Platform::ALL {
            let home = PathBuf::from("user-home");
            assert_eq!(
                resolve_client_config_path("zcode", &home, platform),
                Some(home.join(".zcode/cli/config.json"))
            );
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(
            resolve_client_config_path_linux("zcode", Path::new("user-home")),
            Some(PathBuf::from("user-home/.zcode/cli/config.json"))
        );
    }

    #[test]
    fn zcode_snippet_detection_preserves_other_clients_server_named_servers() {
        for (definition, transport) in [
            (
                serde_json::json!({"command":"node","args":["server.js"]}),
                "stdio",
            ),
            (
                serde_json::json!({"type":"http","url":"https://example.test/mcp"}),
                "http",
            ),
            (
                serde_json::json!({"type":"local","command":["node","server.js"]}),
                "stdio",
            ),
            (
                serde_json::json!({"type":"remote","url":"https://example.test/mcp"}),
                "http",
            ),
        ] {
            let content = serde_json::json!({"mcp":{"servers":definition}}).to_string();
            let parsed = super::super::parse_snippet(&content).unwrap();
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].name, "servers");
            assert_eq!(parsed[0].transport, transport);
        }
        // Transport field names can also be names inside ZCode's nested map.
        let parsed = super::super::parse_snippet(
            r#"{"mcp":{"servers":{"command":{"command":"node"},"type":{"type":"http","url":"https://example.test/mcp"}}}}"#,
        ).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "command");
        assert_eq!(parsed[1].name, "type");
    }

    #[test]
    fn zcode_inventory_parses_transports_and_aliases_without_leaking_values() {
        let servers = parse(r#"{"mcp":{"servers":{
            "local":{"command":"node","args":["server.js"],"environment":{"TOKEN":"secret"}},
            "remote":{"type":"remote","url":"https://example.test/mcp","http_headers":{"Authorization":"secret"}},
            "events":{"type":"sse","url":"https://example.test/sse","enabled":false,"enable":true}
        }}}"#).unwrap();
        assert_eq!(servers.len(), 3);
        assert_eq!(servers[0].transport, "sse");
        assert_eq!(servers[1].transport, "stdio");
        assert_eq!(servers[1].env_keys, ["TOKEN"]);
        assert_eq!(servers[2].transport, "http");
        assert_eq!(servers[2].env_keys, ["Authorization"]);
        assert!(!serde_json::to_string(&servers).unwrap().contains("secret"));
    }

    #[test]
    fn zcode_install_and_remove_preserve_settings_siblings_and_existing_entries() {
        let fixture = Fixture::new();
        let original = r#"{
            // This model annotation survives changes to MCP.
            "modelStream":{"model":"keep-me"},
            "permission":{"mode":"plan"},
            "mcp":{"futureSetting":{"keep":true},"servers":{
                "existing":{"type":"stdio","command":"node","cwd":"/srv/work","timeoutMs":4500,"enabled":false,"protocolVersion":"legacy"}
            }}
        }"#;
        fixture.write(&fixture.native(), original);
        let before = root_from_content(original).unwrap();
        edit_gateway(&fixture.native(), Some(&gateway())).unwrap();
        let (installed, source) = read_root(&fixture.native()).unwrap();
        assert_eq!(installed["modelStream"], before["modelStream"]);
        assert_eq!(installed["permission"], before["permission"]);
        assert_eq!(
            installed["mcp"]["futureSetting"],
            before["mcp"]["futureSetting"]
        );
        assert_eq!(
            installed["mcp"]["servers"]["existing"],
            before["mcp"]["servers"]["existing"]
        );
        assert_eq!(installed["mcp"]["servers"]["toolport"]["type"], "stdio");
        assert!(source.unwrap().contains("model annotation"));
        let servers = parse(&std::fs::read_to_string(fixture.native()).unwrap()).unwrap();
        assert_eq!(
            resolve_entry_state(
                &servers,
                Some(&ManagedEntry::from_gateway_entry(&gateway()))
            ),
            GatewayEntryState::Managed
        );
        edit_gateway(&fixture.native(), None).unwrap();
        assert_eq!(read_root(&fixture.native()).unwrap().0, before);
    }

    #[test]
    fn zcode_fallback_is_inventory_only_and_never_implies_installation() {
        let fixture = Fixture::new();
        let original = r#"{"mcpServers":{"shared":{"command":"node"}},"other":{"keep":true}}"#;
        fixture.write(&fixture.fallback(), original);
        assert!(detect(&fixture.native()).unwrap().0.is_empty());
        std::fs::create_dir_all(fixture.0.join(".zcode")).unwrap();
        let (servers, exists) = detect(&fixture.native()).unwrap();
        assert!(!exists, "config_exists describes only the native config");
        assert_eq!(servers[0].name, "shared");
        for result in [
            edit_gateway(&fixture.native(), Some(&gateway())),
            write_servers(&fixture.native(), &[gateway()]),
            validate_import(&fixture.native(), &["shared".into()], true),
        ] {
            let error = result.unwrap_err();
            assert!(error.contains("would hide"));
            assert!(error.contains("Copy them"));
            assert!(!fixture.native().exists());
        }
        validate_import(&fixture.native(), &["shared".into()], false).unwrap();
        edit_gateway(&fixture.native(), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.fallback()).unwrap(),
            original
        );
    }

    #[test]
    fn zcode_disconnect_refuses_a_gateway_remaining_or_reappearing_in_fallback() {
        let fixture = Fixture::new();
        let fallback = r#"{"mcpServers":{"toolport":{"command":"/opt/toolport-gateway"}}}"#;
        fixture.write(&fixture.fallback(), fallback);
        std::fs::create_dir_all(fixture.0.join(".zcode")).unwrap();
        for native in [
            None,
            Some(r#"{"mcp":{"servers":{}}}"#),
            Some(r#"{"mcp":{"servers":{"toolport":{"command":"/opt/toolport-gateway"}}}}"#),
        ] {
            if let Some(native) = native {
                fixture.write(&fixture.native(), native);
            }
            let detected = detect(&fixture.native()).unwrap().0;
            assert!(detected.iter().any(detected_is_gateway));
            let error = edit_gateway(&fixture.native(), None).unwrap_err();
            assert!(error.contains("Cannot disconnect"));
            assert!(error.contains("shared config manually"));
            assert_eq!(
                std::fs::read_to_string(fixture.fallback()).unwrap(),
                fallback
            );
            assert_eq!(
                std::fs::read_to_string(fixture.native()).ok().as_deref(),
                native
            );
        }
        // A remaining native server prevents fallback activation after removal.
        fixture.write(
            &fixture.native(),
            r#"{"mcp":{"servers":{"toolport":{"command":"/opt/toolport-gateway"},"local":{"command":"node"}}}}"#,
        );
        edit_gateway(&fixture.native(), None).unwrap();
        let detected = detect(&fixture.native()).unwrap().0;
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].name, "local");
        assert_eq!(
            std::fs::read_to_string(fixture.fallback()).unwrap(),
            fallback
        );
    }

    #[test]
    fn zcode_native_map_wins_even_when_its_only_server_is_disabled() {
        let fixture = Fixture::new();
        fixture.write(
            &fixture.native(),
            r#"{"mcp":{"servers":{"native":{"command":"node","enabled":false}}}}"#,
        );
        fixture.write(&fixture.fallback(), "invalid ignored fallback");
        assert_eq!(detect(&fixture.native()).unwrap().0[0].name, "native");
        edit_gateway(&fixture.native(), Some(&gateway())).unwrap();
        assert_eq!(detect(&fixture.native()).unwrap().0.len(), 2);
    }

    #[test]
    fn zcode_empty_native_map_reads_fallback_but_malformed_native_never_does() {
        let fixture = Fixture::new();
        fixture.write(
            &fixture.native(),
            r#"{"mcp":{"servers":{}},"ui":{"theme":"dark"}}"#,
        );
        fixture.write(
            &fixture.fallback(),
            r#"{"mcpServers":{"fallback":{"command":"node"}}}"#,
        );
        assert_eq!(detect(&fixture.native()).unwrap().0[0].name, "fallback");
        for malformed in [
            "broken",
            "[]",
            r#"{"mcp":null}"#,
            r#"{"mcp":{"servers":[]}}"#,
            r#"{"mcp":{"servers":{"bad":{"command":42}}}}"#,
        ] {
            fixture.write(&fixture.native(), malformed);
            assert!(detect(&fixture.native()).is_err());
            assert!(edit_gateway(&fixture.native(), Some(&gateway())).is_err());
            assert!(write_servers(&fixture.native(), &[gateway()]).is_err());
            assert_eq!(
                std::fs::read_to_string(fixture.native()).unwrap(),
                malformed
            );
        }
    }

    #[test]
    fn zcode_nested_duplicate_keys_are_rejected_without_rewriting() {
        let fixture = Fixture::new();
        for original in [
            r#"{"mcp":{"servers":{},"servers":{"old":{"command":"node"}}}}"#,
            r#"{"mcp":{},"mcp":{"servers":{"old":{"command":"node"}}}}"#,
            r#"{"mcp":{"servers":{"same":{"command":"node"},"same":{"command":"python"}}}}"#,
        ] {
            fixture.write(&fixture.native(), original);
            assert!(edit_gateway(&fixture.native(), Some(&gateway()))
                .unwrap_err()
                .contains("duplicate"));
            assert!(edit_gateway(&fixture.native(), None).is_err());
            assert!(write_servers(&fixture.native(), &[gateway()]).is_err());
            assert!(super::super::parse_snippet(original)
                .unwrap_err()
                .contains("duplicate"));
            assert_eq!(std::fs::read_to_string(fixture.native()).unwrap(), original);
        }
    }

    #[test]
    fn zcode_repair_preserves_disabled_gateway_with_false_winning() {
        let fixture = Fixture::new();
        for flags in [
            r#""enabled":false"#,
            r#""enable":false"#,
            r#""enabled":true,"enable":false"#,
            r#""enabled":false,"enable":true"#,
        ] {
            fixture.write(&fixture.native(), &format!(r#"{{"mcp":{{"servers":{{"conduit":{{"command":"/old/conduit-gateway",{flags}}}}}}}}}"#));
            assert!(gateway_entry_needs_rewrite(
                "conduit",
                "/old/conduit-gateway",
                "/opt/toolport-gateway-current",
                None
            ));
            edit_gateway(&fixture.native(), Some(&gateway())).unwrap();
            let root = read_root(&fixture.native()).unwrap().0;
            assert_eq!(root["mcp"]["servers"]["toolport"]["enabled"], false);
            assert!(root["mcp"]["servers"].get("conduit").is_none());
        }
    }

    #[test]
    fn zcode_snippets_preserve_values_and_reject_behavior_the_import_cannot_carry() {
        let supported = r#"{"ui":{"theme":"dark"},"mcp":{"servers":{"local":{"command":"node","args":["s.js"],"environment":{"PATH_HINT":"/srv"}},"remote":{"type":"sse","url":"https://example.test/sse","headers":{"X-Key":"pasted"}}}}}"#;
        let servers = super::super::parse_snippet(supported).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].env[0].value.as_deref(), Some("/srv"));
        assert_eq!(servers[1].transport, "sse");
        assert_eq!(servers[1].env[0].value.as_deref(), Some("pasted"));
        for (field, value) in [
            ("cwd", serde_json::json!("/srv")),
            ("timeoutMs", serde_json::json!(1234)),
            ("protocolVersion", serde_json::json!("auto")),
            ("protocolVersion", serde_json::json!("legacy")),
            ("protocolVersion", serde_json::json!("2026-07-28")),
            ("enabled", serde_json::json!(false)),
            ("enable", serde_json::json!(false)),
        ] {
            let mut definition = serde_json::json!({"command":"node"});
            definition[field] = value;
            let text = serde_json::json!({"mcp":{"servers":{"affected":definition}}}).to_string();
            let error = super::super::parse_snippet(&text).unwrap_err();
            assert!(error.contains("affected"), "{error}");
            assert!(error.contains("preserve"), "{error}");
            assert!(!error.contains("Could not detect format"), "{error}");
        }
        let oauth = r#"{"mcp":{"servers":{"oauth":{"type":"http","url":"https://example.test/mcp","oauth":{"type":"authorization_code","clientSecret":"do-not-print"}}}}}"#;
        let error = super::super::parse_snippet(oauth).unwrap_err();
        assert!(error.contains("'oauth'"));
        assert!(!error.contains("do-not-print"));
    }

    #[test]
    fn zcode_readers_refuse_oversized_and_nonregular_configs() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.native().parent().unwrap()).unwrap();
        let file = std::fs::File::create(fixture.native()).unwrap();
        file.set_len(MAX_CONFIG_BYTES + 1).unwrap();
        assert!(detect(&fixture.native())
            .unwrap_err()
            .contains("config limit"));
        assert!(edit_gateway(&fixture.native(), Some(&gateway())).is_err());
        drop(file);
        std::fs::remove_file(fixture.native()).unwrap();
        std::fs::create_dir(fixture.native()).unwrap();
        assert!(detect(&fixture.native())
            .unwrap_err()
            .contains("not a regular file"));
        std::fs::remove_dir(fixture.native()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(fixture.native().as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            assert!(detect(&fixture.native())
                .unwrap_err()
                .contains("not a regular file"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn zcode_unreadable_install_directory_is_an_error_not_an_absent_client() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fixture = Fixture::new();
        fixture.write(&fixture.native(), r#"{"mcp":{"servers":{}}}"#);
        let marker = fixture.0.join(".zcode");
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
        let detected = detect(&fixture.native());
        // Restore access before asserting so even a failure can clean the fixture.
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(detected
            .unwrap_err()
            .contains("Could not read ZCode config"));
    }
}
