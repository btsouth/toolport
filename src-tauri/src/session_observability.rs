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
    pub dispatch_ms: Option<u64>,
    pub cold: Option<bool>,
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

/// Persist only bounded printable name/version tokens, never locations or secrets.
pub fn display_label(label: &str) -> Option<String> {
    let label = crate::approval::sanitize_client_label(label)?;
    let label = crate::registry::redact_secret_text(&label);
    let safe = label.replace("<redacted>", "[redacted]");
    if !safe.chars().all(|c| {
        c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '+' | '(' | ')' | '[' | ']')
    }) {
        // Reject the entire label: a path containing spaces must not retain
        // its trailing directory or filename as an apparently valid name token.
        return Some("[private]".into());
    }
    crate::approval::sanitize_client_label(&safe)
}

pub struct DispatchTimer {
    started: Instant,
    previous: (Option<u64>, Option<bool>),
}
impl DispatchTimer {
    pub fn start(cold: bool) -> Self {
        let previous = CURRENT.with(|c| {
            let mut c = c.borrow_mut();
            let previous = (c.dispatch_ms, c.cold);
            c.dispatch_ms = None;
            c.cold = Some(cold);
            previous
        });
        Self {
            started: Instant::now(),
            previous,
        }
    }
    pub fn finish(&mut self) {
        CURRENT.with(|c| {
            c.borrow_mut().dispatch_ms =
                Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64)
        });
    }
}
impl Drop for DispatchTimer {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            let mut c = c.borrow_mut();
            c.dispatch_ms = self.previous.0;
            c.cold = self.previous.1;
        });
    }
}

pub fn enrich(entry: &mut Value) {
    let ctx = current();
    if let Some(ms) = ctx.dispatch_ms {
        entry["dispatchMs"] = json!(ms);
    }
    if let Some(cold) = ctx.cold {
        entry["cold"] = json!(cold);
    }
    if let Some(id) = ctx.session_id {
        entry["sessionId"] = json!(id);
    }
    if let Some(id) = ctx.run_id {
        entry["runId"] = json!(id);
        // A nested downstream failure can echo script input. Retain only the
        // verdict on correlated calls, never arbitrary error/result text.
        if let Some(obj) = entry.as_object_mut() {
            obj.remove("error");
        }
        if entry["ok"] == false && entry["tool"] != "run_script" {
            entry["failureKind"] = json!(crate::codemode::FailureKind::DownstreamFailure);
        }
    }
    if let Some(name) = ctx.client_name {
        if let Some(name) = display_label(&name) {
            entry["clientName"] = json!(name);
        }
    }
    if let Some(label) = ctx.client_label {
        if let Some(label) = display_label(&label) {
            entry["clientLabel"] = json!(label);
        }
    }
    // Broker decisions are recorded outside the gateway request context too.
    for field in ["clientName", "clientLabel"] {
        if let Some(label) = entry[field].as_str() {
            let safe = display_label(label);
            if let Some(safe) = safe {
                entry[field] = json!(safe);
            } else if let Some(object) = entry.as_object_mut() {
                object.remove(field);
            }
        }
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
    audit_path: Option<std::path::PathBuf>,
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
        Self::start_attributed(client, client, name, label, transport, reason)
    }
    pub fn start_attributed(
        client: Option<&str>,
        display_client: Option<&str>,
        name: Option<&str>,
        label: Option<&str>,
        transport: &'static str,
        reason: &'static str,
    ) -> Arc<Self> {
        let client_type = display_client
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
            audit_path: crate::audit::audit_path(),
            // Anonymous process IDs and token-derived legacy principals are not retained.
            client: client
                .filter(|c| {
                    c.starts_with("client:")
                        || c.strip_prefix("adapter:")
                            .is_some_and(|id| crate::clients::known_adapter_name(id).is_some())
                })
                .map(str::to_string),
            name: display_label(&crate::clients::trusted_client_name(display_client, name))
                .unwrap_or_else(|| "An AI client".into()),
            client_type,
            label: label.and_then(display_label),
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
            dispatch_ms: None,
            cold: None,
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
        if let Some(path) = &self.audit_path {
            crate::audit::record_session_at(path, row);
        }
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
    fn display_identity_never_replaces_the_recorded_principal() {
        let _data = crate::registry::DataDirTestEnv::new("f3-display-identity");
        let session = Session::start_attributed(
            Some("adapter:adapter-pid-123"),
            Some("adapter:claude-code"),
            None,
            None,
            "stdio",
            "initialize",
        );
        session.close(CloseReason::ClientDisconnect);
        let rows = crate::audit::read_recent(2).unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row["clientType"], "claude-code");
            assert_eq!(row["clientName"], "Claude Code");
            assert!(row.get("client").is_none());
        }
    }

    #[test]
    fn private_paths_never_reach_session_or_correlated_audit_rows() {
        let data = crate::registry::DataDirTestEnv::new("f3-label-paths");
        for path in ["/home/private/customer.env", "C:\\private\\customer.env", "~/private/customer.env", "\\\\server\\private\\customer.env", "file:///home/private/customer.env", "file:///home/private/my customer.env", "../private/customer.env", "private/customer.env", "/home/private/my customer.env", "C:\\private\\my customer.env", "\\\\server\\private folder\\customer.env"] {
            let label = format!("review {path} sk-live-abcdefghijk123456789");
            let session = Session::start(None, None, Some(&label), "stdio", "initialize");
            let _context = ContextGuard::enter(Context {run_id:Some("opaque".into()), ..session.context()});
            let mut row = json!({"tool":"run_script", "ok":false});
            enrich(&mut row);
            assert!(!row.to_string().contains("customer.env"), "{row}");
            session.close(CloseReason::ClientDisconnect);
        }
        assert!(crate::telemetry::flush_for_test(
            std::time::Duration::from_secs(5)
        ));
        let audit = std::fs::read_to_string(data.dir.join("audit.jsonl")).unwrap();
        assert!(!audit.contains("customer.env"), "{audit}");
        assert!(!audit.contains("sk-live-abcdefghijk123456789"));
        assert_eq!(display_label("Claude Code 1.2.3-beta"), Some("Claude Code 1.2.3-beta".into()));
    }

    #[test]
    fn pending_approval_retains_captured_identity_and_run_after_request_ends() {
        let _data = crate::registry::DataDirTestEnv::new("session-pending-approval-context");
        let pending = {
            let _guard = ContextGuard::enter(Context {
                session_id: Some("opaque-session".into()),
                run_id: Some("opaque-run".into()),
                client_name: Some("Unknown app (via Cursor)".into()),
                client_label: Some("kt 1 https://private.example/token".into()),
                ..Context::default()
            });
            crate::audit::PendingApprovalAudit::new(
                "fixture",
                "work",
                None,
                None,
                "destructive",
                "opaque-args-hash",
            )
        };
        pending.finish("withdrawn", 5);
        let rows = crate::audit::read_recent(1).unwrap();
        let row = &rows[0];
        assert_eq!(row["clientName"], "Unknown app (via Cursor)");
        assert_eq!(row["clientLabel"], "[private]");
        assert_eq!(row["sessionId"], "opaque-session");
        assert_eq!(row["runId"], "opaque-run");
        assert_eq!(row["decision"], "withdrawn");
        assert!(!row.to_string().contains("private.example"));
    }

    #[test]
    fn session_summary_is_bounded_private_and_excluded_from_call_stats() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-session-observation-{}",
            crate::approval::new_correlation_id()
        ));
        let _data = crate::registry::DataDirOverride::set(&dir);
        let session = Session::start(
            Some("adapter:adapter-pid-123"),
            Some("Unknown app (via fixture)"),
            Some("kt 1 https://private.example/secret"),
            "stdio",
            "initialize",
        );
        let first = catalog_delivery(&[json!({"name":"one", "description":"not retained"})]);
        let changed = catalog_delivery(&[json!({"name":"two", "description":"not retained"})]);
        for _ in 0..7 {
            session.list_delivered(first);
        }
        session.list_delivered(changed);
        for _ in 0..3 {
            session.notification_delivered();
        }
        session.close(CloseReason::ClientDisconnect);
        session.close(CloseReason::Expired);
        session.list_delivered(first);
        assert!(crate::telemetry::flush_for_test(
            std::time::Duration::from_secs(5)
        ));
        let rows = crate::audit::read_all().unwrap();
        assert_eq!(
            rows.len(),
            8,
            "start, four list and two notification checkpoints, one close"
        );
        let close = rows.iter().find(|r| r["phase"] == "close").unwrap();
        assert_eq!(close["toolsListCount"], 8);
        assert_eq!(close["listChangedCount"], 3);
        assert_eq!(close["firstCatalogSize"], 1);
        assert_eq!(close["firstCatalogRevision"], 1);
        assert_eq!(close["catalogRevision"], 2);
        assert_eq!(close["contentChanged"], true);
        assert_eq!(close["clientType"], "unknown");
        assert!(close.get("client").is_none());
        assert_eq!(close["clientLabel"], "[private]");
        assert_eq!(close["sessionId"].as_str().unwrap().len(), 32);
        assert_eq!(crate::audit::stats().unwrap()["total"], 0);
        let text = serde_json::to_string(&rows).unwrap();
        for secret in ["private.example", "not retained", "adapter-pid-123"] {
            assert!(!text.contains(secret));
        }
        for row in &rows {
            for field in [
                "query",
                "script",
                "input",
                "arguments",
                "error",
                "env",
                "cwd",
                "url",
                "fingerprint",
            ] {
                assert!(row.get(field).is_none());
            }
        }
        assert_eq!(crate::audit::recent_sessions(1000).unwrap().len(), 1);
        drop(session);
        drop(_data);
        assert!(crate::telemetry::retire_dir_for_test(
            &dir,
            std::time::Duration::from_secs(5)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn correlated_downstream_failures_keep_verdict_without_echoed_input() {
        let _guard = ContextGuard::enter(Context {
            run_id: Some("opaque-run".into()),
            ..Context::default()
        });
        let mut row = json!({"ok":false, "tool":"get", "error":"SECRET_SCRIPT_INPUT"});
        enrich(&mut row);
        assert_eq!(row["failureKind"], "downstream_failure");
        assert_eq!(row["runId"], "opaque-run");
        assert!(row.get("error").is_none());
    }

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
