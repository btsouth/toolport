//! Launch-at-login command path.
//!
//! `tauri_plugin_autostart` registers `std::env::current_exe()`. Inside an
//! AppImage that path is the ephemeral FUSE mount (`/tmp/.mount_*`), which
//! disappears when the app exits, so the next login execs a path that no
//! longer exists.
//!
//! Client gateway install already special-cases `$APPIMAGE` (see `clients.rs`).
//! Autostart uses the same rule: when `$APPIMAGE` is set, the desktop Exec is
//! that persistent file. Non-AppImage Linux / macOS / Windows keep `current_exe`.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Args the desktop app expects on an auto-start (tray, no window flash).
pub const AUTOSTART_ARGS: &[&str] = &["--hidden"];

/// Path written into the launch-at-login command.
///
/// `$APPIMAGE` is the persistent file the user launched. `current_exe` inside
/// an AppImage is the FUSE mount and must not be registered.
pub fn resolve_autostart_app_path(appimage: Option<&OsStr>, current_exe: &Path) -> PathBuf {
    match appimage {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => current_exe.to_path_buf(),
    }
}

/// Resolve from the live process environment.
pub fn resolve_autostart_app_path_from_env() -> std::io::Result<PathBuf> {
    Ok(resolve_autostart_app_path(
        std::env::var_os("APPIMAGE").as_deref(),
        &std::env::current_exe()?,
    ))
}

/// True when `path` looks like an AppImageKit FUSE mount
/// (`{temp}/.mount_{prefix}{hash}/...`).
pub fn is_ephemeral_fuse_mount(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with(".mount_"))
    })
}

/// Quote one Exec= token per the desktop-entry spec (spaces, quotes).
pub fn quote_desktop_exec_arg(arg: &str) -> String {
    let safe = arg.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | ':' | '=' | '+' | '~')
    });
    if safe {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

pub fn linux_autostart_exec(app_path: &Path, args: &[&str]) -> String {
    std::iter::once(app_path.display().to_string())
        .chain(args.iter().map(|a| (*a).to_string()))
        .map(|part| quote_desktop_exec_arg(&part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Same shape as `auto-launch` 0.5 so we replace the plugin's file in place.
pub fn linux_desktop_entry(app_name: &str, app_path: &Path, args: &[&str]) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={app_name}\n\
         Comment={app_name}startup script\n\
         Exec={}\n\
         StartupNotify=false\n\
         Terminal=false\n",
        linux_autostart_exec(app_path, args)
    )
}

/// First Exec= command token (the binary), unquoted.
pub fn desktop_exec_command(contents: &str) -> Option<String> {
    let rest = contents
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))?;
    parse_first_exec_arg(rest)
}

fn parse_first_exec_arg(exec: &str) -> Option<String> {
    let exec = exec.trim();
    if exec.is_empty() {
        return None;
    }
    if let Some(rest) = exec.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                }
                '"' => break,
                _ => out.push(c),
            }
        }
        Some(out)
    } else {
        Some(exec.split_whitespace().next()?.to_string())
    }
}

pub fn rewrite_desktop_exec(contents: &str, app_path: &Path, args: &[&str]) -> String {
    let exec = format!("Exec={}", linux_autostart_exec(app_path, args));
    let mut found = false;
    let mut out = String::new();
    for line in contents.lines() {
        if !found && line.starts_with("Exec=") {
            out.push_str(&exec);
            out.push('\n');
            found = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&exec);
        out.push('\n');
    }
    out
}

/// Rewrite when the existing Exec is a FUSE mount, missing, or (if `$APPIMAGE`
/// is in play) not the persistent AppImage path.
pub fn should_repair_desktop_exec(
    current_exec: Option<&Path>,
    resolved: &Path,
    appimage_set: bool,
) -> bool {
    match current_exec {
        None => true,
        Some(p) if is_ephemeral_fuse_mount(p) => true,
        Some(p) if appimage_set && p != resolved => true,
        _ => false,
    }
}

pub fn write_linux_autostart(
    dest: &Path,
    app_name: &str,
    app_path: &Path,
    args: &[&str],
) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, linux_desktop_entry(app_name, app_path, args))
}

pub fn repair_linux_autostart_file(
    dest: &Path,
    app_path: &Path,
    args: &[&str],
    appimage_set: bool,
) -> std::io::Result<bool> {
    let contents = match std::fs::read_to_string(dest) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let current = desktop_exec_command(&contents).map(PathBuf::from);
    if !should_repair_desktop_exec(current.as_deref(), app_path, appimage_set) {
        return Ok(false);
    }
    std::fs::write(dest, rewrite_desktop_exec(&contents, app_path, args))?;
    Ok(true)
}

/// Directory `auto-launch` / `tauri-plugin-autostart` write on Linux.
pub fn linux_autostart_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".config").join("autostart"))
}

pub fn linux_autostart_file(app_name: &str) -> Option<PathBuf> {
    Some(linux_autostart_dir()?.join(format!("{app_name}.desktop")))
}

pub fn enable_linux(app_name: &str) -> Result<(), String> {
    let app_path = resolve_autostart_app_path_from_env().map_err(|e| e.to_string())?;
    let dest = linux_autostart_file(app_name).ok_or("Could not resolve the autostart directory")?;
    write_linux_autostart(&dest, app_name, &app_path, AUTOSTART_ARGS).map_err(|e| e.to_string())
}

pub fn disable_linux(app_name: &str) -> Result<(), String> {
    let dest = linux_autostart_file(app_name).ok_or("Could not resolve the autostart directory")?;
    match std::fs::remove_file(&dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn is_enabled_linux(app_name: &str) -> Result<bool, String> {
    let dest = linux_autostart_file(app_name).ok_or("Could not resolve the autostart directory")?;
    Ok(dest.is_file())
}

/// If launch-at-login is already on, rewrite a FUSE-mount Exec to `$APPIMAGE`.
pub fn repair_linux(app_name: &str) {
    let Ok(app_path) = resolve_autostart_app_path_from_env() else {
        return;
    };
    let Some(dest) = linux_autostart_file(app_name) else {
        return;
    };
    let appimage_set = std::env::var_os("APPIMAGE").is_some_and(|p| !p.is_empty());
    let _ = repair_linux_autostart_file(&dest, &app_path, AUTOSTART_ARGS, appimage_set);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-autostart-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appimage_autostart_uses_persistent_file_not_fuse_mount() {
        let appimage = PathBuf::from("/home/user/apps/Toolport.AppImage");
        let fuse = PathBuf::from("/tmp/.mount_ToolpoXXXX/usr/bin/conduit");
        let resolved = resolve_autostart_app_path(Some(appimage.as_os_str()), &fuse);
        assert_eq!(resolved, appimage);
        assert!(!is_ephemeral_fuse_mount(&resolved));
        assert!(is_ephemeral_fuse_mount(&fuse));
        let exec = linux_autostart_exec(&resolved, AUTOSTART_ARGS);
        assert!(
            exec.starts_with("/home/user/apps/Toolport.AppImage"),
            "Exec must be $APPIMAGE, got {exec}"
        );
        assert!(
            !exec.contains(".mount_"),
            "Exec must not mention the FUSE mount, got {exec}"
        );
    }

    #[test]
    fn without_appimage_autostart_uses_current_exe() {
        let exe = PathBuf::from("/usr/bin/toolport");
        assert_eq!(resolve_autostart_app_path(None, &exe), exe);
        assert_eq!(resolve_autostart_app_path(Some(OsStr::new("")), &exe), exe);
    }

    #[test]
    fn enabling_from_appimage_then_unmounting_still_points_at_existing_file() {
        let dir = unique_dir("unmount");
        let persistent = dir.join("Toolport.AppImage");
        std::fs::write(&persistent, b"elf").unwrap();
        let fuse = dir.join(".mount_ToolpoXXXX").join("usr/bin/conduit");
        // Deliberately do not create `fuse`: the mount is gone after exit.
        assert!(!fuse.exists());

        let dest = dir.join("autostart").join("Toolport.desktop");
        let path = resolve_autostart_app_path(Some(persistent.as_os_str()), &fuse);
        write_linux_autostart(&dest, "Toolport", &path, AUTOSTART_ARGS).unwrap();

        let contents = std::fs::read_to_string(&dest).unwrap();
        let exec = desktop_exec_command(&contents).expect("Exec=");
        assert_eq!(Path::new(&exec), persistent.as_path());
        assert!(
            Path::new(&exec).exists(),
            "autostart must still point at an existing file after unmount"
        );
        assert!(!is_ephemeral_fuse_mount(Path::new(&exec)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repair_rewrites_fuse_mount_exec_to_appimage() {
        let dir = unique_dir("repair");
        let persistent = dir.join("Toolport.AppImage");
        std::fs::write(&persistent, b"elf").unwrap();
        let dest = dir.join("Toolport.desktop");
        let stale = linux_desktop_entry(
            "Toolport",
            Path::new("/tmp/.mount_ToolpoDEAD/usr/bin/conduit"),
            AUTOSTART_ARGS,
        );
        std::fs::write(&dest, &stale).unwrap();

        assert!(repair_linux_autostart_file(&dest, &persistent, AUTOSTART_ARGS, true).unwrap());
        let exec = desktop_exec_command(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(Path::new(&exec), persistent.as_path());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repair_leaves_non_appimage_current_exe_alone() {
        let dir = unique_dir("leave");
        let dest = dir.join("Toolport.desktop");
        let exe = Path::new("/usr/bin/toolport");
        write_linux_autostart(&dest, "Toolport", exe, AUTOSTART_ARGS).unwrap();
        assert!(!repair_linux_autostart_file(&dest, exe, AUTOSTART_ARGS, false).unwrap());
        let exec = desktop_exec_command(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(exec, "/usr/bin/toolport");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quoted_appimage_paths_round_trip() {
        let path = PathBuf::from("/home/user/My Apps/Toolport 1.13.AppImage");
        let entry = linux_desktop_entry("Toolport", &path, AUTOSTART_ARGS);
        assert_eq!(
            desktop_exec_command(&entry).as_deref(),
            Some(path.to_str().unwrap())
        );
    }
}
