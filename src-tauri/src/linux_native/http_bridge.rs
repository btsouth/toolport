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
    /// At start: bring back an endpoint the user left on. Never asks to
    /// unlock the keyring: while it is locked the endpoint shows as locked
    /// and starts on the first unlock, or on the user's Retry; a start that
    /// fails is tried again a few times while it shows the wait.
    pub fn restore(&self) -> Result<bool, String> {
        match crate::http_bridge::restore_with_retries(
            self.clone(),
            Self::state_of,
            Self::restore_once,
            1,
        ) {
            crate::http_bridge::Restore::Running => Ok(true),
            crate::http_bridge::Restore::Off | crate::http_bridge::Restore::Held => Ok(false),
            crate::http_bridge::Restore::Failed(error) => Err(error),
        }
    }

    fn state_of(&self) -> &crate::http_bridge::HttpBridgeState {
        &self.state
    }

    /// One restore attempt. The lifecycle lock is held from the saved setting
    /// through the start, so a Stop that turned the endpoint off is seen here
    /// and never undone; no keyring call in it may ask to unlock.
    fn restore_once(&self) -> crate::http_bridge::Restore {
        use crate::http_bridge::Restore;
        let _lifecycle = LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::secrets::without_prompts(|| {
            let registry = match crate::registry::load() {
                Ok(registry) => registry,
                Err(error) => return Restore::Failed(error),
            };
            if !registry.http_bridge_enabled {
                return Restore::Off;
            }
            if self.status().running {
                return Restore::Running;
            }
            if crate::http_bridge::hold_for_keyring(&self.state) {
                return Restore::Held;
            }
            match self.start_locked(registry.http_bridge_port) {
                Ok(_) => Restore::Running,
                Err(error) => Restore::Failed(error),
            }
        })
    }

    /// The user's Retry on a locked endpoint: may ask to unlock the keyring.
    pub fn retry_restore(&self) -> Result<crate::http_bridge::HttpBridgeStatus, String> {
        let _lifecycle = LIFECYCLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let registry = crate::registry::load()?;
        if !registry.http_bridge_enabled {
            // Turned off meanwhile.
            return Ok(self.status());
        }
        self.start_locked(registry.http_bridge_port)
    }

    /// The user's Start. While the keyring is locked it never asks: it saves
    /// the setting and waits like an endpoint left on (shows "Keyring locked",
    /// starts on the first unlock); Retry is the one that may ask.
    pub fn turn_on(
        &self,
        port: Option<u16>,
    ) -> Result<crate::http_bridge::HttpBridgeStatus, String> {
        {
            let _lifecycle = LIFECYCLE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.status().running || !crate::secrets::store_would_prompt() {
                return self.start_locked(port);
            }
            crate::registry::update(|registry| {
                registry.http_bridge_enabled = true;
                if port.is_some() {
                    registry.http_bridge_port = port;
                }
                Ok(())
            })?;
        }
        self.restore()?;
        Ok(self.status())
    }

    fn start_locked(
        &self,
        port: Option<u16>,
    ) -> Result<crate::http_bridge::HttpBridgeStatus, String> {
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
            return Err(crate::i18n::trf(
                "Could not save the HTTP endpoint token: {value0}",
                &[("value0", &error.to_string())],
            ));
        }
        if let Err(error) = crate::registry::update(|registry| {
            registry.http_bridge_enabled = true;
            registry.http_bridge_port = status.port;
            Ok(())
        }) {
            if !was_running {
                let _ = crate::http_bridge::stop(&self.state);
            }
            return Err(crate::i18n::trf(
                "Could not save the HTTP endpoint setting: {value0}",
                &[("value0", &error.to_string())],
            ));
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
        let extra_keep = crate::clients::resolve_gateway_path_readonly()
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
                failed.push(crate::i18n::trf(
                    "Could not restart the Shared HTTP endpoint: {value0}",
                    &[("value0", &error.to_string())],
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

#[cfg(test)]
mod tests {
    /// Failure mode: with the endpoint left on, every start read its saved
    /// bearer token, and a locked keyring asked to be unlocked.
    #[test]
    fn a_start_with_the_endpoint_left_on_never_asks_to_unlock_the_keyring() {
        crate::secrets::tests::with_isolated_vault(|| {
            crate::registry::save(&crate::registry::Registry {
                http_bridge_enabled: true,
                http_bridge_port: Some(1),
                ..Default::default()
            })
            .unwrap();
            let bridge = super::BridgeController::default();
            let mut restored = None;
            let prompts = crate::secrets::locked_store::prompts_during(|| {
                restored = Some(bridge.restore());
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            assert_eq!(restored, Some(Ok(false)));
            assert!(!bridge.status().running);
            // End its wait for an unlock inside this test's data dir.
            crate::http_bridge::stop(&bridge.state).unwrap();
        });
    }

    /// Start while the keyring is locked never asks: it waits, shown as
    /// locked; Stop ends that wait.
    #[test]
    fn starting_the_endpoint_while_the_keyring_is_locked_waits() {
        crate::secrets::tests::with_isolated_vault(|| {
            crate::registry::save(&crate::registry::Registry::default()).unwrap();
            let bridge = super::BridgeController::default();
            let mut started = None;
            let prompts = crate::secrets::locked_store::prompts_during(|| {
                started = Some(bridge.turn_on(None));
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            let status = started.unwrap().unwrap();
            assert!(status.keyring_locked && !status.running, "{status:?}");
            assert!(crate::registry::load().unwrap().http_bridge_enabled);
            bridge.stop().unwrap();
            assert!(!crate::registry::load().unwrap().http_bridge_enabled);
        });
    }

    /// Failure mode: an endpoint waiting for the keyring could not be turned
    /// off; its saved setting stayed on and the first unlock started it.
    #[test]
    fn stopping_an_endpoint_that_waits_for_the_keyring_turns_it_off_for_good() {
        crate::secrets::tests::with_isolated_vault(|| {
            crate::registry::save(&crate::registry::Registry {
                http_bridge_enabled: true,
                http_bridge_port: Some(1),
                ..Default::default()
            })
            .unwrap();
            let bridge = super::BridgeController::default();
            let prompts = crate::secrets::locked_store::prompts_during(|| {
                assert_eq!(bridge.restore(), Ok(false));
                assert!(bridge.status().keyring_locked);
                let stopped = bridge.stop().unwrap();
                assert!(!stopped.running && !stopped.keyring_locked, "{stopped:?}");
            });
            assert!(
                prompts.is_empty(),
                "turning it off read the keyring: {prompts:?}"
            );
            assert!(!crate::registry::load().unwrap().http_bridge_enabled);
            // Unlocked now: the waiter's next poll must not bring it back, and
            // a late restore keeps it off.
            std::thread::sleep(crate::http_bridge::KEYRING_POLL * 2);
            assert!(!bridge.status().running);
            assert_eq!(bridge.restore(), Ok(false));
            assert!(!crate::registry::load().unwrap().http_bridge_enabled);
        });
    }
}
