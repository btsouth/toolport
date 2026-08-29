use std::sync::{Arc, Mutex};

const VAULT_SERVER: &str = "__toolport_http_bridge__";
const VAULT_KEY: &str = "bearer";
static LIFECYCLE: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
pub struct BridgeController {
    state: Arc<crate::http_bridge::HttpBridgeState>,
    restart_advice: Arc<Mutex<Vec<crate::gateway_publish::ClientNeedingRestart>>>,
}

#[derive(Debug, Clone, Default)]
pub struct ReapOutcome {
    pub killed: Vec<String>,
    pub failed: Vec<String>,
    pub needs_restart: Vec<crate::gateway_publish::ClientNeedingRestart>,
}

impl BridgeController {
    pub fn restore(&self) -> Result<bool, String> {
        let registry = crate::registry::load()?;
        if !registry.http_bridge_enabled {
            return Ok(false);
        }
        self.start(registry.http_bridge_port)?;
        Ok(true)
    }

    pub fn start(&self, port: Option<u16>) -> Result<crate::http_bridge::HttpBridgeStatus, String> {
        let _lifecycle = LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was_running = crate::http_bridge::status(&self.state).running;
        let token = crate::secrets::get_secret_result(VAULT_SERVER, VAULT_KEY)?;
        let status = crate::http_bridge::start_with_token_at(&self.state, port, token)?;
        let token = status
            .token
            .as_deref()
            .ok_or("The HTTP endpoint started without a bearer token")?;
        if let Err(error) = crate::secrets::set_secret(VAULT_SERVER, VAULT_KEY, token) {
            if !was_running {
                let _ = crate::http_bridge::stop(&self.state);
            }
            return Err(format!("Could not save the HTTP endpoint token: {error}"));
        }
        if let Err(error) = crate::registry::update(|registry| {
            registry.http_bridge_enabled = true;
            registry.http_bridge_port = status.port;
            Ok(())
        }) {
            if !was_running {
                let _ = crate::http_bridge::stop(&self.state);
            }
            return Err(format!("Could not save the HTTP endpoint setting: {error}"));
        }
        Ok(status)
    }

    pub fn stop(&self) -> Result<crate::http_bridge::HttpBridgeStatus, String> {
        let _lifecycle = LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::registry::update(|registry| {
            registry.http_bridge_enabled = false;
            Ok(())
        })?;
        crate::http_bridge::stop(&self.state)
    }

    pub fn status(&self) -> crate::http_bridge::HttpBridgeStatus {
        crate::http_bridge::status(&self.state)
    }

    pub fn stop_stale_gateways(&self) -> ReapOutcome {
        let _lifecycle = LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = crate::http_bridge::status(&self.state);
        let extra_keep = crate::clients::resolve_gateway_path()
            .into_iter()
            .collect::<Vec<_>>();
        let report = crate::gateway_publish::reap_stale(&extra_keep);
        let mut failed = report.failed;
        for remaining in report.remaining {
            if !failed.contains(&remaining) {
                failed.push(remaining);
            }
        }
        let needs_restart = {
            let mut stored = self
                .restart_advice
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for fresh in report.needs_restart {
                if !stored
                    .iter()
                    .any(|current| current.client_pid == fresh.client_pid)
                {
                    stored.push(fresh);
                }
            }
            stored.retain(|entry| crate::gateway_publish::pid_is_running(entry.client_pid));
            stored.clone()
        };
        if before.running && !crate::http_bridge::status(&self.state).running {
            if let Err(error) =
                crate::http_bridge::start_with_token_at(&self.state, before.port, before.token)
            {
                failed.push(format!(
                    "Could not restart the Shared HTTP endpoint: {error}"
                ));
            }
        }
        ReapOutcome {
            killed: report.killed,
            failed,
            needs_restart,
        }
    }

    /// The clients still launching a superseded gateway, revalidated so an app
    /// the user already restarted drops off the list.
    pub fn restart_advice(&self) -> Vec<crate::gateway_publish::ClientNeedingRestart> {
        let mut stored = self
            .restart_advice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        stored.retain(|entry| crate::gateway_publish::pid_is_running(entry.client_pid));
        stored.clone()
    }

    pub fn shutdown(&self) {
        crate::http_bridge::kill_on_exit(&self.state);
    }
}
