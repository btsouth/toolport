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
use std::sync::atomic::{AtomicU8, Ordering};

static ROLE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Adapter,
    Daemon,
    Private,
    HttpProxy,
    LegacyHttp,
    LegacyTty,
}

pub fn set_role(role: Role) {
    ROLE.store(
        match role {
            Role::Adapter => 1,
            Role::Daemon => 2,
            Role::Private => 0,
            Role::HttpProxy => 3,
            Role::LegacyHttp => 4,
            Role::LegacyTty => 5,
        },
        Ordering::Relaxed,
    );
}

fn format_line(msg: &str, millis: u64, pid: u32, role: &str) -> String {
    let seconds = millis / 1000 % 86_400;
    let msg = crate::registry::redact_secret_text(&crate::redact_url_userinfo(msg))
        .replace(['\n', '\r'], " ");
    format!(
        "{}T{:02}:{:02}:{:02}.{:03}Z pid={pid} role={role} {msg}",
        crate::usage_report::utc_day(millis),
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        millis % 1000
    )
}

/// Keep the always-on gateway log bounded; trimmed to roughly the back half once
/// it grows past this, so a long-running client can't let it grow without limit.
pub const GATEWAY_LOG_CAP: u64 = 256 * 1024;

/// Queue one formatted line without disk IO. Overload and persistence failures
/// share the telemetry health counters; logging never blocks a connection.
pub fn append(msg: &str) {
    let Some(path) = crate::registry::gateway_log_path() else {
        return;
    };
    queue_at(&path, msg);
}

pub(crate) fn queue_at(path: &Path, msg: &str) {
    crate::telemetry::record(path, &line(msg), crate::telemetry::Rotation::Gateway);
}

pub(crate) fn line(msg: &str) -> String {
    let role = match ROLE.load(Ordering::Relaxed) {
        1 => "adapter",
        2 => "daemon",
        3 => "http-proxy",
        4 => "legacy-http",
        5 => "legacy-tty",
        _ => "private",
    };
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    format_line(msg, millis, std::process::id(), role)
}

/// How long an append waits for the shared log lock before writing without it.
///
/// Deliberately far shorter than the registry's own deadline. This lock exists
/// only to serialize the *trim*; the append underneath it is a single `O_APPEND`
/// write that is already safe unserialized. Waiting the registry's five seconds
/// therefore buys nothing and costs everything: [`append`] is called four times
/// on the gateway's startup path BEFORE it reads its first line of stdin, so
/// under contention a client waited up to twenty seconds for its `initialize`
/// reply and gave up long before that (SBS-1019). Trimming is best effort by
/// design - see the `Err` arm below - so conceding the lock quickly is the
/// cheapest thing this function can do.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// [`LOCK_WAIT`], but never shorter than an explicitly configured store deadline.
///
/// `TOOLPORT_LOCK_TIMEOUT_MS` may only RAISE this, never lower it: the tests that
/// set it are asserting that a contended append genuinely waits its turn, and
/// they cannot do that against a deadline shorter than the hold they stage. A
/// caller that wants less contention tolerance than 250ms is asking for torn
/// diagnostics, so the floor stands.
fn lock_wait() -> std::time::Duration {
    crate::brand::env_var("TOOLPORT_LOCK_TIMEOUT_MS", "CONDUIT_LOCK_TIMEOUT_MS")
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(std::time::Duration::from_millis)
        .map_or(LOCK_WAIT, |configured| configured.max(LOCK_WAIT))
}

/// Append `msg` to `path` and trim if needed, holding the sibling lock across
/// both so a concurrent writer cannot land a line that this process's stale
/// trim snapshot then overwrites (SBS-869). A lock we cannot take degrades to
/// an unlocked append rather than to a lost line.
#[cfg(test)]
pub(crate) fn append_to(path: &Path, msg: &str) {
    let _ = append_batch_to(path, &[msg.to_string()]);
}

pub(crate) fn append_batch_to(
    path: &Path,
    lines: &[String],
) -> Result<(), crate::telemetry::AppendError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match crate::registry::lock_at_for(path, lock_wait()) {
        Ok(_lock) => {
            for (index, line) in lines.iter().enumerate() {
                append_line(path, line).map_err(|message| crate::telemetry::AppendError {
                    message,
                    unconfirmed: Some(lines.len() - index),
                })?;
            }
            try_trim_log_if_large(path).map_err(crate::telemetry::AppendError::after_append)
        }
        // A contended diagnostic lock defers only rotation. No caller waits here:
        // this runs on the telemetry writer, with each whole line in one append.
        Err(_) => {
            for (index, line) in lines.iter().enumerate() {
                append_line(path, line).map_err(|message| crate::telemetry::AppendError {
                    message,
                    unconfirmed: Some(lines.len() - index),
                })?;
            }
            Ok(())
        }
    }
}

/// One `O_APPEND` write of the whole record, so even the unlocked fallback
/// cannot interleave half a line with another writer's.
fn append_line(path: &Path, msg: &str) -> Result<(), String> {
    let mut file = crate::registry::open_append_private(path).map_err(|error| error.to_string())?;
    file.write_all(format!("{msg}\n").as_bytes())
        .map_err(|error| error.to_string())
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
    let _ = try_trim_log_if_large(path);
}

fn try_trim_log_if_large(path: &Path) -> Result<(), String> {
    let over = std::fs::metadata(path)
        .map(|m| m.len() > GATEWAY_LOG_CAP)
        .unwrap_or(false);
    if !over {
        return Ok(());
    }
    let data = std::fs::read(path).map_err(|error| error.to_string())?;
    let keep_from = data.len().saturating_sub((GATEWAY_LOG_CAP / 2) as usize);
    let start = data[keep_from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| keep_from + i + 1)
        .unwrap_or(keep_from);
    // Lossy so a non-UTF-8 byte cannot skip the trim: atomic_write takes &str.
    let kept = String::from_utf8_lossy(&data[start..]);
    crate::registry::atomic_write(path, &kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn log_line_has_timestamp_pid_role_and_redacts_credentials() {
        for role in [
            "adapter",
            "daemon",
            "private",
            "http-proxy",
            "legacy-http",
            "legacy-tty",
        ] {
            let line = format_line("connect https://alice:password@example.com api_key=sk-live-secretvalue1234567890\nforged", 1234, 42, role);
            assert!(line.starts_with(&format!("1970-01-01T00:00:01.234Z pid=42 role={role} ")));
            assert!(!line.contains("password"));
            assert!(!line.contains("secretvalue"));
            assert!(!line.contains('\n'));
        }
    }

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
