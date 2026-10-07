//! The host daemon's own record: panics, stderr, and how the last run ended.
//!
//! The daemon is spawned detached, so before this its stderr went nowhere and a
//! crash left only a stale descriptor. Now `daemon.log` in the data directory
//! takes its stderr and every panic with its location, capped at
//! [`MAX_LOG_BYTES`] with one previous file kept as `daemon.log.1`. Each daemon
//! run also leaves a marker in `daemon-runs/` that only a clean exit removes. The
//! next daemon to start finds a marker whose process is gone, records why in
//! `last-daemon-exit.json` for the app, and shows one line in `toolport_status`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::child_ledger::ProcessId;

pub const LOG_FILE: &str = "daemon.log";
/// Where the next daemon records an unclean end it found, for the app to read.
pub const LAST_EXIT_FILE: &str = "last-daemon-exit.json";
const RUNS_DIR: &str = "daemon-runs";
/// One megabyte of panics and stderr is weeks of a quiet daemon and enough
/// context for a noisy one; the previous file doubles it.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE)
}

/// Move a full log aside, replacing the previous one.
fn rotate_if_full(dir: &Path) {
    let path = log_path(dir);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, dir.join(format!("{LOG_FILE}.1")));
    }
}

fn open_for_append(dir: &Path) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(dir);
    rotate_if_full(dir);
    crate::registry::open_append_private(&log_path(dir)).ok()
}

/// The file a newly spawned daemon writes its stderr to. Rotated before each
/// start, so the cap holds across runs.
pub fn stderr_file(dir: &Path) -> Option<std::fs::File> {
    open_for_append(dir)
}

/// Append one line stamped with the time and this process id.
pub fn append(dir: &Path, line: &str) {
    if let Some(mut file) = open_for_append(dir) {
        let _ = writeln!(file, "{} pid={} {line}", now_ms(), std::process::id());
    }
}

/// Record every panic, on any thread, with its message and location, then run
/// the previous hook so stderr still gets the usual report.
pub fn install_panic_hook(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-text panic payload".to_string());
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
            .unwrap_or_else(|| "an unknown location".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        append(
            &dir,
            &format!(
                "panic in thread '{thread}' at {location}: {}",
                message.replace('\n', " ")
            ),
        );
        previous(info);
    }));
}

/// How a previous daemon run ended, as `last-daemon-exit.json` stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousExit {
    pub pid: u32,
    pub started_at_ms: u128,
    pub clean_shutdown: bool,
    /// The last panic that run recorded, or why none explains it.
    pub reason: String,
    pub detected_at_ms: u128,
    pub detected_by_pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMarker {
    process: ProcessId,
    started_at_ms: u128,
}

fn marker_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(RUNS_DIR).join(format!("{pid}.json"))
}

/// The one-line note this daemon shows in `toolport_status`, if the run before
/// it ended badly.
static PREVIOUS_EXIT_NOTE: Mutex<Option<String>> = Mutex::new(None);

pub fn previous_exit_note() -> Option<String> {
    PREVIOUS_EXIT_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The last panic `pid` recorded in the current or previous log.
fn last_panic_of(dir: &Path, pid: u32) -> Option<String> {
    let needle = format!(" pid={pid} panic ");
    [dir.join(format!("{LOG_FILE}.1")), log_path(dir)]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|log| {
            log.lines()
                .filter(|line| line.contains(&needle))
                .map(|line| {
                    line.split_once(&needle)
                        .map_or(line, |(_, rest)| rest)
                        .to_string()
                })
                .collect::<Vec<_>>()
        })
        .last()
        .map(|panic| format!("panic {panic}"))
}

/// Start this daemon's run: find runs that ended without a clean exit, record
/// the latest for the app and `toolport_status`, and leave our own marker.
/// Returns the unclean end it found, if any.
pub fn begin_run(dir: &Path) -> Option<PreviousExit> {
    let me = std::process::id();
    let mut found: Option<PreviousExit> = None;
    if let Ok(entries) = std::fs::read_dir(dir.join(RUNS_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(marker) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<RunMarker>(&raw).ok())
            else {
                continue;
            };
            if marker.process.pid == me || marker.process.is_alive() {
                continue;
            }
            let _ = std::fs::remove_file(&path);
            if found
                .as_ref()
                .is_some_and(|newer| newer.started_at_ms >= marker.started_at_ms)
            {
                continue;
            }
            let reason = last_panic_of(dir, marker.process.pid).unwrap_or_else(|| {
                "no panic was recorded, so it was most likely killed (for example by a signal, \
                 the OOM killer or a logout)"
                    .to_string()
            });
            found = Some(PreviousExit {
                pid: marker.process.pid,
                started_at_ms: marker.started_at_ms,
                clean_shutdown: false,
                reason,
                detected_at_ms: now_ms(),
                detected_by_pid: me,
            });
        }
    }
    if let Some(exit) = &found {
        if let Ok(raw) = serde_json::to_string_pretty(exit) {
            let _ = crate::registry::atomic_write(&dir.join(LAST_EXIT_FILE), &raw);
        }
        append(
            dir,
            &format!(
                "previous daemon pid={} ended without a clean shutdown: {}",
                exit.pid, exit.reason
            ),
        );
        *PREVIOUS_EXIT_NOTE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!(
            "The previous host daemon (pid {}) stopped without a clean shutdown: {}. \
             Details are in {LOG_FILE} in the data directory.",
            exit.pid, exit.reason
        ));
    }
    if let Some(process) = ProcessId::of(me) {
        let marker = RunMarker {
            process,
            started_at_ms: now_ms(),
        };
        let path = marker_path(dir, me);
        let _ = std::fs::create_dir_all(dir.join(RUNS_DIR));
        if let Ok(raw) = serde_json::to_string(&marker) {
            let _ = crate::registry::atomic_write(&path, &raw);
        }
    }
    append(dir, &format!("daemon start {}", env!("CARGO_PKG_VERSION")));
    found
}

/// Mark this run as ended cleanly. Call just before a deliberate exit.
pub fn end_run_cleanly(dir: &Path) {
    append(dir, "daemon clean exit");
    let _ = std::fs::remove_file(marker_path(dir, std::process::id()));
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-daemon-log-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_dead_run_without_a_clean_exit_is_reported_with_its_panic() {
        let dir = scratch("unclean");
        let me = ProcessId::of(std::process::id()).unwrap();
        // A run whose pid is now someone else: same pid, another start time.
        let dead = ProcessId {
            pid: me.pid + 1_000_000,
            start: 1,
        };
        std::fs::create_dir_all(dir.join(RUNS_DIR)).unwrap();
        std::fs::write(
            marker_path(&dir, dead.pid),
            serde_json::to_string(&RunMarker {
                process: dead,
                started_at_ms: 5,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            log_path(&dir),
            format!(
                "1 pid={} panic in thread 'router' at src/router.rs:1:2: boom\n",
                dead.pid
            ),
        )
        .unwrap();

        let exit = begin_run(&dir).expect("the unclean run is found");
        assert_eq!(exit.pid, dead.pid);
        assert!(!exit.clean_shutdown);
        assert!(
            exit.reason.contains("src/router.rs:1:2: boom"),
            "{}",
            exit.reason
        );
        let stored: PreviousExit =
            serde_json::from_str(&std::fs::read_to_string(dir.join(LAST_EXIT_FILE)).unwrap())
                .unwrap();
        assert_eq!(stored, exit);
        assert!(previous_exit_note().unwrap().contains("boom"));
        assert!(!marker_path(&dir, dead.pid).exists());
        assert!(marker_path(&dir, me.pid).exists());

        // A clean exit leaves nothing for the next start to report.
        end_run_cleanly(&dir);
        assert!(!marker_path(&dir, me.pid).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_rotates_and_keeps_one_previous_file() {
        let dir = scratch("rotate");
        std::fs::write(log_path(&dir), vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        append(&dir, "after the cap");
        let current = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert!(current.contains("after the cap"));
        assert!(current.len() < 1024);
        assert_eq!(
            std::fs::metadata(dir.join(format!("{LOG_FILE}.1")))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
