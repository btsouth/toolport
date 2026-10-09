//! Bounded session summaries and request attribution. Never retains request content.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Default)]
pub struct Context {
    pub session_id: Option<String>,
    pub client_name: Option<String>,
    pub client_label: Option<String>,
    pub run_id: Option<String>,
}

thread_local! {
    static CURRENT: std::cell::RefCell<Context> = std::cell::RefCell::new(Context::default());
}

pub fn current() -> Context {
    CURRENT.with(|c| c.borrow().clone())
}
pub struct ContextGuard(Context);
impl ContextGuard {
    pub fn enter(context: Context) -> Self {
        Self(CURRENT.with(|c| c.replace(context)))
    }
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| c.replace(std::mem::take(&mut self.0)));
    }
}

pub fn enrich(entry: &mut Value) {
    let ctx = current();
    if let Some(id) = ctx.session_id {
        entry["sessionId"] = json!(id);
    }
    if let Some(id) = ctx.run_id {
        entry["runId"] = json!(id);
    }
    if let Some(name) = ctx.client_name {
        entry["clientName"] = json!(name);
    }
    if let Some(label) = ctx.client_label {
        entry["clientLabel"] = json!(label);
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CloseReason {
    ClientDisconnect,
    ClientDelete,
    Expired,
    Reinitialize,
    GatewayShutdown,
    RequestComplete,
}
impl CloseReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientDisconnect => "client_disconnect",
            Self::ClientDelete => "client_delete",
            Self::Expired => "expired",
            Self::Reinitialize => "reinitialize",
            Self::GatewayShutdown => "gateway_shutdown",
            Self::RequestComplete => "request_complete",
        }
    }
}

/// A fingerprint is compared in memory only. Revisions are local monotonic counters,
/// never hashes of content in the retained log.
#[derive(Clone, Copy)]
pub struct CatalogDelivery {
    pub count: usize,
    pub fingerprint: [u8; 32],
}
pub fn catalog_delivery(tools: &[Value]) -> CatalogDelivery {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    struct HashWriter<'a>(&'a mut sha2::Sha256);
    impl std::io::Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let _ = serde_json::to_writer(HashWriter(&mut hasher), tools);
    CatalogDelivery {
        count: tools.len(),
        fingerprint: hasher.finalize().into(),
    }
}

pub struct Session {
    id: String,
    client: Option<String>,
    name: String,
    client_type: String,
    label: Option<String>,
    transport: &'static str,
    started: Instant,
    counts: Mutex<Counts>,
}
#[derive(Default)]
struct Counts {
    lists: u64,
    notifications: u64,
    revision: u64,
    content_changes: u64,
    first: Option<(usize, u64)>,
    fingerprint: Option<[u8; 32]>,
    closed: bool,
}

impl Session {
    pub fn start(
        client: Option<&str>,
        name: Option<&str>,
        label: Option<&str>,
        transport: &'static str,
        reason: &'static str,
    ) -> Arc<Self> {
        let client_type = client
            .and_then(|c| c.strip_prefix("adapter:"))
            .filter(|id| crate::clients::known_adapter_name(id).is_some())
            .unwrap_or(if client.is_some_and(|c| c.starts_with("client:")) {
                "registered_http"
            } else {
                "unknown"
            })
            .to_string();
        let session = Arc::new(Self {
            id: crate::approval::new_correlation_id(),
            // Anonymous process IDs and token-derived legacy principals are not retained.
            client: client
                .filter(|c| {
                    c.starts_with("client:")
                        || c.strip_prefix("adapter:")
                            .is_some_and(|id| crate::clients::known_adapter_name(id).is_some())
                })
                .map(str::to_string),
            name: crate::clients::trusted_client_name(client, name),
            client_type,
            label: label.and_then(crate::approval::sanitize_client_label),
            transport,
            started: Instant::now(),
            counts: Mutex::new(Counts::default()),
        });
        session.record("start", reason, &Counts::default());
        session
    }
    pub fn is_request(&self) -> bool {
        self.transport == "http_request"
    }
    pub fn context(&self) -> Context {
        Context {
            session_id: Some(self.id.clone()),
            client_name: Some(self.name.clone()),
            client_label: self.label.clone(),
            run_id: None,
        }
    }
    pub fn list_delivered(&self, catalog: CatalogDelivery) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if counts.closed {
            return;
        }
        counts.lists = counts.lists.saturating_add(1);
        if counts.fingerprint != Some(catalog.fingerprint) {
            if counts.fingerprint.is_some() {
                counts.content_changes = counts.content_changes.saturating_add(1);
            }
            counts.revision = counts.revision.saturating_add(1);
            counts.fingerprint = Some(catalog.fingerprint);
        }
        let revision = counts.revision;
        counts.first.get_or_insert((catalog.count, revision));
        // Logarithmic checkpoints cap a noisy client's disk writes to 64 per counter.
        if counts.lists.is_power_of_two() {
            self.record("checkpoint", "tools_list", &counts);
        }
    }
    pub fn notification_delivered(&self) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if counts.closed {
            return;
        }
        counts.notifications = counts.notifications.saturating_add(1);
        if counts.notifications.is_power_of_two() {
            self.record("checkpoint", "list_changed", &counts);
        }
    }
    pub fn close(&self, reason: CloseReason) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if counts.closed {
            return;
        }
        counts.closed = true;
        self.record("close", reason.as_str(), &counts);
    }
    fn record(&self, phase: &str, reason: &str, counts: &Counts) {
        let mut row = json!({"kind":"session", "server":"toolport", "tool":"session", "sessionId":self.id,
            "phase":phase, "reason":reason, "clientType":self.client_type, "clientName":self.name,
            "gatewayVersion":env!("CARGO_PKG_VERSION"), "transport":self.transport,
            "toolsListCount":counts.lists, "listChangedCount":counts.notifications,
            "catalogRevision":counts.revision, "contentChanged":counts.content_changes > 0,
            "contentChangeCount":counts.content_changes,
            "sessionDurationMs":self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            "deliveryBoundary":"transport_write"});
        if let Some(client) = &self.client {
            row["client"] = json!(client);
        }
        if let Some(label) = &self.label {
            row["clientLabel"] = json!(label);
        }
        if let Some((count, revision)) = counts.first {
            row["firstCatalogSize"] = json!(count);
            row["firstCatalogRevision"] = json!(revision);
        }
        crate::audit::record_session(row);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close(CloseReason::GatewayShutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_detects_equal_count_content_changes() {
        let a = catalog_delivery(&[json!({"name":"one"})]);
        let b = catalog_delivery(&[json!({"name":"two"})]);
        assert_eq!(a.count, b.count);
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_eq!(
            a.fingerprint,
            catalog_delivery(&[json!({"name":"one"})]).fingerprint
        );
    }
    #[test]
    fn contexts_restore_nested_runs_and_do_not_record_payloads() {
        let _guard = ContextGuard::enter(Context {
            session_id: Some("opaque".into()),
            client_name: Some("Unknown app (via node)".into()),
            ..Context::default()
        });
        {
            let _run = ContextGuard::enter(Context {
                run_id: Some("run".into()),
                ..current()
            });
            assert_eq!(current().run_id.as_deref(), Some("run"));
        }
        let mut row = json!({});
        enrich(&mut row);
        assert_eq!(
            row,
            json!({"sessionId":"opaque", "clientName":"Unknown app (via node)"})
        );
    }
}
