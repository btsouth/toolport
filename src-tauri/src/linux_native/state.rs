use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::gio;

use crate::registry::{self, Registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivityView {
    pub(super) timestamp_ms: u64,
    pub(super) server: String,
    pub(super) tool: String,
    pub(super) client: Option<String>,
    pub(super) ok: bool,
    pub(super) held: bool,
    pub(super) duration_ms: Option<u64>,
    pub(super) error: Option<String>,
    /// Values pseudonymized in this call's result before the model saw them.
    /// `None` when the redaction pass did not run for this call.
    pub(super) pii_replaced: Option<u64>,
    /// The redaction pass ran but did not fully apply (session map full, or the
    /// result exceeded the scan cap) - some values reached the model in the clear.
    pub(super) pii_incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivitySnapshot {
    pub(super) recent: Vec<ActivityView>,
    pub(super) security_events: Vec<serde_json::Value>,
    pub(super) search_traces: Vec<serde_json::Value>,
    pub(super) inspect_calls: Vec<serde_json::Value>,
    pub(super) call_count: usize,
    pub(super) error_count: usize,
    pub(super) average_duration_ms: Option<u64>,
    /// Cumulative tool-definition tokens lazy discovery kept out of client context,
    /// from the local savings log; not derived from the audit entries.
    pub(super) tokens_saved: u64,
    pub(super) savings_list_loads: u64,
    pub(super) savings_peak_catalog: u64,
    pub(super) savings_since_ts: u64,
    /// Every pinned tool's provenance, newest-changed first.
    pub(super) tool_identities: Vec<crate::integrity::ToolIdentity>,
    /// Per-server aggregation of the full retained log (`audit::stats` rows),
    /// busiest first; each row carries its per-tool breakdown.
    pub(super) server_stats: Vec<serde_json::Value>,
}

impl ActivitySnapshot {
    fn from_entries(entries: Vec<serde_json::Value>, recent_limit: usize) -> Self {
        let mut duration_total = 0u64;
        let mut duration_count = 0u64;
        let mut error_count = 0usize;
        let mut call_count = 0usize;
        let mut calls = Vec::new();

        for entry in entries {
            let Some(ok) = crate::audit::tool_call_ok(&entry) else {
                continue;
            };
            call_count += 1;
            if !ok {
                error_count += 1;
            }
            let duration_ms = entry.get("durationMs").and_then(serde_json::Value::as_u64);
            if let Some(duration) = duration_ms {
                duration_total = duration_total.saturating_add(duration);
                duration_count += 1;
            }
            if calls.len() < recent_limit {
                calls.push(ActivityView {
                    timestamp_ms: entry
                        .get("ts")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    server: activity_server(&entry),
                    tool: entry
                        .get("tool")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Unknown tool")
                        .to_string(),
                    client: entry
                        .get("clientName")
                        .or_else(|| entry.get("client"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string),
                    ok,
                    held: entry
                        .get("held")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    duration_ms,
                    error: entry
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(activity_error_summary),
                    pii_replaced: entry.get("piiReplaced").and_then(serde_json::Value::as_u64),
                    pii_incomplete: entry
                        .get("piiIncomplete")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }

        Self {
            call_count,
            recent: calls,
            security_events: Vec::new(),
            search_traces: Vec::new(),
            inspect_calls: Vec::new(),
            error_count,
            average_duration_ms: (duration_count > 0).then(|| duration_total / duration_count),
            tokens_saved: 0,
            savings_list_loads: 0,
            savings_peak_catalog: 0,
            savings_since_ts: 0,
            tool_identities: Vec::new(),
            server_stats: Vec::new(),
        }
    }
}

fn activity_server(entry: &serde_json::Value) -> String {
    if let Some(server) = entry
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|server| !server.is_empty())
    {
        return server.to_string();
    }
    entry
        .get("tool")
        .and_then(serde_json::Value::as_str)
        .and_then(|tool| tool.split_once("__").map(|(server, _)| server.trim()))
        .filter(|server| !server.is_empty())
        .unwrap_or("Unknown server")
        .to_string()
}

/// Group provenance rows by source server for display, preserving the incoming
/// newest-changed-first order both across groups and inside each one. An alias
/// that could not be attributed lands in an explicit "Unattributed" group rather
/// than being guessed or hidden.
pub(super) fn group_tool_identities(
    identities: &[crate::integrity::ToolIdentity],
) -> Vec<(String, Vec<crate::integrity::ToolIdentity>)> {
    let mut groups: Vec<(String, Vec<crate::integrity::ToolIdentity>)> = Vec::new();
    for identity in identities {
        let label = if identity.server_name.is_empty() {
            if identity.server_id.is_empty() {
                "Unattributed".to_string()
            } else {
                identity.server_id.clone()
            }
        } else {
            identity.server_name.clone()
        };
        match groups.iter_mut().find(|(name, _)| *name == label) {
            Some((_, members)) => members.push(identity.clone()),
            None => groups.push((label, vec![identity.clone()])),
        }
    }
    groups
}

/// The savings tile's number, compressed the way the shipping sidebar shows it.
pub(super) fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn activity_error_summary(error: &str) -> String {
    const MAX_CHARS: usize = 180;
    let readable = serde_json::from_str::<serde_json::Value>(error)
        .ok()
        .and_then(|value| activity_error_message(&value).map(str::to_string))
        .unwrap_or_else(|| error.to_string());
    let collapsed = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return collapsed;
    }
    let mut summary = collapsed.chars().take(MAX_CHARS).collect::<String>();
    summary.push('…');
    summary
}

fn activity_error_message(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(object) => {
            for key in ["message", "error", "detail"] {
                if let Some(message) = object.get(key).and_then(serde_json::Value::as_str) {
                    if !message.trim().is_empty() {
                        return Some(message);
                    }
                }
            }
            object.values().find_map(activity_error_message)
        }
        serde_json::Value::Array(values) => values.iter().find_map(activity_error_message),
        _ => None,
    }
}

pub(super) fn load_activity_snapshot() -> Result<ActivitySnapshot, String> {
    let mut entries = crate::audit::read_all()
        .map_err(|error| format!("could not read retained activity: {error}"))?;
    for entry in &mut entries {
        let server = activity_server(entry);
        let needs_server = entry
            .get("server")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|server| server.trim().is_empty());
        if needs_server {
            if let Some(object) = entry.as_object_mut() {
                object.insert("server".to_string(), serde_json::Value::String(server));
            }
        }
    }
    let server_stats = crate::audit::stats_for_entries(&entries)
        .get("servers")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut snapshot = ActivitySnapshot::from_entries(entries, 100);
    snapshot.server_stats = server_stats;
    snapshot.security_events = crate::integrity::read_recent(25)
        .map_err(|error| format!("could not read security events: {error}"))?;
    snapshot.search_traces = crate::searchtrace::read_recent(25)
        .map_err(|error| format!("could not read discovery traces: {error}"))?;
    snapshot.inspect_calls = crate::inspect::read_recent(25)
        .map_err(|error| format!("could not read inspector captures: {error}"))?;
    let savings = crate::savings::summary();
    let savings_number = |key: &str| {
        savings
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    snapshot.tokens_saved = savings_number("tokensSaved");
    snapshot.savings_list_loads = savings_number("listLoads");
    snapshot.savings_peak_catalog = savings_number("peakCatalog");
    snapshot.savings_since_ts = savings_number("sinceTs");
    let registry = crate::registry::load()
        .map_err(|error| format!("could not read the registry for tool identities: {error}"))?;
    snapshot.tool_identities =
        crate::integrity::tool_identities(&registry.servers, &registry.profiles)
            .map_err(|error| format!("could not read tool identity pins: {error}"))?;
    Ok(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientGatewayState {
    Connected,
    Customized,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) app_present: bool,
    pub(super) config_exists: bool,
    pub(super) uses_connectors: bool,
    pub(super) server_count: usize,
    /// Config-file servers other than the gateway itself: what a one-shot
    /// migration would import and then move out of the client. Plugin-detected
    /// servers are excluded because migration leaves them untouched.
    pub(super) movable_server_count: usize,
    pub(super) gateway_state: ClientGatewayState,
    pub(super) shared_http: bool,
    pub(super) scope_id: Option<String>,
    pub(super) scope_name: Option<String>,
    pub(super) discovery_mode: Option<String>,
    pub(super) config_error: bool,
}

impl ClientView {
    fn from_detected(client: crate::clients::DetectedClient) -> Self {
        Self {
            id: client.id,
            name: client.name,
            app_present: client.app_present,
            config_exists: client.config_exists,
            uses_connectors: client.uses_connectors,
            server_count: client.servers.len() + client.plugin_servers.len(),
            movable_server_count: client
                .servers
                .iter()
                .filter(|server| !crate::clients::detected_is_gateway(server))
                .count(),
            gateway_state: match client.entry_state {
                crate::clients::GatewayEntryState::Managed => ClientGatewayState::Connected,
                crate::clients::GatewayEntryState::Customized => ClientGatewayState::Customized,
                crate::clients::GatewayEntryState::Absent => ClientGatewayState::Disconnected,
            },
            shared_http: false,
            scope_id: None,
            scope_name: None,
            discovery_mode: None,
            config_error: client.error.is_some(),
        }
    }
}

pub(super) struct ClientSnapshot {
    pub(super) clients: Vec<ClientView>,
    pub(super) profiles: Vec<ProfileView>,
}

pub(super) fn detect_client_views() -> Result<ClientSnapshot, String> {
    let path = registry::resolved_path().ok_or_else(|| "registry path unavailable".to_string())?;
    let registry = match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<Registry>(&contents)
            .map_err(|error| format!("could not parse registry: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::default(),
        Err(error) => return Err(format!("could not read registry: {error}")),
    };
    let mut clients = crate::clients::detect_clients();
    crate::clients::apply_entry_states(&mut clients, &registry.client_managed_entries);
    let shared_http_clients = registry
        .http_clients
        .iter()
        .filter_map(|client| client.id.strip_prefix("client:").map(str::to_string))
        .collect::<std::collections::HashSet<_>>();
    let mut clients = clients
        .into_iter()
        .map(ClientView::from_detected)
        .collect::<Vec<_>>();
    for client in &mut clients {
        client.shared_http = shared_http_clients.contains(&client.id);
        client.scope_id = registry
            .client_scopes
            .get(&client.id)
            .filter(|scope| !scope.is_empty())
            .cloned();
        client.scope_name = client.scope_id.as_ref().and_then(|scope| {
            registry
                .profiles
                .iter()
                .find(|profile| profile.id == *scope || profile.name == *scope)
                .map(|profile| profile.name.clone())
        });
        client.discovery_mode = registry.client_discovery.get(&client.id).cloned();
    }
    clients.sort_by(|left, right| {
        right
            .app_present
            .cmp(&left.app_present)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(ClientSnapshot {
        clients,
        profiles: registry
            .profiles
            .into_iter()
            .map(|profile| ProfileView {
                id: profile.id,
                name: profile.name,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) transport: String,
    pub(super) transport_id: String,
    pub(super) command: Option<String>,
    pub(super) args: Vec<String>,
    pub(super) url: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) secret_keys: Vec<String>,
    pub(super) client_credentials: Option<ClientCredentialsView>,
    pub(super) enabled: bool,
    pub(super) requires_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientCredentialsView {
    pub(super) client_id: String,
    pub(super) token_endpoint_auth_method: Option<String>,
    pub(super) scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistrySnapshot {
    pub(super) servers: Vec<ServerView>,
    pub(super) profiles: Vec<ProfileView>,
    pub(super) enabled_count: usize,
    pub(super) profile_count: usize,
    pub(super) active_profile_id: String,
    pub(super) active_profile: String,
    pub(super) active_profile_tool_scope: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileView {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RegistryState {
    Ready(RegistrySnapshot),
    FirstRun,
    Unavailable,
}

impl RegistrySnapshot {
    pub(super) fn from_registry(registry: Registry) -> Self {
        let active_profile_id = registry.active_profile_id();
        let active_profile = registry
            .profiles
            .iter()
            .find(|profile| profile.id == active_profile_id)
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "Default".to_string());
        let active_profile_tool_scope = registry
            .profiles
            .iter()
            .find(|profile| profile.id == active_profile_id)
            .map(|profile| profile.tool_scope.clone())
            .unwrap_or_default();
        let servers = registry
            .servers
            .iter()
            .map(|server| {
                let enabled = registry.is_enabled(&active_profile_id, &server.id);
                ServerView {
                    id: server.id.clone(),
                    name: server.name.clone(),
                    transport: transport_label(&server.transport).to_string(),
                    transport_id: server.transport.clone(),
                    command: server.command.clone(),
                    args: server.args.clone(),
                    url: server.url.clone(),
                    cwd: server.cwd.clone(),
                    secret_keys: server
                        .env
                        .iter()
                        .filter(|entry| entry.secret)
                        .map(|entry| entry.key.clone())
                        .collect(),
                    client_credentials: server.client_credentials.as_ref().map(|credentials| {
                        ClientCredentialsView {
                            client_id: credentials.client_id.clone(),
                            token_endpoint_auth_method: credentials
                                .token_endpoint_auth_method
                                .clone(),
                            scope: credentials.scope.clone(),
                        }
                    }),
                    enabled,
                    requires_review: !enabled && server.needs_team_enable_review(),
                }
            })
            .collect::<Vec<_>>();
        let profiles = registry
            .profiles
            .iter()
            .map(|profile| ProfileView {
                id: profile.id.clone(),
                name: profile.name.clone(),
            })
            .collect();

        Self {
            enabled_count: servers.iter().filter(|server| server.enabled).count(),
            profile_count: registry.profiles.len(),
            active_profile_id,
            active_profile,
            active_profile_tool_scope,
            profiles,
            servers,
        }
    }
}

pub(super) fn transport_label(transport: &str) -> &'static str {
    match transport {
        "stdio" => "Local stdio",
        "http" => "Remote HTTP",
        "sse" => "Remote SSE",
        _ => "Custom transport",
    }
}

fn load_read_only(path: Option<&Path>) -> RegistryState {
    let Some(path) = path else {
        return RegistryState::Unavailable;
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegistryState::FirstRun;
        }
        Err(error) => {
            eprintln!("toolport-gtk: could not read registry: {error}");
            return RegistryState::Unavailable;
        }
    };
    match serde_json::from_str::<Registry>(&contents) {
        Ok(registry) => RegistryState::Ready(RegistrySnapshot::from_registry(registry)),
        Err(error) => {
            eprintln!("toolport-gtk: could not parse registry: {error}");
            RegistryState::Unavailable
        }
    }
}

pub(super) struct RegistryController {
    path: Option<PathBuf>,
    render: Box<dyn Fn(RegistryState)>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    reload_pending: Cell<bool>,
}

impl RegistryController {
    pub(super) fn new(render: impl Fn(RegistryState) + 'static) -> Rc<Self> {
        Rc::new(Self {
            path: registry::resolved_path(),
            render: Box::new(render),
            monitors: RefCell::new(Vec::new()),
            reload_pending: Cell::new(false),
        })
    }

    pub(super) fn attach(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        self.reload();
        self.start_monitors();

        let controller = Rc::clone(self);
        window.connect_destroy(move |_| {
            controller.monitors.borrow_mut().clear();
        });
    }

    fn reload(&self) {
        (self.render)(load_read_only(self.path.as_deref()));
    }

    fn schedule_reload(controller: Weak<Self>) {
        let Some(controller) = controller.upgrade() else {
            return;
        };
        if controller.reload_pending.replace(true) {
            return;
        }
        gtk::glib::idle_add_local_once(move || {
            if let Some(controller) = Rc::downgrade(&controller).upgrade() {
                controller.reload_pending.set(false);
                controller.reload();
            }
        });
    }

    fn start_monitors(self: &Rc<Self>) {
        if !self.monitors.borrow().is_empty() {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };

        let mut monitors = Vec::new();
        if let Ok(monitor) = gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            connect_reload(&monitor, Rc::downgrade(self));
            monitors.push(monitor);
        }
        if let Some(parent) = path.parent() {
            if let Ok(monitor) = gio::File::for_path(parent)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                let target = path.clone();
                let controller = Rc::downgrade(self);
                monitor.connect_changed(move |_, file, other_file, _| {
                    let changed_target = file.path().as_deref() == Some(target.as_path())
                        || other_file.and_then(gio::File::path).as_deref()
                            == Some(target.as_path());
                    if changed_target {
                        Self::schedule_reload(controller.clone());
                    }
                });
                monitors.push(monitor);
            }
        }
        *self.monitors.borrow_mut() = monitors;
    }
}

fn connect_reload(monitor: &gio::FileMonitor, controller: Weak<RegistryController>) {
    monitor.connect_changed(move |_, _, _, _| {
        RegistryController::schedule_reload(controller.clone());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ServerEntry;

    #[test]
    fn token_counts_compress_like_the_shipping_sidebar() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(950), "950");
        assert_eq!(format_token_count(12_340), "12.3k");
        assert_eq!(format_token_count(999_949), "999.9k");
        assert_eq!(format_token_count(4_500_000), "4.5M");
    }

    fn server(id: &str, name: &str, transport: &str) -> ServerEntry {
        ServerEntry {
            id: id.to_string(),
            name: name.to_string(),
            transport: transport.to_string(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            cwd: None,
            source: None,
            disabled_tools: Vec::new(),
            client_credentials: None,
            request_timeout_ms: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn maps_only_non_secret_server_fields() {
        let mut registry = Registry::default();
        let mut local = server("local", "Files", "stdio");
        local.env.push(crate::registry::EnvVar {
            key: "TOKEN".into(),
            value: Some("must-not-enter-the-view".into()),
            secret: true,
        });
        registry.servers.push(local);
        registry.servers.push(server("remote", "GitHub", "http"));
        registry.profiles[0]
            .enabled_server_ids
            .push("remote".into());

        let snapshot = RegistrySnapshot::from_registry(registry);

        assert_eq!(snapshot.enabled_count, 1);
        assert_eq!(snapshot.profile_count, 1);
        assert_eq!(snapshot.active_profile, "Default");
        assert_eq!(
            snapshot.servers,
            vec![
                ServerView {
                    id: "local".into(),
                    name: "Files".into(),
                    transport: "Local stdio".into(),
                    transport_id: "stdio".into(),
                    command: None,
                    args: Vec::new(),
                    url: None,
                    cwd: None,
                    secret_keys: vec!["TOKEN".into()],
                    client_credentials: None,
                    enabled: false,
                    requires_review: false,
                },
                ServerView {
                    id: "remote".into(),
                    name: "GitHub".into(),
                    transport: "Remote HTTP".into(),
                    transport_id: "http".into(),
                    command: None,
                    args: Vec::new(),
                    url: None,
                    cwd: None,
                    secret_keys: Vec::new(),
                    client_credentials: None,
                    enabled: true,
                    requires_review: false,
                },
            ]
        );
    }

    #[test]
    fn client_view_keeps_inventory_but_discards_config_details() {
        let detected = crate::clients::DetectedClient {
            id: "codex".into(),
            name: "Codex".into(),
            uses_connectors: false,
            config_path: "/private/config.toml".into(),
            config_exists: true,
            app_present: true,
            servers: vec![crate::clients::McpServer {
                name: "private".into(),
                transport: "stdio".into(),
                command: Some("secret-command".into()),
                args: vec!["secret-argument".into()],
                env_keys: vec!["SECRET_TOKEN".into()],
                url: None,
            }],
            plugin_servers: Vec::new(),
            gateway_installed: true,
            entry_state: crate::clients::GatewayEntryState::Managed,
            error: None,
        };

        let view = ClientView::from_detected(detected);

        assert_eq!(view.server_count, 1);
        assert_eq!(view.gateway_state, ClientGatewayState::Connected);
        let rendered = format!("{view:?}");
        assert!(!rendered.contains("/private"));
        assert!(!rendered.contains("secret-command"));
        assert!(!rendered.contains("SECRET_TOKEN"));
    }

    #[test]
    fn activity_view_excludes_arguments_results_and_hashes() {
        let snapshot = ActivitySnapshot::from_entries(
            vec![serde_json::json!({
                "ts": 42,
                "server": "github",
                "tool": "create_issue",
                "ok": false,
                "durationMs": 125,
                "clientName": "Codex",
                "error": "permission denied",
                "args": { "token": "must-not-enter-the-view" },
                "result": "private-result",
                "argsHash": "private-hash"
            })],
            100,
        );

        assert_eq!(snapshot.call_count, 1);
        assert_eq!(snapshot.error_count, 1);
        assert_eq!(snapshot.average_duration_ms, Some(125));
        assert_eq!(snapshot.recent[0].client.as_deref(), Some("Codex"));
        let rendered = format!("{snapshot:?}");
        assert!(!rendered.contains("must-not-enter-the-view"));
        assert!(!rendered.contains("private-result"));
        assert!(!rendered.contains("private-hash"));
    }

    #[test]
    fn activity_summary_counts_all_calls_but_limits_recent_rows() {
        let entries = (0..105)
            .map(|index| {
                serde_json::json!({
                    "ts": index,
                    "server": "files",
                    "tool": "read",
                    "ok": index != 104,
                    "durationMs": 10
                })
            })
            .collect();

        let snapshot = ActivitySnapshot::from_entries(entries, 100);

        assert_eq!(snapshot.call_count, 105);
        assert_eq!(snapshot.recent.len(), 100);
        assert_eq!(snapshot.error_count, 1);
        assert_eq!(snapshot.average_duration_ms, Some(10));
    }

    #[test]
    fn blank_activity_servers_use_the_qualified_tool_prefix() {
        let snapshot = ActivitySnapshot::from_entries(
            vec![serde_json::json!({
                "server": "",
                "tool": "cloudflare_full_api__create_pages_domain",
                "ok": false
            })],
            100,
        );

        assert_eq!(snapshot.recent[0].server, "cloudflare_full_api");
        assert_eq!(
            activity_server(&serde_json::json!({ "tool": "unqualified" })),
            "Unknown server"
        );
    }

    #[test]
    fn empty_activity_has_no_average_and_never_divides_by_zero() {
        let snapshot = ActivitySnapshot::from_entries(Vec::new(), 100);

        assert_eq!(snapshot.call_count, 0);
        assert_eq!(snapshot.average_duration_ms, None);
        assert!(snapshot.recent.is_empty());
    }

    #[test]
    fn activity_errors_are_single_line_and_bounded_for_cards() {
        let error = format!("first line\n{}", "private-ish detail ".repeat(30));
        let summary = activity_error_summary(&error);

        assert!(!summary.contains('\n'));
        assert!(summary.chars().count() <= 181);
        assert!(summary.ends_with('…'));
        assert_eq!(
            activity_error_summary(r#"{"errors":[{"message":"Authentication error"}]}"#),
            "Authentication error"
        );
    }

    #[test]
    fn read_only_load_does_not_change_registry_bytes() {
        let path = std::env::temp_dir().join(format!(
            "toolport-gtk-registry-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let registry = Registry::default();
        let original = serde_json::to_vec_pretty(&registry).unwrap();
        std::fs::write(&path, &original).unwrap();

        assert!(matches!(
            load_read_only(Some(&path)),
            RegistryState::Ready(_)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn later_reads_reflect_an_external_registry_change() {
        let path = std::env::temp_dir().join(format!(
            "toolport-gtk-refresh-registry-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let mut registry = Registry::default();
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();

        let RegistryState::Ready(initial) = load_read_only(Some(&path)) else {
            panic!("the initial registry should load");
        };
        assert!(initial.servers.is_empty());

        registry.servers.push(server("files", "Files", "stdio"));
        registry.profiles[0].enabled_server_ids.push("files".into());
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();

        let RegistryState::Ready(updated) = load_read_only(Some(&path)) else {
            panic!("the updated registry should load");
        };
        assert_eq!(updated.servers.len(), 1);
        assert_eq!(updated.enabled_count, 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_and_invalid_registries_are_not_silently_defaulted() {
        let missing = std::env::temp_dir().join(format!(
            "toolport-gtk-missing-registry-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert_eq!(load_read_only(Some(&missing)), RegistryState::FirstRun);

        let invalid = missing.with_file_name(format!(
            "toolport-gtk-invalid-registry-{}.json",
            std::process::id()
        ));
        std::fs::write(&invalid, b"{ invalid").unwrap();
        assert_eq!(load_read_only(Some(&invalid)), RegistryState::Unavailable);
        let _ = std::fs::remove_file(invalid);
    }
}
