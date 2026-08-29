//! Shell-neutral one-off server connections and health probes.
//!
//! The gateway owns long-lived downstream connections. Desktop shells use this
//! module for explicit user-driven tests and playground operations only.

use std::time::Duration;

use crate::downstream::{resolve_root_token, DownstreamServer, StdioTransport};
use crate::registry::ServerEntry;
use crate::{remote, secrets};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub server_id: String,
    pub ok: bool,
    pub tool_count: usize,
    pub error: Option<String>,
    pub auth_required: bool,
}

fn missing_secret(server: &ServerEntry) -> bool {
    server.env.iter().any(|entry| {
        entry.secret
            && entry.value.is_none()
            && matches!(secrets::get_secret_result(&server.id, &entry.key), Ok(None))
    })
}

pub fn connect_server(server: &ServerEntry) -> Result<DownstreamServer, String> {
    if let Some(command) = &server.command {
        let mut env = Vec::new();
        for entry in &server.env {
            if let Some(value) = &entry.value {
                env.push((entry.key.clone(), value.clone()));
            } else if entry.secret {
                match secrets::get_secret_result(&server.id, &entry.key) {
                    Ok(Some(value)) => env.push((entry.key.clone(), value)),
                    Ok(None) => {
                        return Err(format!(
                            "missing secret '{}': add its value under this server's secrets",
                            entry.key
                        ));
                    }
                    Err(error) => {
                        return Err(format!(
                            "could not read secret '{}' from the keychain: {error}",
                            entry.key
                        ));
                    }
                }
            }
        }
        let cwd = server
            .cwd
            .as_deref()
            .and_then(|cwd| resolve_root_token(cwd, None));
        let transport = StdioTransport::spawn(command, &server.args, &env, cwd.as_deref())?;
        DownstreamServer::connect(server.id.clone(), Box::new(transport))
    } else if server.url.is_some() {
        remote::connect_remote(server)
    } else {
        Err("no command or url".to_string())
    }
}

pub fn probe_one(server: &ServerEntry) -> ProbeResult {
    match connect_server(server) {
        Ok(connection) => ProbeResult {
            server_id: server.id.clone(),
            ok: true,
            tool_count: connection.tools.len(),
            error: None,
            auth_required: false,
        },
        Err(error) => ProbeResult {
            server_id: server.id.clone(),
            ok: false,
            tool_count: 0,
            auth_required: remote::is_auth_error(&error) || missing_secret(server),
            error: Some(error),
        },
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(90);

pub fn probe_one_bounded(server: &ServerEntry) -> ProbeResult {
    let (sender, receiver) = std::sync::mpsc::channel();
    let server_for_probe = server.clone();
    std::thread::spawn(move || {
        let _ = sender.send(probe_one(&server_for_probe));
    });
    receiver
        .recv_timeout(PROBE_TIMEOUT)
        .unwrap_or_else(|_| ProbeResult {
            server_id: server.id.clone(),
            ok: false,
            tool_count: 0,
            error: Some(format!("timed out after {}s", PROBE_TIMEOUT.as_secs())),
            auth_required: false,
        })
}

pub fn probe_registered(server_id: &str) -> Result<ProbeResult, String> {
    let path = crate::registry::resolved_path().ok_or("could not resolve the registry path")?;
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read the registry: {error}"))?;
    let registry = serde_json::from_str::<crate::registry::Registry>(&contents)
        .map_err(|error| format!("could not parse the registry: {error}"))?;
    let server = registry
        .servers
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("server '{server_id}' not found"))?;
    Ok(probe_one_bounded(&server))
}

pub fn enabled_servers(registry: &crate::registry::Registry) -> Vec<ServerEntry> {
    registry
        .enabled_servers()
        .into_iter()
        .filter(|server| !crate::clients::is_gateway_server(server))
        .cloned()
        .collect()
}

pub fn probe_many(servers: Vec<ServerEntry>) -> Vec<ProbeResult> {
    servers
        .into_iter()
        .map(|server| std::thread::spawn(move || probe_one_bounded(&server)))
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerEntry {
        ServerEntry {
            id: "probe".into(),
            name: "Probe".into(),
            transport: "stdio".into(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            cwd: None,
            source: None,
            disabled_tools: Vec::new(),
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn invalid_definition_returns_a_bounded_non_auth_failure() {
        let result = probe_one_bounded(&server());

        assert!(!result.ok);
        assert!(!result.auth_required);
        assert_eq!(result.tool_count, 0);
        assert_eq!(result.error.as_deref(), Some("no command or url"));
    }

    #[test]
    fn enabled_server_selection_excludes_disabled_and_gateway_entries() {
        let mut registry = crate::registry::Registry::default();
        let mut enabled = server();
        enabled.id = "enabled".into();
        enabled.name = "Enabled".into();
        let mut disabled = server();
        disabled.id = "disabled".into();
        disabled.name = "Disabled".into();
        let mut gateway = server();
        gateway.id = "toolport".into();
        gateway.name = "Toolport".into();
        gateway.command = Some("toolport-gateway".into());
        registry.servers.extend([enabled, disabled, gateway]);
        registry
            .set_server_enabled("default", "enabled", true)
            .unwrap();
        registry
            .set_server_enabled("default", "toolport", true)
            .unwrap();

        let selected = enabled_servers(&registry);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "enabled");
    }

    #[test]
    fn probe_many_returns_each_bounded_result() {
        let mut first = server();
        first.id = "first".into();
        let mut second = server();
        second.id = "second".into();

        let results = probe_many(vec![first, second]);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| !result.ok));
    }
}
