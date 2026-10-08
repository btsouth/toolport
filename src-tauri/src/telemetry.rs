//! Bounded background writer for audit, savings and search-trace logs.
//!
//! Admission never performs disk IO or waits for queue space. Overload and write
//! failures are counted in process-lifetime health, exposed by status and Activity.
//! A crash can lose queued records. Flush is a bounded FIFO barrier, not an fsync
//! guarantee; a timed-out barrier leaves the writer running and health degraded.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const BATCH_MAX: usize = 64;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const QUEUE_CAPACITY: usize = 8192;

/// A failed append distinguishes records already landed from unconfirmed writes.
#[derive(Debug)]
pub(crate) struct AppendError {
    pub message: String,
    pub unconfirmed: Option<usize>,
}

impl From<String> for AppendError {
    fn from(message: String) -> Self {
        Self {
            message,
            unconfirmed: None,
        }
    }
}

impl AppendError {
    pub(crate) fn after_append(message: String) -> Self {
        Self {
            message,
            unconfirmed: Some(0),
        }
    }
}
const FLUSH_BUDGET: Duration = Duration::from_millis(500);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    Gateway,
    TeamActivity,
    TrimTail { max_bytes: u64, keep_lines: usize },
    Savings { max_bytes: u64, keep_lines: usize },
}

struct Record {
    path: std::path::PathBuf,
    line: String,
    rotation: Rotation,
}

enum Msg {
    Record(Record),
    Flush(SyncSender<()>),
}

#[derive(Default)]
struct Counters {
    queue_dropped: AtomicU64,
    write_failed_records: AtomicU64,
    write_failures: AtomicU64,
    incomplete_flushes: AtomicU64,
    non_audit_queued: AtomicU64,
    dropped_since: AtomicU64,
}

/// Counters since this process started, not durable totals. A failed batch may
/// have partially landed; `write_failed_records` means persistence was unconfirmed.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub queue_dropped: u64,
    pub write_failed_records: u64,
    pub write_failures: u64,
    pub incomplete_flushes: u64,
}

impl Health {
    pub fn notice(&self) -> Option<String> {
        if self == &Self::default() {
            return None;
        }
        Some(format!(
            "Activity, savings, Teams reporting and diagnostics may be incomplete: {} records dropped, {} records with unconfirmed writes, {} write failures, {} incomplete flushes since gateway start.",
            self.queue_dropped, self.write_failed_records, self.write_failures, self.incomplete_flushes
        ))
    }

    fn merge(&mut self, other: &Self) {
        self.queue_dropped = self.queue_dropped.saturating_add(other.queue_dropped);
        self.write_failed_records = self
            .write_failed_records
            .saturating_add(other.write_failed_records);
        self.write_failures = self.write_failures.saturating_add(other.write_failures);
        self.incomplete_flushes = self
            .incomplete_flushes
            .saturating_add(other.incomplete_flushes);
    }
}

impl Counters {
    fn health(&self) -> Health {
        Health {
            queue_dropped: self.queue_dropped.load(Ordering::Relaxed),
            write_failed_records: self.write_failed_records.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
            incomplete_flushes: self.incomplete_flushes.load(Ordering::Relaxed),
        }
    }
}

struct Writer {
    non_audit_limit: u64,
    tx: Option<SyncSender<Msg>>,
    counters: Arc<Counters>,
}

static WRITER: OnceLock<Writer> = OnceLock::new();

// A FIFO flush cannot stop concurrent tests from queuing more records into a
// process-global data-dir override. Retired fixture paths never accept new IO.
#[cfg(any(test, feature = "test-support"))]
static RETIRED_TEST_DIRS: std::sync::RwLock<Vec<std::path::PathBuf>> =
    std::sync::RwLock::new(Vec::new());

fn writer() -> &'static Writer {
    WRITER.get_or_init(|| Writer::spawn(QUEUE_CAPACITY, append_batch))
}

impl Writer {
    fn spawn(
        capacity: usize,
        append: impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError> + Send + 'static,
    ) -> Self {
        let counters = Arc::new(Counters::default());
        let worker_counters = counters.clone();
        let (tx, rx) = mpsc::sync_channel(capacity);
        let tx = std::thread::Builder::new()
            .name("toolport-telemetry".into())
            .spawn(move || writer_loop(rx, &worker_counters, append))
            .ok()
            .map(|_| tx);
        Self {
            tx,
            counters,
            non_audit_limit: capacity.saturating_sub((capacity / 4).max(1)) as u64,
        }
    }

    fn record(&self, record: Record) {
        let audit = record
            .path
            .file_name()
            .is_some_and(|name| name == "audit.jsonl");
        if !audit
            && self
                .counters
                .non_audit_queued
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
                    (queued < self.non_audit_limit).then_some(queued + 1)
                })
                .is_err()
        {
            self.count_drop();
            return;
        }
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.try_send(Msg::Record(record)).is_err())
        {
            if !audit {
                self.counters
                    .non_audit_queued
                    .fetch_sub(1, Ordering::Relaxed);
            }
            self.count_drop();
        }
    }

    fn count_drop(&self) {
        let _ = self.counters.dropped_since.compare_exchange(
            0,
            now_ms(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.counters.queue_dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn flush(&self, budget: Duration) -> bool {
        let Some(tx) = &self.tx else {
            self.counters
                .incomplete_flushes
                .fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        if tx.try_send(Msg::Flush(done_tx)).is_ok() && done_rx.recv_timeout(budget).is_ok() {
            return true;
        }
        self.counters
            .incomplete_flushes
            .fetch_add(1, Ordering::Relaxed);
        false
    }
}

pub(crate) fn record(path: &Path, line: &str, rotation: Rotation) {
    #[cfg(any(test, feature = "test-support"))]
    let retired = RETIRED_TEST_DIRS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(any(test, feature = "test-support"))]
    if retired.iter().any(|dir| path.starts_with(dir)) {
        return;
    }
    // Hold the admission guard through enqueue so retirement's barrier covers
    // every accepted record, including callers that captured the path earlier.
    writer().record(Record {
        path: path.to_path_buf(),
        line: line.to_string(),
        rotation,
    });
}

/// Bounded barrier for in-process readers. False means queued writes are still
/// pending or the writer is unavailable; callers must not claim a complete read.
pub fn flush() -> bool {
    WRITER.get().is_none_or(|writer| writer.flush(FLUSH_BUDGET))
}

#[cfg(any(test, feature = "test-support"))]
pub fn flush_for_test(budget: Duration) -> bool {
    WRITER.get().is_none_or(|writer| writer.flush(budget))
}

/// Stop telemetry admission to a unique scratch directory, then drain accepted
/// records before its removal. Release its DataDirOverride first and keep the
/// data-dir test lock until cleanup finishes. Late records from concurrent tests
/// are intentionally discarded; the path cannot be reused in this process.
#[cfg(any(test, feature = "test-support"))]
pub fn retire_dir_for_test(dir: &Path, budget: Duration) -> bool {
    RETIRED_TEST_DIRS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(dir.to_path_buf());
    flush_for_test(budget)
}

/// Drain on orderly gateway exit for at most two seconds. Never joins a writer
/// stuck in filesystem IO. The OS ends it when the process exits.
pub fn shutdown() {
    if WRITER
        .get()
        .is_some_and(|writer| !writer.flush(SHUTDOWN_BUDGET))
    {
        eprintln!("toolport: telemetry shutdown budget exhausted; queued records may be lost");
    }
}

/// Flush diagnostics within the shutdown budget before any explicit process exit.
pub fn exit_with(code: i32) -> ! {
    shutdown();
    std::process::exit(code)
}

pub fn health() -> Health {
    WRITER
        .get()
        .map(|writer| writer.counters.health())
        .unwrap_or_default()
}

/// Activity reads the current shared daemon's live counters over its authenticated
/// identity endpoint. Failure is visible as unknown; it is never a healthy zero.
/// This read-only probe never starts a daemon or touches disk on the call path.
pub fn activity_health() -> serde_json::Value {
    let local = health();
    let Some(dir) = crate::registry::conduit_dir() else {
        return serde_json::json!(local);
    };
    let compat =
        crate::topology::CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string());
    let path = crate::daemon::descriptor_path(&dir, &compat);
    let Some(descriptor) = crate::daemon::read_descriptor(&path) else {
        return serde_json::json!(local);
    };
    if descriptor.pid == std::process::id() {
        return serde_json::json!(local);
    }
    daemon_health(local, &descriptor, &compat)
}

fn daemon_health(
    mut local: Health,
    descriptor: &crate::daemon::DaemonDescriptor,
    compat: &crate::topology::CompatKey,
) -> serde_json::Value {
    if !crate::daemon::process_exists(descriptor.pid) {
        return serde_json::json!(local);
    }
    let result = crate::daemon::attempt_identity_probe(descriptor)
        .map_err(|_| ())
        .and_then(|identity| {
            if !identity.is_compatible_with(compat) || identity.pid != descriptor.pid {
                return Err(());
            }
            identity.telemetry.ok_or(())
        });
    match result {
        Ok(remote) => {
            local.merge(&remote);
            serde_json::json!(local)
        }
        Err(()) => {
            let mut value = serde_json::json!(local);
            value["unavailable"] = serde_json::json!(true);
            value
        }
    }
}

/// Shared wording for the existing native Activity status surface.
pub fn activity_notices(value: &serde_json::Value) -> Vec<String> {
    let mut notes = crate::daemon::status_notes();
    if let Ok(health) = serde_json::from_value::<Health>(value.clone()) {
        if let Some(notice) = health.notice() {
            notes.push(notice);
        }
    }
    if let Some(dropped) = value["retainedDropped"].as_u64().filter(|count| *count > 0) {
        notes.push(format!("{dropped} dropped telemetry records are recorded in retained history. Activity and savings may be incomplete."));
    }
    if value["unavailable"].as_bool() == Some(true) {
        notes.push(
            "Gateway telemetry health is unavailable. Activity and savings may be incomplete."
                .into(),
        );
    }
    notes
}

fn writer_loop(
    rx: Receiver<Msg>,
    counters: &Counters,
    mut append: impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError>,
) {
    let mut pending = Vec::new();
    let mut gaps = GapState::default();
    loop {
        let mut done = None;
        match rx.recv() {
            Ok(Msg::Record(record)) => {
                if record
                    .path
                    .file_name()
                    .is_none_or(|name| name != "audit.jsonl")
                {
                    counters.non_audit_queued.fetch_sub(1, Ordering::Relaxed);
                }
                pending.push(record);
            }
            Ok(Msg::Flush(waiter)) => done = Some(waiter),
            Err(_) => break,
        }
        if done.is_none() {
            let deadline = Instant::now() + FLUSH_INTERVAL;
            while pending.len() < BATCH_MAX {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(Msg::Record(record)) => {
                        if record
                            .path
                            .file_name()
                            .is_none_or(|name| name != "audit.jsonl")
                        {
                            counters.non_audit_queued.fetch_sub(1, Ordering::Relaxed);
                        }
                        pending.push(record);
                    }
                    Ok(Msg::Flush(waiter)) => {
                        done = Some(waiter);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        if let Some(record) = pending.first() {
            gaps.dir = record.path.parent().map(Path::to_path_buf);
        }
        deliver(&mut pending, counters, &mut append);
        gaps.deliver(counters, &mut append);
        if let Some(done) = done {
            let _ = done.send(());
        }
    }
}

fn deliver(
    pending: &mut Vec<Record>,
    counters: &Counters,
    append: &mut impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError>,
) {
    let mut groups: Vec<(std::path::PathBuf, Rotation, Vec<String>)> = Vec::new();
    for record in pending.drain(..) {
        match groups.iter_mut().find(|group| group.0 == record.path) {
            Some(group) => group.2.push(record.line),
            None => groups.push((record.path, record.rotation, vec![record.line])),
        }
    }
    for (path, rotation, lines) in groups {
        if let Err(error) = append(&path, &lines, rotation) {
            counters.write_failures.fetch_add(1, Ordering::Relaxed);
            counters.write_failed_records.fetch_add(
                error.unconfirmed.unwrap_or(lines.len()) as u64,
                Ordering::Relaxed,
            );
            // Fixed text: paths and OS/downstream errors can contain credentials.
            // Do not enqueue diagnostics about a failed diagnostic write recursively.
            if rotation != Rotation::Gateway {
                // Keep diagnostics beside the failed record's captured path, even
                // if a test override or data directory migration has since changed.
                crate::gatewaylog::queue_at(&path.with_file_name("gateway.log"), "telemetry batch persistence failed; Activity, savings, Teams reporting and diagnostics may be incomplete");
            } else {
                eprintln!("toolport: gateway diagnostic write failed");
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct GapState {
    dir: Option<std::path::PathBuf>,
    audit_reported: u64,
    log_reported: u64,
}

impl GapState {
    fn deliver(
        &mut self,
        counters: &Counters,
        append: &mut impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError>,
    ) {
        let dropped = counters.queue_dropped.load(Ordering::Relaxed);
        let Some(dir) = &self.dir else { return };
        #[cfg(any(test, feature = "test-support"))]
        if RETIRED_TEST_DIRS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|retired| dir.starts_with(retired))
        {
            return;
        }
        let since = counters.dropped_since.load(Ordering::Relaxed);
        for (name, reported, rotation) in [
            (
                "audit.jsonl",
                &mut self.audit_reported,
                Rotation::TrimTail {
                    max_bytes: crate::audit::MAX_AUDIT_BYTES,
                    keep_lines: crate::audit::KEEP_LINES,
                },
            ),
            ("gateway.log", &mut self.log_reported, Rotation::Gateway),
        ] {
            if dropped <= *reported {
                continue;
            }
            let count = dropped - *reported;
            let line = if name == "audit.jsonl" {
                serde_json::json!({"kind":"telemetry_gap", "dropped":count, "since":since, "ts":now_ms()}).to_string()
            } else {
                crate::gatewaylog::line(&format!(
                    "telemetry gap: {count} records dropped since {since}"
                ))
            };
            match append(&dir.join(name), &[line], rotation) {
                Ok(()) => *reported = dropped,
                Err(error) => {
                    counters.write_failures.fetch_add(1, Ordering::Relaxed);
                    let unconfirmed = error.unconfirmed.unwrap_or(1);
                    counters
                        .write_failed_records
                        .fetch_add(unconfirmed as u64, Ordering::Relaxed);
                    // Rotation can fail after the marker lands. Never duplicate it.
                    if unconfirmed == 0 {
                        *reported = dropped;
                    }
                }
            }
        }
    }
}

fn append_batch(path: &Path, lines: &[String], rotation: Rotation) -> Result<(), AppendError> {
    match rotation {
        Rotation::Gateway => crate::gatewaylog::append_batch_to(path, lines),
        Rotation::TeamActivity => {
            crate::team_activity::append_records_at(path, lines).map_err(Into::into)
        }
        Rotation::TrimTail {
            max_bytes,
            keep_lines,
        } => crate::registry::append_lines_with_outcome(path, lines, max_bytes, keep_lines, None),
        Rotation::Savings {
            max_bytes,
            keep_lines,
        } => crate::savings::append_lines_at(path, lines, max_bytes, keep_lines),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn retired_directory_rejects_a_record_with_a_previously_captured_path() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch("retired");
        let data = crate::registry::DataDirOverride::set(&dir);
        crate::gatewaylog::append("before teardown");
        assert!(flush_for_test(Duration::from_secs(5)));
        let (captured_tx, captured_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            let path = crate::registry::gateway_log_path().unwrap();
            captured_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            record(&path, "after teardown", Rotation::Gateway);
        });
        captured_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(data);
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        assert!(std::fs::read_to_string(dir.join("gateway.log"))
            .unwrap()
            .contains("before teardown"));
        std::fs::remove_dir_all(&dir).unwrap();
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        assert!(flush_for_test(Duration::from_secs(5)));
        assert!(
            !dir.exists(),
            "late telemetry recreated the retired directory"
        );
    }

    #[test]
    fn retired_directory_does_not_receive_gap_markers_on_later_flushes() {
        let dir = scratch("retired-gap");
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(&dir).unwrap();
        let counters = Counters::default();
        counters.queue_dropped.store(1, Ordering::Relaxed);
        let mut gaps = GapState {
            dir: Some(dir.clone()),
            ..GapState::default()
        };
        gaps.deliver(&counters, &mut append_batch);
        assert!(!dir.exists(), "gap marker recreated the retired directory");
    }

    #[test]
    fn gateway_and_adapter_explicit_exits_use_bounded_flush() {
        for source in [
            include_str!("bin/toolport-gateway.rs"),
            include_str!("stdio_adapter.rs"),
        ] {
            assert!(!source.contains("std::process::exit("));
            assert!(source.contains("telemetry::exit_with("));
        }
    }

    #[test]
    fn reserved_audit_capacity_persists_drop_evidence_once() {
        let dir = scratch("reserved");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut gate = Some(release_rx);
        let (batch_tx, batch_rx) = mpsc::sync_channel(8);
        let writer = Writer::spawn(4, move |path, lines, rotation| {
            if let Some(gate) = gate.take() {
                started_tx.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            let result = append_batch(path, lines, rotation);
            if lines.iter().any(|line| line.contains("denied")) {
                batch_tx.send(()).unwrap();
            }
            result
        });
        let audit = dir.join("audit.jsonl");
        writer.record(Record {
            path: audit.clone(),
            ..fixture_record(0)
        });
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        writer
            .tx
            .as_ref()
            .unwrap()
            .try_send(Msg::Flush(done_tx))
            .unwrap_or_else(|_| panic!("flush queue full"));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Counter traffic exhausts only its share. An approval still fits.
        for n in 1..=4 {
            writer.record(Record {
                path: dir.join("savings.jsonl"),
                line: serde_json::json!({"n":n}).to_string(),
                rotation: trimmed(1_000_000, 100),
            });
        }
        writer.record(Record {
            path: audit.clone(),
            line: r#"{"kind":"approval","decision":"denied"}"#.into(),
            rotation: trimmed(1_000_000, 100),
        });
        assert_eq!(writer.counters.health().queue_dropped, 1);
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // The first delivery writes the gap even before another call arrives.
        let content = std::fs::read_to_string(&audit).unwrap();
        assert!(content.contains("telemetry_gap"));
        assert!(std::fs::read_to_string(dir.join("gateway.log"))
            .unwrap()
            .contains("telemetry gap: 1 records dropped"));
        batch_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(writer.flush(Duration::from_secs(5)));
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(audit)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(
            rows.iter()
                .filter(|row| row["kind"] == "telemetry_gap")
                .count(),
            1
        );
        assert!(rows.iter().any(|row| row["decision"] == "denied"));
        assert_eq!(
            rows.iter()
                .find(|row| row["kind"] == "telemetry_gap")
                .unwrap()["dropped"],
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gap_rotation_failure_does_not_duplicate_landed_marker() {
        let dir = scratch("gap-rotation");
        let counters = Counters::default();
        counters.queue_dropped.store(2, Ordering::Relaxed);
        counters.dropped_since.store(42, Ordering::Relaxed);
        let mut gaps = GapState {
            dir: Some(dir.clone()),
            ..GapState::default()
        };
        let mut calls = 0;
        let mut append = |_: &Path, _: &[String], _: Rotation| {
            calls += 1;
            Err(AppendError::after_append("rotation failed".into()))
        };
        gaps.deliver(&counters, &mut append);
        gaps.deliver(&counters, &mut append);
        assert_eq!(calls, 2);
        assert_eq!(counters.health().write_failures, 2);
        assert_eq!(counters.health().write_failed_records, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_failures_count_only_unconfirmed_lines() {
        use crate::registry::tests::{with_atomic_failure, FailingAtomicWriteStep::Rename};
        let dir = scratch("confirmed-rotation");
        for rotation in [
            trimmed(1, 1),
            Rotation::Savings {
                max_bytes: 1,
                keep_lines: 1,
            },
            Rotation::Gateway,
        ] {
            let path = dir.join("log");
            let prefix = if rotation == Rotation::Gateway {
                "x".repeat(crate::gatewaylog::GATEWAY_LOG_CAP as usize + 1)
            } else {
                r#"{"v":2,"kind":"list","tokensSaved":5}"#.into()
            };
            std::fs::write(&path, format!("{prefix}\n")).unwrap();
            let counters = Counters::default();
            let mut records = vec![Record {
                path: path.clone(),
                line: r#"{"v":2,"kind":"list","tokensSaved":5}"#.into(),
                rotation,
            }];
            with_atomic_failure(Rename, || {
                deliver(&mut records, &counters, &mut append_batch)
            });
            assert_eq!(counters.health().write_failures, 1);
            assert_eq!(counters.health().write_failed_records, 0);
            assert!(std::fs::read_to_string(path)
                .unwrap()
                .contains("tokensSaved"));
        }
        assert!(flush_for_test(Duration::from_secs(5)));
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dead_daemon_descriptor_does_not_leave_health_unavailable() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch("dead-health");
        let _data = crate::registry::DataDirOverride::set(&dir);
        let compat =
            crate::topology::CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string());
        let mut descriptor =
            crate::daemon::DaemonDescriptor::new("127.0.0.1:1", "test-token", &compat);
        descriptor.pid = 999_999_999;
        crate::daemon::write_descriptor(
            &crate::daemon::descriptor_path(&dir, &compat),
            &descriptor,
        )
        .unwrap();
        assert_ne!(activity_health()["unavailable"], true);
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-telemetry-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn trimmed(max_bytes: u64, keep_lines: usize) -> Rotation {
        Rotation::TrimTail {
            max_bytes,
            keep_lines,
        }
    }

    #[test]
    fn activity_reads_shared_gateway_counters_and_reports_unknown_health() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch("activity-health");
        let _data = crate::registry::DataDirOverride::set(&dir);
        let compat =
            crate::topology::CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string());
        for remote in [
            Some(Health {
                queue_dropped: 7,
                ..Health::default()
            }),
            None,
        ] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let endpoint = server.server_addr().to_ip().unwrap().to_string();
            let mut descriptor =
                crate::daemon::DaemonDescriptor::new(endpoint, "test-only-token", &compat);
            descriptor.pid = std::process::id();
            crate::daemon::write_descriptor(
                &crate::daemon::descriptor_path(&dir, &compat),
                &descriptor,
            )
            .unwrap();
            let body = serde_json::json!({"compat": compat.fingerprint(), "protocol": crate::daemon::PROTOCOL_GENERATION, "pid": descriptor.pid, "gatewayVersion": env!("CARGO_PKG_VERSION"), "telemetry": remote});
            let server_thread = std::thread::spawn(move || {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), crate::daemon::IDENTITY_PATH);
                assert!(request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer test-only-token"));
                request
                    .respond(tiny_http::Response::from_string(body.to_string()))
                    .unwrap();
            });
            let status = daemon_health(health(), &descriptor, &compat);
            server_thread.join().unwrap();
            if remote.is_some() {
                assert!(status["queueDropped"].as_u64().unwrap() >= 7);
                assert_ne!(status["unavailable"], true);
            } else {
                assert_eq!(status["unavailable"], true);
                assert!(activity_notices(&status)
                    .iter()
                    .any(|note| note.contains("health is unavailable")));
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn fixture_record(n: u64) -> Record {
        Record {
            path: PathBuf::from("unused/audit.jsonl"),
            line: n.to_string(),
            rotation: trimmed(1024, 100),
        }
    }

    #[test]
    fn slow_writer_and_full_queue_never_block_admission_or_flush() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut gate = Some(release_rx);
        let writer = Arc::new(Writer::spawn(2, move |_, _, _| {
            if let Some(gate) = gate.take() {
                started_tx.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            Ok(())
        }));
        writer.record(fixture_record(0));
        // FIFO barrier forces delivery without a sleep or waiting for a timer.
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        writer
            .tx
            .as_ref()
            .unwrap()
            .try_send(Msg::Flush(barrier_tx))
            .unwrap_or_else(|_| panic!("barrier queue full"));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (returned_tx, returned_rx) = mpsc::sync_channel(1);
        let caller = writer.clone();
        let handle = std::thread::spawn(move || {
            caller.record(fixture_record(1));
            caller.record(fixture_record(2));
            caller.record(fixture_record(3));
            let flushed = caller.flush(Duration::ZERO);
            returned_tx
                .send((caller.counters.health(), flushed))
                .unwrap();
        });
        // A bounded receive proves the caller completes WHILE the writer is held.
        let result = returned_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        handle.join().unwrap();
        barrier_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (health, flushed) = result.expect("admission stalled behind the slow writer");
        assert!(!flushed);
        assert_eq!(health.queue_dropped, 1);
        assert_eq!(health.incomplete_flushes, 1);
        let status = serde_json::to_value(&health).unwrap();
        assert_eq!(status["queueDropped"], 1);
        assert!(health.notice().unwrap().contains("1 records dropped"));
        assert!(writer.flush(Duration::from_secs(5)));
    }

    #[test]
    fn disconnected_writer_counts_drops_without_disk_fallback() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let writer = Writer {
            tx: Some(tx),
            non_audit_limit: 1,
            counters: Arc::new(Counters::default()),
        };
        writer.record(fixture_record(0));
        assert_eq!(writer.counters.health().queue_dropped, 1);
        assert!(!writer.flush(Duration::ZERO));
    }

    #[test]
    fn failed_batches_count_unconfirmed_records_for_audit_and_savings() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch("failed-batches");
        let _data = crate::registry::DataDirOverride::set(dir.join("current"));
        for rotation in [
            trimmed(1024, 100),
            Rotation::TeamActivity,
            Rotation::Savings {
                max_bytes: 1024,
                keep_lines: 100,
            },
        ] {
            for error in [
                "Permission denied",
                "No space left on device",
                "rename failed",
            ] {
                let counters = Counters::default();
                let mut pending = vec![fixture_record(1), fixture_record(2)];
                for record in &mut pending {
                    record.path = dir.join("audit.jsonl");
                    record.rotation = rotation;
                }
                deliver(&mut pending, &counters, &mut |_, _, _| {
                    Err(error.to_string().into())
                });
                let health = counters.health();
                assert_eq!(health.write_failures, 1);
                assert_eq!(health.write_failed_records, 2);
                assert!(health
                    .notice()
                    .unwrap()
                    .contains("2 records with unconfirmed writes"));
            }
        }
        assert!(flush_for_test(Duration::from_secs(5)));
        assert!(dir.join("gateway.log").exists());
        assert!(!dir.join("current/gateway.log").exists());
        drop(_data);
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Concurrent recorders all land exactly once, never torn or interleaved.
    #[test]
    fn concurrent_records_then_flush_lose_nothing() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 200;
        let dir = scratch("concurrent");
        let path = dir.join("audit.jsonl");

        let mut handles = Vec::new();
        for thread in 0..THREADS {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                for seq in 0..PER_THREAD {
                    let line = format!("{{\"t\":{thread},\"n\":{seq}}}");
                    record(&path, &line, trimmed(64 * 1024 * 1024, 100_000));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(flush_for_test(Duration::from_secs(5)));

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), THREADS * PER_THREAD, "one line per record");
        let unique: std::collections::HashSet<&str> = lines.iter().copied().collect();
        assert_eq!(
            unique.len(),
            lines.len(),
            "no duplicate or interleaved line"
        );
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("each line is whole JSON");
        }
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Crossing the cap rotates on the writer thread and keeps the newest lines,
    /// matching `append_line_locked_trims_once_past_the_cap`.
    #[test]
    fn crossing_the_cap_rotates_and_keeps_newest() {
        let dir = scratch("rotation");
        let path = dir.join("audit.jsonl");
        for i in 0..10 {
            record(&path, &format!("{{\"i\":{i}}}"), trimmed(1, 3));
        }
        assert!(flush_for_test(Duration::from_secs(5)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"i\":7}\n{\"i\":8}\n{\"i\":9}\n");
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A record call never waits on a cross-process lock held elsewhere: it queues
    /// and returns, and the writer lands the line once the lock is free.
    #[test]
    fn record_does_not_block_on_a_held_lock() {
        let dir = scratch("held-lock");
        let path = dir.join("audit.jsonl");
        // Drain anything other tests queued before holding the append lock.
        assert!(flush_for_test(Duration::from_secs(5)));
        let guard = crate::registry::lock_at(&path).expect("hold the append lock");

        let start = Instant::now();
        record(&path, "{\"held\":true}", trimmed(1024 * 1024, 100));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "record waited on the lock: {elapsed:?}"
        );

        drop(guard);
        assert!(flush_for_test(Duration::from_secs(5)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("{\"held\":true}"),
            "line landed: {content}"
        );
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
