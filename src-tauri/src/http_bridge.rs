//! Shell-neutral lifecycle for Toolport's supervised HTTP gateway.
//!
//! Desktop shells own persistence and user-facing policy. This module owns the
//! child process, authenticated readiness check, and clean shutdown so every
//! shell supervises the same runtime implementation.

use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
pub struct HttpBridge {
    pub(crate) child: Option<std::process::Child>,
    pub(crate) port: Option<u16>,
    pub(crate) token: Option<String>,
    pub(crate) proxy_mode: bool,
}

pub type HttpBridgeState = Mutex<HttpBridge>;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpBridgeStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub token: Option<String>,
}

impl HttpBridgeStatus {
    pub fn new(port: Option<u16>, token: Option<String>) -> Self {
        Self {
            running: port.is_some(),
            url: port.map(|port| format!("http://localhost:{port}")),
            port,
            token,
        }
    }
}

pub fn status(state: &HttpBridgeState) -> HttpBridgeStatus {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    alive(&mut bridge);
    HttpBridgeStatus::new(bridge.port, bridge.token.clone())
}

/// Reap the child if it has already exited; returns true if it is still alive.
pub fn alive(bridge: &mut HttpBridge) -> bool {
    let alive = match bridge.child.as_mut() {
        Some(child) => !matches!(child.try_wait(), Ok(Some(_))),
        None => false,
    };
    if !alive {
        bridge.child = None;
        bridge.port = None;
        bridge.token = None;
        bridge.proxy_mode = false;
    }
    alive
}

pub fn identity_ready(port: u16, token: &str) -> bool {
    use std::io::Read as _;

    let response = match ureq::get(&format!("http://127.0.0.1:{port}/"))
        .timeout(Duration::from_millis(300))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(response) if response.status() == 200 => response,
        _ => return false,
    };
    let mut body = String::new();
    response
        .into_reader()
        .take(4 * 1024)
        .read_to_string(&mut body)
        .is_ok()
        && body.starts_with("Toolport gateway (HTTP mode).")
}

fn proxy_selected(
    override_value: Option<&str>,
    topology: Option<crate::registry::GatewayTopology>,
) -> bool {
    match override_value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("legacy") => false,
        Some(value) if value.eq_ignore_ascii_case("daemon") => true,
        _ => topology == Some(crate::registry::GatewayTopology::Daemon),
    }
}

fn bridge_uses_daemon() -> bool {
    let topology = crate::registry::load_resolved_with_source()
        .ok()
        .filter(|(_, source)| source.is_authoritative())
        .map(|(registry, _)| registry.gateway_topology_effective());
    let override_value =
        crate::brand::env_var("TOOLPORT_GATEWAY_TOPOLOGY", "CONDUIT_GATEWAY_TOPOLOGY");
    proxy_selected(override_value.as_deref(), topology)
}

#[cfg(test)]
mod tests {
    use super::proxy_selected;
    use crate::registry::GatewayTopology::{Daemon, Legacy};

    #[test]
    fn desktop_proxy_selection_keeps_the_legacy_rollback() {
        assert!(proxy_selected(None, Some(Daemon)));
        assert!(!proxy_selected(None, Some(Legacy)));
        assert!(!proxy_selected(None, None));
        assert!(!proxy_selected(Some("legacy"), Some(Daemon)));
        assert!(proxy_selected(Some("daemon"), Some(Legacy)));
    }
}

pub fn start_with_token_at(
    state: &HttpBridgeState,
    port: Option<u16>,
    token: Option<String>,
) -> Result<HttpBridgeStatus, String> {
    let port = port.unwrap_or(8765);
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if alive(&mut bridge) {
        return Ok(HttpBridgeStatus::new(bridge.port, bridge.token.clone()));
    }
    if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        return Err(format!(
            "Port {port} is already in use. Stop whatever is using it, then try again."
        ));
    }
    let bin = crate::clients::resolve_gateway_path()
        .ok_or_else(|| "toolport-gateway binary not found next to the app".to_string())?;
    let token = match token {
        Some(token) => token,
        None => {
            let mut bytes = [0u8; 24];
            getrandom::getrandom(&mut bytes)
                .map_err(|error| format!("could not generate a token: {error}"))?;
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }
    };
    let proxy_mode = bridge_uses_daemon();
    let mut command = std::process::Command::new(&bin);
    command
        .arg(if proxy_mode { "--http-proxy" } else { "--http" })
        .arg(port.to_string())
        .env("TOOLPORT_HTTP_TOKEN", &token)
        .env("CONDUIT_HTTP_TOKEN", &token)
        // The proxy treats stdin EOF as the app's service lease ending.
        .stdin(if proxy_mode {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the HTTP bridge: {error}"))?;
    let startup_timeout = if proxy_mode { 25 } else { 5 };
    let deadline = std::time::Instant::now() + Duration::from_secs(startup_timeout);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "The HTTP endpoint exited on startup ({status}). Is port {port} already in use?"
            ));
        }
        if identity_ready(port, &token) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "The HTTP endpoint did not come up on port {port} within {startup_timeout}s."
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bridge.child = Some(child);
    bridge.port = Some(port);
    bridge.token = Some(token.clone());
    bridge.proxy_mode = proxy_mode;
    Ok(HttpBridgeStatus::new(Some(port), Some(token)))
}

pub fn stop_with(
    bridge: &mut HttpBridge,
    kill_child: impl FnOnce(&mut std::process::Child) -> std::io::Result<()>,
) -> Result<HttpBridgeStatus, String> {
    if let Some(mut child) = bridge.child.take() {
        let gracefully_exited = if bridge.proxy_mode {
            // Closing the parent-held pipe tells the proxy to release the
            // daemon lease and stop its public listener.
            drop(child.stdin.take());
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break true,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    _ => break false,
                }
            }
        } else {
            false
        };
        let stopped = if gracefully_exited {
            Ok(())
        } else {
            match kill_child(&mut child) {
                Ok(()) => child.wait().map(|_| ()),
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(kill_error),
                    Err(wait_error) => Err(wait_error),
                },
            }
        };
        if let Err(error) = stopped {
            bridge.child = Some(child);
            return Err(match bridge.port {
                Some(port) => format!("Toolport HTTP endpoint on port {port}: {error}"),
                None => format!("Toolport HTTP endpoint: {error}"),
            });
        }
    }
    bridge.port = None;
    bridge.token = None;
    bridge.proxy_mode = false;
    Ok(HttpBridgeStatus::new(None, None))
}

pub fn stop(state: &HttpBridgeState) -> Result<HttpBridgeStatus, String> {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_with(&mut bridge, std::process::Child::kill)
}

pub fn tracked_port_and_token(state: &HttpBridgeState) -> Option<(Option<u16>, Option<String>)> {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    alive(&mut bridge).then(|| (bridge.port, bridge.token.clone()))
}

pub fn kill_on_exit(state: &HttpBridgeState) {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = stop_with(&mut bridge, std::process::Child::kill);
}
