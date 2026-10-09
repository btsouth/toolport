//! Shell-neutral server capability and tool runner operations.

use crate::registry::ServerEntry;

fn server(server_id: &str) -> Result<ServerEntry, String> {
    let registry = crate::registry::load()?;
    let server = registry
        .servers
        .iter()
        .find(|server| server.id == server_id)
        .cloned()
        .ok_or_else(|| format!("server '{server_id}' not found"))?;
    // Connecting runs the server, so an unreviewed team server must already be on.
    if !registry.is_enabled(&registry.default_access_id(), &server.id) {
        server.check_enable_allowed(false)?;
    }
    Ok(server)
}

pub fn list_tools(server_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut tools = crate::server_runtime::connect_server(&server(server_id)?)?
        .tools
        .materialize_all();
    annotate_quarantine(server_id, &mut tools)?;
    Ok(tools)
}

// Quarantine is keyed by the exposed alias, not by the raw downstream name.
// Reuse the router's allocator so renamed tools and same-server collisions match.
fn annotate_quarantine(server_id: &str, tools: &mut [serde_json::Value]) -> Result<(), String> {
    let registry = crate::registry::load()?;
    let prefix = crate::router::sanitize_segment(server_id);
    let ambiguous_prefix = registry.servers.iter().any(|server| {
        server.id != server_id && crate::router::sanitize_segment(&server.id) == prefix
    });
    let aliases = crate::router::Router::server_tool_aliases(
        server_id,
        tools,
        registry.tool_overrides.clone(),
    );
    let quarantined = crate::integrity::quarantined_checked(Some(&registry.default_access_id()));
    for tool in tools {
        let alias = tool
            .get("name")
            .and_then(serde_json::Value::as_str)
            .and_then(|name| aliases.get(name));
        // Another server may have reserved a renamed alias. Without its live catalog
        // the state is unknown, never an invented clean bill of health.
        let ambiguous_alias = alias.is_some_and(|alias| {
            registry.servers.iter().any(|server| {
                server.id != server_id
                    && alias.starts_with(&format!(
                        "{}__",
                        crate::router::sanitize_segment(&server.id)
                    ))
            }) || registry.tool_overrides.iter().any(|(id, overrides)| {
                id != server_id
                    && overrides.values().any(|value| {
                        value
                            .name
                            .as_deref()
                            .is_some_and(|name| crate::router::sanitize_segment(name) == *alias)
                    })
            })
        });
        tool["toolportQuarantine"] = serde_json::json!(match (&quarantined, alias) {
            (Ok(names), Some(alias)) if !ambiguous_prefix && !ambiguous_alias =>
                if names.contains(alias)
                    || tool
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|original| {
                            crate::router::Router::legacy_tool_policy_name(
                                &registry.tool_overrides,
                                server_id,
                                original,
                            )
                            .is_some_and(|legacy| names.contains(&legacy))
                        })
                {
                    "quarantined"
                } else {
                    "clear"
                },
            _ => "unknown",
        });
    }
    Ok(())
}

#[derive(Clone)]
pub struct Capabilities {
    pub tools: Vec<serde_json::Value>,
    pub resources: Vec<serde_json::Value>,
    pub prompts: Vec<serde_json::Value>,
}

pub fn capabilities(server_id: &str) -> Result<Capabilities, String> {
    let mut downstream = crate::server_runtime::connect_server(&server(server_id)?)?;
    downstream.load_resources_prompts();
    let mut tools = downstream.tools.materialize_all();
    annotate_quarantine(server_id, &mut tools)?;
    Ok(Capabilities {
        tools,
        resources: downstream.resources,
        prompts: downstream.prompts,
    })
}

pub fn call_tool(
    server_id: &str,
    tool: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Use the same gateway dispatch as clients, including profile scope, quarantine,
    // human approval and result inspection. Never call the downstream directly.
    let gateway = crate::clients::resolve_gateway_path_readonly()
        .ok_or("Could not locate the toolport-gateway binary")?;
    let registry = crate::registry::load()?;
    let env = vec![
        (
            "TOOLPORT_DATA_DIR".to_string(),
            crate::registry::conduit_dir()
                .ok_or("Toolport data directory unavailable")?
                .to_string_lossy()
                .into_owned(),
        ),
        ("TOOLPORT_PROFILE".to_string(), registry.default_access_id()),
    ];
    let transport = crate::downstream::StdioTransport::spawn(
        &gateway.to_string_lossy(),
        &[],
        &env,
        None,
        false,
    )?;
    let mut gateway =
        crate::downstream::DownstreamServer::connect("tools-tab".to_string(), Box::new(transport))?;
    gateway
        .call(
            "toolport_call_tool",
            serde_json::json!({
                "_toolportTarget": {"serverId": server_id, "tool": tool},
                "arguments": arguments
            }),
        )
        .map_err(|error| error.to_string())
}

pub fn list_resources(server_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut downstream = crate::server_runtime::connect_server(&server(server_id)?)?;
    downstream.load_resources_prompts();
    Ok(downstream.resources)
}

pub fn list_prompts(server_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut downstream = crate::server_runtime::connect_server(&server(server_id)?)?;
    downstream.load_resources_prompts();
    Ok(downstream.prompts)
}

pub fn read_resource(server_id: &str, uri: &str) -> Result<serde_json::Value, String> {
    let mut downstream = crate::server_runtime::connect_server(&server(server_id)?)?;
    downstream
        .read_resource(uri)
        .map_err(|error| error.to_string())
}

pub fn get_prompt(
    server_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut downstream = crate::server_runtime::connect_server(&server(server_id)?)?;
    downstream
        .get_prompt(name, arguments)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reserved_alias_quarantine_annotation_uses_legacy_binding() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-playground-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let data = crate::registry::DataDirOverride::set(&dir);
        let mut registry = crate::registry::Registry::default();
        registry.tool_overrides.insert(
            "s".into(),
            std::collections::HashMap::from([(
                "echo".into(),
                crate::registry::ToolOverride {
                    name: Some("toolport_custom_echo".into()),
                    description: None,
                    unknown_fields: Default::default(),
                },
            )]),
        );
        crate::registry::save_to(&dir.join("registry.json"), &registry).unwrap();
        let profile = registry.default_access_id();
        crate::integrity::apply_quarantine(
            Some(&profile),
            &[json!({"name":"toolport_custom_echo", "annotations":{"destructiveHint":true}})],
            &[
                json!({"type":"tool_drift", "tool":"toolport_custom_echo", "server":"s",
                "change":"changed", "severity":"high"}),
            ],
        )
        .unwrap();
        let mut tools = vec![json!({"name":"echo"}), json!({"name":"add"})];
        annotate_quarantine("s", &mut tools).unwrap();
        assert_eq!(tools[0]["toolportQuarantine"], "quarantined");
        assert_eq!(tools[1]["toolportQuarantine"], "clear");
        drop(data);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
