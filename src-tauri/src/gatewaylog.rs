//! The always-on gateway log (`gateway.log`).
//!
//! This is what `gather_diagnostics` bundles into a bug report, so it stays on
//! regardless of `TOOLPORT_DEBUG` and holds connection-lifecycle facts only:
//! starts, connect successes, connect failures, and catalogs that came back
//! incomplete.
//!
//! It lives in the library rather than the gateway binary because the code that
//! knows a catalog was truncated is [`crate::downstream`], and a warning only
//! that module can see is worthless if it cannot reach the file a user actually
//! sends us. MCP clients swallow a gateway's stderr, so `eprintln!` alone means
//! a silent truncation is indistinguishable from a healthy connect in any
//! after-the-fact diagnosis - which is exactly how a downstream server served a
//! 3-tool prefix of its 40-tool catalog for days without leaving a trace.

use std::io::Write;
use std::path::Path;

/// Keep the always-on gateway log bounded; trimmed to roughly the back half once
/// it grows past this, so a long-running client can't let it grow without limit.
pub const GATEWAY_LOG_CAP: u64 = 256 * 1024;

/// Append one line to the gateway log. Best-effort: logging must never take down
/// a connection, so every failure here is swallowed.
pub fn append(msg: &str) {
    let Some(path) = crate::registry::gateway_log_path() else {
        return;
    };
    append_to(&path, msg);
}

/// Append `msg` to `path` and trim if needed, holding the sibling lock across
/// both so a concurrent writer cannot land a line that this process's stale
/// trim snapshot then overwrites (SBS-869). A lock we cannot take degrades to
/// an unlocked append rather than to a lost line.
pub(crate) fn append_to(path: &Path, msg: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Atomic replacement protects readers from an empty window, but only this
    // shared cross-process critical section prevents a stale trim snapshot
    // from replacing a line another gateway just appended (SBS-869).
    match crate::registry::lock_at(path) {
        Ok(_lock) => {
            append_line(path, msg);
            trim_log_if_large(path);
        }
        // Never trade a line for the lock. This log exists so a diagnostics
        // bundle still shows the connect failure, and before SBS-869 the append
        // ran with no lock at all - so a stale lock file or a contended
        // deadline must not make us quieter than the code we replaced. Write
        // the line and skip only the trim, which is the half that is unsafe
        // unserialized; the next append that does win the lock re-bounds the
        // file.
        Err(error) => {
            eprintln!(
                "toolport: appending to '{}' without the gateway log lock ({error}); trim deferred",
                path.display()
            );
            append_line(path, msg);
        }
    }
}

/// One `O_APPEND` write of the whole record, so even the unlocked fallback
/// cannot interleave half a line with another writer's.
fn append_line(path: &Path, msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(format!("{msg}\n").as_bytes());
    }
}

/// Trim the log to roughly its back half once it exceeds [`GATEWAY_LOG_CAP`],
/// cutting at a line boundary so the survivor never starts mid-record.
///
/// The kept tail is written with `atomic_write` (temp + fsync + rename), never
/// a truncating `fs::write`, so a concurrent diagnostics read never sees an
/// empty file and a concurrent append cannot land in a truncated hole
/// (SBS-869). Callers that already hold `registry::lock_at` (production
/// `append`) keep it across this replace; the function still works without
/// that lock so the gateway binary's existing trim test can call it directly.
pub fn trim_log_if_large(path: &Path) {
    let over = std::fs::metadata(path)
        .map(|m| m.len() > GATEWAY_LOG_CAP)
        .unwrap_or(false);
    if !over {
        return;
    }
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    let keep_from = data.len().saturating_sub((GATEWAY_LOG_CAP / 2) as usize);
    let start = data[keep_from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| keep_from + i + 1)
        .unwrap_or(keep_from);
    // Lossy so a non-UTF-8 byte cannot skip the trim: atomic_write takes &str.
    let kept = String::from_utf8_lossy(&data[start..]);
    let _ = crate::registry::atomic_write(path, &kept);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_log_path() -> PathBuf {
        let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "toolport-sbs869-gatewaylog-{}-{nanos}-{n}.log",
            std::process::id()
        ))
    }

    fn lock_sibling(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(".lock");
        PathBuf::from(s)
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(lock_sibling(path));
    }

    fn over_cap_body() -> String {
        let filler = "x".repeat(GATEWAY_LOG_CAP as usize + 8192);
        format!("OLDEST\n{filler}\nNEWEST\n")
    }

    fn assert_trimmed_tail(after: &str) {
        assert!((after.len() as u64) <= GATEWAY_LOG_CAP, "still over cap");
        assert!(after.ends_with("NEWEST\n"), "lost the newest line");
        assert!(
            !after.contains("OLDEST"),
            "kept the oldest line past the cap"
        );
        assert!(!after.starts_with('x'), "did not cut on a line boundary");
    }

    /// Failure mode: an over-cap gateway.log is not bounded, or the kept tail
    /// starts mid-record / drops the newest line.
    #[test]
    fn trim_over_cap_keeps_back_half_on_a_line_boundary() {
        let path = unique_log_path();
        std::fs::write(&path, over_cap_body()).unwrap();

        trim_log_if_large(&path);

        let after = std::fs::read_to_string(&path).unwrap();
        assert_trimmed_tail(&after);
        cleanup(&path);
    }

    /// Failure mode: trim rewrites gateway.log with a truncating write, so a
    /// concurrent diagnostics read can observe an empty file (SBS-869 race 1)
    /// and a concurrent append can land in the hole then be overwritten (race 2).
    ///
    /// `atomic_write` creates a sibling temp, sets owner-only 0o600, then
    /// renames; `fs::write` truncates in place and keeps the old inode/mode.
    /// Creating the over-cap file with `fs::write` (then 0o644) makes both
    /// signals fail if someone reverts the production write.
    #[test]
    fn trim_replaces_via_new_inode_and_owner_only_mode() {
        let path = unique_log_path();
        std::fs::write(&path, over_cap_body()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let before = std::fs::metadata(&path).unwrap();
        #[cfg(unix)]
        let before_ino = {
            use std::os::unix::fs::MetadataExt;
            before.ino()
        };

        trim_log_if_large(&path);

        let after = std::fs::read_to_string(&path).unwrap();
        assert_trimmed_tail(&after);
        let after_meta = std::fs::metadata(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                after_meta.ino(),
                before_ino,
                "trim must replace via rename, not truncate in place"
            );
            assert_eq!(
                after_meta.mode() & 0o777,
                0o600,
                "atomic_write sets owner-only before writing"
            );
        }
        #[cfg(not(unix))]
        {
            let _ = (before, after_meta);
        }
        cleanup(&path);
    }

    /// Failure mode: a small gateway.log is rewritten even though it is under
    /// the cap.
    #[test]
    fn trim_under_cap_is_a_noop() {
        let path = unique_log_path();
        let content = "small\nunder-cap\n";
        std::fs::write(&path, content).unwrap();

        trim_log_if_large(&path);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    /// Failure mode: the append that crosses [`GATEWAY_LOG_CAP`] trims away a
    /// line an earlier append just wrote (SBS-869 race 2, one process).
    #[test]
    fn appends_that_cross_the_cap_keep_every_line_written_after_the_cut() {
        let path = unique_log_path();
        // One giant already-over-cap line, so the first append is what runs the
        // trim and the line-boundary cut lands right after that prefix: the
        // kept tail is then exactly the appends, with nothing to hide a loss.
        std::fs::write(
            &path,
            format!("{}\n", "o".repeat(GATEWAY_LOG_CAP as usize + 4096)),
        )
        .unwrap();

        append_to(&path, "UNIQUE_A");
        append_to(&path, "UNIQUE_B");
        append_to(&path, "UNIQUE_C");

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            (after.len() as u64) <= GATEWAY_LOG_CAP,
            "the trim-triggering append left the file over cap"
        );
        assert_eq!(after, "UNIQUE_A\nUNIQUE_B\nUNIQUE_C\n");
        cleanup(&path);
    }

    fn wait_for_path(path: &Path, label: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(path.exists(), "timed out waiting for {label}");
    }

    fn wait_for_child(child: &mut std::process::Child, label: &str) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match child.try_wait().expect("poll gateway log child") {
                Some(status) => return status,
                None if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("timed out waiting for {label}");
                }
            }
        }
    }

    /// The separate process for
    /// [`append_to_waits_for_a_log_lock_another_process_holds`]. Inert without
    /// its env vars, so a normal run of this module skips it.
    #[test]
    fn gatewaylog_lock_sentinel_child() {
        let Some(path) = std::env::var_os("TOOLPORT_GATEWAYLOG_SENTINEL_PATH") else {
            return;
        };
        let attempting = PathBuf::from(
            std::env::var_os("TOOLPORT_GATEWAYLOG_SENTINEL_ATTEMPTING")
                .expect("sentinel attempting path"),
        );
        let done = PathBuf::from(
            std::env::var_os("TOOLPORT_GATEWAYLOG_SENTINEL_DONE").expect("sentinel done path"),
        );

        std::fs::write(&attempting, "attempting").expect("signal sentinel append attempt");
        append_to(Path::new(&path), "UNIQUE_CHILD");
        std::fs::write(done, "done").expect("signal sentinel append complete");
    }

    /// Failure mode: `append_to` writes without the shared cross-process lock,
    /// so a second gateway's line can land inside another process's read-then-
    /// replace trim window and be lost (SBS-869 race 2, two processes). Drop
    /// the lock from `append_to` and the child's line lands immediately.
    #[test]
    fn append_to_waits_for_a_log_lock_another_process_holds() {
        let root = std::env::temp_dir().join(format!(
            "toolport-sbs869-gatewaylog-lock-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("gateway.log");
        let attempting = root.join("attempting");
        let done = root.join("done");
        std::fs::write(&path, "SEED\n").unwrap();
        // The child has to wait out the hold below, not time out into the
        // unlocked fallback append. Children inherit the raised deadline.
        let _lock_budget = crate::registry::LockTimeoutOverride::generous();
        let held = crate::registry::lock_at(&path).expect("hold the gateway log lock");

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gatewaylog::tests::gatewaylog_lock_sentinel_child",
                "--nocapture",
            ])
            .env("TOOLPORT_GATEWAYLOG_SENTINEL_PATH", &path)
            .env("TOOLPORT_GATEWAYLOG_SENTINEL_ATTEMPTING", &attempting)
            .env("TOOLPORT_GATEWAYLOG_SENTINEL_DONE", &done)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn independent gateway log appender");
        wait_for_path(&attempting, "sentinel append attempt");
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Read the verdict before releasing, but assert after, so a failure
        // still unblocks and reaps the child.
        let blocked = !done.exists();
        drop(held);

        let status = wait_for_child(&mut child, "gateway log sentinel child");
        assert!(
            blocked,
            "a separate process must not append while another holds the log lock"
        );
        assert!(status.success(), "sentinel child failed: {status}");
        assert!(
            done.exists(),
            "the child's append must finish once the lock frees"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "SEED\nUNIQUE_CHILD\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
