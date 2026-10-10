//! Explicit data removal. Package removal and upgrades never call this module.
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub data_dir: String,
    pub resources: Vec<String>,
    pub report_path: String,
}

#[derive(Debug, Serialize)]
pub struct Leftover {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub clients: Vec<crate::clients::DisconnectResult>,
    pub removed: Vec<String>,
    pub leftovers: Vec<Leftover>,
}

const AUTOSTART_NAMES: &[&str] = &["Toolport", "Conduit", "conduit", "ToolportNativePreview"];

/// Removal never flushes telemetry: doing so could recreate the deleted logs.
pub fn exit_after_removal(status: i32) -> ! {
    std::process::exit(status)
}

pub fn wait_for_desktop_exit() -> Result<(), String> {
    wait_for_exit_pipe(std::io::stdin(), std::time::Duration::from_secs(30))
}

fn wait_for_exit_pipe(
    mut input: impl std::io::Read + Send + 'static,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("purge-desktop-exit".into())
        .spawn(move || {
            let result = std::io::copy(&mut input, &mut std::io::sink())
                .map(|_| ())
                .map_err(|error| format!("Desktop exit pipe: {error}"));
            let _ = sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver.recv_timeout(timeout).map_err(|error| {
        format!("Toolport did not close within {} seconds ({error}). Nothing was removed. Close Toolport and retry.", timeout.as_secs())
    })?
}

fn data_dir() -> Result<PathBuf, String> {
    let dir =
        crate::registry::conduit_dir().ok_or("Could not resolve Toolport's data directory")?;
    let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let shared = [
        dirs::home_dir(),
        dirs::config_dir(),
        dirs::data_dir(),
        Some(std::env::temp_dir()),
    ];
    if !dir.is_absolute()
        || canonical.parent().is_none()
        || shared
            .into_iter()
            .flatten()
            .any(|path| canonical == std::fs::canonicalize(&path).unwrap_or(path))
        || std::fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!("Refusing unsafe data directory: {}", dir.display()));
    }
    Ok(dir)
}

fn verify_data_ownership(dir: &Path) -> Result<(), String> {
    if dir.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("Toolport" | "Conduit" | "Toolport-dev" | "Conduit-dev")
        )
    }) {
        return Ok(());
    }
    let owned = std::fs::read(dir.join("registry.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value["version"].is_u64() && value["servers"].is_array() && value["profiles"].is_array()
        });
    if owned {
        Ok(())
    } else {
        Err(format!(
            "Could not verify Toolport ownership of custom data directory {}. Nothing was removed.",
            dir.display()
        ))
    }
}

fn autostart_files(home: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let (base, ext) = (home.join("Library/LaunchAgents"), "plist");
    #[cfg(not(target_os = "macos"))]
    let (base, ext) = (home.join(".config/autostart"), "desktop");
    AUTOSTART_NAMES
        .iter()
        .map(|name| base.join(format!("{name}.{ext}")))
        .collect()
}

pub fn plan() -> Result<Plan, String> {
    let dir = data_dir()?;
    let home = dirs::home_dir().ok_or("Could not resolve your home directory")?;
    let report = home.join(format!(
        "Toolport-removal-report-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
    ));
    let mut resources = vec![
        format!("All contents of {}: registry, settings, logs, caches, migration exports, client backups, encrypted secrets and published gateways in bin/", dir.display()),
        "All credentials in Toolport's reserved conduit-mcp service, including Team tokens, OAuth state, master keys and orphaned Windows chunks. This service is shared by Toolport installs for this user.".into(),
        "Toolport's daemon for this data directory, after all active sessions have closed".into(),
    ];
    #[cfg(target_os = "windows")]
    resources.extend(AUTOSTART_NAMES.iter().map(|name| format!("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\{name} and StartupApproved\\Run\\{name} (only Toolport commands)")));
    #[cfg(not(target_os = "windows"))]
    resources.extend(
        autostart_files(&home)
            .iter()
            .map(|path| format!("{} (only Toolport's startup entry)", path.display())),
    );
    Ok(Plan {
        data_dir: dir.display().to_string(),
        resources,
        report_path: report.display().to_string(),
    })
}

/// The pipe stays open until the entire desktop process exits, including workers.
/// No timing assumption about desktop shutdown is needed by the child.
#[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
pub fn launch_after_exit(report_path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let home = dirs::home_dir().ok_or("Could not resolve your home directory")?;
    if report_path.parent() != Some(home.as_path())
        || !report_path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with("Toolport-removal-report-")
        })
    {
        return Err("Invalid removal report path".into());
    }
    let binary = crate::gateway_publish::bundled_gateway_source().ok_or(
        "Could not find the bundled gateway. Use toolport-gateway --remove-data --confirm instead.",
    )?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let report = options
        .open(report_path)
        .map_err(|error| error.to_string())?;
    let mut command = Command::new(binary);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let mut child = command
        .args(["--remove-data", "--confirm", "--after-desktop-exit"])
        .stdin(Stdio::piped())
        .stdout(report)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    // Deliberately held by the OS until process exit. Child waits for EOF.
    if let Some(pipe) = child.stdin.take() {
        std::mem::forget(pipe);
    }
    Ok(())
}

pub fn run() -> Result<Report, String> {
    verify_data_ownership(&data_dir()?)?;
    run_with(
        || crate::clients::disconnect_all(false),
        |dir| {
            let mut blockers = crate::daemon::stop_for_purge(dir);
            blockers.extend(crate::gateway_publish::purge_blockers(dir));
            blockers
        },
        crate::secrets::purge::remove,
        remove_autostart,
    )
}

fn run_with(
    disconnect: impl FnOnce() -> Result<Vec<crate::clients::DisconnectResult>, String>,
    stop: impl FnOnce(&Path) -> Vec<Leftover>,
    keyring: impl FnOnce() -> Result<Vec<Leftover>, String>,
    autostart: impl FnOnce(&mut Report) -> Result<(), String>,
) -> Result<Report, String> {
    use fs2::FileExt;
    let dir = data_dir()?;
    let mut report = Report::default();
    if !dir.exists() {
        return Err(format!(
            "No Toolport data directory at {}. Nothing was removed.",
            dir.display()
        ));
    }
    let owner_path = dir.join("broker/owner.lock");
    if std::fs::symlink_metadata(dir.join("broker"))
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        || std::fs::symlink_metadata(&owner_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "Refusing symlinked broker ownership path: {}",
            owner_path.display()
        ));
    }
    std::fs::create_dir_all(owner_path.parent().unwrap()).map_err(|error| error.to_string())?;
    let owner = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&owner_path)
        .map_err(|error| format!("{}: {error}", owner_path.display()))?;
    owner.try_lock_exclusive().map_err(|error| {
        format!(
            "Close Toolport before removing data. {}: {error}",
            owner_path.display()
        )
    })?;
    report.clients = disconnect()?;
    if report
        .clients
        .iter()
        .any(|client| client.error.is_some() || !client.warnings.is_empty())
    {
        for client in &report.clients {
            if let Some(error) = &client.error {
                report.leftovers.push(Leftover {
                    path: client.path.clone(),
                    error: error.clone(),
                });
            }
            if !client.warnings.is_empty() {
                report.leftovers.push(Leftover {
                    path: client.path.clone(),
                    error: client.warnings.join("; "),
                });
            }
        }
        report.leftovers.push(Leftover { path: dir.display().to_string(), error: "Client restoration failed or retained an edited connection. All Toolport data and credentials were retained for recovery.".into() });
        return Ok(report);
    }
    report.leftovers.extend(stop(&dir));
    if !report.leftovers.is_empty() {
        report.leftovers.push(Leftover {
            path: dir.display().to_string(),
            error: "Close the listed sessions and retry. Data and credentials were retained."
                .into(),
        });
        return Ok(report);
    }
    match keyring() {
        Ok(leftovers) => {
            if leftovers.is_empty() {
                report.removed.push("keychain:conduit-mcp".into());
            }
            report.leftovers.extend(leftovers);
        }
        Err(error) => report.leftovers.push(Leftover {
            path: "keychain:conduit-mcp (inventory unavailable)".into(),
            error,
        }),
    }
    autostart(&mut report)?;
    // Keep the recovery data if an external resource could not be removed.
    if !report.leftovers.is_empty() {
        report.leftovers.push(Leftover {
            path: dir.display().to_string(),
            error: "Retained for retry because credentials or startup entries remain.".into(),
        });
        return Ok(report);
    }
    let registry_path = dir.join("registry.json");
    remove_contents(
        &dir,
        &[owner_path.clone(), registry_path.clone()],
        &mut report,
    );
    if report.leftovers.is_empty() {
        remove_path(&registry_path, &mut report);
    } else if registry_path.exists() {
        report.leftovers.push(Leftover {
            path: registry_path.display().to_string(),
            error: "Retained as ownership and recovery evidence for retry.".into(),
        });
    }
    // Windows cannot unlink an open owner lock. It is the last resource removed.
    drop(owner);
    remove_path(&owner_path, &mut report);
    remove_empty_dirs(&dir, &mut report);
    Ok(report)
}

fn remove_path(path: &Path, report: &mut Report) {
    let result = std::fs::symlink_metadata(path).and_then(|metadata| {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        }
    });
    match result {
        Ok(()) => report.removed.push(path.display().to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => report.leftovers.push(Leftover {
            path: path.display().to_string(),
            error: error.to_string(),
        }),
    }
}

fn remove_contents(dir: &Path, keep: &[PathBuf], report: &mut Report) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            report.leftovers.push(Leftover {
                path: dir.display().to_string(),
                error: error.to_string(),
            });
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if keep.iter().any(|keep| keep == &path) {
                    continue;
                }
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        remove_contents(&path, keep, report);
                        // A directory holding the owner lock stays until it is released.
                        if !keep.iter().any(|keep| keep.starts_with(&path)) {
                            remove_path(&path, report);
                        }
                    }
                    Ok(_) => remove_path(&path, report),
                    Err(error) => report.leftovers.push(Leftover {
                        path: path.display().to_string(),
                        error: error.to_string(),
                    }),
                }
            }
            Err(error) => report.leftovers.push(Leftover {
                path: dir.display().to_string(),
                error: error.to_string(),
            }),
        }
    }
}

fn remove_empty_dirs(dir: &Path, report: &mut Report) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                remove_empty_dirs(&entry.path(), report);
            }
        }
    }
    // Leave the data directory itself present and empty; never unlink a live lock's parent.
    if dir.file_name().is_some_and(|name| name == "broker") {
        remove_path(dir, report);
    }
}

fn owned_autostart(contents: &str, name: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        contents.contains(&format!("<string>{name}</string>"))
            && contents.contains("<string>--hidden</string>")
    }
    #[cfg(not(target_os = "macos"))]
    {
        contents.lines().any(|line| line == format!("Name={name}"))
            && contents
                .lines()
                .any(|line| line == format!("Comment={name}startup script"))
    }
}

fn remove_autostart(report: &mut Report) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        report
            .leftovers
            .extend(crate::windows_autostart::remove_toolport_entries(
                AUTOSTART_NAMES,
            ));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir().ok_or("Could not resolve your home directory")?;
        for (path, name) in autostart_files(&home).iter().zip(AUTOSTART_NAMES) {
            match std::fs::read_to_string(path) {
                Ok(contents) if owned_autostart(&contents, name) => remove_path(path, report),
                Ok(_) => report.leftovers.push(Leftover {
                    path: path.display().to_string(),
                    error: "Startup entry has different ownership. It was preserved.".into(),
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => report.leftovers.push(Leftover {
                    path: path.display().to_string(),
                    error: error.to_string(),
                }),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_handoff_observes_eof_and_reports_a_stuck_parent() {
        assert!(wait_for_exit_pipe(
            std::io::Cursor::new(Vec::<u8>::new()),
            std::time::Duration::from_secs(5)
        )
        .is_ok());
        struct Pending(std::sync::mpsc::Receiver<()>);
        impl std::io::Read for Pending {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Ok(0)
            }
        }
        let (release, pending) = std::sync::mpsc::channel();
        let error =
            wait_for_exit_pipe(Pending(pending), std::time::Duration::from_millis(20)).unwrap_err();
        assert!(error.contains("Nothing was removed. Close Toolport and retry."));
        drop(release);
    }
    #[test]
    fn purge_temp_data_never_follows_symlinks_or_touches_native_client_data() {
        let env = crate::registry::DataDirTestEnv::new("purge_temp_data");
        let dir = env.dir.join("data");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/toolport-gateway"), "fixture").unwrap();
        std::fs::write(dir.join("registry.json"), "fixture").unwrap();
        let native = dir.parent().unwrap().join("native-client");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("config.json"), "native bytes").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&native, dir.join("link")).unwrap();
        let mut report = Report::default();
        remove_contents(&dir, &[], &mut report);
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_to_string(native.join("config.json")).unwrap(),
            "native bytes"
        );
        drop(env);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn purge_autostart_requires_toolports_marker() {
        assert!(owned_autostart(
            &crate::autostart::linux_desktop_entry(
                "Toolport",
                Path::new("/fixture/toolport-gtk"),
                &["--hidden"]
            ),
            "Toolport"
        ));
        assert!(!owned_autostart(
            "[Desktop Entry]\nName=Toolport\nExec=native-app",
            "Toolport"
        ));
    }
    #[test]
    fn failed_disconnect_retains_recovery_data_and_never_calls_keyring() {
        let _env = crate::registry::DataDirTestEnv::new("purge_failed_disconnect");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), "recovery bytes").unwrap();
        let report = run_with(
            || {
                Ok(vec![crate::clients::DisconnectResult {
                    client_id: "fixture".into(),
                    path: "/fixture/client.json".into(),
                    dry_run: false,
                    error: Some("native config conflict".into()),
                    warnings: Vec::new(),
                }])
            },
            |_| panic!("must not stop before restoration"),
            || panic!("must not remove secrets"),
            |_| panic!("must not remove startup entries"),
        )
        .unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.clients[0].path, "/fixture/client.json");
        assert_eq!(
            std::fs::read_to_string(dir.join("registry.json")).unwrap(),
            "recovery bytes"
        );
    }

    #[test]
    fn fake_keyring_failure_reports_exact_leftovers_and_keeps_data_for_retry() {
        let _env = crate::registry::DataDirTestEnv::new("purge_keyring_failure");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("secrets.enc"), "recovery bytes").unwrap();
        let report = run_with(
            || Ok(Vec::new()),
            |_| Vec::new(),
            || {
                Ok(vec![Leftover {
                    path: "secret-service:/fixture/locked-item".into(),
                    error: "locked".into(),
                }])
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            report.leftovers[0].path,
            "secret-service:/fixture/locked-item"
        );
        assert!(dir.join("secrets.enc").exists());
        let report = run_with(
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }

    #[test]
    fn active_session_prevents_secret_or_data_removal() {
        let _env = crate::registry::DataDirTestEnv::new("purge_busy_session");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), "keep").unwrap();
        let report = run_with(
            || Ok(Vec::new()),
            |_| {
                vec![Leftover {
                    path: "/fixture/toolport-gateway".into(),
                    error: "PID 123 is busy".into(),
                }]
            },
            || panic!("busy session"),
            |_| panic!("busy session"),
        )
        .unwrap();
        assert_eq!(report.leftovers[0].path, "/fixture/toolport-gateway");
        assert!(dir.join("registry.json").exists());
    }

    #[test]
    fn temp_home_autostart_inventory_does_not_include_native_clients() {
        let _env = crate::registry::DataDirTestEnv::new("purge_temp_home");
        let home = _env.dir.join("home");
        let files = autostart_files(&home);
        assert_eq!(files.len(), AUTOSTART_NAMES.len());
        assert!(files.iter().all(|file| file.starts_with(&home)));
        assert!(!files
            .iter()
            .any(|file| file.to_string_lossy().contains("Claude")));
    }
    #[test]
    fn custom_native_directory_is_not_a_toolport_data_directory() {
        let env = crate::registry::DataDirTestEnv::new("purge_ownership");
        std::fs::write(
            env.dir.join("registry.json"),
            r#"{"mcpServers":{"native":{"command":"native"}}}"#,
        )
        .unwrap();
        assert!(verify_data_ownership(&env.dir).is_err());
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        assert!(verify_data_ownership(&env.dir).is_ok());
    }
    #[test]
    fn retained_edited_gateway_blocks_purge_even_without_a_disconnect_error() {
        let _env = crate::registry::DataDirTestEnv::new("purge_retained_gateway");
        let report = run_with(
            || {
                Ok(vec![crate::clients::DisconnectResult {
                    client_id: "fixture".into(),
                    path: "/fixture/edited-client.json".into(),
                    dry_run: false,
                    error: None,
                    warnings: vec!["kept your edited toolport entry".into()],
                }])
            },
            |_| panic!("connection remains"),
            || panic!("connection remains"),
            |_| panic!("connection remains"),
        )
        .unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.leftovers[0].path, "/fixture/edited-client.json");
    }
}
