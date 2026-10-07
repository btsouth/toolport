//! Shell-neutral MCP playground operations.

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
    if !registry.is_enabled(&registry.active_profile_id(), &server.id) {
        server.check_enable_allowed(false)?;
    }
    Ok(server)
}

pub fn list_tools(server_id: &str) -> Result<Vec<serde_json::Value>, String> {
    crate::server_runtime::connect_server(&server(server_id)?).map(|downstream| downstream.tools)
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
    Ok(Capabilities {
        tools: downstream.tools,
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
        ("TOOLPORT_PROFILE".to_string(), registry.active_profile_id()),
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
