//! Best-effort local process attribution, never an authorization principal.
//! Only basenames survive the bounded parent walk. No argv, environment or cwd reads.

use std::collections::HashSet;
use std::path::Path;

const MAX_PARENTS: usize = 8;

#[derive(Default)]
pub struct ParentApp(std::sync::OnceLock<Option<String>>);
impl ParentApp {
    pub fn resolve(&self, pid: u32) -> Option<String> {
        self.resolve_with(|| parent_app(pid))
    }
    fn resolve_with(&self, read: impl FnOnce() -> Option<String>) -> Option<String> {
        self.0.get_or_init(read).clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Generation {
    parent: u32,
    started: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn stable_process(
    mut generation: impl FnMut() -> Option<Generation>,
    name: impl FnOnce() -> Option<String>,
) -> Option<(Generation, String)> {
    let before = generation()?;
    let name = name()?;
    (generation()? == before).then_some((before, name))
}

pub fn parent_app(pid: u32) -> Option<String> {
    walk(pid, process)
}

fn walk(mut pid: u32, mut read: impl FnMut(u32) -> Option<(Generation, String)>) -> Option<String> {
    let mut seen = HashSet::new();
    let mut fallback = None;
    let mut child_started = None;
    for _ in 0..MAX_PARENTS {
        if pid <= 1 || !seen.insert(pid) {
            break;
        }
        let (generation, name) = read(pid)?;
        // A terminated parent's PID can be reused by a newer unrelated process.
        if child_started.is_some_and(|started| generation.started > started) {
            return None;
        }
        child_started = Some(generation.started);
        // Skip the adapter itself; examine its parents, including generic launchers.
        if seen.len() > 1 {
            let name = Path::new(&name).file_name()?.to_str()?;
            let name = crate::approval::sanitize_client_label(name)?;
            let name = crate::approval::shorten_client_label(&name, 48);
            let stem = name
                .to_ascii_lowercase()
                .trim_end_matches(".exe")
                .to_string();
            if !matches!(
                stem.as_str(),
                "node"
                    | "nodejs"
                    | "bash"
                    | "sh"
                    | "zsh"
                    | "fish"
                    | "env"
                    | "cmd"
                    | "powershell"
                    | "pwsh"
                    | "python"
                    | "python3"
                    | "toolport-gateway"
                    | "conduit-gateway"
            ) {
                return Some(name);
            }
            fallback.get_or_insert(name);
        }
        pid = generation.parent;
    }
    fallback
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> Option<(Generation, String)> {
    stable_process(
        || linux_generation(pid),
        || {
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .or_else(|| {
                    std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .ok()
                        .map(|n| n.trim().to_string())
                })
        },
    )
}

#[cfg(target_os = "linux")]
fn linux_generation(pid: u32) -> Option<Generation> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm can contain spaces and parentheses. Fields after its last ')' start at state.
    let fields: Vec<_> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    Some(Generation {
        parent: fields.get(1)?.parse().ok()?,
        started: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "macos")]
fn process(pid: u32) -> Option<(Generation, String)> {
    stable_process(
        || macos_generation(pid),
        || {
            let mut buf = [0u8; 4096];
            let n = unsafe {
                libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32)
            };
            if n <= 0 {
                return None;
            }
            let path = std::ffi::CStr::from_bytes_until_nul(&buf)
                .ok()?
                .to_str()
                .ok()?;
            Some(Path::new(path).file_name()?.to_str()?.to_string())
        },
    )
}

#[cfg(target_os = "macos")]
fn macos_generation(pid: u32) -> Option<Generation> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some(Generation {
        parent: info.pbi_ppid,
        started: info
            .pbi_start_tvsec
            .checked_mul(1_000_000)?
            .checked_add(info.pbi_start_tvusec)?,
    })
}

#[cfg(windows)]
fn process(pid: u32) -> Option<(Generation, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let before = windows_generation(pid)?;
    let found = unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == pid {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found = Some((
                        entry.th32ParentProcessID,
                        String::from_utf16_lossy(&entry.szExeFile[..len]),
                    ));
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }?;
    let after = windows_generation(pid)?;
    (before == after).then_some((
        Generation {
            parent: found.0,
            started: before,
        },
        found.1,
    ))
}

#[cfg(windows)]
fn windows_generation(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(process);
        (ok != 0).then_some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process(_: u32) -> Option<(Generation, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_parent_walk_per_lifetime_including_failures() {
        for result in [Some("Cursor".to_string()), None] {
            let cached = ParentApp::default();
            let mut walks = 0;
            for _ in 0..20 {
                assert_eq!(
                    cached.resolve_with(|| {
                        walks += 1;
                        result.clone()
                    }),
                    result
                );
            }
            assert_eq!(walks, 1);
        }
    }

    #[test]
    fn reused_process_generation_returns_unknown() {
        let mut reads = 0;
        assert_eq!(
            stable_process(
                || {
                    reads += 1;
                    Some(Generation {
                        parent: 9,
                        started: reads,
                    })
                },
                || Some("Cursor".into())
            ),
            None
        );
        assert_eq!(reads, 2);
    }

    #[test]
    fn missing_or_reused_parent_discards_launcher_fallback() {
        assert_eq!(
            walk(10, |pid| match pid {
                10 => Some((
                    Generation {
                        parent: 9,
                        started: 100
                    },
                    "toolport-gateway".into()
                )),
                9 => Some((
                    Generation {
                        parent: 8,
                        started: 90
                    },
                    "node".into()
                )),
                _ => None,
            }),
            None
        );
    }

    #[test]
    fn newer_parent_returns_unknown_even_after_a_launcher() {
        for launcher in [false, true] {
            assert_eq!(
                walk(10, |pid| match pid {
                    10 => Some((
                        Generation {
                            parent: 9,
                            started: 100
                        },
                        "toolport-gateway".into()
                    )),
                    9 if launcher => Some((
                        Generation {
                            parent: 8,
                            started: 100
                        },
                        "node".into()
                    )),
                    _ => Some((
                        Generation {
                            parent: 1,
                            started: 200
                        },
                        "UnrelatedApp".into()
                    )),
                }),
                None
            );
        }
        for parent_started in [90, 100] {
            assert_eq!(
                walk(10, |pid| Some((
                    Generation {
                        parent: if pid == 10 { 9 } else { 1 },
                        started: if pid == 10 { 100 } else { parent_started },
                    },
                    "Cursor".into()
                )))
                .as_deref(),
                Some("Cursor")
            );
        }
    }

    #[test]
    fn walks_launchers_and_retains_only_a_sanitized_basename() {
        let rows = [
            (10, (9, "toolport-gateway")),
            (9, (8, "node")),
            (8, (1, "/private/app/Cursor\u{202e}")),
        ];
        assert_eq!(
            walk(10, |pid| rows.iter().find(|r| r.0 == pid).map(|r| (
                Generation {
                    parent: r.1 .0,
                    started: r.0 as u64
                },
                r.1 .1.into()
            )))
            .as_deref(),
            Some("Cursor")
        );
    }
    #[test]
    fn bounded_missing_and_cyclic_chains_are_best_effort() {
        let mut reads = 0;
        assert_eq!(
            walk(100, |pid| {
                reads += 1;
                Some((
                    Generation {
                        parent: pid - 1,
                        started: pid as u64,
                    },
                    "node".into(),
                ))
            })
            .as_deref(),
            Some("node")
        );
        assert_eq!(reads, MAX_PARENTS);
        assert_eq!(walk(10, |_| None), None);
        assert_eq!(
            walk(10, |pid| Some((
                Generation {
                    parent: pid,
                    started: 100
                },
                "node".into()
            ))),
            None
        );
    }
}
