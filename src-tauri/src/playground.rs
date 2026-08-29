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
    if server.needs_team_enable_review() {
        let profile = registry.active_profile_id();
        if !registry.is_enabled(&profile, &server.id) {
            return Err(
                "this team server runs a local command or private address; enable it from Teams after review"
                    .into(),
            );
        }
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
    let server = server(server_id)?;
    let mut downstream = crate::server_runtime::connect_server(&server)?;
    let started = std::time::Instant::now();
    let result = downstream
        .call(tool, arguments)
        .map_err(|error| error.to_string());
    let duration_ms = started.elapsed().as_millis() as u64;
    let ok = result
        .as_ref()
        .map(|result| {
            !result
                .get("isError")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let error = result.as_ref().err().map(String::as_str);
    crate::audit::record_timed(&server.id, tool, ok, Some(duration_ms), error, None);
    result
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
