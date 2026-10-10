//! Best-effort local process attribution, never an authorization principal.
//! Only safe names survive the bounded parent walk. Interpreter arguments are
//! inspected with a fixed cap; no arguments, environment or cwd are retained.

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
            let name = crate::session_observability::display_label(name)?;
            let name = crate::approval::shorten_client_label(&name, 48);
            let stem = name
                .to_ascii_lowercase()
                .trim_end_matches(".exe")
                .to_string();
            if !is_interpreter(&stem)
                && !matches!(stem.as_str(), "toolport-gateway" | "conduit-gateway")
            {
                return Some(name);
            }
            if !is_interpreter(&stem) {
                fallback.get_or_insert(name);
            }
        }
        pid = generation.parent;
    }
    fallback
}

fn is_interpreter(name: &str) -> bool {
    let stem = name.trim_end_matches(".exe").to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "bash"
            | "sh"
            | "dash"
            | "zsh"
            | "fish"
            | "env"
            | "npx"
            | "uv"
            | "uvx"
            | "cmd"
            | "powershell"
            | "pwsh"
    ) || stem
        .strip_prefix("pythonw")
        .or_else(|| stem.strip_prefix("python"))
        .is_some_and(|suffix| {
            suffix
                .trim_end_matches('t')
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
        })
}

fn command_basename(arg: &str) -> Option<String> {
    // Options, launch verbs and inline programs are not script names.
    if arg.is_empty() || arg.starts_with('-') || matches!(arg, "run" | "exec" | "x") {
        return None;
    }
    let basename = arg.rsplit(['/', '\\']).next()?;
    let stem = basename.rsplit_once('.').map_or(basename, |(stem, _)| stem);
    if matches!(stem, "index" | "main" | "cli" | "server" | "__main__") {
        let mut parents = arg.rsplit(['/', '\\']).skip(1).take(3);
        let parent = parents
            .find(|part| !matches!(*part, "" | "." | ".." | "bin" | "src" | "dist" | "lib"))?;
        let safe = crate::session_observability::display_label(parent)?;
        return (safe == parent && !is_interpreter(parent)).then_some(safe);
    }
    crate::session_observability::display_label(basename)
        .map(|name| crate::approval::shorten_client_label(&name, 48))
}

fn interpreter_script<'a>(name: &str, mut args: impl Iterator<Item = &'a str>) -> Option<String> {
    let stem = name.trim_end_matches(".exe").to_ascii_lowercase();
    let shell = matches!(
        stem.as_str(),
        "sh" | "bash" | "dash" | "zsh" | "fish" | "cmd" | "pwsh" | "powershell"
    );
    for arg in args.by_ref().take(32) {
        // Do not mistake an inline command or a module/option value for a script.
        if shell
            && (arg.eq_ignore_ascii_case("/c")
                || arg.eq_ignore_ascii_case("/k")
                || arg.eq_ignore_ascii_case("-command")
                || arg.eq_ignore_ascii_case("-encodedcommand")
                || (arg.starts_with('-') && arg.trim_start_matches('-').contains('c')))
            || (stem.starts_with("python") && matches!(arg, "-m" | "-c"))
            || matches!(arg, "-e" | "--eval" | "-p" | "--print")
        {
            return None;
        }
        if arg.starts_with('-') || matches!(arg, "run" | "exec" | "x") || is_interpreter(arg) {
            continue;
        }
        return command_basename(arg);
    }
    None
}

#[cfg(target_os = "linux")]
fn interpreter_command(pid: u32, name: &str) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(format!("/proc/{pid}/cmdline")).ok()?;
    let mut buf = Vec::new();
    file.take(4096).read_to_end(&mut buf).ok()?;
    // Ignore the final fragment when the cap cuts through an argument.
    let end = buf.iter().rposition(|b| *b == 0)?;
    let argv = std::str::from_utf8(&buf[..end]).ok()?;
    interpreter_script(name, argv.split('\0').skip(1))
}

#[cfg(target_os = "macos")]
fn interpreter_command(pid: u32, name: &str) -> Option<String> {
    // Probe the complete argument area, including the leading argc integer.
    // An undersized buffer can return its tail (environment strings).
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size = 0usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < std::mem::size_of::<libc::c_int>()
    {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let argv = procargs_argv(buf.get(..size)?)?;
    interpreter_script(name, argv.into_iter().skip(1))
}

#[cfg(any(target_os = "macos", test))]
fn procargs_argv(buf: &[u8]) -> Option<Vec<&str>> {
    let int_size = std::mem::size_of::<i32>();
    let argc = i32::from_ne_bytes(buf.get(..int_size)?.try_into().ok()?);
    if argc < 2 {
        return None;
    }
    let mut args = buf.get(int_size..)?;
    let path_end = args.iter().position(|b| *b == 0)?;
    args = args.get(path_end..)?;
    args = args.get(args.iter().position(|b| *b != 0)?..)?;
    if argc as usize > args.len() {
        return None;
    }
    let mut argv = Vec::new();
    for _ in 0..argc {
        let end = args.iter().position(|b| *b == 0)?;
        argv.push(std::str::from_utf8(&args[..end]).ok()?);
        args = &args[end + 1..];
    }
    Some(argv)
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> Option<(Generation, String)> {
    stable_process(
        || linux_generation(pid),
        || {
            let name = std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .or_else(|| {
                    std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .ok()
                        .map(|n| n.trim().to_string())
                })?;
            if is_interpreter(&name) {
                Some(interpreter_command(pid, &name).unwrap_or(name))
            } else {
                Some(name)
            }
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
            let name = Path::new(path).file_name()?.to_str()?.to_string();
            if is_interpreter(&name) {
                Some(interpreter_command(pid, &name).unwrap_or(name))
            } else {
                Some(name)
            }
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
    // Keep launchers in the chain so the walk can reach their owning app.
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
    fn interpreter_wrappers_keep_walking_and_scripts_get_safe_names() {
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            ("sh", &["-c", "secret inline command"], None),
            ("bash", &["-lc", "secret inline command"], None),
            ("zsh", &["-l", "-c", "secret inline command"], None),
            ("python", &["-m", "x"], None),
            ("node", &["--flag", "x.js"], Some("x.js")),
            ("npx", &["-y", "pkg"], Some("pkg")),
            ("cmd.exe", &["/c", "npx"], None),
            (
                "uv",
                &["run", "python", "/private/inbox.py"],
                Some("inbox.py"),
            ),
            ("bun", &["run", "/private/inbox.js"], Some("inbox.js")),
            (
                "deno",
                &["run", "--allow-read", "/private/inbox.ts"],
                Some("inbox.ts"),
            ),
            ("uv", &["exec", "x", "/private/inbox.py"], Some("inbox.py")),
            ("node", &["/private/pkg/index.js"], Some("pkg")),
            ("node", &["/private/pkg/bin/cli.js"], Some("pkg")),
            ("python", &["/private/pkg/main.py"], Some("pkg")),
            ("node", &["/private/pkg/server.js"], Some("pkg")),
            ("python", &["/private/pkg/__main__.py"], Some("pkg")),
            ("node", &["index.js"], None),
            (
                "node",
                &["/private/sk-live-abcdefghijk123456789/index.js"],
                None,
            ),
            ("node", &["/private/https:secret/cli.js"], None),
            ("node", &["--eval", "secret inline command"], None),
        ];
        for &(interpreter, args, expected) in cases {
            let script = interpreter_script(interpreter, args.iter().copied());
            assert_eq!(script.as_deref(), expected, "{interpreter} {args:?}");
            let name = script.unwrap_or_else(|| interpreter.into());
            assert_eq!(
                walk(10, |pid| Some((
                    Generation {
                        parent: if pid == 10 {
                            9
                        } else if pid == 9 {
                            8
                        } else {
                            1
                        },
                        started: pid as u64
                    },
                    if pid == 10 {
                        "toolport-gateway".into()
                    } else if pid == 9 {
                        name.clone()
                    } else {
                        "Cursor".into()
                    },
                )))
                .as_deref(),
                Some(expected.unwrap_or("Cursor")),
                "{interpreter} {args:?}"
            );
        }
    }

    #[test]
    fn macos_procargs_respects_argc_and_rejects_truncation() {
        fn buffer(argc: i32, argv: &[u8]) -> Vec<u8> {
            let mut buf = argc.to_ne_bytes().to_vec();
            buf.extend_from_slice(b"/usr/bin/python\0\0\0");
            buf.extend_from_slice(argv);
            buf
        }
        assert_eq!(procargs_argv(&buffer(1, b"python\0HOME=/Users/x\0")), None);
        assert_eq!(
            procargs_argv(&buffer(2, b"python\0/private/inbox.py\0HOME=/Users/x\0")),
            Some(vec!["python", "/private/inbox.py"])
        );
        assert_eq!(
            procargs_argv(&buffer(2, b"python\0/private/inbox.py")),
            None
        );
        assert_eq!(procargs_argv(&buffer(3, b"python\0inbox.py\0")), None);
        let mut large = buffer(2, b"python\0inbox.py\0HOME=");
        large.extend_from_slice(&[b'x'; 8192]);
        large.push(0);
        assert_eq!(procargs_argv(&large), Some(vec!["python", "inbox.py"]));
        assert_eq!(procargs_argv(&[2, 0]), None);
        assert_eq!(procargs_argv(&buffer(-1, b"python\0")), None);
    }

    #[test]
    fn interpreter_names_and_script_basename_are_private_and_bounded() {
        for name in [
            "python3.14",
            "python3.14t",
            "pythonw.exe",
            "python",
            "node",
            "bun",
            "deno",
            "bash",
            "sh",
            "zsh",
            "fish",
            "env",
            "npx",
            "uv",
            "uvx",
            "pwsh.exe",
        ] {
            assert!(is_interpreter(name), "{name}");
        }
        assert!(!is_interpreter("Cursor"));
        assert_eq!(
            command_basename("/private/customer/scripts/inbox"),
            Some("inbox".into())
        );
        assert_eq!(
            command_basename(r"C:\private\inbox.py"),
            Some("inbox.py".into())
        );
        for arg in ["-c", "--eval", ""] {
            assert_eq!(command_basename(arg), None);
        }
        assert_eq!(
            command_basename("/private/sk-live-abcdefghijk123456789"),
            Some("[redacted]".into())
        );
        assert_eq!(
            command_basename("/private/https:secret"),
            Some("[private]".into())
        );
    }

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
            None
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
