//! Shell-neutral server capability and tool runner operations.

use crate::registry::ServerEntry;
use std::path::Path;

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

/// The progress token a Tools tab Run sends, so the gateway can say when it is
/// waiting for the server to start.
const RUN_PROGRESS_TOKEN: &str = "tools-tab-run";

/// How long the gateway waits for a Tools tab Run's server to start: as long
/// as the server itself may take to start (its own startup limit, or two
/// minutes for a command that downloads its package first), never less than
/// the setup catalog wait.
pub fn start_wait(server: &crate::registry::ServerEntry) -> std::time::Duration {
    let start = server.initialize_timeout().ok().flatten().or_else(|| {
        server
            .command
            .as_deref()
            .map(|command| crate::downstream::stdio_connect_timeout(command, &server.args))
    });
    start
        .unwrap_or_default()
        .max(crate::downstream::SETUP_CATALOG_WAIT_BUDGET)
}

/// How long one Tools tab Run may take: the gateway's wait for a cold server's
/// first catalog, a pending approval, and the server's own call limit, plus a
/// margin for the gateway itself. A Run is cancelled, never cut short.
pub fn run_time_limit(server: Option<&crate::registry::ServerEntry>) -> std::time::Duration {
    let call = server
        .and_then(|server| server.request_timeout_ms)
        .map(std::time::Duration::from_millis)
        .unwrap_or(crate::downstream::STDIO_READ_TIMEOUT);
    server
        .map(start_wait)
        .unwrap_or(crate::downstream::SETUP_CATALOG_WAIT_BUDGET)
        + std::time::Duration::from_secs(crate::approval::DEFAULT_TIMEOUT_SECS)
        + call
        + std::time::Duration::from_secs(5)
}

/// One Run on the Tools tab. Clone it to cancel from another thread; while the
/// gateway is starting the server, [`ToolRun::is_starting`] is true.
#[derive(Clone)]
pub struct ToolRun {
    _registry: crate::downstream::CancelRegistry,
    cancel: crate::downstream::CancelContext,
    starting: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for ToolRun {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRun {
    pub fn new() -> Self {
        let registry = crate::downstream::CancelRegistry::new();
        let cancel = registry
            .begin_client_request("tools-tab".into())
            .expect("a new registry is open");
        Self {
            _registry: registry,
            cancel,
            starting: Default::default(),
        }
    }

    /// Stop the Run: the gateway is told to cancel the call, including a wait
    /// for the server to start, and the caller gets a cancelled error at once.
    pub fn cancel(&self) {
        self.cancel.cancel(Some("Cancelled from the Tools tab"));
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn is_starting(&self) -> bool {
        self.starting.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// What the gateway reported about a Run while it is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    Starting,
    Started,
}

pub fn call_tool(
    server_id: &str,
    tool: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    call_tool_run(server_id, tool, arguments, &ToolRun::new(), |_| {})
}

/// [`call_tool`] for a Run its caller can cancel and watch.
pub fn call_tool_run(
    server_id: &str,
    tool: &str,
    arguments: serde_json::Value,
    run: &ToolRun,
    on_phase: impl Fn(RunPhase) + Send + Sync + 'static,
) -> Result<serde_json::Value, String> {
    // Use the same gateway dispatch as clients, including profile scope, quarantine,
    // human approval and result inspection. Never call the downstream directly.
    let gateway = crate::clients::resolve_gateway_path_readonly()
        .ok_or("Could not locate the toolport-gateway binary")?;
    call_tool_with_gateway(&gateway, server_id, tool, arguments, run, on_phase)
}

/// [`call_tool_run`] through a given gateway binary.
pub fn call_tool_with_gateway(
    gateway: &Path,
    server_id: &str,
    tool: &str,
    arguments: serde_json::Value,
    run: &ToolRun,
    on_phase: impl Fn(RunPhase) + Send + Sync + 'static,
) -> Result<serde_json::Value, String> {
    let registry = crate::registry::load()?;
    let limit = run_time_limit(
        registry
            .servers
            .iter()
            .find(|server| server.id == server_id),
    );
    let env = vec![
        (
            "TOOLPORT_DATA_DIR".to_string(),
            crate::registry::conduit_dir()
                .ok_or("Toolport data directory unavailable")?
                .to_string_lossy()
                .into_owned(),
        ),
        ("TOOLPORT_PROFILE".to_string(), registry.default_access_id()),
        // Activity lists the Run as Toolport's Tools tab, not as this process.
        (
            "TOOLPORT_CLIENT_ID".to_string(),
            crate::clients::TOOLS_TAB_CLIENT_ID.to_string(),
        ),
        // A server with a call limit past the adapter's own request deadline
        // still gets the whole Run.
        (
            crate::stdio_adapter::REQUEST_TIMEOUT_ENV.to_string(),
            limit.as_millis().to_string(),
        ),
    ];
    let mut transport = crate::downstream::StdioTransport::spawn(
        &gateway.to_string_lossy(),
        &[],
        &env,
        None,
        false,
    )?;
    // Report each change once, whether the gateway or the wait below noticed it.
    let on_phase: std::sync::Arc<dyn Fn(RunPhase) + Send + Sync> = std::sync::Arc::new(on_phase);
    let report = {
        let starting = std::sync::Arc::clone(&run.starting);
        move |phase: RunPhase| {
            let now = phase == RunPhase::Starting;
            if starting.swap(now, std::sync::atomic::Ordering::SeqCst) != now {
                on_phase(phase);
            }
        }
    };
    let report = std::sync::Arc::new(report);
    let from_gateway = std::sync::Arc::clone(&report);
    transport.set_progress_sink(Some(std::sync::Arc::new(move |note: serde_json::Value| {
        let params = &note["params"];
        if params["progressToken"] != RUN_PROGRESS_TOKEN {
            return;
        }
        match params["_meta"]["toolport/phase"].as_str() {
            Some("starting") => from_gateway(RunPhase::Starting),
            Some("started") => from_gateway(RunPhase::Started),
            _ => {}
        }
    })));
    if run.is_cancelled() {
        return Err(cancelled());
    }
    // Connect and call on a worker, so Cancel returns at once even while the
    // gateway is still waiting for servers to start before it answers the
    // handshake. A worker whose Run was cancelled makes no call, and dropping
    // its connection stops that gateway.
    // Whether the handshake is done, held while reporting so a late "starting"
    // from the wait below can never follow the handshake's "started".
    let connected = std::sync::Arc::new(std::sync::Mutex::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    {
        let run = run.clone();
        let connected = std::sync::Arc::clone(&connected);
        let report = std::sync::Arc::clone(&report);
        let server_id = server_id.to_string();
        let tool = tool.to_string();
        std::thread::spawn(move || {
            let gateway = crate::downstream::DownstreamServer::connect(
                "tools-tab".to_string(),
                Box::new(transport),
            );
            {
                let mut done = connected
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *done = true;
                report(RunPhase::Started);
            }
            let mut gateway = match gateway {
                Ok(gateway) => gateway,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            };
            if run.is_cancelled() {
                return;
            }
            gateway.set_call_timeout(limit);
            let result = gateway.call_with_cancel(
                "toolport_call_tool",
                serde_json::json!({
                    "_toolportTarget": {"serverId": server_id, "tool": tool},
                    "arguments": arguments
                }),
                Some(run.cancel.clone()),
                Some(&serde_json::json!({
                    "progressToken": RUN_PROGRESS_TOKEN,
                    crate::downstream::START_NOTICES_META_KEY: true,
                })),
            );
            let _ = sender.send(result.map_err(|error| match error {
                crate::downstream::TransportError::Classified(
                    crate::call_failure::CallFailureKind::Timeout { .. },
                    _,
                ) => crate::i18n::trf(
                    "No response after {value0}s. The server may be slow or unreachable; check its status under Servers, then retry.",
                    &[("value0", &limit.as_secs().to_string())],
                ),
                error => error.to_string(),
            }));
        });
    }
    // The gateway answers the Tools tab's handshake without waiting for
    // servers, so a slow handshake means the shared gateway itself is starting,
    // which it does to start this server.
    let began = std::time::Instant::now();
    let result = loop {
        match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => break result,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(crate::i18n::tr("the tool call stopped unexpectedly"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if run.is_cancelled() {
            break Err(cancelled());
        }
        if began.elapsed() >= std::time::Duration::from_millis(500) {
            let done = connected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*done {
                report(RunPhase::Starting);
            }
        }
    };
    run.starting
        .store(false, std::sync::atomic::Ordering::SeqCst);
    match result {
        Err(_) if run.is_cancelled() => Err(cancelled()),
        result => result,
    }
}

fn cancelled() -> String {
    crate::i18n::tr("Stopped. The server was told to cancel the call.")
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
