//! Shell-neutral one-off server connections and health probes.
//!
//! The gateway owns long-lived downstream connections. Desktop shells use this
//! module for explicit user-driven tests and playground operations only.

use std::time::Duration;

use crate::downstream::{resolve_root_token, DownstreamServer, StdioTransport};
use crate::registry::ServerEntry;
use crate::{remote, secrets};

#[derive(Debug, Clone, serde::Serialize)]
// Parity scenarios (linux_native::parity) read probe answers back into this type.
#[cfg_attr(test, derive(serde::Deserialize))]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub server_id: String,
    pub ok: bool,
    pub tool_count: usize,
    pub error: Option<String>,
    pub auth_required: bool,
    pub failure: Option<crate::call_failure::CallFailureKind>,
    pub auth_target: Option<crate::call_failure::AuthTarget>,
    /// Not checked: the server keeps values in a locked keyring, and a check
    /// nobody asked for does not ask to unlock it.
    #[serde(default)]
    pub keyring_locked: bool,
}

impl ProbeResult {
    pub fn failure_label(&self) -> &str {
        use crate::call_failure::{AuthTarget, CallFailureKind as K};
        if self.keyring_locked {
            return "Keyring locked";
        }
        if self.auth_required {
            return match self.auth_target {
                Some(AuthTarget::Scope) => "Permission required",
                Some(AuthTarget::ServiceCredential) => "Service key required",
                _ => "Needs sign-in",
            };
        }
        if self
            .error
            .as_deref()
            .is_some_and(|raw| raw.contains("downstream server exited"))
        {
            return "Server stopped";
        }
        if self
            .error
            .as_deref()
            .is_some_and(|raw| raw.contains("Broken pipe"))
        {
            return "Connection closed";
        }
        match self.failure.as_ref() {
            Some(K::Auth {
                target: AuthTarget::Scope,
            }) => "Permission required",
            Some(K::Auth { .. }) => "Needs sign-in",
            Some(K::Timeout { .. }) => "Timed out",
            Some(K::Unavailable { .. }) => "Unreachable",
            Some(K::ServerError { .. }) => "Server failed",
            Some(K::Quota) => "Rate limited",
            Some(K::Cancelled) => "Check cancelled",
            _ => {
                // Older/local launch errors have no transport code. This is only
                // display text; it never changes policy or authorizes a retry.
                let raw = self.error.as_deref().unwrap_or("");
                if raw.contains("exited") || raw.contains("EOF") || raw.contains("closed stdout") {
                    "Server stopped"
                } else if raw.contains("No such file") || raw.contains("not found") {
                    "Command or file not found"
                } else {
                    "Connection failed"
                }
            }
        }
    }
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
            && !entry.unknown_fields.contains_key("source")
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
    connect_server_classified(server).map_err(|error| error.to_string())
}

fn connect_server_classified(
    server: &ServerEntry,
) -> Result<DownstreamServer, crate::call_failure::CallFailure> {
    if let Some(command) = &server.command {
        let resolved_server =
            crate::secret_refs::resolve_server(server).map_err(|e| e.to_string())?;
        let server = &resolved_server;
        let env = environment_for_probe_with(server, secrets::get_secret_result)?;
        let cwd = server
            .cwd
            .as_deref()
            .and_then(|cwd| resolve_root_token(cwd, None));
        let resolved = crate::launch_inputs::resolve_args(server)?;
        let mut transport = StdioTransport::spawn(
            command,
            &resolved.args,
            &env,
            cwd.as_deref(),
            server.inherit_env,
        )
        .map_err(|error| resolved.redact(error))?;
        if let Some(timeout) = server.initialize_timeout()? {
            transport.set_connect_timeout(timeout);
        }
        DownstreamServer::connect_classified(
            server.id.clone(),
            remote::protect_transport(server, Box::new(transport)),
        )
        .map_err(|error| {
            crate::call_failure::CallFailure::new(error.kind, resolved.redact(error.detail))
        })
    } else if server.url.is_some() {
        remote::connect_remote_classified(server, None, None, None, None)
    } else {
        Err("no command or url".to_string().into())
    }
}

pub fn probe_one(server: &ServerEntry) -> ProbeResult {
    match connect_server_classified(server) {
        Ok(connection) => ProbeResult {
            server_id: server.id.clone(),
            ok: true,
            tool_count: connection.tools.len(),
            error: None,
            auth_required: false,
            failure: None,
            auth_target: None,
            keyring_locked: false,
        },
        Err(error) => failed_probe(server, error),
    }
}

fn failed_probe(server: &ServerEntry, error: crate::call_failure::CallFailure) -> ProbeResult {
    let missing_credential = missing_secret(server);
    let typed_auth = match error.kind {
        crate::call_failure::CallFailureKind::Auth { target } => Some(target),
        _ => None,
    };
    // Prose is a compatibility fallback for unclassified launch errors only.
    let auth_rejected = typed_auth.is_some()
        || (error.kind == crate::call_failure::CallFailureKind::Internal
            && remote::is_auth_error(&error.detail));
    ProbeResult {
        server_id: server.id.clone(),
        ok: false,
        tool_count: 0,
        auth_required: auth_rejected || missing_credential,
        auth_target: if missing_credential {
            Some(crate::call_failure::AuthTarget::ServiceCredential)
        } else if let Some(target) = typed_auth {
            Some(target)
        } else if auth_rejected && server.url.is_some() {
            Some(crate::call_failure::AuthTarget::Endpoint)
        } else {
            None
        },
        failure: Some(error.kind),
        error: Some(error.detail),
        keyring_locked: false,
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
    // The keyring gates are per thread: a caller that may not ask to unlock (a quiet
    // Connect all step, background work) keeps that on the probe's thread. A caller
    // that may ask is never made quiet.
    let noninteractive = crate::clients::backup_vault::is_noninteractive();
    let quiet = crate::secrets::prompts_blocked();
    std::thread::spawn(move || {
        let probe = || probe_one(&server_for_probe);
        let result = if noninteractive {
            crate::clients::backup_vault::noninteractive(probe)
        } else if quiet {
            crate::secrets::without_prompts(probe)
        } else {
            probe()
        };
        let _ = sender.send(result);
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
            failure: Some(crate::call_failure::CallFailureKind::Timeout { after_send: false }),
            auth_target: None,
            keyring_locked: false,
        })
}

/// Test Connection for a saved server: a user action, so it may ask to unlock
/// the store.
pub fn probe_registered(server_id: &str) -> Result<ProbeResult, String> {
    let (server, enabled) = registered_server_state(server_id)?;
    // A team or shared server still waiting for review runs nothing: no secret
    // is read and no command starts until the user consents in Teams.
    if !enabled && server.needs_team_enable_review() {
        return Err(crate::registry::TEAM_REVIEW_REQUIRED.to_string());
    }
    Ok(probe_one_bounded(&server))
}

/// The health check a shell runs on its own: at start, after a change, when a
/// page comes back. Nobody asked for it, so it must not ask to unlock the store.
pub fn probe_health_bounded(server: &ServerEntry) -> ProbeResult {
    probe_health_checked(server, &KeyringCheck::default())
}

/// One keyring check for a round of health checks, made by the first one
/// that needs it: which servers keep a locked value in the store, read once,
/// not once per server competing for the store.
#[derive(Clone, Default)]
pub struct KeyringCheck(
    std::sync::Arc<std::sync::OnceLock<Option<std::collections::HashSet<String>>>>,
);

impl KeyringCheck {
    /// Whether this server keeps a locked value; unknown counts as locked.
    fn locked(
        &self,
        server: &ServerEntry,
        check: impl FnOnce() -> Option<std::collections::HashSet<String>>,
    ) -> bool {
        match self.0.get_or_init(check) {
            None => true,
            Some(owners) => {
                owners.contains(&server.id)
                    || crate::local_auth::owner(&server.id)
                        .is_ok_and(|owner| owners.contains(&owner))
            }
        }
    }
}

/// [`probe_health_bounded`] sharing its round's keyring check.
pub fn probe_health_checked(server: &ServerEntry, check: &KeyringCheck) -> ProbeResult {
    // Property reads that ask nothing: a read of this server's locked value
    // could raise the unlock prompt. A server with nothing stored is checked
    // even while the default collection stays locked.
    if keeps_values_in_store(server) && check.locked(server, secrets::owners_with_locked_values) {
        return ProbeResult {
            server_id: server.id.clone(),
            ok: false,
            tool_count: 0,
            error: Some("keyring locked".into()),
            auth_required: false,
            failure: None,
            auth_target: None,
            keyring_locked: true,
        };
    }
    probe_one_bounded(server)
}

/// Whether connecting may read a value from the store: a secret with no
/// inline value and no outside source, or a remote server (its sign-in and
/// token state live there). Generous on purpose: it only decides what is
/// skipped while the store is locked.
pub(crate) fn keeps_values_in_store(server: &ServerEntry) -> bool {
    let stored = |secret: bool,
                  value: &Option<String>,
                  fields: &serde_json::Map<String, serde_json::Value>| {
        secret && value.is_none() && !fields.contains_key("source")
    };
    server.url.is_some()
        || server
            .env
            .iter()
            .any(|entry| stored(entry.secret, &entry.value, &entry.unknown_fields))
        || server.launch.as_ref().is_some_and(|launch| {
            launch
                .inputs
                .iter()
                .any(|input| stored(input.secret, &input.value, &input.unknown_fields))
        })
}

/// [`probe_health_checked`] for a saved server.
pub fn probe_health_registered(
    server_id: &str,
    check: &KeyringCheck,
) -> Result<ProbeResult, String> {
    Ok(probe_health_checked(&registered_server(server_id)?, check))
}

/// Servers whose health check found the keyring locked (or busy), for
/// [`recheck_locked_once_open`]. One thread runs at a time; each round's
/// list replaces the one before.
pub struct LockedRecheck {
    servers: Vec<ServerEntry>,
    round: u64,
    running: bool,
}

impl Default for LockedRecheck {
    fn default() -> Self {
        Self::new()
    }
}

impl LockedRecheck {
    pub const fn new() -> Self {
        Self {
            servers: Vec::new(),
            round: 0,
            running: false,
        }
    }

    /// A finished round's locked servers (a round checks every enabled
    /// server). True: start the recheck thread, none runs yet.
    pub fn replace(&mut self, locked: Vec<ServerEntry>) -> bool {
        self.servers = locked;
        self.round += 1;
        let start = !self.running && !self.servers.is_empty();
        self.running |= start;
        start
    }

    /// The thread could not start.
    pub fn stopped(&mut self) {
        self.running = false;
    }

    /// The list to check with its round, or `None`: nothing left, so the
    /// thread ends.
    fn next(&mut self) -> Option<(u64, Vec<ServerEntry>)> {
        if self.servers.is_empty() {
            self.running = false;
            return None;
        }
        Some((self.round, self.servers.clone()))
    }

    /// Keep what is still locked. A list a newer round put in place while
    /// this one was checked stands as it is.
    fn settle(&mut self, round: u64, results: &[ProbeResult]) {
        if round != self.round {
            return;
        }
        self.servers.retain(|server| {
            results
                .iter()
                .find(|result| result.server_id == server.id)
                .is_none_or(|result| result.keyring_locked)
        });
    }
}

/// Servers whose health check found the keyring locked (or busy), checked
/// again without asking each time a poll finds it open, until none is
/// locked. Nothing else checks them again while the page stays open. Start
/// it only when [`LockedRecheck::replace`] says so.
pub fn recheck_locked_once_open(
    locked: &std::sync::Mutex<LockedRecheck>,
    poll: Duration,
    report: impl Fn(&ProbeResult) + Sync,
) {
    let queue = || {
        locked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    loop {
        std::thread::sleep(poll);
        if queue().next().is_none() {
            return;
        }
        if secrets::store_would_prompt() {
            continue;
        }
        let Some((round, servers)) = queue().next() else {
            return;
        };
        let check = KeyringCheck::default();
        let results: Vec<ProbeResult> = std::thread::scope(|scope| {
            let handles: Vec<_> = servers
                .iter()
                .map(|server| scope.spawn(|| probe_health_checked(server, &check)))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        results.iter().for_each(&report);
        queue().settle(round, &results);
    }
}

fn registered_server(server_id: &str) -> Result<ServerEntry, String> {
    registered_server_state(server_id).map(|(server, _)| server)
}

/// A saved server and whether its switch is on.
fn registered_server_state(server_id: &str) -> Result<(ServerEntry, bool), String> {
    let path = crate::registry::resolved_path().ok_or("could not resolve the registry path")?;
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read the registry: {error}"))?;
    let registry = crate::registry::parse_registry_contents(&contents)
        .map_err(|error| format!("could not parse the registry: {error}"))?;
    let enabled = registry.enabled_here(server_id);
    registry
        .servers
        .into_iter()
        .find(|server| server.id == server_id)
        .map(|server| (server, enabled))
        .ok_or_else(|| format!("server '{server_id}' not found"))
}

/// The servers the Servers page shows as on. Default access only limits what
/// clients without their own access set can see, so it must not narrow this list:
/// a server left out would stay on "Checking..." with no way to finish.
pub fn enabled_servers(registry: &crate::registry::Registry) -> Vec<ServerEntry> {
    registry
        .servers
        .iter()
        .filter(|server| registry.enabled_here(&server.id))
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
            enabled: false,
            inherit_env: false,
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
            hide_prompts: false,
            client_credentials: None,
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    /// Failure mode: with the default collection locked all session and no
    /// Toolport value in it, every remote server showed "Keyring locked";
    /// 2.3.5 checked them. Only a server with a locked stored value waits.
    #[test]
    fn a_locked_default_collection_holds_only_servers_with_stored_values() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut remote = server();
            remote.id = "no-auth".into();
            remote.transport = "http".into();
            remote.url = Some("http://127.0.0.1:1/mcp".into());
            let mut plain = server();
            plain.id = "plain".into();
            plain.command = Some("/bin/false".into());
            let mut stored = server();
            stored.id = "stored".into();
            stored.command = Some("/bin/false".into());
            stored.env = vec![crate::registry::EnvVar {
                key: "API_KEY".into(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            }];
            let mut results = Vec::new();
            let prompts = crate::secrets::locked_store::locked_values_during(&["stored"], || {
                assert!(
                    crate::secrets::store_would_prompt(),
                    "the default is locked"
                );
                let check = KeyringCheck::default();
                for server in [&remote, &plain, &stored] {
                    results.push(probe_health_checked(server, &check));
                }
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            assert!(!results[0].keyring_locked, "{:?}", results[0]);
            assert!(!results[1].keyring_locked, "{:?}", results[1]);
            assert!(results[2].keyring_locked, "{:?}", results[2]);
        });
    }

    /// A round of health checks makes one keyring check, not one per server.
    #[test]
    fn a_round_of_health_checks_checks_the_keyring_once() {
        let check = KeyringCheck::default();
        let calls = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let check = check.clone();
                let calls = &calls;
                scope.spawn(move || {
                    check.locked(&server(), || {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        None
                    })
                });
            }
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Failure mode: a round that marked rows locked while the recheck was
    /// probing lost them to the recheck's cleanup, and a round soon after
    /// the list emptied started a second recheck thread.
    #[test]
    fn a_newer_round_keeps_its_locked_rows_and_one_recheck_runs() {
        let named = |id: &str| ServerEntry {
            id: id.into(),
            ..server()
        };
        let open = |id: &str| ProbeResult {
            server_id: id.into(),
            ok: true,
            tool_count: 1,
            error: None,
            auth_required: false,
            failure: None,
            auth_target: None,
            keyring_locked: false,
        };
        let mut queue = LockedRecheck::new();
        assert!(
            queue.replace(vec![named("a")]),
            "the first starts the thread"
        );
        let (round, _) = queue.next().unwrap();
        // While it probes "a", a newer round finds "a" and "b" locked.
        assert!(!queue.replace(vec![named("a"), named("b")]), "one thread");
        queue.settle(round, &[open("a")]);
        assert_eq!(queue.servers.len(), 2, "the newer round's rows stay");
        // A round that empties the list, then one that fills it again
        // before the thread's next tick: still one thread.
        assert!(!queue.replace(Vec::new()));
        assert!(!queue.replace(vec![named("c")]));
        let (round, _) = queue.next().unwrap();
        queue.settle(round, &[open("c")]);
        assert!(queue.next().is_none() && !queue.running);
        assert!(
            queue.replace(vec![named("d")]),
            "ended: the next starts one"
        );
    }

    /// Failure mode: a row that showed "Keyring locked" (a locked or busy
    /// keyring) kept it after the keyring opened, until a Refresh.
    #[test]
    fn locked_rows_are_checked_again_once_the_keyring_opens() {
        crate::secrets::tests::with_isolated_vault(|| {
            let mut remote = server();
            remote.transport = "http".into();
            remote.url = Some("http://127.0.0.1:1/mcp".into());
            let locked = std::sync::Mutex::new(LockedRecheck::new());
            assert!(locked.lock().unwrap().replace(vec![remote]));
            let (sender, results) = std::sync::mpsc::channel();
            let sender = std::sync::Mutex::new(sender);
            std::thread::scope(|scope| {
                let mut early = None;
                let prompts = crate::secrets::locked_store::prompts_during(|| {
                    scope.spawn(|| {
                        recheck_locked_once_open(&locked, Duration::from_millis(10), |result| {
                            let _ = sender.lock().unwrap().send(result.clone());
                        })
                    });
                    early = Some(results.recv_timeout(Duration::from_millis(300)));
                });
                assert!(prompts.is_empty(), "{prompts:?}");
                assert!(early.unwrap().is_err(), "checked while locked");
                let result = results
                    .recv_timeout(Duration::from_secs(30))
                    .expect("checked once open");
                assert_eq!(result.server_id, "probe");
                assert!(!result.keyring_locked, "{result:?}");
            });
            let queue = locked.lock().unwrap();
            assert!(queue.servers.is_empty() && !queue.running);
        });
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
    fn typed_probe_failures_win_over_auth_words_in_details() {
        use crate::call_failure::{AuthTarget, CallFailure, CallFailureKind as K};
        let mut endpoint = server();
        endpoint.url = Some("https://example.invalid/401/".into());
        for (kind, detail, label) in [
            (
                K::Unavailable { after_send: false },
                "could not reach https://example.invalid/401/",
                "Unreachable",
            ),
            (
                K::ServerError { after_send: true },
                "HTTP 500: upstream unauthorized",
                "Server failed",
            ),
        ] {
            let result = failed_probe(&endpoint, CallFailure::new(kind.clone(), detail));
            assert!(!result.auth_required);
            assert_eq!(result.auth_target, None);
            assert_eq!(result.failure, Some(kind));
            assert_eq!(result.failure_label(), label);
        }
        let fallback = failed_probe(&endpoint, CallFailure::new(K::Internal, "unauthorized"));
        assert!(fallback.auth_required);
        assert_eq!(fallback.auth_target, Some(AuthTarget::Endpoint));
        let scope = failed_probe(
            &endpoint,
            CallFailure::new(
                K::Auth {
                    target: AuthTarget::Scope,
                },
                "401 unauthorized",
            ),
        );
        assert_eq!(scope.failure_label(), "Permission required");
        assert_eq!(scope.auth_target, Some(AuthTarget::Scope));
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
    fn enabled_servers_ignores_a_default_access_set_that_leaves_servers_out() {
        let mut registry = crate::registry::Registry::default();
        let mut first = server();
        first.id = "first".into();
        first.enabled = true;
        let mut second = server();
        second.id = "second".into();
        second.enabled = true;
        let mut off = server();
        off.id = "off".into();
        registry.servers.extend([first, second, off]);
        let work = registry.add_profile("Work");
        registry.set_access_server(&work, "first", true).unwrap();
        registry.set_default_access(Some(&work)).unwrap();
        assert_eq!(registry.enabled_servers().len(), 1);

        let ids = |registry: &crate::registry::Registry| {
            enabled_servers(registry)
                .into_iter()
                .map(|server| server.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&registry), ["first", "second"]);

        let empty = registry.add_profile("Empty");
        registry.set_default_access(Some(&empty)).unwrap();
        assert!(registry.enabled_servers().is_empty());
        assert_eq!(ids(&registry), ["first", "second"]);
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
    fn test_connection_starts_nothing_for_a_team_server_awaiting_review() {
        let data = crate::registry::DataDirTestEnv::new("probe-registered-team-review");
        let marker = data.dir.join("started");
        let registry = serde_json::json!({
            "version": 3,
            "servers": [{
                "id": "team-cmd",
                "name": "Team command",
                "transport": "stdio",
                "command": "sh",
                "args": ["-c", format!("touch '{}'", marker.display())],
                "env": [],
                "enabled": false,
                "source": "team:t",
                "teamEnableReview": true
            }],
            "profiles": [{ "id": "default", "name": "Default", "enabledServerIds": [] }],
            "activeProfileId": "default"
        });
        std::fs::write(
            crate::registry::resolved_path().unwrap(),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        let error = probe_registered("team-cmd").unwrap_err();

        assert_eq!(error, crate::registry::TEAM_REVIEW_REQUIRED);
        assert!(!marker.exists(), "the team command must not start");
    }

    #[test]
    fn probe_passes_composed_launch_argument_to_real_stdio_child() {
        let _data = crate::registry::DataDirTestEnv::new(
            "probe_passes_composed_launch_argument_to_real_stdio_child",
        );
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
                    unknown_fields: Default::default(),
                },
                LaunchInput {
                    key: "KEY".into(),
                    label: "Key".into(),
                    secret: false,
                    required: true,
                    value: Some("key".into()),
                    unknown_fields: Default::default(),
                },
                LaunchInput {
                    key: "SECRET".into(),
                    label: "Secret".into(),
                    secret: true,
                    required: true,
                    value: Some("secret".into()),
                    unknown_fields: Default::default(),
                },
            ],
            bindings: vec![ArgBinding {
                index: 1,
                parts: vec![
                    ArgPart::Input {
                        key: "ACCOUNT".into(),
                        unknown_fields: Default::default(),
                    },
                    ArgPart::Literal {
                        value: "/".into(),
                        unknown_fields: Default::default(),
                    },
                    ArgPart::Input {
                        key: "KEY".into(),
                        unknown_fields: Default::default(),
                    },
                    ArgPart::Literal {
                        value: ":".into(),
                        unknown_fields: Default::default(),
                    },
                    ArgPart::Input {
                        key: "SECRET".into(),
                        unknown_fields: Default::default(),
                    },
                ],
                unknown_fields: Default::default(),
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
            unknown_fields: Default::default(),
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
