pub mod approval;
#[cfg(feature = "desktop")]
mod approval_broker;
pub mod audit;
pub mod autostart;
#[cfg(target_os = "windows")]
pub mod windows_autostart;
pub mod brand;
pub mod catalog;
pub mod clients;
pub mod codemode;
#[cfg(feature = "desktop")]
mod desktop;
pub mod downstream;
pub mod gateway_publish;
pub mod gatewaylog;
pub mod hooks;
pub mod agent_permissions;
pub mod agent_guard;
pub mod hostenv;
pub mod inspect;
pub mod instructions;
pub mod integrity;
pub mod launcher;
pub mod metrics;
pub mod oauth;
pub mod pii;
pub mod rate_limits;
pub mod registry;
pub mod remote;
pub mod router;
pub mod routine_advisor;
pub mod routine_candidates;
pub mod routine_catalog;
pub mod routines;
pub mod rules;
pub mod savings;
pub mod searchtrace;
pub mod secrets;
pub mod semantic;
pub mod shaping;
pub mod stacks;
pub mod teams;
pub mod topology;
pub mod usage_report;
pub mod vendors;

pub(crate) use registry::redact_url_userinfo;

#[cfg(feature = "desktop")]
pub fn run() {
    desktop::run();
}
