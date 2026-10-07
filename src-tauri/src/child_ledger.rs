//! Which downstream servers this gateway started, so the next gateway can stop
//! the ones a killed or crashed gateway left behind.
//!
//! Each gateway process that binds a data directory keeps one small file,
//! `children/<pid>.json`, naming itself and every stdio child it spawned, each
//! with its process start time. An entry leaves when its child is reaped. On
//! start, a gateway reads the files of owners that are gone and kills each
//! recorded child's process group, but only after proving the group is the one
//! it recorded: the leader still has its recorded start time, or, once the
//! leader is gone, every process left in the group carries the random tag the
//! child was started with in its environment. A group that cannot be proven is
//! never signalled.
//!
//! Unix only. On Windows every stdio child already runs in a Job Object that
//! the OS closes, killing the tree, when the gateway process ends however it
//! ends. Nothing here runs until a gateway calls [`bind_data_dir`], so unit
//! tests that spawn servers never write or reap anything.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One process, named by pid plus start time so a recycled pid cannot pass for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessId {
    pub pid: u32,
    /// Platform start time: clock ticks since boot on Linux, microseconds since
    /// the epoch on macOS. Only ever compared with itself.
    pub start: u64,
}

impl ProcessId {
    /// The live process at `pid`, if this platform can name it.
    pub fn of(pid: u32) -> Option<Self> {
        process_start(pid).map(|start| Self { pid, start })
    }

    /// Whether `pid` still names this exact process.
    pub fn is_alive(&self) -> bool {
        process_start(self.pid) == Some(self.start)
    }
}

/// The environment variable that tags a spawned server and its descendants.
pub const TAG_VAR: &str = "TOOLPORT_LEDGER_ID";

/// A fresh random tag for one spawned server.
pub fn new_tag() -> String {
    let mut bytes = [0u8; 12];
    let _ = getrandom::getrandom(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One spawned server. It leads its own process group (`process_group(0)`),
/// so its pid is also the group id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChildEntry {
    #[serde(flatten)]
    process: ProcessId,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerFile {
    owner: ProcessId,
    children: Vec<ChildEntry>,
}

struct Ledger {
    path: PathBuf,
    file: LedgerFile,
}

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);

fn ledger_dir(dir: &Path) -> PathBuf {
    dir.join("children")
}

/// Start recording this process's children under `dir`.
pub fn bind_data_dir(dir: &Path) {
    let Some(owner) = ProcessId::of(std::process::id()) else {
        return;
    };
    let path = ledger_dir(dir).join(format!("{}.json", owner.pid));
    *LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ledger {
        path,
        file: LedgerFile {
            owner,
            children: Vec::new(),
        },
    });
}

fn save(ledger: &Ledger) {
    if ledger.file.children.is_empty() {
        let _ = std::fs::remove_file(&ledger.path);
        return;
    }
    if let Some(parent) = ledger.path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string(&ledger.file) {
        let _ = crate::registry::atomic_write(&ledger.path, &raw);
    }
}

/// Record a child just spawned with `tag` in its environment. Call while the
/// child is unreaped, so its pid cannot have been recycled yet.
pub fn record(pid: u32, tag: &str) {
    let mut guard = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ledger) = guard.as_mut() else {
        return;
    };
    let Some(child) = ProcessId::of(pid) else {
        return;
    };
    ledger
        .file
        .children
        .retain(|entry| entry.process.pid != pid);
    ledger.file.children.push(ChildEntry {
        process: child,
        tag: Some(tag.to_string()),
    });
    save(ledger);
}

/// Drop a child that has been reaped.
pub fn forget(pid: u32) {
    let mut guard = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(ledger) = guard.as_mut() else {
        return;
    };
    let before = ledger.file.children.len();
    ledger
        .file
        .children
        .retain(|entry| entry.process.pid != pid);
    if ledger.file.children.len() != before {
        save(ledger);
    }
}

/// Kill the process groups that gateways which are gone left running under
/// `dir`, and remove their ledgers. Returns how many groups were signalled.
pub fn reap_orphans(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(ledger_dir(dir)) else {
        return 0;
    };
    let me = std::process::id();
    let mut signalled = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(file) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<LedgerFile>(&raw).ok())
        else {
            continue;
        };
        if file.owner.pid == me || file.owner.is_alive() {
            continue;
        }
        for child in &file.children {
            if kill_orphan_group(child) {
                signalled += 1;
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    signalled
}

/// SIGKILL a dead gateway's child group, only when the group is provably the
/// one that gateway started: its leader still has the recorded start time, or,
/// on Linux, the leader is gone and every process left in the group carries
/// the child's tag. A recycled pid can lead a stranger's group, but it cannot
/// carry a random tag it was never given.
#[cfg(unix)]
fn kill_orphan_group(child: &ChildEntry) -> bool {
    let pgid = child.process.pid as i32;
    // SAFETY: plain libc calls on integer ids.
    let leader_matches = child.process.is_alive() && unsafe { libc::getpgid(pgid) } == pgid;
    let provable = leader_matches
        || child.tag.as_deref().is_some_and(|tag| {
            process_start(child.process.pid).is_none() && group_carries_tag(pgid, tag)
        });
    // SAFETY: as above; the group was proven to be the recorded one.
    provable && unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0
}

#[cfg(not(unix))]
fn kill_orphan_group(_child: &ChildEntry) -> bool {
    false
}

/// Whether the group has members and every one of them has `TAG_VAR=tag` in
/// its environment. A member whose environment cannot be read fails the test.
#[cfg(target_os = "linux")]
fn group_carries_tag(pgid: i32, tag: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let wanted = format!("{TAG_VAR}={tag}");
    let mut members = 0;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if linux_stat(pid).is_none_or(|stat| stat.pgrp != pgid) {
            continue;
        }
        let tagged = std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
            environ
                .split(|byte| *byte == 0)
                .any(|entry| entry == wanted.as_bytes())
        });
        if !tagged {
            return false;
        }
        members += 1;
    }
    members > 0
}

/// Elsewhere the group cannot be listed cheaply, so an orphan whose leader is
/// gone is left alone.
#[cfg(all(unix, not(target_os = "linux")))]
fn group_carries_tag(_pgid: i32, _tag: &str) -> bool {
    false
}

#[cfg(target_os = "linux")]
struct LinuxStat {
    pgrp: i32,
    start: u64,
}

/// Fields of `/proc/<pid>/stat` after the parenthesized command name, which
/// may itself contain spaces and parentheses.
#[cfg(target_os = "linux")]
fn linux_stat(pid: u32) -> Option<LinuxStat> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_stat(&raw)
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(raw: &str) -> Option<LinuxStat> {
    let rest = &raw[raw.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state(3) ppid(4) pgrp(5) ... starttime(22), 1-based.
    Some(LinuxStat {
        pgrp: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "linux")]
pub fn process_start(pid: u32) -> Option<u64> {
    linux_stat(pid).map(|stat| stat.start)
}

#[cfg(target_os = "macos")]
pub fn process_start(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is a correctly sized, writable proc_bsdinfo.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (written == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// No way to prove identity here, so nothing is ever recorded or signalled.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_start(_pid: u32) -> Option<u64> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_parsing_survives_odd_command_names() {
        let raw = "4242 (a) b) (c) S 1 4242 4242 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
        let stat = parse_linux_stat(raw).unwrap();
        assert_eq!(stat.pgrp, 4242);
        assert_eq!(stat.start, 987654);
    }

    #[test]
    fn this_process_is_named_and_alive() {
        let me = ProcessId::of(std::process::id()).expect("own start time");
        assert!(me.is_alive());
        let recycled = ProcessId {
            pid: me.pid,
            start: me.start + 1,
        };
        assert!(
            !recycled.is_alive(),
            "a different start time is a different process"
        );
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-ledger-{}-{}",
            std::process::id(),
            new_tag()
        ));
        std::fs::create_dir_all(ledger_dir(&dir)).unwrap();
        dir
    }

    fn write_ledger(dir: &Path, name: &str, owner: ProcessId, child: ChildEntry) {
        std::fs::write(
            ledger_dir(dir).join(format!("{name}.json")),
            serde_json::to_string(&LedgerFile {
                owner,
                children: vec![child],
            })
            .unwrap(),
        )
        .unwrap();
    }

    fn entry(pid: u32, tag: Option<&str>) -> ChildEntry {
        ChildEntry {
            process: ProcessId::of(pid).unwrap(),
            tag: tag.map(str::to_string),
        }
    }

    #[test]
    fn a_dead_owners_group_is_killed_and_a_live_owners_is_not() {
        use std::os::unix::process::CommandExt;
        let dir = scratch();
        let spawn = || {
            std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap()
        };
        let mut orphan = spawn();
        let mut kept = spawn();
        // A live owner that is not this process, and a gone one: the same pid
        // with a start time it never had.
        let live = ProcessId::of(kept.id()).unwrap();
        let gone = ProcessId {
            pid: live.pid,
            start: live.start + 1,
        };
        write_ledger(&dir, "gone", gone, entry(orphan.id(), None));
        write_ledger(&dir, "live", live, entry(kept.id(), None));
        // A recorded child whose pid now names another process is never signalled.
        write_ledger(
            &dir,
            "stale",
            gone,
            ChildEntry {
                process: ProcessId {
                    pid: kept.id(),
                    start: live.start + 1,
                },
                tag: None,
            },
        );

        assert_eq!(reap_orphans(&dir), 1);
        assert!(
            !orphan.wait().unwrap().success(),
            "the orphan's group was killed"
        );
        assert!(
            kept.try_wait().unwrap().is_none(),
            "a live owner's child is untouched"
        );
        assert!(!ledger_dir(&dir).join("gone.json").exists());
        assert!(ledger_dir(&dir).join("live.json").exists());
        let _ = kept.kill();
        let _ = kept.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launcher that died leaves its server in the group. Only a group whose
    /// every member carries the recorded tag is killed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_leaderless_group_is_killed_only_when_every_member_carries_the_tag() {
        use std::os::unix::process::CommandExt;
        let dir = scratch();
        // An owner no process can be: far above any real pid_max.
        let gone = ProcessId {
            pid: 999_999_999,
            start: 1,
        };
        // A shell that starts a grandchild in its own group, reports its pid,
        // then is killed, leaving the grandchild leaderless.
        let leaderless = |tag: &str| {
            let mut shell = std::process::Command::new("sh")
                .args(["-c", "sleep 30 & echo $!; wait"])
                .env(TAG_VAR, tag)
                .process_group(0)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let recorded = entry(shell.id(), Some("recorded-tag"));
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(shell.stdout.take().unwrap()),
                &mut line,
            )
            .unwrap();
            let grandchild: u32 = line.trim().parse().unwrap();
            // `$!` is known right after fork. Until the grandchild has exec'd
            // `sleep`, its environment can read back empty, which the reaper
            // rightly treats as unproven. Wait for the exec under load.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !std::fs::read(format!("/proc/{grandchild}/cmdline"))
                .is_ok_and(|cmdline| cmdline.starts_with(b"sleep"))
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the grandchild never exec'd sleep"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = shell.kill();
            let _ = shell.wait();
            (recorded, ProcessId::of(grandchild).unwrap())
        };
        let (ours, our_grandchild) = leaderless("recorded-tag");
        let (stranger, stranger_grandchild) = leaderless("some-other-tag");
        write_ledger(&dir, "ours", gone, ours);
        // The stranger's group is recorded with our tag, as if its leader's pid
        // had been recycled: the member's environment does not match.
        write_ledger(&dir, "stranger", gone, stranger);

        assert_eq!(reap_orphans(&dir), 1);
        // Gone, or a zombie waiting for init to reap it.
        let running = |id: ProcessId| {
            id.is_alive()
                && !std::fs::read_to_string(format!("/proc/{}/stat", id.pid))
                    .unwrap_or_default()
                    .contains(") Z ")
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while running(our_grandchild) {
            assert!(
                std::time::Instant::now() < deadline,
                "the tagged group survived"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            stranger_grandchild.is_alive(),
            "an untagged group is left alone"
        );
        // SAFETY: the stranger grandchild is this test's own, proven by start time.
        unsafe { libc::kill(stranger_grandchild.pid as i32, libc::SIGKILL) };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
