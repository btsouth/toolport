pub mod approval;
#[cfg(feature = "desktop")]
mod approval_broker;
pub mod audit;
pub mod brand;
pub mod catalog;
pub mod clients;
pub mod codemode;
#[cfg(feature = "desktop")]
mod desktop;
pub mod downstream;
pub mod gateway_publish;
pub mod gatewaylog;
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
pub mod savings;
pub mod searchtrace;
pub mod secrets;
pub mod semantic;
pub mod shaping;
pub mod stacks;
pub mod teams;
pub mod usage_report;
pub mod vendors;

pub(crate) use registry::{arg_looks_secret, redact_url_userinfo};

#[cfg(feature = "desktop")]
pub fn run() {
    desktop::run();
}
