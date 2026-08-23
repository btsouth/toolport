//! Hand host binaries the host's environment, not the AppImage's.
//!
//! An AppImage launches through `AppRun`, which exports the bundle's own
//! library and plugin paths into our process so our bundled payload can find
//! them:
//!
//! ```text
//! LD_LIBRARY_PATH=$APPDIR/usr/lib/:$APPDIR/usr/lib/x86_64-linux-gnu/:...
//! GTK_PATH, GIO_EXTRA_MODULES, GSETTINGS_SCHEMA_DIR, PYTHONHOME, ...
//! ```
//!
//! Every process we spawn inherits that. For our own payload (the bundled
//! gateway) it is exactly right. For anything else it is poison: the bundle
//! carries Ubuntu 22.04's libraries, so a *system* binary loads our old glib
//! or brotli instead of the host's and dies at dynamic-link time on a rolling
//! release, before it runs a line of its own code:
//!
//! ```text
//! zenity:   symbol lookup error: /usr/lib/libjson-glib-1.0.so.0:
//!           undefined symbol: g_once_init_leave_pointer
//! chromium: symbol lookup error: /usr/lib/chromium/chromium:
//!           undefined symbol: BrotliDecoderAttachDictionary
//! ```
//!
//! That is what makes an OAuth "opening browser" do nothing at all, and it is
//! latent on every MCP server that is a native binary or pulls a native node or
//! python module. So anything we exec that is not our own bundled payload goes
//! through [`strip_bundled_env`] first.
//!
//! Gated on `APPDIR`, which only an AppImage sets, so this is a no-op for the
//! `.deb`, the AUR package, and a dev build.
//!
//! Deliberately NOT an `env_clear()`: an MCP server legitimately needs the
//! user's `PATH`, `HOME`, proxy variables, and whatever its own `env` block
//! configures. Only the variables `AppRun` and its GTK hook introduce are
//! removed. `PATH` is also left alone even though `AppRun` prepends
//! `$APPDIR/usr/bin` to it: the only things that shadows are `xdg-open`,
//! `xdg-mime` and our own binaries, none of which has caused a failure, and
//! rewriting it would have to happen after every other `PATH` edit at the call
//! site.
//!
//! The `ps` / `kill` helpers in `gateway_publish.rs` are deliberately left
//! alone. They are host binaries too, but they resolve to libc and nothing else,
//! and both were checked under the bundle's `LD_LIBRARY_PATH` and ran clean. The
//! spawns that go through this are the ones that reach real desktop or user
//! software: a browser, a file manager, a login shell, an MCP server.

use std::process::Command;

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// Restore the standard per-user D-Bus address when a terminal or agent launcher
/// stripped the desktop session environment.
///
/// systemd-logind creates `/run/user/<uid>` and its `bus` socket on Omarchy and
/// other systemd Linux desktops. Some AI clients launch MCP children with only
/// `HOME` and `USER`; Secret Service then attempts X11 autolaunch, which is disabled
/// on Wayland, and every vaulted server disappears from the live router. Recover only
/// the current user's owned runtime directory and owned Unix socket. An explicit
/// `DBUS_SESSION_BUS_ADDRESS` is authoritative and is never replaced.
#[cfg(target_os = "linux")]
pub fn restore_session_bus_env() -> Option<PathBuf> {
    let dbus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let uid = effective_uid();
    let (runtime_dir, bus) = session_bus_candidate(
        dbus.as_deref(),
        runtime.as_deref(),
        uid,
        Path::new("/run/user"),
    )?;

    if runtime.as_ref().map_or(true, |v| v.is_empty()) {
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    }
    std::env::set_var(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", bus.to_str()?),
    );
    Some(bus)
}

#[cfg(not(target_os = "linux"))]
pub fn restore_session_bus_env() -> Option<std::path::PathBuf> {
    None
}

#[cfg(target_os = "linux")]
fn effective_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no arguments and has no failure mode.
    unsafe { geteuid() }
}

#[cfg(target_os = "linux")]
fn session_bus_candidate(
    dbus: Option<&OsStr>,
    runtime: Option<&OsStr>,
    uid: u32,
    runtime_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    if dbus.is_some_and(|value| !value.is_empty()) {
        return None;
    }
    let runtime_dir = runtime
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root.join(uid.to_string()));
    let runtime_meta = std::fs::symlink_metadata(&runtime_dir).ok()?;
    if !runtime_meta.file_type().is_dir() || runtime_meta.uid() != uid {
        return None;
    }
    let bus = runtime_dir.join("bus");
    let bus_meta = std::fs::symlink_metadata(&bus).ok()?;
    if !bus_meta.file_type().is_socket() || bus_meta.uid() != uid {
        return None;
    }
    Some((runtime_dir, bus))
}

/// Every variable `AppRun.wrapped` and `apprun-hooks/linuxdeploy-plugin-gtk.sh`
/// export, minus the two handled specially below (`XDG_DATA_DIRS`, which is
/// prepended to rather than replaced, and `PATH`, see the module docs).
///
/// Taken from the shipped AppImage rather than from linuxdeploy's source, so it
/// describes what we actually build. A future linuxdeploy that exports one more
/// variable leaves that one leaking; it does not break anything here.
#[cfg(all(unix, not(target_os = "macos")))]
const BUNDLED_VARS: &[&str] = &[
    // AppRun.wrapped
    "LD_LIBRARY_PATH",
    "PERLLIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    // linuxdeploy-plugin-gtk.sh
    "APPDIR",
    "GDK_BACKEND",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_EXTRA_MODULES",
    "GSETTINGS_SCHEMA_DIR",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "GTK_THEME",
    // Not exported by this AppRun, but the loader honours it and a future hook
    // could set it; removing an unset variable costs nothing.
    "LD_PRELOAD",
];

/// Remove the AppImage's bundled library and plugin paths from `cmd`.
///
/// Call it on any [`Command`] that runs something other than our own bundled
/// payload, and call it *before* applying caller-supplied environment, so a
/// value the caller set on purpose still wins.
///
/// A no-op outside an AppImage, and on macOS and Windows.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn strip_bundled_env(cmd: &mut Command) {
    let Ok(appdir) = std::env::var("APPDIR") else {
        return; // not running from an AppImage
    };
    strip_for_appdir(cmd, &appdir, std::env::var("XDG_DATA_DIRS").ok().as_deref());
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn strip_bundled_env(_cmd: &mut Command) {}

/// The half of [`strip_bundled_env`] that does not read the process environment,
/// so it can be tested without mutating it.
#[cfg(all(unix, not(target_os = "macos")))]
fn strip_for_appdir(cmd: &mut Command, appdir: &str, data_dirs: Option<&str>) {
    for key in BUNDLED_VARS {
        cmd.env_remove(key);
    }
    // XDG_DATA_DIRS is the one AppRun *prepends* to rather than replaces, so it
    // still carries the host's entries. Dropping the whole variable would leave
    // xdg-open unable to find any .desktop file; drop only our entries.
    if let Some(dirs) = data_dirs {
        let kept: Vec<&str> = dirs
            .split(':')
            .filter(|p| !p.is_empty() && !p.starts_with(appdir))
            .collect();
        cmd.env("XDG_DATA_DIRS", kept.join(":"));
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(unix, not(target_os = "macos")))]
    use super::*;

    #[cfg(all(unix, not(target_os = "macos")))]
    fn env_of(cmd: &Command) -> Vec<(String, Option<String>)> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn removes_every_bundled_var() {
        let mut cmd = Command::new("true");
        strip_for_appdir(&mut cmd, "/tmp/.mount_Toolpo1234", None);
        let env = env_of(&cmd);
        for key in BUNDLED_VARS {
            let entry = env.iter().find(|(k, _)| k == key);
            assert_eq!(
                entry.map(|(_, v)| v.clone()),
                Some(None),
                "{key} should be marked for removal"
            );
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn keeps_host_data_dirs_and_drops_ours() {
        let appdir = "/tmp/.mount_Toolpo1234";
        let mut cmd = Command::new("true");
        strip_for_appdir(
            &mut cmd,
            appdir,
            Some(&format!("{appdir}/usr/share:/usr/share:/home/u/.local/share")),
        );
        let dirs = env_of(&cmd)
            .into_iter()
            .find(|(k, _)| k == "XDG_DATA_DIRS")
            .and_then(|(_, v)| v)
            .expect("XDG_DATA_DIRS should be rewritten, not removed");
        assert_eq!(dirs, "/usr/share:/home/u/.local/share");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn leaves_xdg_data_dirs_alone_when_unset() {
        let mut cmd = Command::new("true");
        strip_for_appdir(&mut cmd, "/tmp/.mount_Toolpo1234", None);
        assert!(!env_of(&cmd).iter().any(|(k, _)| k == "XDG_DATA_DIRS"));
    }

    /// The .deb, the AUR package and a dev build must be untouched. They never
    /// set APPDIR, so the guard in `strip_bundled_env` is what protects them.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn no_op_without_appdir() {
        let previous = std::env::var("APPDIR").ok();
        std::env::remove_var("APPDIR");
        let mut cmd = Command::new("true");
        strip_bundled_env(&mut cmd);
        let empty = cmd.get_envs().next().is_none();
        if let Some(v) = previous {
            std::env::set_var("APPDIR", v);
        }
        assert!(empty, "nothing should be changed outside an AppImage");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovers_an_owned_systemd_session_bus_from_a_stripped_environment() {
        use std::os::unix::net::UnixListener;

        let dir =
            std::env::temp_dir().join(format!("toolport-hostenv-recover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let runtime = dir.join(effective_uid().to_string());
        std::fs::create_dir(&runtime).unwrap();
        let bus = runtime.join("bus");
        let _listener = UnixListener::bind(&bus).unwrap();

        assert_eq!(
            session_bus_candidate(None, None, effective_uid(), &dir),
            Some((runtime, bus))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn never_replaces_an_explicit_bus_or_trusts_an_unsafe_candidate() {
        use std::os::unix::net::UnixListener;

        let dir =
            std::env::temp_dir().join(format!("toolport-hostenv-unsafe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let runtime = dir.join(effective_uid().to_string());
        std::fs::create_dir(&runtime).unwrap();
        let bus = runtime.join("bus");
        let listener = UnixListener::bind(&bus).unwrap();

        assert_eq!(
            session_bus_candidate(
                Some(OsStr::new("unix:path=/explicit/bus")),
                Some(runtime.as_os_str()),
                effective_uid(),
                &dir,
            ),
            None
        );
        assert_eq!(
            session_bus_candidate(None, Some(runtime.as_os_str()), effective_uid() + 1, &dir),
            None,
            "a runtime directory owned by another uid is not trusted"
        );

        drop(listener);
        std::fs::remove_file(&bus).unwrap();
        std::fs::write(&bus, "not a socket").unwrap();
        assert_eq!(
            session_bus_candidate(None, Some(runtime.as_os_str()), effective_uid(), &dir),
            None,
            "a regular file named bus is not trusted"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
