pub mod approval;
#[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
pub(crate) mod approval_broker;
pub mod audit;
pub mod autostart;
pub mod brand;
pub mod catalog;
pub mod call_failure;
pub mod child_ledger;
pub mod clients;
pub mod tool_definitions;
pub mod codemode;
pub mod codemode_worker;
pub mod daemon;
pub mod daemon_log;
#[cfg(feature = "desktop")]
mod desktop;
pub mod diagnostics_controller;
pub mod downstream;
pub mod downstream_backoff;
pub mod gateway_publish;
pub mod gatewaylog;
pub mod guard_cleanup;
pub mod hooks;
pub mod hostenv;
pub mod http_bridge;
mod import_credentials;
pub mod inspect;
pub mod instructions;
pub mod integrity;
pub mod launch_inputs;
pub mod launcher;
pub(crate) mod local_auth;
#[cfg(all(target_os = "linux", feature = "gtk-desktop"))]
pub mod linux_native;
pub mod metrics;
pub mod oauth;
mod oauth_controller;
pub mod observability_controller;
pub mod pii;
pub mod playground;
pub mod rate_limits;
pub mod registry;
pub mod registry_controller;
pub mod remote;
pub mod router;
pub mod savings;
pub(crate) mod schema_compat;
pub mod searchtrace;
pub mod secrets;
pub mod semantic;
pub mod server_runtime;
pub mod session_store;
pub mod shaping;
pub mod sharing_controller;
pub mod stdio_adapter;
pub mod team_activity;
pub mod teams;
pub mod teams_plan;
pub mod telemetry;
pub mod topology;
pub mod usage_report;
pub mod vendors;
#[cfg(target_os = "windows")]
pub mod windows_autostart;

pub(crate) use registry::redact_url_userinfo;

#[cfg(feature = "desktop")]
pub fn run() {
    desktop::run();
}
