//! Best-effort local process attribution, never an authorization principal.
//! Only basenames survive the bounded parent walk. No argv, environment or cwd reads.

use std::collections::HashSet;
use std::path::Path;

const MAX_PARENTS: usize = 8;

pub fn parent_app(pid: u32) -> Option<String> {
    walk(pid, process)
}

fn walk(mut pid: u32, mut read: impl FnMut(u32) -> Option<(u32, String)>) -> Option<String> {
    let mut seen = HashSet::new();
    let mut fallback = None;
    for _ in 0..MAX_PARENTS {
        if pid <= 1 || !seen.insert(pid) {
            break;
        }
        let Some((parent, name)) = read(pid) else {
            break;
        };
        // Skip the adapter itself; examine its parents, including generic launchers.
        if seen.len() > 1 {
            let name = Path::new(&name).file_name()?.to_str()?;
            let name = crate::approval::sanitize_client_label(name)?;
            let name = crate::approval::shorten_client_label(&name, 48);
            let stem = name.trim_end_matches(".exe").to_ascii_lowercase();
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
        pid = parent;
    }
    fallback
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> Option<(u32, String)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()?;
    let name = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|n| n.trim().to_string())
        })?;
    Some((parent, name))
}

#[cfg(target_os = "macos")]
fn process(pid: u32) -> Option<(u32, String)> {
    // sysctl KERN_PROC_PID supplies only the parent id. proc_pidpath supplies
    // the executable path, immediately reduced to its basename.
    let mut info: libc::kinfo_proc = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&info);
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as i32,
    ];
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            (&mut info as *mut libc::kinfo_proc).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 || size != std::mem::size_of_val(&info) {
        return None;
    }
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, size: u32) -> i32;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = std::ffi::CStr::from_bytes_until_nul(&buf)
        .ok()?
        .to_str()
        .ok()?;
    Some((
        info.kp_eproc.e_ppid as u32,
        Path::new(path).file_name()?.to_str()?.to_string(),
    ))
}

#[cfg(windows)]
fn process(pid: u32) -> Option<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
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
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process(_: u32) -> Option<(u32, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn walks_launchers_and_retains_only_a_sanitized_basename() {
        let rows = [
            (10, (9, "toolport-gateway")),
            (9, (8, "node")),
            (8, (1, "/private/app/Cursor\u{202e}")),
        ];
        assert_eq!(
            walk(10, |pid| rows
                .iter()
                .find(|r| r.0 == pid)
                .map(|r| (r.1 .0, r.1 .1.into())))
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
                Some((pid - 1, "node".into()))
            })
            .as_deref(),
            Some("node")
        );
        assert_eq!(reads, MAX_PARENTS);
        assert_eq!(walk(10, |_| None), None);
        assert_eq!(walk(10, |pid| Some((pid, "node".into()))), None);
    }
}
