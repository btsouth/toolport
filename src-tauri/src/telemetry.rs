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
/// How soon a log whose lock another process holds is tried again.
const RETRY_INTERVAL: Duration = Duration::from_millis(20);
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
#[cfg(not(any(test, feature = "test-support")))]
const FLUSH_BUDGET: Duration = Duration::from_millis(500);
// Unit tests share one writer across thousands of parallel tests, and a loaded CI runner
// can take longer than 500 ms to drain it. Tests check what was read, not how fast.
// Integration tests build the library with test-support instead of cfg(test).
#[cfg(any(test, feature = "test-support"))]
const FLUSH_BUDGET: Duration = Duration::from_secs(10);
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
    /// A FIFO barrier, answered once every record queued before it has landed or
    /// been counted, and forgotten once its caller's deadline passes.
    Flush(SyncSender<()>, Option<Instant>),
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
    fn count_drops(&self, count: u64) {
        let _ =
            self.dropped_since
                .compare_exchange(0, now_ms(), Ordering::Relaxed, Ordering::Relaxed);
        self.queue_dropped.fetch_add(count, Ordering::Relaxed);
    }

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
            .spawn(move || {
                // A busy log keeps its batch for a later pass; see the helper.
                crate::registry::try_locks_once_on_this_thread();
                writer_loop(rx, &worker_counters, capacity, append)
            })
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
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
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
        self.counters.count_drops(1);
    }

    fn flush(&self, budget: Duration) -> bool {
        let Some(tx) = &self.tx else {
            self.counters
                .incomplete_flushes
                .fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        #[cfg(any(test, feature = "test-support"))]
        let started = Instant::now();
        let deadline = Instant::now().checked_add(budget);
        let queued = tx.try_send(Msg::Flush(done_tx, deadline)).is_ok();
        if queued && done_rx.recv_timeout(budget).is_ok() {
            return true;
        }
        #[cfg(any(test, feature = "test-support"))]
        eprintln!(
            "telemetry flush failed: barrier_queued={queued}, elapsed={:?}, health={:?}",
            started.elapsed(),
            self.counters.health()
        );
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
/// stuck in filesystem IO. The OS ends it when the process exits. Every orderly exit
/// runs this, so it also removes this process's sealed result spills.
pub fn shutdown() {
    crate::secret_redact::flush_pending_scrub(Duration::from_secs(2));
    crate::shaping::release_spills();
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
    crate::daemon_log::flush_stderr_relay(Duration::from_millis(500));
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

/// Lines for one log, in arrival order.
struct Batch {
    /// Arrival number of the oldest line, so a flush knows whether it waits on it.
    first_seq: u64,
    path: std::path::PathBuf,
    rotation: Rotation,
    lines: Vec<String>,
}

fn is_audit(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "audit.jsonl")
}

fn writer_loop(
    rx: Receiver<Msg>,
    counters: &Counters,
    capacity: usize,
    mut append: impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError>,
) {
    let received = |record: &Record| {
        if !is_audit(&record.path) {
            counters.non_audit_queued.fetch_sub(1, Ordering::Relaxed);
        }
    };
    let mut pending = Vec::new();
    // Arrival number of the next record received; `pending[i]` is `pending_seq + i`.
    let (mut next_seq, mut pending_seq) = (0u64, 0u64);
    // Batches for logs whose lock another process held, retried every pass.
    let mut kept: Vec<Batch> = Vec::new();
    // Flushes waiting on kept lines that arrived before them, with the arrival
    // number they wait up to and their caller's deadline.
    let mut waiters: Vec<(u64, SyncSender<()>, Option<Instant>)> = Vec::new();
    let mut gaps = GapState::default();
    loop {
        let first = if kept.is_empty() && !gaps.busy {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(RETRY_INTERVAL)
        };
        let mut flushing = false;
        match first {
            Ok(Msg::Record(record)) => {
                received(&record);
                pending.push(record);
                next_seq += 1;
            }
            Ok(Msg::Flush(waiter, deadline)) => {
                waiters.push((next_seq, waiter, deadline));
                flushing = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !flushing && !pending.is_empty() {
            let deadline = Instant::now() + FLUSH_INTERVAL;
            while pending.len() < BATCH_MAX {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(Msg::Record(record)) => {
                        received(&record);
                        pending.push(record);
                        next_seq += 1;
                    }
                    Ok(Msg::Flush(waiter, deadline)) => {
                        waiters.push((next_seq, waiter, deadline));
                        break;
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        if let Some(record) = pending.first() {
            gaps.dir = record.path.parent().map(Path::to_path_buf);
        }
        deliver(
            &mut pending,
            pending_seq,
            &mut kept,
            counters,
            capacity,
            &mut append,
        );
        pending_seq = next_seq;
        gaps.deliver(counters, &mut append);
        // Kept lines are in arrival order within a batch, so a batch's first line
        // says whether a barrier still waits on it. A busy gap marker holds every
        // barrier, so an exit before it lands reports an incomplete flush.
        let oldest_kept = kept.iter().map(|batch| batch.first_seq).min();
        let now = Instant::now();
        waiters.retain(|(upto, waiter, deadline)| {
            let settled = !gaps.busy && oldest_kept.is_none_or(|oldest| oldest >= *upto);
            if settled {
                let _ = waiter.send(());
                return false;
            }
            deadline.is_none_or(|deadline| deadline > now)
        });
    }
}

/// Append each log's lines under its lock. A log whose lock another process
/// holds keeps its batch in `kept`, ahead of any later lines for it, so a slow
/// holder delays only that log. Kept lines stay within the queue's bounds and
/// audit reserve; anything past them is counted as dropped.
fn deliver(
    pending: &mut Vec<Record>,
    pending_seq: u64,
    kept: &mut Vec<Batch>,
    counters: &Counters,
    capacity: usize,
    append: &mut impl FnMut(&Path, &[String], Rotation) -> Result<(), AppendError>,
) {
    let mut batches = std::mem::take(kept);
    for (seq, record) in (pending_seq..).zip(pending.drain(..)) {
        // Prepare savings rows once, so a retry on a busy lock reuses them.
        let line = match record.rotation {
            Rotation::Savings { .. } => crate::savings::prepare_line(&record.line),
            _ => record.line,
        };
        match batches.iter_mut().find(|batch| batch.path == record.path) {
            Some(batch) => batch.lines.push(line),
            None => batches.push(Batch {
                first_seq: seq,
                path: record.path,
                rotation: record.rotation,
                lines: vec![line],
            }),
        }
    }
    let non_audit_limit = capacity.saturating_sub((capacity / 4).max(1));
    let (mut kept_lines, mut kept_non_audit) = (0, 0);
    for Batch {
        first_seq,
        path,
        rotation,
        mut lines,
    } in batches
    {
        crate::registry::take_lock_busy();
        let error = match append(&path, &lines, rotation) {
            Ok(()) => continue,
            Err(error) => error,
        };
        // Contention writes nothing, so the whole batch can wait for a later pass.
        if error.unconfirmed.is_none() && crate::registry::take_lock_busy() {
            let audit = is_audit(&path);
            let mut room = capacity.saturating_sub(kept_lines);
            if !audit {
                room = room.min(non_audit_limit.saturating_sub(kept_non_audit));
            }
            if lines.len() > room {
                counters.count_drops((lines.len() - room) as u64);
                lines.truncate(room);
            }
            kept_lines += lines.len();
            if !audit {
                kept_non_audit += lines.len();
            }
            if !lines.is_empty() {
                kept.push(Batch {
                    first_seq,
                    path,
                    rotation,
                    lines,
                });
            }
            continue;
        }
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct GapState {
    /// A marker found its log busy and is retried on the kept-batch schedule.
    busy: bool,
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
        self.busy = false;
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
            crate::registry::take_lock_busy();
            match append(&dir.join(name), &[line], rotation) {
                Ok(()) => *reported = dropped,
                // A busy log gets the marker on a later pass.
                Err(error) if error.unconfirmed.is_none() && crate::registry::take_lock_busy() => {
                    self.busy = true;
                }
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
        } => crate::savings::append_prepared_lines_at(path, lines, max_bytes, keep_lines),
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
            .try_send(Msg::Flush(done_tx, None))
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
                deliver(
                    &mut records,
                    0,
                    &mut Vec::new(),
                    &counters,
                    QUEUE_CAPACITY,
                    &mut append_batch,
                )
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

    #[test]
    #[cfg(unix)]
    fn live_activity_health_changes_without_retained_store_changes() {
        let data = crate::registry::DataDirTestEnv::new("live-health-revision");
        std::fs::write(data.dir.join("audit.jsonl"), "{\"ts\":1}\n").unwrap();
        let before = crate::observability_controller::activity_revision().unwrap();
        let compat = crate::topology::CompatKey::new(
            env!("CARGO_PKG_VERSION"),
            data.dir.display().to_string(),
        );
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        // An owned disposable process gives the mock descriptor a live foreign PID.
        let mut process = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut descriptor = crate::daemon::DaemonDescriptor::new(
            server.server_addr().to_ip().unwrap().to_string(),
            "test-only-token",
            &compat,
        );
        descriptor.pid = process.id();
        crate::daemon::write_descriptor(
            &crate::daemon::descriptor_path(&data.dir, &compat),
            &descriptor,
        )
        .unwrap();
        let pid = descriptor.pid;
        let fingerprint = compat.fingerprint();
        let responder = std::thread::spawn(move || {
            for remote in [
                Some(Health::default()),
                Some(Health {
                    write_failures: 1,
                    write_failed_records: 2,
                    ..Health::default()
                }),
                None,
                Some(Health::default()),
            ] {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), crate::daemon::IDENTITY_PATH);
                let body = serde_json::json!({"compat": fingerprint, "protocol": crate::daemon::PROTOCOL_GENERATION,
                    "pid": pid, "gatewayVersion": env!("CARGO_PKG_VERSION"), "telemetry": remote});
                request
                    .respond(tiny_http::Response::from_string(body.to_string()))
                    .unwrap();
            }
        });
        let healthy = crate::observability_controller::activity_health();
        let failed = crate::observability_controller::activity_health();
        let unavailable = crate::observability_controller::activity_health();
        let recovered = crate::observability_controller::activity_health();
        process.kill().unwrap();
        process.wait().unwrap();
        responder.join().unwrap();
        assert_eq!(
            crate::observability_controller::activity_revision().unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(data.dir.join("audit.jsonl")).unwrap(),
            "{\"ts\":1}\n"
        );
        assert_ne!(healthy, failed);
        assert!(failed["telemetry"]["writeFailures"].as_u64().unwrap() >= 1);
        assert_eq!(unavailable["telemetry"]["unavailable"], true);
        assert_ne!(recovered["telemetry"]["unavailable"], true);
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
        let (delivered_tx, delivered_rx) = mpsc::sync_channel(1);
        let mut gate = Some(release_rx);
        let writer = Arc::new(Writer::spawn(2, move |_, lines, _| {
            if let Some(gate) = gate.take() {
                started_tx.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            if lines.iter().any(|line| line == "2") {
                delivered_tx.send(()).unwrap();
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
            .try_send(Msg::Flush(barrier_tx, None))
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
        // The first barrier acknowledges only record 0. Its acknowledgement
        // can arrive while records 1 and 2 still fill the queue, where flush
        // deliberately fails admission immediately even with a nonzero budget.
        delivered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
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
                deliver(
                    &mut pending,
                    0,
                    &mut Vec::new(),
                    &counters,
                    QUEUE_CAPACITY,
                    &mut |_, _, _| Err(error.to_string().into()),
                );
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

    /// A writer process for the held-lock tests. Killed and cleaned up on drop, so
    /// a failed assertion never leaves it running.
    struct WriterProcess {
        root: PathBuf,
        child: std::process::Child,
    }

    impl WriterProcess {
        fn spawn(test: &str, root: &Path, lock_timeout_ms: Option<&str>) -> Self {
            let stderr = std::fs::File::create(root.join("child.stderr")).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", test, "--nocapture"])
                .env("TOOLPORT_TELEMETRY_HELD_CHILD", root)
                .stdout(std::process::Stdio::null())
                .stderr(stderr);
            if let Some(ms) = lock_timeout_ms {
                command.env("TOOLPORT_LOCK_TIMEOUT_MS", ms);
            }
            Self {
                root: root.to_path_buf(),
                child: command.spawn().expect("spawn telemetry writer process"),
            }
        }

        fn stderr(&self) -> String {
            std::fs::read_to_string(self.root.join("child.stderr")).unwrap_or_default()
        }

        /// Poll until `done`, failing with the child's stderr if it exits first or
        /// the hang guard expires.
        fn wait_until(&mut self, label: &str, done: impl Fn() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !done() {
                if let Some(status) = self.child.try_wait().unwrap() {
                    panic!(
                        "writer exited ({status}) before {label}:\n{}",
                        self.stderr()
                    );
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {label}:\n{}",
                    self.stderr()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn exit_status(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "writer did not exit:\n{}",
                    self.stderr()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for WriterProcess {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn file_has(path: &Path, needle: &str) -> bool {
        std::fs::read_to_string(path).is_ok_and(|content| content.contains(needle))
    }

    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Another process holding one log's lock must neither drop that log's lines
    /// nor hold up the others. The free log lands while the lock is held, and the
    /// held log's lines, including ones queued after it went busy, land in order
    /// once it is released. The child's store deadline is 1 ms, so falling back to
    /// it drops the batch, and waiting on the lock never writes the free log.
    #[test]
    fn a_held_log_keeps_its_lines_in_order_while_other_logs_drain() {
        if let Some(root) = std::env::var_os("TOOLPORT_TELEMETRY_HELD_CHILD") {
            let root = PathBuf::from(root);
            let held = root.join("gateway.log");
            record(&held, "held-1", Rotation::Gateway);
            record(
                &root.join("audit.jsonl"),
                "{\"free\":1}",
                trimmed(1 << 20, 100),
            );
            wait_for_file(&root.join("free-seen"));
            record(&held, "held-2", Rotation::Gateway);
            record(&held, "held-3", Rotation::Gateway);
            std::fs::write(root.join("queued"), "").unwrap();
            wait_for_file(&root.join("released"));
            assert!(flush_for_test(Duration::MAX));
            assert_eq!(health(), Health::default(), "a record was not persisted");
            return;
        }

        let root = scratch("held-log");
        let held = root.join("gateway.log");
        let free = root.join("audit.jsonl");
        let guard = crate::registry::lock_at(&held).expect("hold the gateway log lock");
        let mut writer = WriterProcess::spawn(
            "telemetry::tests::a_held_log_keeps_its_lines_in_order_while_other_logs_drain",
            &root,
            Some("1"),
        );
        writer.wait_until("the free log while the other is held", || {
            file_has(&free, "{\"free\":1}")
        });
        assert!(!held.exists(), "held log written without its lock");
        std::fs::write(root.join("free-seen"), "").unwrap();
        let queued = root.join("queued");
        writer.wait_until("later lines for the held log", || queued.exists());
        drop(guard);
        std::fs::write(root.join("released"), "").unwrap();
        let status = writer.exit_status();
        assert!(status.success(), "writer failed:\n{}", writer.stderr());
        assert_eq!(
            std::fs::read_to_string(&held).unwrap(),
            "held-1\nheld-2\nheld-3\n"
        );
    }

    /// Shutdown while another process holds one log's lock still lands the other
    /// logs, and reports the held one as an incomplete flush instead of exiting
    /// as if everything landed.
    #[test]
    fn shutdown_lands_free_logs_and_reports_a_held_one() {
        if let Some(root) = std::env::var_os("TOOLPORT_TELEMETRY_HELD_CHILD") {
            let root = PathBuf::from(root);
            record(&root.join("gateway.log"), "held", Rotation::Gateway);
            record(
                &root.join("audit.jsonl"),
                "{\"free\":1}",
                trimmed(1 << 20, 100),
            );
            shutdown();
            return;
        }

        let root = scratch("held-shutdown");
        let held = root.join("gateway.log");
        let free = root.join("audit.jsonl");
        let guard = crate::registry::lock_at(&held).expect("hold the gateway log lock");
        let mut writer = WriterProcess::spawn(
            "telemetry::tests::shutdown_lands_free_logs_and_reports_a_held_one",
            &root,
            None,
        );
        let status = writer.exit_status();
        drop(guard);
        let stderr = writer.stderr();
        assert!(status.success(), "writer failed:\n{stderr}");
        assert!(
            file_has(&free, "{\"free\":1}"),
            "free log lost at shutdown:\n{stderr}"
        );
        assert!(
            stderr.contains("telemetry shutdown budget exhausted"),
            "held log not reported:\n{stderr}"
        );
    }

    fn wait_for(label: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {label}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn gateway_record(path: &Path, line: &str) -> Record {
        Record {
            path: path.to_path_buf(),
            line: line.into(),
            rotation: Rotation::Gateway,
        }
    }

    /// Queue a flush barrier without waiting on it.
    fn barrier(writer: &Writer) -> Receiver<()> {
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        writer
            .tx
            .as_ref()
            .unwrap()
            .try_send(Msg::Flush(done_tx, None))
            .unwrap();
        done_rx
    }

    /// Only contention is retried. Any other lock failure is a failed write, as
    /// it was before busy logs were kept, so health shows it and flush returns.
    #[test]
    fn a_lock_error_other_than_contention_counts_as_a_failed_write() {
        let dir = scratch("lock-error");
        let path = dir.join("audit.jsonl");
        #[cfg(unix)]
        let code = libc::ENOLCK;
        #[cfg(windows)]
        let code = 8; // ERROR_NOT_ENOUGH_MEMORY
        crate::registry::inject_lock_error_for_test(&path, code);
        let writer = Writer::spawn(QUEUE_CAPACITY, append_batch);
        writer.record(Record {
            path: path.clone(),
            line: "{\"i\":1}".into(),
            rotation: trimmed(1 << 20, 100),
        });
        assert!(
            writer.flush(Duration::from_secs(5)),
            "a lock error was retried as contention"
        );
        let health = writer.counters.health();
        assert_eq!((health.write_failures, health.write_failed_records), (1, 1));
        assert!(!path.exists());
        assert!(retire_dir_for_test(&dir, Duration::from_secs(5)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A savings row kept for a busy log is tokenized once, not on every retry.
    #[test]
    fn a_kept_savings_row_is_tokenized_once() {
        let dir = scratch("savings-prepared");
        let path = dir.join("savings-v4.jsonl");
        let probe = format!("tokenize-probe-{}", dir.display());
        let held = crate::registry::lock_at(&path).expect("hold the savings lock");
        let writer = Writer::spawn(QUEUE_CAPACITY, append_batch);
        writer.record(Record {
            path: path.clone(),
            line: serde_json::json!({"v": 2, "kind": "list", "_fullText": probe}).to_string(),
            rotation: Rotation::Savings {
                max_bytes: 1 << 20,
                keep_lines: 100,
            },
        });
        // The writer retries every 20 ms while the lock is held.
        assert!(!writer.flush(Duration::from_millis(300)));
        drop(held);
        assert!(writer.flush(Duration::from_secs(5)));
        assert_eq!(crate::savings::tokenized_probe_count(&probe), 1);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("fullTokens") && !content.contains(&probe));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A flush waits on what was queued before it, not on a busy log whose lines
    /// arrived after it.
    #[test]
    fn a_flush_does_not_wait_on_records_queued_after_it() {
        let dir = scratch("barrier-order");
        let (a, b, c) = (dir.join("a.log"), dir.join("b.log"), dir.join("c.log"));
        let held_a = crate::registry::lock_at(&a).expect("hold a");
        let held_b = crate::registry::lock_at(&b).expect("hold b");
        let writer = Writer::spawn(QUEUE_CAPACITY, append_batch);
        writer.record(gateway_record(&a, "before"));
        let done = barrier(&writer);
        writer.record(gateway_record(&b, "after"));
        writer.record(gateway_record(&c, "marker"));
        // c lands in the pass that kept "after" for b, or a later one.
        wait_for("the free log", || file_has(&c, "marker"));
        drop(held_a);
        assert!(
            done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "flush waited on a record queued after it"
        );
        assert!(file_has(&a, "before"));
        assert!(!b.exists());
        drop(held_b);
        assert!(writer.flush(Duration::from_secs(5)));
        assert!(file_has(&b, "after"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A gap marker whose log is busy is retried on the kept-batch schedule and
    /// holds flush until it lands, even when no record batch is kept.
    #[test]
    fn a_busy_gap_marker_is_retried_and_holds_flush() {
        let dir = scratch("busy-gap");
        let held = dir.join("x.log");
        let gateway = dir.join("gateway.log");
        let held_x = crate::registry::lock_at(&held).expect("hold x");
        let held_gateway = crate::registry::lock_at(&gateway).expect("hold gateway.log");
        // Room for three kept non-audit lines; the rest are counted as dropped.
        let writer = Writer::spawn(4, append_batch);
        for i in 0..10 {
            writer.record(gateway_record(&held, &format!("x-{i}")));
        }
        wait_for("the audit gap marker", || {
            file_has(&dir.join("audit.jsonl"), "telemetry_gap")
        });
        drop(held_x);
        wait_for("the kept lines", || file_has(&held, "x-0"));
        assert!(
            !writer.flush(Duration::from_millis(300)),
            "flush passed with a gap marker unwritten"
        );
        drop(held_gateway);
        wait_for("the gateway gap marker without another flush", || {
            file_has(&gateway, "telemetry gap")
        });
        assert!(writer.flush(Duration::from_secs(5)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
