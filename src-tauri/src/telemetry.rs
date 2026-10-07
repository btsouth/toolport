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
const FLUSH_BUDGET: Duration = Duration::from_millis(500);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    Gateway,
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
            "Activity, savings and diagnostics may be incomplete: {} records dropped, {} records with unconfirmed writes, {} write failures, {} incomplete flushes since gateway start.",
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
    tx: Option<SyncSender<Msg>>,
    counters: Arc<Counters>,
}

static WRITER: OnceLock<Writer> = OnceLock::new();

fn writer() -> &'static Writer {
    WRITER.get_or_init(|| Writer::spawn(QUEUE_CAPACITY, append_batch))
}

impl Writer {
    fn spawn(
        capacity: usize,
        append: impl FnMut(&Path, &[String], Rotation) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let counters = Arc::new(Counters::default());
        let worker_counters = counters.clone();
        let (tx, rx) = mpsc::sync_channel(capacity);
        let tx = std::thread::Builder::new()
            .name("toolport-telemetry".into())
            .spawn(move || writer_loop(rx, &worker_counters, append))
            .ok()
            .map(|_| tx);
        Self { tx, counters }
    }

    fn record(&self, record: Record) {
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.try_send(Msg::Record(record)).is_err())
        {
            self.counters.queue_dropped.fetch_add(1, Ordering::Relaxed);
        }
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
    let mut local = health();
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
    let result = ureq::get(&format!(
        "http://{}{}",
        descriptor.endpoint,
        crate::daemon::IDENTITY_PATH
    ))
    .set("Authorization", &format!("Bearer {}", descriptor.token))
    .timeout(Duration::from_millis(500))
    .call()
    .map_err(|_| ())
    .and_then(|response| response.into_json::<serde_json::Value>().map_err(|_| ()))
    .and_then(|identity| {
        if identity["compat"].as_str() != Some(compat.fingerprint().as_str()) {
            return Err(());
        }
        serde_json::from_value::<Health>(identity["telemetry"].clone()).map_err(|_| ())
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
    mut append: impl FnMut(&Path, &[String], Rotation) -> Result<(), String>,
) {
    let mut pending = Vec::new();
    loop {
        let mut done = None;
        match rx.recv() {
            Ok(Msg::Record(record)) => pending.push(record),
            Ok(Msg::Flush(waiter)) => done = Some(waiter),
            Err(_) => break,
        }
        if done.is_none() {
            let deadline = Instant::now() + FLUSH_INTERVAL;
            while pending.len() < BATCH_MAX {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(Msg::Record(record)) => pending.push(record),
                    Ok(Msg::Flush(waiter)) => {
                        done = Some(waiter);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        deliver(&mut pending, counters, &mut append);
        if let Some(done) = done {
            let _ = done.send(());
        }
    }
}

fn deliver(
    pending: &mut Vec<Record>,
    counters: &Counters,
    append: &mut impl FnMut(&Path, &[String], Rotation) -> Result<(), String>,
) {
    let mut groups: Vec<(std::path::PathBuf, Rotation, Vec<String>)> = Vec::new();
    for record in pending.drain(..) {
        match groups.iter_mut().find(|group| group.0 == record.path) {
            Some(group) => group.2.push(record.line),
            None => groups.push((record.path, record.rotation, vec![record.line])),
        }
    }
    for (path, rotation, lines) in groups {
        if append(&path, &lines, rotation).is_err() {
            counters.write_failures.fetch_add(1, Ordering::Relaxed);
            counters
                .write_failed_records
                .fetch_add(lines.len() as u64, Ordering::Relaxed);
            // Fixed text: paths and OS/downstream errors can contain credentials.
            // Do not enqueue diagnostics about a failed diagnostic write recursively.
            if rotation != Rotation::Gateway {
                crate::gatewaylog::append("telemetry batch persistence failed; Activity, savings and diagnostics may be incomplete");
            } else {
                eprintln!("toolport: gateway diagnostic write failed");
            }
        }
    }
}

fn append_batch(path: &Path, lines: &[String], rotation: Rotation) -> Result<(), String> {
    match rotation {
        Rotation::Gateway => crate::gatewaylog::append_batch_to(path, lines),
        Rotation::TrimTail {
            max_bytes,
            keep_lines,
        } => crate::registry::append_lines_locked(path, lines, max_bytes, keep_lines, None),
        Rotation::Savings {
            max_bytes,
            keep_lines,
        } => crate::savings::append_lines_at(path, lines, max_bytes, keep_lines),
    }
}

#[cfg(test)]
mod tests {
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

    fn fixture_record(n: u64) -> Record {
        Record {
            path: PathBuf::from("unused"),
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
        let _data = crate::registry::DataDirOverride::set(&dir);
        for rotation in [
            trimmed(1024, 100),
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
                    record.rotation = rotation;
                }
                deliver(&mut pending, &counters, &mut |_, _, _| Err(error.into()));
                let health = counters.health();
                assert_eq!(health.write_failures, 1);
                assert_eq!(health.write_failed_records, 2);
                assert!(health
                    .notice()
                    .unwrap()
                    .contains("2 records with unconfirmed writes"));
            }
        }
        assert!(flush());
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
        flush();

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
        let _ = std::fs::remove_dir_all(dir);
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
        flush();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"i\":7}\n{\"i\":8}\n{\"i\":9}\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A record call never waits on a cross-process lock held elsewhere: it queues
    /// and returns, and the writer lands the line once the lock is free.
    #[test]
    fn record_does_not_block_on_a_held_lock() {
        let dir = scratch("held-lock");
        let path = dir.join("audit.jsonl");
        // Drain anything other tests queued before holding the append lock.
        flush();
        let guard = crate::registry::lock_at(&path).expect("hold the append lock");

        let start = Instant::now();
        record(&path, "{\"held\":true}", trimmed(1024 * 1024, 100));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "record waited on the lock: {elapsed:?}"
        );

        drop(guard);
        flush();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("{\"held\":true}"),
            "line landed: {content}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
