//! Background writer for the append-only telemetry logs.
//!
//! The audit, savings and search-trace logs used to be appended on the request
//! thread. That put a cross-process file lock, a write and - whenever a cap was
//! crossed - a full read-and-rewrite with fsync between a request and its
//! response (PERF-05, PERF-10; 46 ms p95 rotation stall). Now the caller formats
//! the line and hands it to one process-wide writer thread, which batches records
//! per file, takes each file's existing cross-process lock once per batch, and
//! applies the existing cap and rotation logic there. The on-disk formats, file
//! names, caps and lock files are unchanged, so the app and a second gateway keep
//! reading and writing the same files; they just see lines up to one flush
//! interval later.
//!
//! Failure policy: the logs are an audit trail, so a record must not vanish
//! silently. When the queue is full or the writer is gone, the caller appends
//! synchronously through the same capped path. The queue is bounded, so a stalled
//! writer cannot grow memory without limit.
//!
//! Durability: a plain batch append is not fsynced per line, so a crash can lose
//! up to the last flush interval ([`FLUSH_INTERVAL`]). Rotation still publishes
//! through [`crate::registry::atomic_write`], which fsyncs its temp file before the
//! rename. [`flush`] blocks until everything queued before it is on disk; gateway
//! shutdown and every in-process reader call it so they never miss their own
//! writes.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Lines held before a batch is written even when the interval has not elapsed.
const BATCH_MAX: usize = 64;
/// Longest a queued line waits for the batch to fill.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Bounded queue. A full queue falls back to the synchronous append, so a burst
/// never blocks the caller and memory cannot grow without limit. Generous enough
/// that the fallback only triggers when the writer is genuinely backed up.
const QUEUE_CAPACITY: usize = 8192;

/// Which existing cap/rotation contract applies to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    /// [`crate::registry::append_lines_locked`]: trim to `keep_lines` once past
    /// `max_bytes`. Used by the audit and search-trace logs.
    TrimTail { max_bytes: u64, keep_lines: usize },
    /// [`crate::savings::append_lines_at`]: fold older lines into one carry line
    /// once past `max_bytes`, preserving the running total.
    Savings { max_bytes: u64, keep_lines: usize },
}

/// One queued append.
struct Record {
    path: std::path::PathBuf,
    line: String,
    rotation: Rotation,
}

enum Msg {
    Record(Record),
    /// Flush everything queued before this message, then acknowledge.
    Flush(SyncSender<()>),
}

static WRITER: OnceLock<Option<SyncSender<Msg>>> = OnceLock::new();

/// The process-wide writer's sender, spawning the thread on first use.
fn sender() -> &'static Option<SyncSender<Msg>> {
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE_CAPACITY);
        match std::thread::Builder::new()
            .name("toolport-telemetry".to_string())
            .spawn(move || writer_loop(rx))
        {
            Ok(_) => Some(tx),
            // No writer thread means every `record` falls back to the synchronous
            // append; the logs still fill, just on the caller.
            Err(_) => None,
        }
    })
}

/// Queue one already-serialized JSONL `line` for `path`, falling back to a
/// synchronous append when the queue is full or the writer is gone. Never blocks
/// on file IO on the calling thread.
pub(crate) fn record(path: &Path, line: &str, rotation: Rotation) {
    let record = Record {
        path: path.to_path_buf(),
        line: line.to_string(),
        rotation,
    };
    let pending = if let Some(tx) = sender() {
        tx.try_send(Msg::Record(record)).err().and_then(rejected_record)
    } else {
        Some(record)
    };
    if let Some(record) = pending {
        append_sync(&record);
    }
}

/// The record a failed `try_send` handed back. `try_send` only returns the message
/// it was given, which is always a record on this path.
fn rejected_record(error: TrySendError<Msg>) -> Option<Record> {
    match error {
        TrySendError::Full(Msg::Record(record))
        | TrySendError::Disconnected(Msg::Record(record)) => Some(record),
        _ => None,
    }
}

/// Block until every line queued before this call has been written and any
/// rotation it triggered has completed.
///
/// A no-op when nothing was ever queued, so read-only processes never start the
/// writer thread just to read.
pub fn flush() {
    let Some(Some(tx)) = WRITER.get() else {
        return;
    };
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
    if tx.send(Msg::Flush(done_tx)).is_ok() {
        // FIFO: the writer reaches this only after every record queued before it.
        // A disconnected writer has already drained or handed its records back to
        // the synchronous fallback.
        let _ = done_rx.recv();
    }
}

fn writer_loop(rx: Receiver<Msg>) {
    let mut pending: Vec<Record> = Vec::new();
    let mut waiters: Vec<SyncSender<()>> = Vec::new();
    loop {
        let mut disconnected = false;
        let flush_now = match rx.recv() {
            Ok(Msg::Record(record)) => {
                pending.push(record);
                false
            }
            Ok(Msg::Flush(done)) => {
                waiters.push(done);
                true
            }
            Err(_) => break,
        };
        if !flush_now {
            let deadline = Instant::now() + FLUSH_INTERVAL;
            while pending.len() < BATCH_MAX {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(remaining) {
                    Ok(Msg::Record(record)) => pending.push(record),
                    Ok(Msg::Flush(done)) => {
                        waiters.push(done);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if !pending.is_empty() {
            deliver(&mut pending);
        }
        // Acknowledge only after the batch is on disk.
        for waiter in waiters.drain(..) {
            let _ = waiter.send(());
        }
        if disconnected {
            break;
        }
    }
    // Channel closed: write whatever is left so a clean shutdown loses nothing.
    if !pending.is_empty() {
        deliver(&mut pending);
    }
}

/// Group a batch by file and append each file's lines under one lock. Batches are
/// at most [`BATCH_MAX`] lines, so a linear scan for the (few) distinct paths is
/// cheaper than allocating a map.
fn deliver(pending: &mut Vec<Record>) {
    let mut groups: Vec<(std::path::PathBuf, Rotation, Vec<String>)> = Vec::new();
    for record in pending.drain(..) {
        match groups.iter_mut().find(|group| group.0 == record.path) {
            Some(group) => group.2.push(record.line),
            None => groups.push((record.path, record.rotation, vec![record.line])),
        }
    }
    for (path, rotation, lines) in groups {
        append_batch(&path, &lines, rotation);
    }
}

fn append_batch(path: &Path, lines: &[String], rotation: Rotation) {
    match rotation {
        Rotation::TrimTail {
            max_bytes,
            keep_lines,
        } => {
            if let Err(error) =
                crate::registry::append_lines_locked(path, lines, max_bytes, keep_lines, None)
            {
                eprintln!("toolport: telemetry batch dropped for '{}': {error}", path.display());
            }
        }
        Rotation::Savings {
            max_bytes,
            keep_lines,
        } => {
            let _ = crate::savings::append_lines_at(path, lines, max_bytes, keep_lines);
        }
    }
}

/// The synchronous fallback, also used directly by tests. Same capped path as the
/// writer thread so a full queue changes only WHEN a line lands, not whether.
fn append_sync(record: &Record) {
    append_batch(&record.path, std::slice::from_ref(&record.line), record.rotation);
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
        assert_eq!(unique.len(), lines.len(), "no duplicate or interleaved line");
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
        // Drain anything other tests queued, so this record takes the non-blocking
        // enqueue path rather than the (allowed) synchronous fallback.
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
        assert!(content.contains("{\"held\":true}"), "line landed: {content}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// When the queue is full the caller falls back to the synchronous append, so
    /// a full channel loses nothing. Uses a local channel to avoid racing the
    /// process-wide writer.
    #[test]
    fn full_queue_falls_back_to_the_synchronous_append() {
        let (tx, _rx) = mpsc::sync_channel::<Msg>(1);
        let dir = scratch("fallback");
        let path = dir.join("audit.jsonl");

        // Fill the lone slot.
        assert!(tx.try_send(Msg::Record(Record {
            path: path.clone(),
            line: "{\"first\":true}".to_string(),
            rotation: trimmed(1024 * 1024, 100),
        })).is_ok());

        let blocked = Record {
            path: path.clone(),
            line: "{\"second\":true}".to_string(),
            rotation: trimmed(1024 * 1024, 100),
        };
        let error = tx.try_send(Msg::Record(blocked)).expect_err("queue is full");
        let record = rejected_record(error).expect("a record came back");
        append_sync(&record);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("{\"second\":true}"), "fallback wrote: {content}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
