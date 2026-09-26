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

fn env_key_required(server: &ServerEntry, key: &str) -> bool {
    server
        .launch
        .as_ref()
        .is_none_or(|launch| launch.required_env.iter().any(|required| required == key))
}

fn missing_secret(server: &ServerEntry) -> bool {
    server.env.iter().any(|entry| {
        entry.secret
            && env_key_required(server, &entry.key)
            && entry.value.is_none()
            && matches!(secrets::get_secret_result(&server.id, &entry.key), Ok(None))
    })
}

fn environment_for_probe_with(
    server: &ServerEntry,
    mut vault: impl FnMut(&str, &str) -> Result<Option<String>, String>,
) -> Result<Vec<(String, String)>, String> {
    let mut env = Vec::new();
    for entry in &server.env {
        if let Some(value) = &entry.value {
            env.push((entry.key.clone(), value.clone()));
        } else if entry.secret {
            match vault(&server.id, &entry.key) {
                Ok(Some(value)) => env.push((entry.key.clone(), value)),
                Ok(None) => {
                    // Curated launch metadata distinguishes required keys
                    // from optional ones (for example, an AWS profile can
                    // replace explicit keys). Match the gateway's behavior
                    // for those optional declarations during Test Connection.
                    if !env_key_required(server, &entry.key) {
                        continue;
                    }
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
    Ok(env)
}

pub fn connect_server(server: &ServerEntry) -> Result<DownstreamServer, String> {
    if let Some(command) = &server.command {
        let env = environment_for_probe_with(server, secrets::get_secret_result)?;
        let cwd = server
            .cwd
            .as_deref()
            .and_then(|cwd| resolve_root_token(cwd, None));
        let resolved = crate::launch_inputs::resolve_args(server)?;
        let mut transport = StdioTransport::spawn(command, &resolved.args, &env, cwd.as_deref())
            .map_err(|error| resolved.redact(error))?;
        if let Some(timeout) = server.initialize_timeout()? {
            transport.set_connect_timeout(timeout);
        }
        DownstreamServer::connect(server.id.clone(), Box::new(transport))
            .map_err(|error| resolved.redact(error))
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
const PROBE_FOLLOWUP_BUDGET: Duration = Duration::from_secs(15);

fn probe_timeout(server: &ServerEntry) -> Duration {
    let initialize = server
        .initialize_timeout()
        .ok()
        .flatten()
        .or_else(|| {
            server
                .command
                .as_deref()
                .map(|command| crate::downstream::stdio_connect_timeout(command, &server.args))
        })
        .unwrap_or(Duration::from_secs(30));
    PROBE_TIMEOUT.max(initialize.saturating_add(PROBE_FOLLOWUP_BUDGET))
}

pub fn probe_one_bounded(server: &ServerEntry) -> ProbeResult {
    let (sender, receiver) = std::sync::mpsc::channel();
    let server_for_probe = server.clone();
    std::thread::spawn(move || {
        let _ = sender.send(probe_one(&server_for_probe));
    });
    let timeout = probe_timeout(server);
    receiver
        .recv_timeout(timeout)
        .unwrap_or_else(|_| ProbeResult {
            server_id: server.id.clone(),
            ok: false,
            tool_count: 0,
            error: Some(format!("timed out after {}s", timeout.as_secs())),
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
    use crate::registry::{ArgBinding, ArgPart, LaunchConfig, LaunchInput};

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
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
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
    fn probe_budget_covers_the_configured_initialize_timeout() {
        let mut configured = server();
        configured.initialize_timeout_ms = Some(300_000);

        assert_eq!(probe_timeout(&configured), Duration::from_secs(315));
    }

    #[test]
    fn launcher_probe_budget_covers_the_default_cold_start_timeout() {
        let mut launcher = server();
        launcher.command = Some("npx".into());
        launcher.args = vec!["-y".into(), "example-server".into()];

        assert_eq!(probe_timeout(&launcher), Duration::from_secs(135));
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

    #[test]
    fn probe_passes_composed_launch_argument_to_real_stdio_child() {
        let mut server = server();
        server.command = Some("node".into());
        server.args = vec![
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/arg-server.mjs").into(),
            "<launch-input>".into(),
        ];
        server.launch = Some(LaunchConfig {
            inputs: vec![
                LaunchInput {
                    key: "ACCOUNT".into(),
                    label: "Account".into(),
                    secret: false,
                    required: true,
                    value: Some("account".into()),
                },
                LaunchInput {
                    key: "KEY".into(),
                    label: "Key".into(),
                    secret: false,
                    required: true,
                    value: Some("key".into()),
                },
                LaunchInput {
                    key: "SECRET".into(),
                    label: "Secret".into(),
                    secret: true,
                    required: true,
                    value: Some("secret".into()),
                },
            ],
            bindings: vec![ArgBinding {
                index: 1,
                parts: vec![
                    ArgPart::Input {
                        key: "ACCOUNT".into(),
                    },
                    ArgPart::Literal { value: "/".into() },
                    ArgPart::Input { key: "KEY".into() },
                    ArgPart::Literal { value: ":".into() },
                    ArgPart::Input {
                        key: "SECRET".into(),
                    },
                ],
            }],
            ..Default::default()
        });
        let ready = probe_one(&server);
        assert!(ready.ok, "{ready:?}");
        assert_eq!(ready.tool_count, 1);

        server.launch.as_mut().unwrap().inputs[2].value = Some("wrong-secret".into());
        let rejected = probe_one(&server);
        assert!(!rejected.ok);
        assert!(!rejected.error.unwrap_or_default().contains("wrong-secret"));

        server.launch.as_mut().unwrap().inputs[2].value = None;
        let missing = probe_one(&server);
        assert!(!missing.ok);
        assert!(missing.error.unwrap_or_default().contains("Secret"));
    }

    #[test]
    fn optional_catalog_env_is_skipped_but_required_env_and_vault_errors_fail() {
        let mut server = server();
        server.env.push(crate::registry::EnvVar {
            key: "API_KEY".into(),
            value: None,
            secret: true,
        });
        server.launch = Some(LaunchConfig::default());
        assert!(!env_key_required(&server, "API_KEY"));
        assert!(environment_for_probe_with(&server, |_, _| Ok(None))
            .unwrap()
            .is_empty());
        server.launch.as_mut().unwrap().required_env = vec!["API_KEY".into()];
        assert!(env_key_required(&server, "API_KEY"));
        assert!(environment_for_probe_with(&server, |_, _| Ok(None))
            .unwrap_err()
            .contains("missing secret"));
        assert!(
            environment_for_probe_with(&server, |_, _| Err("vault locked".into()))
                .unwrap_err()
                .contains("vault locked")
        );
    }
}
