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
    /// Owned credentials that were already gone when removal reached them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub absent: Vec<String>,
    pub leftovers: Vec<Leftover>,
    pub preserved: Vec<String>,
    pub notes: Vec<String>,
    /// Data removal ran to the end and only credential leftovers (and the
    /// registry kept to retry them) remain, so app resources can go too.
    #[serde(skip)]
    pub(crate) only_credentials_left: bool,
}

// Unit tests always use scratch roots. test-support subprocesses opt in with
// TOOLPORT_TEST_ROOT because Windows known folders ignore profile environment.
macro_rules! root_resolver {
    ($name:ident, $kind:literal, $dirs:ident) => {
        fn $name() -> Option<PathBuf> {
            #[cfg(any(test, feature = "test-support"))]
            if let Some(path) = crate::fixture_paths::dir($kind) {
                return Some(path);
            }
            #[cfg(not(test))]
            {
                return dirs::$dirs();
            }
            #[cfg(test)]
            None
        }
    };
}
root_resolver!(home_dir, "home", home_dir);
root_resolver!(config_dir, "config", config_dir);
root_resolver!(data_dir_root, "data", data_dir);
#[cfg(target_os = "linux")]
root_resolver!(cache_dir, "cache", cache_dir);
#[cfg(target_os = "linux")]
root_resolver!(data_local_dir, "local-data", data_local_dir);

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
        home_dir(),
        config_dir(),
        data_dir_root(),
        Some(std::env::temp_dir()),
    ];
    if !dir.is_absolute()
        || canonical.parent().is_none()
        || shared
            .into_iter()
            .flatten()
            .any(|path| canonical == std::fs::canonicalize(&path).unwrap_or(path))
        || std::fs::symlink_metadata(&dir).is_ok_and(|metadata| is_link(&metadata))
    {
        return Err(format!("Refusing unsafe data directory: {}", dir.display()));
    }
    Ok(dir)
}

fn verify_data_ownership(dir: &Path) -> Result<(), String> {
    verify_ownership(
        dir,
        crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR").is_some(),
    )
}

fn verify_removal_ownership(dir: &Path, all_installs: bool) -> Result<(), String> {
    // Explicit global recovery permits a corrupt registry, but still requires it
    // to exist before bypassing the custom-directory ownership check.
    if !all_installs || !dir.join("registry.json").is_file() {
        verify_data_ownership(dir)?;
    }
    Ok(())
}

fn verify_ownership(dir: &Path, overridden: bool) -> Result<(), String> {
    if !overridden
        && dir.file_name().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some("Toolport" | "Conduit" | "Toolport-dev" | "Conduit-dev")
            )
        })
    {
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
    } else if dir.join("registry.json").is_file() {
        Err(format!(
            "Could not verify Toolport ownership of custom data directory {}: {} is not a readable Toolport registry. Nothing was removed. Repair it, or run toolport-gateway --remove-data --all-installs --confirm.",
            dir.display(),
            dir.join("registry.json").display()
        ))
    } else {
        Err(format!(
            "Could not verify Toolport ownership of custom data directory {}. Nothing was removed.",
            dir.display()
        ))
    }
}

fn custom_data_dir(dir: &Path) -> bool {
    crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR").is_some()
        || !dir.file_name().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some("Toolport" | "Conduit" | "Toolport-dev" | "Conduit-dev")
            )
        })
}

fn credential_removal_guidance(custom: bool) -> &'static str {
    if custom {
        "With a custom data directory, use toolport-gateway --remove-data --all-installs --confirm before removing its registry. After the registry is removed, this command cannot verify the directory's ownership; remove preserved conduit-mcp accounts in your OS credential manager."
    } else {
        "To remove all shared credentials, use toolport-gateway --remove-data --all-installs --confirm."
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

fn install_candidates(dir: &Path) -> Vec<PathBuf> {
    let mut bases = vec![dir.parent().map(Path::to_path_buf), config_dir()];
    #[cfg(windows)]
    bases.push(home_dir().map(|home| crate::brand::windows_roaming_base(&home)));
    #[cfg(not(windows))]
    let _ = &mut bases;
    bases
        .into_iter()
        .flatten()
        .flat_map(|base| {
            ["Toolport", "Conduit", "Toolport-dev", "Conduit-dev"].map(|name| base.join(name))
        })
        .collect()
}

fn other_installs(dir: &Path, candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    other_installs_with(dir, candidates, |path| {
        // Explicit overrides use the direct view in registry::compute_conduit_dir.
        if crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR").is_some() {
            path.to_path_buf()
        } else {
            crate::registry::devirtualized_dir(path)
        }
    })
}

fn other_installs_with(
    dir: &Path,
    candidates: Vec<PathBuf>,
    normalize: impl Fn(&Path) -> PathBuf,
) -> Vec<PathBuf> {
    let dir = normalize(dir);
    let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
    let mut others = Vec::new();
    for path in candidates {
        let path = normalize(&path);
        if path.is_dir()
            && std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()) != canonical
            && has_registry_evidence(&path)
            && !others.contains(&path)
        {
            others.push(path);
        }
    }
    others
}

fn has_registry_evidence(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir.join("registry.json")) {
        Ok(_) => return true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return true, // Unreadable ownership must still block cleanup.
    }
    std::fs::read_dir(dir).map_or(true, |entries| {
        entries.into_iter().any(|entry| {
            entry.map_or(true, |entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    generated_file_name(name, |name| {
                        name == "registry.json"
                            || name.starts_with("registry.json.") && owned_data_file(name)
                    })
                })
            })
        })
    })
}

fn inventory_server_ids(path: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let registry: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    registry["servers"]
        .as_array()
        .ok_or("missing servers array")?
        .iter()
        .map(|server| {
            server["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "missing server id".into())
        })
        .collect()
}

fn other_install_server_ids(dir: &Path) -> Result<Vec<String>, String> {
    let registry = dir.join("registry.json");
    let error = |path: &Path, error: String, recovery: bool| {
        format!(
        "Credential ownership inventory for {}: {error}. Credentials were not removed; the selected registry was retained. {}",
        path.display(),
        if recovery { "Restore a readable registry.json in this folder, or delete or rename the named recovery leftover and retry." }
        else { "Repair this registry and retry." }
    )
    };
    match std::fs::symlink_metadata(&registry) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return inventory_server_ids(&registry).map_err(|e| error(&registry, e, false)),
    }
    let entries = std::fs::read_dir(dir).map_err(|e| error(dir, e.to_string(), true))?;
    let mut recovery = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| error(dir, e.to_string(), true))?;
        if entry.file_name().to_str().is_some_and(|name| {
            generated_file_name(name, |name| {
                name == "registry.json"
                    || name.starts_with("registry.json.") && owned_data_file(name)
            })
        }) {
            let path = entry.path();
            let modified = std::fs::symlink_metadata(&path)
                .ok()
                .and_then(|meta| meta.modified().ok());
            recovery.push((modified, path));
        }
    }
    recovery.sort_by(|a, b| b.cmp(a));
    let mut failure = None;
    for (_, path) in recovery {
        // Inventory is read-only and never follows a recovery link or migrates it.
        let result = std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|meta| {
                if !meta.is_file() || is_link(&meta) {
                    return Err("not a regular recovery file".into());
                }
                inventory_server_ids(&path)
            });
        match result {
            Ok(ids) => return Ok(ids),
            Err(e) => {
                failure.get_or_insert_with(|| error(&path, e, true));
            }
        }
    }
    Err(failure.unwrap_or_else(|| {
        error(
            &registry,
            "no registry or recovery file found".into(),
            false,
        )
    }))
}

fn credential_scope(
    dir: &Path,
    others: &[PathBuf],
    all_installs: bool,
) -> Result<crate::secrets::purge::Scope, String> {
    if all_installs {
        return Ok(crate::secrets::purge::Scope {
            all_installs: true,
            server_ids: Default::default(),
        });
    }
    let registry_path = dir.join("registry.json");
    let mut ids = std::collections::HashSet::new();
    if registry_path.exists() {
        // Raw read: loading a registry can migrate it or create recovery files.
        let error = |error| {
            format!(
                "Credential ownership inventory for {}: {error}",
                registry_path.display()
            )
        };
        let registry: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&registry_path).map_err(|e| error(e.to_string()))?,
        )
        .map_err(|e| error(e.to_string()))?;
        let servers = registry["servers"]
            .as_array()
            .ok_or_else(|| error("missing servers array".into()))?;
        ids.extend(
            servers
                .iter()
                .filter_map(|server| server["id"].as_str().map(str::to_owned)),
        );
    }
    for other in others {
        for id in other_install_server_ids(other)? {
            ids.remove(&id);
        }
    }
    // Every backup generation uses one namespace bound to this data directory.
    // Inventory it even after backup files were removed or a write was interrupted.
    ids.insert(crate::clients::backup_storage::install_account(dir));
    Ok(crate::secrets::purge::Scope {
        all_installs,
        server_ids: ids,
    })
}

fn app_resources(home: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    return [
        // Tauri's app data folder (the bundle-id twin of Linux's data_local_dir
        // and Windows' AppData/Local entries below).
        "Library/Application Support/com.tsout.conduit",
        "Library/Caches/com.tsout.conduit",
        "Library/HTTPStorages/com.tsout.conduit",
        "Library/Preferences/com.tsout.conduit.plist",
        "Library/Saved Application State/com.tsout.conduit.savedState",
        "Library/WebKit/com.tsout.conduit",
    ]
    .map(|path| home.join(path))
    .into();
    #[cfg(target_os = "windows")]
    return vec![home.join("AppData/Local/com.tsout.conduit")];
    #[cfg(target_os = "linux")]
    {
        let _ = home;
        [cache_dir(), data_local_dir()]
            .into_iter()
            .flatten()
            .flat_map(|base| [base.join("com.tsout.conduit")])
            .collect()
    }
}

fn remove_app_resources(report: &mut Report, shared: bool) {
    let Some(home) = home_dir() else {
        return;
    };
    remove_resources(app_resources(&home), shared, report);
}

fn remove_resources(paths: Vec<PathBuf>, shared: bool, report: &mut Report) {
    for path in paths {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                report.leftovers.push(Leftover {
                    path: path.display().to_string(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        if shared {
            report.preserved.push(format!(
                "{} (shared app storage; another install or custom data directory is in use)",
                path.display()
            ));
        } else {
            if metadata.is_dir() && !is_link(&metadata) {
                remove_contents(&path, &[], report);
            }
            remove_path(&path, report);
        }
    }
}

pub fn plan() -> Result<Plan, String> {
    plan_with_scope(false)
}

pub fn plan_with_scope(all_installs: bool) -> Result<Plan, String> {
    let dir = data_dir()?;
    verify_removal_ownership(&dir, all_installs)?;
    let others = other_installs(&dir, install_candidates(&dir));
    credential_scope(&dir, &others, all_installs)?;
    let home = home_dir().ok_or("Could not resolve your home directory")?;
    let report = home.join(format!(
        "Toolport-removal-report-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
    ));
    let mut resources = vec![
        format!("Toolport-created entries in {}: registry, settings, logs, caches, migration exports, client backups, encrypted secrets and published gateways in bin/. Other entries are preserved.", dir.display()),
        if all_installs { "ALL credentials in the shared conduit-mcp service for every Toolport install, including master keys and Team tokens. Other installations will lose access to these credentials.".into() }
        else { format!("Only accounts owned by this registry, excluding server ids used by detected other installs. Toolport's shared master key, internal keys, HTTP bridge/client keys and Team tokens are preserved, along with unknown credentials that may belong to another Toolport install. {}", credential_removal_guidance(custom_data_dir(&dir))) },
        "Toolport's daemon for this data directory, after all active sessions have closed".into(),
    ];
    #[cfg(target_os = "windows")]
    resources.extend(AUTOSTART_NAMES.iter().map(|name| format!("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\{name} and HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run\\{name} (only Toolport commands)")));
    #[cfg(not(target_os = "windows"))]
    resources.extend(
        autostart_files(&home)
            .iter()
            .map(|path| format!("{} (only Toolport's startup entry)", path.display())),
    );
    resources.extend(app_resources(&home).iter().map(|path| {
        format!(
            "{} (app cache/preferences/WebView storage; preserved when shared)",
            path.display()
        )
    }));
    resources.extend(others.iter().map(|path| {
        format!(
            "{} (other install detected; its data is preserved)",
            path.display()
        )
    }));
    Ok(Plan {
        data_dir: dir.display().to_string(),
        resources,
        report_path: report.display().to_string(),
    })
}

fn open_removal_report(report_path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(report_path) {
        Ok(file) => return Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(report_path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(format!(
            "Refusing non-file removal report: {}",
            report_path.display()
        ));
    }
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|error| error.to_string())?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let previous = report_path.with_extension(format!("previous-{suffix}.json"));
    // A new hard link preserves exact bytes without overwriting an older archive.
    // An earlier child still holding stdout can finish writing to this archive.
    std::fs::hard_link(report_path, &previous).map_err(|error| {
        format!(
            "Could not preserve previous removal report {}: {error}. Rename it and retry.",
            report_path.display()
        )
    })?;
    std::fs::remove_file(report_path).map_err(|error| error.to_string())?;
    options.open(report_path).map_err(|error| {
        format!(
            "Could not create removal report: {error}. Previous report saved at {}.",
            previous.display()
        )
    })
}

/// The pipe stays open until the entire desktop process exits, including workers.
/// No timing assumption about desktop shutdown is needed by the child.
#[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
pub fn launch_after_exit(report_path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let home = home_dir().ok_or("Could not resolve your home directory")?;
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
    let report = open_removal_report(report_path)?;
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

pub fn write_report(path: &Path, report: &Report) -> Result<(), String> {
    use std::io::Write;
    let mut file = open_removal_report(path)?;
    file.write_all(
        serde_json::to_string_pretty(report)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

pub fn run() -> Result<Report, String> {
    run_with_scope(false)
}

pub fn run_with_scope(all_installs: bool) -> Result<Report, String> {
    let dir = data_dir()?;
    // Same order as planning, so a dry run and a confirmed run report the same error.
    verify_removal_ownership(&dir, all_installs)?;
    let others = other_installs(&dir, install_candidates(&dir));
    let scope = credential_scope(&dir, &others, all_installs)?;
    let mut report = run_with(
        |dir| {
            let mut blockers = crate::gateway_publish::purge_blockers(dir);
            blockers.extend(crate::daemon::purge_idle_blockers(dir));
            blockers
        },
        || crate::clients::disconnect_all_for_purge(all_installs),
        |dir| {
            let mut blockers = crate::daemon::stop_for_purge(dir);
            blockers.extend(crate::gateway_publish::purge_blockers(dir));
            blockers
        },
        || crate::secrets::purge::remove(&scope),
        |report| startup_entries(false, report),
        remove_autostart,
    )?;
    if report.leftovers.is_empty() || report.only_credentials_left {
        remove_app_resources(
            &mut report,
            !others.is_empty()
                || crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR").is_some(),
        );
    }
    Ok(report)
}

fn run_with(
    preflight: impl FnOnce(&Path) -> Vec<Leftover>,
    disconnect: impl FnOnce() -> Result<Vec<crate::clients::DisconnectResult>, String>,
    stop: impl FnOnce(&Path) -> Vec<Leftover>,
    keyring: impl FnOnce() -> Result<crate::secrets::purge::Removal, String>,
    inventory_startup: impl FnOnce(&mut Report) -> Result<(), String>,
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
    report.leftovers.extend(preflight(&dir));
    inventory_startup(&mut report)?;
    if !report.leftovers.is_empty() {
        report.notes.push(
            "Preflight failed. Clients, data, credentials and startup entries are unchanged."
                .into(),
        );
        return Ok(report);
    }
    let owner_path = dir.join("broker/owner.lock");
    if std::fs::symlink_metadata(dir.join("broker")).is_ok_and(|metadata| is_link(&metadata))
        || std::fs::symlink_metadata(&owner_path).is_ok_and(|metadata| is_link(&metadata))
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
    // Finish startup cleanup before touching credentials. Foreign entries are informational.
    autostart(&mut report)?;
    if !report.leftovers.is_empty() {
        report
            .notes
            .push("Startup cleanup failed. Data and credentials were retained for retry.".into());
        return Ok(report);
    }
    let mut team_token_in_file = false;
    match keyring() {
        Ok(removal) => {
            team_token_in_file = removal.team_token_in_file;
            if removal.team_token_removed {
                report.notes.push("Team tokens deleted from this machine were removed locally only; no server-side revoke endpoint is available in the Teams client.".into());
            }
            report.removed.extend(removal.removed);
            if !removal.preserved.is_empty() {
                report.notes.push(format!("Preserved credentials may belong to another Toolport install. Toolport's own shared master key, internal keys, HTTP bridge/client keys and Team tokens remain too. {}", credential_removal_guidance(custom_data_dir(&dir))));
            }
            report.preserved.extend(removal.preserved);
            report.absent.extend(removal.absent);
            report.notes.extend(removal.notes);
            report.leftovers.extend(removal.leftovers);
        }
        Err(error) => report.leftovers.push(Leftover {
            path: "keychain:conduit-mcp (inventory unavailable)".into(),
            error,
        }),
    }
    if !report.absent.is_empty() {
        report.notes.push(format!(
            "{} credential item(s) were already gone when removal reached them; they are listed under absent and nothing of them remains.",
            report.absent.len()
        ));
    }
    // A credential that could not be removed does not stop the rest of the
    // removal. Everything else goes; registry.json and client-locations.json
    // stay (reported below) so a retry has the same ownership inventory.
    let credential_leftovers = report.leftovers.len();
    if credential_leftovers > 0 {
        report.notes.push(format!(
            "{credential_leftovers} credential item(s) could not be removed and are listed under leftovers with the reason. Everything else that could be removed was removed; registry.json was retained so a retry finds the same credentials."
        ));
    }
    let registry_path = dir.join("registry.json");
    let locations_path = dir.join("client-locations.json");
    remove_data_contents(
        &dir,
        &[
            owner_path.clone(),
            registry_path.clone(),
            locations_path.clone(),
        ],
        &mut report,
    );
    if team_token_in_file
        && report
            .removed
            .contains(&dir.join("secrets.enc").display().to_string())
    {
        report.notes.push("Team tokens deleted from this machine were removed locally only; no server-side revoke endpoint is available in the Teams client.".into());
    }
    report.only_credentials_left =
        credential_leftovers > 0 && report.leftovers.len() == credential_leftovers;
    // Bounded long-ID backup names need the location inventory on the next pass.
    if report.leftovers.is_empty() {
        match std::fs::symlink_metadata(&locations_path) {
            Ok(metadata) if !metadata.is_dir() && !is_link(&metadata) => {
                remove_path(&locations_path, &mut report);
            }
            Ok(_) => report.preserved.push(format!(
                "{} (not a Toolport-created entry)",
                locations_path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => report.leftovers.push(Leftover {
                path: locations_path.display().to_string(),
                error: error.to_string(),
            }),
        }
    } else if locations_path.exists() {
        report.leftovers.push(Leftover {
            path: locations_path.display().to_string(),
            error: "Retained as backup ownership evidence for retry.".into(),
        });
    }
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
    remove_empty_dirs(&dir.join("broker"), &mut report);
    Ok(report)
}

pub(crate) fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
thread_local! {
    static REMOVE_FAILURE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn remove_path(path: &Path, report: &mut Report) {
    #[cfg(test)]
    if REMOVE_FAILURE.with(|slot| slot.borrow().as_deref() == Some(path)) {
        report.leftovers.push(Leftover {
            path: path.display().to_string(),
            error: "simulated locked file".into(),
        });
        return;
    }
    let result = std::fs::symlink_metadata(path).and_then(|metadata| {
        // Windows reports directory junctions as links, so is_dir() is false.
        // RemoveDirectory unlinks them without traversing their target.
        #[cfg(windows)]
        let directory = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x10 != 0
        };
        #[cfg(not(windows))]
        let directory = metadata.is_dir() && !is_link(&metadata);
        if directory {
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

// Names come from registry/profile stores, clients, telemetry, daemon/broker,
// secrets, Teams, native-shell state and the gateway's persisted caches.
const DATA_FILES: &[&str] = &[
    "ui-settings.json",
    "ui-settings.lock",
    "client-locations.json",
    "registry.json",
    "registry-recovery.json",
    "registry.personal-sync-status.json",
    "routines.json",
    "secrets.enc",
    "embeddings.json",
    "root-integrity-scopes.json",
    "tool-cache.json",
    "tool-pins.json",
    "quarantine.json",
    "audit.jsonl",
    "security.jsonl",
    "inspect.jsonl",
    "gateway.log",
    "daemon.log",
    "savings.jsonl",
    "savings-v2.jsonl",
    "savings-v3.jsonl",
    "savings-v4.jsonl",
    "search-trace.jsonl",
    "search-trace-v2.jsonl",
    "oauth-debug.log",
    "last-daemon-exit.json",
    "approval-endpoint.json",
    "secret-reference-approvals.json",
    "rate_limit_counters.json",
    "downstream_backoff.json",
    "team-sync-status.json",
    "http-bridge-update-resume.json",
    "gtk-security-dismissed.json",
    "gtk-window-state.json",
    "gtk-skills-project.json",
    ".secrets-file-migrated",
    ".keychain-dpk-migrated",
    ".tray-hint-shown",
    ".gtk-star-prompt-done",
    "gtk-onboarding-complete",
    "agent-activity-removed",
    "agent-guard-removed",
    "legacy-topology-retired.notice",
    // Startup secret scrub of old logs (secret_redact).
    "secret-scrub-v1.json",
    "secret-scrub-pending",
];

fn hex_name(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u128>().is_ok()
}

// registry::atomic_write and lock_at generate siblings of a known file, never
// siblings of a directory. Apply the same rule at every selected depth.
fn generated_file_name(name: &str, owned: impl Fn(&str) -> bool) -> bool {
    let temp_of_owned = |name: &str| {
        name.strip_suffix(".conduit-tmp").is_some_and(|base| {
            base.rsplit_once('.').is_some_and(|(base, seq)| {
                decimal(seq)
                    && base.rsplit_once('.').is_some_and(|(base, pid)| {
                        decimal(pid) && pid.parse::<u32>().is_ok() && owned(base)
                    })
            })
        })
    };
    if owned(name) || temp_of_owned(name) {
        return true;
    }
    // A lock beside an owned file, or beside a temp copy of one (a scrub of a
    // write in flight locked `<file>.<pid>.<seq>.conduit-tmp`).
    name.strip_suffix(".lock")
        .is_some_and(|base| owned(base) || temp_of_owned(base))
}

fn owned_data_name(name: &str, legacy: &std::collections::HashSet<String>) -> bool {
    generated_file_name(name, |name| owned_data_file(name) || legacy.contains(name))
        || name
            .rsplit_once(".unreadable-")
            .is_some_and(|(base, timestamp)| {
                decimal(timestamp)
                    && (base == "tool-pins.json"
                        || base
                            .strip_prefix("tool-pins-v2-")
                            .and_then(|key| key.strip_suffix(".json"))
                            .is_some_and(|key| hex_name(key, 64)))
            })
        || matches!(
            name,
            "secret-service.lock"
                | "task-handle-key.lock"
                | "client-config-mutation.lock"
                | "secret-scrub.lock"
        )
        || name
            .strip_prefix("daemon-")
            .and_then(|s| s.strip_suffix(".lock"))
            .is_some_and(|s| hex_name(s, 64))
        || name
            .strip_prefix("oauth-refresh-")
            .and_then(|s| s.strip_suffix(".lock"))
            .is_some_and(|id| !id.is_empty() && crate::router::sanitize_segment(id) == id)
}

fn owned_data_file(name: &str) -> bool {
    if name.strip_suffix(".bak").is_some_and(|base| {
        base == "tool-pins.json"
            || base
                .strip_prefix("tool-pins-v2-")
                .and_then(|s| s.strip_suffix(".json"))
                .is_some_and(|s| hex_name(s, 64))
    }) || name
        .strip_prefix("ui-settings.json.corrupt-")
        .is_some_and(|suffix| {
            let parts: Vec<_> = suffix.split('-').collect();
            parts.len() == 3 && parts.iter().all(|part| decimal(part))
        })
    {
        return true;
    }
    if DATA_FILES.contains(&name)
        || matches!(
            name,
            "gateway.log.1" | "daemon.log.1" | "tool-cache.servers.json"
        )
    {
        return true;
    }
    if let Some(suffix) = name.strip_prefix("registry.json.") {
        return matches!(suffix, "bak" | "bak.seq")
            || suffix.strip_prefix("bak.").is_some_and(decimal)
            || suffix
                .strip_prefix('v')
                .and_then(|s| s.strip_suffix(".bak"))
                .is_some_and(|s| {
                    s.split_once('-').is_some_and(|(v, ms)| {
                        decimal(v) && v.parse::<u32>().is_ok() && decimal(ms)
                    })
                })
            || suffix.strip_prefix("unreadable-").is_some_and(|s| {
                decimal(s)
                    || s.strip_prefix("sha256-")
                        .is_some_and(|hash| hex_name(hash, 64))
            });
    }
    if name
        .strip_prefix("daemon-")
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|s| hex_name(s, 64))
        || name
            .strip_prefix("team-activity-")
            .and_then(|s| s.strip_suffix(".json"))
            .is_some_and(|s| hex_name(s, 32))
    {
        return true;
    }
    ["tool-cache-", "tool-pins-", "quarantine-"]
        .iter()
        .any(|prefix| {
            let Some(suffix) = name.strip_prefix(prefix) else {
                return false;
            };
            let key = suffix
                .strip_suffix(".json")
                .and_then(|s| s.strip_prefix("v2-"));
            if let Some(key) = key {
                let key = if *prefix == "tool-cache-" {
                    key.strip_suffix(".servers").unwrap_or(key)
                } else {
                    key
                };
                return hex_name(key, 64);
            }
            // Legacy names are ambiguous without the registry's actual profile refs.
            false
        })
}

fn gateway_file(name: &str) -> bool {
    if name == "gateway-manifest.json" {
        return true;
    }
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    ["toolport-gateway", "conduit-gateway"].iter().any(|base| {
        if stem == *base {
            return true;
        }
        let Some(suffix) = stem.strip_prefix(base).and_then(|s| s.strip_prefix('-')) else {
            return false;
        };
        if [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ]
        .contains(&suffix)
        {
            return true;
        }
        // Published versions, including prerelease versions and a 12-hex digest.
        let core = suffix.split(['-', '+']).next().unwrap_or_default();
        core.split('.').count() == 3
            && core.split('.').all(decimal)
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    })
}

fn bin_file(name: &str) -> bool {
    if generated_file_name(name, gateway_file) {
        return true;
    }
    if let Some(base) = name.strip_prefix('.').and_then(|s| s.strip_suffix(".tmp")) {
        return base.rsplit_once('.').is_some_and(|(base, stamp)| {
            decimal(stamp)
                && base
                    .rsplit_once('.')
                    .is_some_and(|(base, pid)| decimal(pid) && gateway_file(base))
        });
    }
    // clients::refresh_legacy_gateway uses with_extension(<128-bit id>.tmp).
    name.strip_suffix(".tmp")
        .and_then(|s| s.rsplit_once('.'))
        .is_some_and(|(base, id)| hex_name(id, 32) && gateway_file(base))
}

pub(crate) fn owned_backup_file(client: &crate::clients::PurgeBackupClient, name: &str) -> bool {
    generated_file_name(name, |name| backup_file(client, name))
}

fn backup_file(client: &crate::clients::PurgeBackupClient, name: &str) -> bool {
    name == "moved-servers.json"
        || name
            .strip_prefix("original-")
            .or_else(|| name.strip_prefix("released-"))
            .and_then(|s| s.strip_suffix(".json"))
            .is_some_and(|s| hex_name(s, 64))
        || name.split_once('-').is_some_and(|(stamp, basename)| {
            decimal(stamp) && stamp.parse::<u128>().is_ok() && client.owns_basename(basename)
        })
}

fn export_file(name: &str) -> bool {
    [
        ("rules-", ".md"),
        ("routines-", ".json"),
        ("agent-permissions-", ".json"),
    ]
    .iter()
    .any(|(prefix, ext)| {
        let Some(date) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(ext)) else {
            return false;
        };
        let parts: Vec<_> = date.split('-').collect();
        (parts.len() == 3 || parts.len() == 4)
            && parts[0].len() == 4
            && decimal(parts[0])
            && parts[1].len() == 2
            && parts[1].parse::<u8>().is_ok_and(|m| (1..=12).contains(&m))
            && parts[2].len() == 2
            && parts[2].parse::<u8>().is_ok_and(|d| (1..=31).contains(&d))
            && (parts.len() == 3
                || decimal(parts[3]) && parts[3].parse::<u32>().is_ok_and(|n| n >= 2))
    })
}

fn pid_json(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|s| decimal(s) && s.parse::<u32>().is_ok())
}

fn oauth_file(name: &str) -> bool {
    if let Some(hash) = name
        .strip_prefix("auth-write-")
        .and_then(|s| s.strip_suffix(".lock"))
    {
        return hex_name(hash, 64);
    }
    [".lock", ".owner"]
        .iter()
        .any(|ext| name.strip_suffix(ext).is_some_and(|s| hex_name(s, 64)))
}

fn vector_file(name: &str) -> bool {
    hex_name(name, 64)
        || name.strip_suffix(".tmp").is_some_and(|base| {
            base.rsplit_once('.').is_some_and(|(base, seq)| {
                decimal(seq)
                    && base
                        .rsplit_once('.')
                        .is_some_and(|(hash, pid)| hex_name(hash, 64) && decimal(pid))
            })
        })
}

#[derive(PartialEq)]
enum DataEntry {
    File,
    Directory,
}

#[cfg(test)]
fn owned_data_entry(relative: &Path) -> Option<DataEntry> {
    owned_data_entry_with_legacy(relative, &Default::default())
}

#[cfg(test)]
fn owned_data_entry_with_legacy(
    relative: &Path,
    legacy: &std::collections::HashSet<String>,
) -> Option<DataEntry> {
    // Pure name checks must not read the location store or require a data root.
    owned_data_entry_with_backups(relative, legacy, &Default::default())
}

fn owned_data_entry_with_backups(
    relative: &Path,
    legacy: &std::collections::HashSet<String>,
    backups: &std::collections::HashMap<String, crate::clients::PurgeBackupClient>,
) -> Option<DataEntry> {
    let backup_client = |name: &str| {
        backups
            .get(name)
            .cloned()
            .or_else(|| crate::clients::purge_backup_short_location(name))
    };
    let parts: Option<Vec<_>> = relative.iter().map(|part| part.to_str()).collect();
    let parts = parts?;
    let file = match parts.as_slice() {
        [name]
            if matches!(
                *name,
                "logs"
                    | "bin"
                    | "broker"
                    | "backups"
                    | "exports"
                    | "children"
                    | "daemon-runs"
                    | "oauth-locks"
                    | "search-vectors"
            ) =>
        {
            return Some(DataEntry::Directory)
        }
        [name] => owned_data_name(name, legacy),
        ["bin", name] => bin_file(name),
        ["broker", name] => matches!(*name, "owner.lock" | "approval.sock"),
        ["backups", client] if backup_client(client).is_some() => {
            return Some(DataEntry::Directory)
        }
        ["backups", client, name] => {
            backup_client(client).is_some_and(|client| owned_backup_file(&client, name))
        }
        ["exports", name] => generated_file_name(name, export_file),
        ["children" | "daemon-runs", name] => generated_file_name(name, pid_json),
        ["oauth-locks", name] => oauth_file(name),
        ["search-vectors", name] => vector_file(name),
        // Current log writers use the data root. A generic logs/ folder is not owned.
        _ => false,
    };
    file.then_some(DataEntry::File)
}

fn legacy_store_names(dir: &Path) -> std::collections::HashSet<String> {
    // Snapshot before registry deletion, without migration or recovery writes.
    let registry = std::fs::read(dir.join("registry.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let mut names = std::collections::HashSet::new();
    if let Some(profiles) = registry
        .as_ref()
        .and_then(|value| value["profiles"].as_array())
    {
        for profile in profiles {
            for field in ["id", "name"] {
                if let Some(reference) = profile[field].as_str() {
                    let slug = crate::registry::legacy_profile_store_slug(reference);
                    if !slug.is_empty() {
                        for prefix in ["tool-cache-", "tool-pins-", "quarantine-"] {
                            names.insert(format!("{prefix}{slug}.json"));
                            if prefix == "tool-cache-" {
                                names.insert(format!("{prefix}{slug}.servers.json"));
                            }
                        }
                    }
                }
            }
        }
    }
    names
}

fn remove_data_contents(dir: &Path, keep: &[PathBuf], report: &mut Report) {
    let legacy = legacy_store_names(dir);
    let backups = crate::clients::purge_backup_inventory();
    remove_selected_contents(dir, dir, keep, &legacy, &backups, report);
}

fn remove_selected_contents(
    root: &Path,
    dir: &Path,
    keep: &[PathBuf],
    legacy: &std::collections::HashSet<String>,
    backups: &std::collections::HashMap<String, crate::clients::PurgeBackupClient>,
    report: &mut Report,
) {
    let entries =
        match std::fs::read_dir(dir).and_then(|entries| entries.collect::<Result<Vec<_>, _>>()) {
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
        let path = entry.path();
        if keep.contains(&path) {
            continue;
        }
        let selected = path
            .strip_prefix(root)
            .ok()
            .and_then(|relative| owned_data_entry_with_backups(relative, legacy, backups));
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if selected == Some(DataEntry::Directory)
                    && metadata.is_dir()
                    && !is_link(&metadata) =>
            {
                remove_selected_contents(root, &path, keep, legacy, backups, report);
                if !keep.iter().any(|keep| keep.starts_with(&path)) {
                    remove_empty_dirs(&path, report);
                }
            }
            Ok(metadata)
                if selected == Some(DataEntry::File)
                    && !metadata.is_dir()
                    && !is_link(&metadata) =>
            {
                remove_path(&path, report)
            }
            Ok(_) => report
                .preserved
                .push(format!("{} (not a Toolport-created entry)", path.display())),
            Err(error) => report.leftovers.push(Leftover {
                path: path.display().to_string(),
                error: error.to_string(),
            }),
        }
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
                    Ok(kind)
                        if kind.is_dir()
                            && std::fs::symlink_metadata(&path)
                                .is_ok_and(|metadata| !is_link(&metadata)) =>
                    {
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
    match std::fs::read_dir(dir) {
        Ok(mut entries) => match entries.next() {
            None => remove_path(dir, report),
            Some(Ok(_)) => {} // Selected cleanup already reported preserved children.
            Some(Err(error)) => report.leftovers.push(Leftover {
                path: dir.display().to_string(),
                error: error.to_string(),
            }),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => report.leftovers.push(Leftover {
            path: dir.display().to_string(),
            error: error.to_string(),
        }),
    }
}

/// A LaunchAgent in the shape tauri-plugin-autostart writes for Toolport: its
/// Label is `name` and it starts the app with `--hidden`.
#[cfg(any(target_os = "macos", test))]
fn owned_launch_agent(contents: &str, name: &str) -> bool {
    contents.contains(&format!("<string>{name}</string>"))
        && contents.contains("<string>--hidden</string>")
}

/// The program a LaunchAgent starts: the first `ProgramArguments` string.
#[cfg(any(target_os = "macos", test))]
fn launch_agent_program(contents: &str) -> Option<PathBuf> {
    let (_, after) = contents.split_once("<key>ProgramArguments</key>")?;
    let array = after.trim_start().strip_prefix("<array>")?;
    let first = array.trim_start().strip_prefix("<string>")?;
    let (value, _) = first.split_once("</string>")?;
    let value = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// The installed app an executable belongs to: its outermost `.app` bundle, or
/// its folder when it is not in one.
#[cfg(any(target_os = "macos", test))]
fn install_root(exe: &Path) -> &Path {
    exe.ancestors()
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
        .last()
        .unwrap_or_else(|| exe.parent().unwrap_or(exe))
}

/// Whether `path` is inside this user's Trash: `~/.Trash` on the startup volume,
/// or `/Volumes/<volume>/.Trashes/<uid>` on another mounted volume. A `.Trashes`
/// folder anywhere else (a copied backup) is not a Trash.
#[cfg(any(target_os = "macos", test))]
fn in_trash(path: &Path, home: &Path, uid: u32) -> bool {
    if path.starts_with(home.join(".Trash")) {
        return true;
    }
    let uid = uid.to_string();
    let mut parts = path.components();
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (
            Some(std::path::Component::RootDir),
            Some(volumes),
            Some(std::path::Component::Normal(_)),
            Some(trashes),
            Some(owner),
        ) if volumes.as_os_str() == "Volumes"
            && trashes.as_os_str() == ".Trashes"
            && owner.as_os_str() == uid.as_str()
    )
}

/// Whether a daemon whose executable `removed_exe` is gone may remove this
/// launch-at-login plist. It must be Toolport's own (the uninstaller's
/// ownership check), start a program inside the same app as that executable,
/// that program must no longer exist, and there must be positive evidence the
/// app was removed rather than replaced: `now_at`, where the running image is
/// now (it follows a rename), is inside a Trash folder. An updater or Homebrew
/// moving the bundle aside, an app that is just missing, or an unknown location
/// all keep the plist. `exists` should answer true when it cannot tell.
#[cfg(any(target_os = "macos", test))]
fn stale_launch_agent(
    contents: &str,
    name: &str,
    removed_exe: &Path,
    now_at: Option<&Path>,
    home: &Path,
    uid: u32,
    exists: impl Fn(&Path) -> bool,
) -> bool {
    if !owned_launch_agent(contents, name) {
        return false;
    }
    let Some(program) = launch_agent_program(contents) else {
        return false;
    };
    install_root(&program) == install_root(removed_exe)
        && !exists(&program)
        && now_at.is_some_and(|path| in_trash(path, home, uid))
}

/// Where this process's executable is now. macOS resolves `proc_pidpath` from the
/// open image's vnode, so it follows a rename (into the Trash, or aside by an
/// updater); `current_exe` would keep the launch path. `None` once the file is
/// deleted or the path cannot be read.
#[cfg(target_os = "macos")]
fn running_image_path() -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buf is valid for writes of buf.len() bytes for the call.
    let len = unsafe {
        libc::proc_pidpath(
            std::process::id() as libc::c_int,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// The daemon confirmed its own app is gone: if the app is in the Trash, remove
/// Toolport's launch-at-login LaunchAgent that would start it, and unload it, so a
/// drag to the Trash with Launch at login on leaves nothing behind. Only plists
/// [`stale_launch_agent`] accepts are touched; a bundle moved anywhere else (an
/// update in progress) keeps its plist.
#[cfg(target_os = "macos")]
pub fn remove_stale_launch_agents(removed_exe: &Path) -> Report {
    let mut report = Report::default();
    let Some(home) = home_dir() else {
        return report;
    };
    let now_at = running_image_path();
    // SAFETY: getuid has no failure mode.
    let uid = unsafe { libc::getuid() };
    for (path, name) in autostart_files(&home).iter().zip(AUTOSTART_NAMES) {
        match std::fs::read_to_string(path) {
            Ok(contents)
                if stale_launch_agent(
                    &contents,
                    name,
                    removed_exe,
                    now_at.as_deref(),
                    &home,
                    uid,
                    |program| program.try_exists().unwrap_or(true),
                ) =>
            {
                remove_path(path, &mut report);
                unload_launch_agent(name);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => report.leftovers.push(Leftover {
                path: path.display().to_string(),
                error: error.to_string(),
            }),
        }
    }
    report
}

/// Best effort `launchctl bootout gui/<uid>/<label>`. A plist that was never
/// loaded (the usual case before the next login) just makes it fail.
#[cfg(target_os = "macos")]
fn unload_launch_agent(label: &str) {
    let uid = unsafe { libc::getuid() };
    let Ok(mut child) = std::process::Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}/{label}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn owned_autostart(contents: &str, name: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        owned_launch_agent(contents, name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        contents.lines().any(|line| line == format!("Name={name}"))
            && contents
                .lines()
                .any(|line| line == format!("Comment={name}startup script"))
    }
}

/// Remove only Toolport's own launch-at-login entries, for an uninstaller
/// that keeps data. Changed or foreign entries are preserved and listed.
pub fn remove_startup_entries() -> Result<Report, String> {
    let mut report = Report::default();
    remove_autostart(&mut report)?;
    Ok(report)
}

fn remove_autostart(report: &mut Report) -> Result<(), String> {
    startup_entries(true, report)
}

fn startup_entries(remove: bool, report: &mut Report) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_autostart::purge_entries(AUTOSTART_NAMES, remove, report);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = home_dir().ok_or("Could not resolve your home directory")?;
        startup_files(autostart_files(&home), remove, report);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn startup_files(files: Vec<PathBuf>, remove: bool, report: &mut Report) {
    for (path, name) in files.iter().zip(AUTOSTART_NAMES) {
        match std::fs::read_to_string(path) {
            Ok(contents) if owned_autostart(&contents, name) => {
                if remove {
                    remove_path(path, report);
                }
            }
            Ok(_) => {
                let preserved = format!("{} (foreign startup entry preserved)", path.display());
                if !report.preserved.contains(&preserved) {
                    report.preserved.push(preserved);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => report.leftovers.push(Leftover {
                path: path.display().to_string(),
                error: error.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ui_settings_and_pin_recovery_files() {
        let legacy = Default::default();
        for name in [
            "ui-settings.json",
            "ui-settings.lock",
            "gtk-skills-project.json",
            "ui-settings.json.123.1.conduit-tmp",
            "ui-settings.json.corrupt-123-12-0",
            "tool-pins.json.bak",
            &format!("tool-pins-v2-{}.json.bak", "a".repeat(64)),
        ] {
            assert!(owned_data_name(name, &legacy), "{name}");
        }
        for name in [
            "ui-settings-notes.json",
            "tool-pins-custom.json.bak",
            "ui-settings.json.corrupt-notes",
        ] {
            assert!(!owned_data_name(name, &legacy), "{name}");
        }
    }
    #[test]
    fn review_r4_purge_removes_only_exact_unreadable_pin_copies() {
        let env = crate::registry::DataDirTestEnv::new("purge-unreadable-pins");
        let profile = format!("tool-pins-v2-{}.json", "a".repeat(64));
        let owned = [
            "tool-pins.json.unreadable-123".to_string(),
            format!("{profile}.unreadable-456"),
        ];
        let preserved = [
            "tool-pins.json.unreadable-notes".to_string(),
            "tool-pins.json.unreadable-123.txt".to_string(),
            "tool-pins-user.json.unreadable-789".to_string(),
            format!("tool-pins-v2-{}.json.unreadable-1", "g".repeat(64)),
        ];
        for name in owned.iter().chain(&preserved) {
            std::fs::write(env.dir.join(name), "bytes").unwrap();
        }
        let mut report = Report::default();
        remove_data_contents(&env.dir, &[], &mut report);
        for name in owned {
            assert!(!env.dir.join(name).exists());
        }
        for name in preserved {
            assert!(env.dir.join(name).exists());
        }
    }

    #[test]
    fn purge_r7_location_backups_use_writer_names_and_preserve_collisions() {
        let env = crate::registry::DataDirTestEnv::new("purge_r7_locations");
        let home = env.dir.join("home");
        std::fs::create_dir(&home).unwrap();
        let _home = crate::clients::TestHome::set(&home);
        let mut backups = Vec::new();
        let mut configs = Vec::new();
        for (kind, folder, basename) in [
            ("claude-code", ".claude2".to_owned(), ".claude.json"),
            (
                "claude-code",
                format!(".claude-{}", "x".repeat(210)),
                ".claude.json",
            ),
            (
                "claude-code",
                format!(".claude-{}", "é".repeat(100)),
                ".claude.json",
            ),
            ("codex", "manual-codex".to_owned(), "config.toml"),
        ] {
            let folder = home.join(folder);
            std::fs::create_dir(&folder).unwrap();
            let config = folder.join(basename);
            std::fs::write(
                &config,
                if kind == "codex" {
                    r#"model = "fixture""#
                } else {
                    r#"{"mcpServers":{}}"#
                },
            )
            .unwrap();
            let id = crate::clients::add_client_location(kind, folder.to_str().unwrap()).unwrap();
            let write = crate::clients::write_servers(&id, &[]).unwrap();
            backups.push(PathBuf::from(write.backup.unwrap()));
            configs.push((config.clone(), std::fs::read(&config).unwrap()));
            let dir = crate::clients::backup_dir(&id).unwrap();
            std::fs::write(dir.join("2024-taxes.pdf"), "user bytes").unwrap();
            // A plausible generated basename with a foreign extension stays.
            std::fs::write(dir.join("123-config.toml.notes"), "user bytes").unwrap();
        }
        let long_dir = backups[1].parent().unwrap();
        let component = long_dir.file_name().unwrap().to_str().unwrap();
        assert_eq!(component.len(), 185);
        let wrong_hash =
            env.dir
                .join("backups")
                .join(format!("{}-{}", &component[..120], "0".repeat(64)));
        std::fs::create_dir(&wrong_hash).unwrap();
        std::fs::write(wrong_hash.join("123-.claude.json"), "user bytes").unwrap();
        let lookalike = env.dir.join("client-locations.json.notes");
        std::fs::write(&lookalike, "user bytes").unwrap();
        // Store and backup atomic-write siblings use exactly the same grammar.
        for name in [
            "client-locations.json.lock",
            "client-locations.json.123.1.conduit-tmp",
        ] {
            std::fs::write(env.dir.join(name), "generated").unwrap();
        }
        let snapshot = crate::clients::purge_backup_inventory();
        std::fs::remove_file(env.dir.join("client-locations.json")).unwrap();
        assert!(
            owned_data_entry_with_backups(
                backups[1].strip_prefix(&env.dir).unwrap(),
                &Default::default(),
                &snapshot
            )
            .is_some(),
            "snapshot survives store deletion"
        );
        // Restore the writer's inventory for the full removal path.
        for (config, _) in &configs {
            crate::clients::add_client_location(
                if config.file_name().unwrap() == "config.toml" {
                    "codex"
                } else {
                    "claude-code"
                },
                config.parent().unwrap().to_str().unwrap(),
            )
            .unwrap();
        }
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert!(!env.dir.join("client-locations.json").exists());
        for backup in backups {
            assert!(!backup.exists(), "{}", backup.display());
            let dir = backup.parent().unwrap();
            assert_eq!(
                std::fs::read(dir.join("2024-taxes.pdf")).unwrap(),
                b"user bytes"
            );
            assert_eq!(
                std::fs::read(dir.join("123-config.toml.notes")).unwrap(),
                b"user bytes"
            );
            assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2);
        }
        assert_eq!(
            std::fs::read(wrong_hash.join("123-.claude.json")).unwrap(),
            b"user bytes"
        );
        assert_eq!(std::fs::read(lookalike).unwrap(), b"user bytes");
        for (config, bytes) in configs {
            assert_eq!(std::fs::read(config).unwrap(), bytes);
        }
    }

    #[test]
    fn purge_long_location_backup_failure_retains_inventory_for_retry() {
        let env = crate::registry::DataDirTestEnv::new("purge_long_location_retry");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let home = env.dir.join("home");
        let folder = home.join(format!(".claude-{}", "x".repeat(210)));
        std::fs::create_dir_all(&folder).unwrap();
        let _home = crate::clients::TestHome::set(&home);
        let config = folder.join(".claude.json");
        std::fs::write(&config, "{}").unwrap();
        let id =
            crate::clients::add_client_location("claude-code", folder.to_str().unwrap()).unwrap();
        let write = crate::clients::write_servers(&id, &[]).unwrap();
        let backup = PathBuf::from(write.backup.unwrap());
        let backup_dir = backup.parent().unwrap();
        assert_eq!(backup_dir.file_name().unwrap().to_str().unwrap().len(), 185);
        let user_file = backup_dir.join("2024-taxes.pdf");
        std::fs::write(&user_file, "user bytes").unwrap();
        let store = env.dir.join("client-locations.json");
        let inventory = std::fs::read(&store).unwrap();
        let purge = || {
            run_with(
                |_| Vec::new(),
                || Ok(Vec::new()),
                |_| Vec::new(),
                || Ok(crate::secrets::purge::Removal::default()),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap()
        };
        REMOVE_FAILURE.with(|slot| slot.replace(Some(backup.clone())));
        let first = purge();
        REMOVE_FAILURE.with(|slot| slot.replace(None));
        assert!(first
            .leftovers
            .iter()
            .any(|leftover| leftover.path == backup.display().to_string()));
        assert!(backup.exists());
        assert_eq!(
            std::fs::read(&store).unwrap(),
            inventory,
            "retry needs the location inventory"
        );
        assert!(env.dir.join("registry.json").exists());
        let retry = purge();
        assert!(retry.leftovers.is_empty(), "{:?}", retry.leftovers);
        assert!(!backup.exists());
        assert!(!store.exists());
        assert!(!env.dir.join("registry.json").exists());
        assert_eq!(std::fs::read_dir(backup_dir).unwrap().count(), 1);
        assert_eq!(std::fs::read(user_file).unwrap(), b"user bytes");
        assert!(config.exists());
    }

    #[test]
    fn purge_location_inventory_collisions_remain_preserved() {
        for linked in [false, true] {
            if linked && !cfg!(unix) {
                continue;
            }
            let env = crate::registry::DataDirTestEnv::new("purge_location_collision");
            crate::registry::save(&crate::registry::Registry::default()).unwrap();
            let store = env.dir.join("client-locations.json");
            let user = env.dir.join("user.json");
            std::fs::write(&user, "[]").unwrap();
            if linked {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&user, &store).unwrap();
            } else {
                std::fs::create_dir(&store).unwrap();
            }
            let report = run_with(
                |_| Vec::new(),
                || Ok(Vec::new()),
                |_| Vec::new(),
                || Ok(crate::secrets::purge::Removal::default()),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap();
            assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
            assert!(std::fs::symlink_metadata(&store).is_ok());
            assert!(report
                .preserved
                .iter()
                .any(|entry| entry.starts_with(&store.display().to_string())));
            assert_eq!(std::fs::read(user).unwrap(), b"[]");
            assert!(!env.dir.join("registry.json").exists());
        }
    }

    #[test]
    fn purge_r7_forgotten_long_location_uses_its_recovery_record() {
        let env = crate::registry::DataDirTestEnv::new("purge_r7_forgotten_long");
        let home = env.dir.join("home");
        let folder = home.join(format!(".claude-{}", "é".repeat(100)));
        std::fs::create_dir_all(&folder).unwrap();
        let _home = crate::clients::TestHome::set(&home);
        let config = folder.join(".claude.json");
        std::fs::write(&config, "{}").unwrap();
        let id =
            crate::clients::add_client_location("claude-code", folder.to_str().unwrap()).unwrap();
        let write = crate::clients::write_servers(&id, &[]).unwrap();
        let backup = PathBuf::from(write.backup.unwrap());
        let record = write.recovery_path.unwrap();
        let component = backup
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        let wrong =
            env.dir
                .join("backups")
                .join(format!("{}-{}", &component[..120], "0".repeat(64)));
        std::fs::create_dir(&wrong).unwrap();
        let copied = wrong.join(record.file_name().unwrap());
        std::fs::copy(&record, &copied).unwrap();
        crate::clients::remove_client_location(&id).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert!(
            !backup.parent().unwrap().exists(),
            "forgotten generated folder removed"
        );
        assert!(
            copied.exists(),
            "a copied valid record cannot claim another folder"
        );
    }

    #[test]
    fn purge_r7_roots_are_injectable_without_os_profile_reads() {
        let env = crate::registry::DataDirTestEnv::new("purge_r7_roots");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let root = env.dir.join("scratch");
        let _roots = crate::fixture_paths::RootGuard::set(root.clone());
        assert_eq!(home_dir(), Some(root.clone()));
        assert!(config_dir().unwrap().starts_with(&root));
        assert!(data_dir_root().unwrap().starts_with(&root));
        assert!(install_candidates(&env.dir.join("data/Toolport"))
            .into_iter()
            .all(|path| path.starts_with(&env.dir)));
        assert!(app_resources(&root)
            .into_iter()
            .all(|path| path.starts_with(&root)));
        let plan = plan().unwrap();
        assert!(Path::new(&plan.report_path).starts_with(&root));
    }

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
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(dir.join("junction"))
                .arg(&native)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "Windows junction fixture could not be created"
            );
        }
        remove_contents(&dir, &[], &mut report);
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_to_string(native.join("config.json")).unwrap(),
            "native bytes"
        );
        drop(env);
    }
    /// macOS acceptance after "Remove Toolport data": the secret scrub's marker
    /// and lock, and a lock the scrub took on a tool cache write in flight, were
    /// left in the data folder as entries Toolport did not create.
    #[test]
    fn purge_removes_the_secret_scrub_files_and_locks_on_temp_writes() {
        let env = crate::registry::DataDirTestEnv::new("purge_scrub_leftovers");
        let dir = env.dir.join("data");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = format!("tool-cache-v2-{}.json", "c".repeat(64));
        let created = [
            "secret-scrub-v1.json".to_string(),
            "secret-scrub.lock".to_string(),
            "secret-scrub-pending".to_string(),
            format!("{cache}.7842.4.conduit-tmp.lock"),
            format!("{cache}.7842.4.conduit-tmp"),
            "registry.json.123.1.conduit-tmp.lock".to_string(),
        ];
        for name in &created {
            std::fs::write(dir.join(name), "fixture").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "user bytes").unwrap();
        let mut report = Report::default();
        remove_data_contents(&dir, &[], &mut report);
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["notes.txt"]);
        // Only a lock on a temp copy of an owned file, not any doubled suffix.
        let legacy = Default::default();
        for name in [
            "secret-scrub.json",
            "registry.json.lock.lock",
            "notes.txt.123.1.conduit-tmp.lock",
            "registry.json.notes.conduit-tmp.lock",
        ] {
            assert!(!owned_data_name(name, &legacy), "{name}");
        }
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
            |_| Vec::new(),
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
            |_| Ok(()),
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
    fn fake_keyring_failure_removes_everything_else_and_keeps_the_registry_for_retry() {
        let _env = crate::registry::DataDirTestEnv::new("purge_keyring_failure");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("secrets.enc"), "secret bytes").unwrap();
        std::fs::write(dir.join("registry.json"), r#"{"servers":[]}"#).unwrap();
        std::fs::write(dir.join("tool-cache.json"), "{}").unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || {
                Ok(crate::secrets::purge::Removal {
                    removed: vec!["secret-service:/fixture/removed-item".into()],
                    absent: vec!["keychain:conduit-mcp/legacy/mock::TOKEN".into()],
                    leftovers: vec![Leftover {
                        path: "secret-service:/fixture/locked-item".into(),
                        error: "locked".into(),
                    }],
                    ..Default::default()
                })
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        // The failed item is reported exactly, the absent one apart from it.
        assert_eq!(
            report.leftovers[0].path,
            "secret-service:/fixture/locked-item"
        );
        assert_eq!(report.absent, ["keychain:conduit-mcp/legacy/mock::TOKEN"]);
        assert!(report
            .removed
            .contains(&"secret-service:/fixture/removed-item".to_string()));
        // Everything else was removed; only the registry stays, and says why.
        assert!(!dir.join("secrets.enc").exists());
        assert!(!dir.join("tool-cache.json").exists());
        assert!(dir.join("registry.json").exists());
        assert!(report
            .leftovers
            .iter()
            .any(|left| left.path.ends_with("registry.json") && left.error.contains("retry")));
        assert!(report.only_credentials_left);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["absent"][0], "keychain:conduit-mcp/legacy/mock::TOKEN");
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }

    #[test]
    fn absent_credentials_alone_complete_the_removal() {
        let _env = crate::registry::DataDirTestEnv::new("purge_keyring_absent");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), r#"{"servers":[]}"#).unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || {
                Ok(crate::secrets::purge::Removal {
                    removed: vec!["keychain:conduit-mcp/data-protection/mock::TOKEN".into()],
                    absent: vec!["keychain:conduit-mcp/legacy/mock::TOKEN".into()],
                    ..Default::default()
                })
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert!(!dir.join("registry.json").exists());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("already gone")));
    }

    #[test]
    fn active_session_prevents_secret_or_data_removal() {
        let _env = crate::registry::DataDirTestEnv::new("purge_busy_session");
        let dir = crate::registry::conduit_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), "keep").unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| {
                vec![Leftover {
                    path: "/fixture/toolport-gateway".into(),
                    error: "PID 123 is busy".into(),
                }]
            },
            || panic!("busy session"),
            |_| Ok(()),
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
    fn a_daemon_removes_only_its_own_launch_agent_once_its_app_is_gone() {
        fn plist(label: &str, program: &str, hidden: bool) -> String {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n  <dict>\n  \
                 <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  \
                 <array><string>{program}</string>{}</array>\n  <key>RunAtLoad</key>\n  <true/>\n  \
                 </dict>\n</plist>",
                if hidden {
                    "<string>--hidden</string>"
                } else {
                    ""
                }
            )
        }
        let trashed = Path::new("/Applications/Toolport.app/Contents/MacOS/toolport-gateway");
        let app = "/Applications/Toolport.app/Contents/MacOS/Toolport";
        let second = "/Users/me/Applications/Toolport.app/Contents/MacOS/Toolport";
        let gone = |_: &Path| false;
        let present = |_: &Path| true;
        let only_app_gone = |path: &Path| path != Path::new(app);
        let other = "/Applications/Other.app/Contents/MacOS/Other";
        let no_program =
            "<plist><dict><key>Label</key><string>Toolport</string><string>--hidden</string></dict></plist>";
        type Exists<'a> = &'a dyn Fn(&Path) -> bool;
        // (case, plist, does its program exist, remove it)
        let cases: [(&str, String, Exists, bool); 7] = [
            ("its app is gone", plist("Toolport", app, true), &gone, true),
            ("reinstalled", plist("Toolport", app, true), &present, false),
            ("second copy", plist("Toolport", second, true), &gone, false),
            ("another app", plist("Other", other, true), &gone, false),
            ("not hidden", plist("Toolport", app, false), &gone, false),
            (
                "only it is gone",
                plist("Toolport", app, true),
                &only_app_gone,
                true,
            ),
            ("no program", no_program.into(), &gone, false),
        ];
        let home = Path::new("/Users/me");
        let in_trash = Path::new("/Users/me/.Trash/Toolport.app/Contents/MacOS/toolport-gateway");
        for (case, contents, exists, remove) in cases {
            assert_eq!(
                stale_launch_agent(
                    &contents,
                    "Toolport",
                    trashed,
                    Some(in_trash),
                    home,
                    501,
                    exists
                ),
                remove,
                "{case}"
            );
        }
        // R2-2: only a running image now inside a Trash folder is evidence of
        // removal. An updater's or Homebrew's move aside, a missing file and an
        // unknown location keep the plist.
        let ours = plist("Toolport", app, true);
        for (now_at, remove) in [
            (Some("/Volumes/Data/.Trashes/501/Toolport.app/Contents/MacOS/toolport-gateway"), true),
            (Some("/Volumes/Data/.Trashes/502/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            // R3-4: a `.Trashes` folder outside a mounted volume's root is not a Trash.
            (Some("/Users/me/backup/.Trashes/501/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            (Some("/Volumes/Data/backup/.Trashes/501/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            (Some("/Applications/Toolport.app.old/Contents/MacOS/toolport-gateway"), false),
            (Some("/private/var/folders/xy/T/tauri_updated_app/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            (Some("/opt/homebrew/Caskroom/toolport/2.3.5/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            (Some("/Users/me/Downloads/Toolport.app/Contents/MacOS/toolport-gateway"), false),
            (None, false),
        ] {
            assert_eq!(
                stale_launch_agent(&ours, "Toolport", trashed, now_at.map(Path::new), home, 501, gone),
                remove,
                "{now_at:?}"
            );
        }
        // A gateway outside any bundle owns only plists in its own folder.
        let loose = Path::new("/opt/toolport/toolport-gateway");
        let loose_trash = Path::new("/Users/me/.Trash/toolport/toolport-gateway");
        assert!(stale_launch_agent(
            &plist("Toolport", "/opt/toolport/Toolport", true),
            "Toolport",
            loose,
            Some(loose_trash),
            home,
            501,
            gone
        ));
        assert!(!stale_launch_agent(
            &plist("Toolport", app, true),
            "Toolport",
            loose,
            Some(loose_trash),
            home,
            501,
            gone
        ));
        assert_eq!(
            launch_agent_program(&plist(
                "Toolport",
                "/Applications/A &amp; B.app/Contents/MacOS/Toolport",
                true
            )),
            Some(PathBuf::from(
                "/Applications/A & B.app/Contents/MacOS/Toolport"
            ))
        );
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
            |_| Vec::new(),
            || {
                Ok(vec![crate::clients::DisconnectResult {
                    client_id: "fixture".into(),
                    path: "/fixture/edited-client.json".into(),
                    dry_run: false,
                    error: None,
                    warnings: vec![
                        "Toolport kept the toolport entry you edited. Remove it by hand if you uninstall Toolport.".into(),
                    ],
                }])
            },
            |_| panic!("connection remains"),
            || panic!("connection remains"),
            |_| Ok(()),
            |_| panic!("connection remains"),
        )
        .unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.leftovers[0].path, "/fixture/edited-client.json");
    }
    #[test]
    fn purge_named_override_requires_registry_evidence() {
        let env = crate::registry::DataDirTestEnv::new("purge_named_override");
        let dir = env.dir.join("Toolport");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("personal.txt"), "keep").unwrap();
        assert!(verify_ownership(&dir, true).is_err());
        std::fs::write(
            dir.join("registry.json"),
            r#"{"version":1,"servers":[],"profiles":[]}"#,
        )
        .unwrap();
        assert!(verify_ownership(&dir, true).is_ok());
    }

    #[test]
    fn purge_preflight_blocks_before_disconnect_or_any_deletion() {
        let env = crate::registry::DataDirTestEnv::new("purge_read_only_preflight");
        let report = run_with(
            |_| {
                vec![Leftover {
                    path: "busy daemon".into(),
                    error: "active listener".into(),
                }]
            },
            || panic!("must not disconnect"),
            |_| panic!("must not stop"),
            || panic!("must not delete credentials"),
            |_| Ok(()),
            |_| panic!("must not delete startup"),
        )
        .unwrap();
        assert!(report.clients.is_empty() && report.removed.is_empty());
        assert_eq!(report.leftovers[0].path, "busy daemon");
        assert!(env.dir.exists());
    }

    #[test]
    fn purge_startup_inventory_failure_keeps_clients_and_credentials() {
        let _env = crate::registry::DataDirTestEnv::new("purge_startup_inventory");
        let report = run_with(
            |_| Vec::new(),
            || panic!("must not disconnect"),
            |_| panic!("must not stop"),
            || panic!("must not delete credentials"),
            |report| {
                report.leftovers.push(Leftover {
                    path: "startup".into(),
                    error: "unreadable".into(),
                });
                Ok(())
            },
            |_| panic!("must not delete startup"),
        )
        .unwrap();
        assert!(report.removed.is_empty() && report.clients.is_empty());
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn purge_foreign_startup_does_not_block_data_removal() {
        let env = crate::registry::DataDirTestEnv::new("purge_foreign_startup");
        let home = env.dir.with_extension("fake-home");
        let files = autostart_files(&home);
        std::fs::create_dir_all(files[0].parent().unwrap()).unwrap();
        std::fs::write(&files[0], "foreign startup entry").unwrap();
        let data = env.dir.clone();
        // The startup file lives outside the selected data folder.
        std::fs::write(data.join("secrets.enc"), "fixture").unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |report| {
                startup_files(files.clone(), false, report);
                Ok(())
            },
            |report| {
                startup_files(files.clone(), true, report);
                Ok(())
            },
        )
        .unwrap();
        assert!(report.leftovers.is_empty());
        assert_eq!(report.preserved.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&files[0]).unwrap(),
            "foreign startup entry"
        );
        assert!(!data.join("secrets.enc").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn purge_startup_deletion_failure_precedes_credentials() {
        let env = crate::registry::DataDirTestEnv::new("purge_startup_delete");
        std::fs::write(env.dir.join("registry.json"), "keep").unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || panic!("must not delete credentials"),
            |_| Ok(()),
            |report| {
                report.leftovers.push(Leftover {
                    path: "startup".into(),
                    error: "denied".into(),
                });
                Ok(())
            },
        )
        .unwrap();
        assert!(report.removed.is_empty());
        assert!(env.dir.join("registry.json").exists());
    }

    #[test]
    fn purge_install_inventory_protects_main_shared_ids_and_team_token() {
        let env = crate::registry::DataDirTestEnv::new("purge_installs");
        let dev = env.dir.join("Toolport-dev");
        let main = env.dir.join("Toolport");
        let legacy = env.dir.join("Conduit");
        for dir in [&dev, &main, &legacy] {
            std::fs::create_dir(dir).unwrap();
        }
        std::fs::write(
            dev.join("registry.json"),
            r#"{"servers":[{"id":"shared"},{"id":"dev-only"}],"team":{}}"#,
        )
        .unwrap();
        std::fs::write(
            main.join("registry.json"),
            r#"{"servers":[{"id":"shared"}]}"#,
        )
        .unwrap();
        std::fs::write(legacy.join("registry.json"), r#"{"servers":[]}"#).unwrap();
        let others = other_installs(&dev, vec![dev.clone(), main.clone(), legacy.clone()]);
        let candidates = install_candidates(&dev);
        assert!(candidates.contains(&main) && candidates.contains(&legacy));
        assert!(others.contains(&main) && others.contains(&legacy));
        let scope = credential_scope(&dev, &others, false).unwrap();
        assert_eq!(
            scope.server_ids,
            std::collections::HashSet::from([
                "dev-only".into(),
                crate::clients::backup_storage::install_account(&dev)
            ])
        );
        assert!(!scope.all_installs);
        let scope = credential_scope(&dev, &[], false).unwrap();
        assert!(!scope.all_installs);
        assert_eq!(
            scope.server_ids,
            std::collections::HashSet::from([
                "shared".into(),
                "dev-only".into(),
                crate::clients::backup_storage::install_account(&dev)
            ])
        );
        assert!(credential_scope(&dev, &others, true).unwrap().all_installs);
        std::fs::remove_file(main.join("registry.json")).unwrap();
        assert!(credential_scope(&dev, &others, false).is_err());
    }

    #[test]
    fn purge_app_storage_inventory_uses_bundle_id() {
        let home = Path::new("/fixture/home");
        let paths = app_resources(home);
        assert!(!paths.is_empty());
        assert!(paths
            .iter()
            .all(|path| path.to_string_lossy().contains("com.tsout.conduit")));
        #[cfg(target_os = "macos")]
        assert_eq!(paths.len(), 6);
    }

    #[test]
    fn purge_owned_app_storage_is_removed_and_shared_storage_is_reported() {
        let env = crate::registry::DataDirTestEnv::new("purge_app_storage");
        let path = env.dir.join("com.tsout.conduit");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("cookies"), "fixture").unwrap();
        let mut report = Report::default();
        remove_resources(vec![path.clone()], true, &mut report);
        assert!(path.join("cookies").exists());
        assert_eq!(report.preserved.len(), 1);
        remove_resources(vec![path.clone()], false, &mut report);
        assert!(!path.exists());
        assert!(report.leftovers.is_empty());
    }

    #[test]
    fn purge_data_names_preserve_user_files_after_first_run() {
        let env = crate::registry::DataDirTestEnv::new("purge_data_names");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        for name in [
            "personal.txt",
            "registry.json.owner-notes",
            "registry.json.bak.notes",
            "daemon-notes",
            "quarantine-notes.txt",
        ] {
            std::fs::write(env.dir.join(name), "keep").unwrap();
        }
        std::fs::create_dir_all(env.dir.join("documents/broker")).unwrap();
        std::fs::write(env.dir.join("documents/broker/notes"), "keep nested").unwrap();
        for name in [
            "gtk-skills-project.json",
            "registry.json.bak",
            "registry.json.bak.123",
            "registry.json.v2-123.bak",
            "registry.json.123.1.conduit-tmp",
            "registry.json.unreadable-123",
            "registry.personal-sync-status.json",
            "daemon-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "tool-cache-v2-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "team-activity-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json.lock",
            "secret-service.lock",
            "downstream_backoff.json",
            "agent-guard-removed",
        ] {
            std::fs::write(env.dir.join(name), "fixture").unwrap();
        }
        for name in ["bin/toolport-gateway", "bin/gateway-manifest.json", "broker/approval.sock",
            "backups/claude-code/123-.claude.json", "backups/claude-code/moved-servers.json",
            "backups/claude-code/original-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "exports/rules-2026-10-11.md", "exports/routines-2026-10-11-2.json", "exports/agent-permissions-2026-10-11.json",
            "children/123.json", "daemon-runs/123.json",
            "oauth-locks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.lock",
            "oauth-locks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.owner",
            "oauth-locks/auth-write-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.lock",
            "search-vectors/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
            let path = env.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fixture").unwrap();
        }
        std::fs::create_dir(env.dir.join("logs")).unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        assert_eq!(report.preserved.len(), 6);
        assert!(!env.dir.join("registry.json").exists());
        assert!(!env.dir.join("secret-service.lock").exists());
        assert_eq!(
            std::fs::read_to_string(env.dir.join("documents/broker/notes")).unwrap(),
            "keep nested"
        );
        assert_eq!(std::fs::read_dir(&env.dir).unwrap().count(), 6);
    }

    #[test]
    fn purge_corrupt_registry_requires_global_scope_and_names_the_file() {
        let env = crate::registry::DataDirTestEnv::new("purge_corrupt_registry");
        let path = env.dir.join("registry.json");
        std::fs::write(&path, "broken").unwrap();
        let error = credential_scope(&env.dir, &[], false).err().unwrap();
        assert!(error.contains(path.to_str().unwrap()), "{error}");
        assert!(credential_scope(&env.dir, &[], true).unwrap().all_installs);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    }

    #[test]
    fn purge_global_credentials_require_explicit_consent() {
        let env = crate::registry::DataDirTestEnv::new("purge_sole_install");
        assert!(!credential_scope(&env.dir, &[], false).unwrap().all_installs);
        std::fs::write(env.dir.join("registry.json"), r#"{"servers":[]}"#).unwrap();
        assert!(!credential_scope(&env.dir, &[], false).unwrap().all_installs);
        assert!(credential_scope(&env.dir, &[], true).unwrap().all_installs);
        let other = env.dir.join("other");
        std::fs::create_dir(&other).unwrap();
        assert!(credential_scope(&env.dir, &[other], false).is_err());
    }

    #[test]
    fn purge_team_note_requires_a_deleted_team_token() {
        let env = crate::registry::DataDirTestEnv::new("purge_team_note");
        for (team_token_removed, team_token_in_file) in
            [(false, false), (true, false), (false, true)]
        {
            std::fs::write(env.dir.join("secrets.enc"), "fixture").unwrap();
            let report = run_with(
                |_| Vec::new(),
                || Ok(Vec::new()),
                |_| Vec::new(),
                || {
                    Ok(crate::secrets::purge::Removal {
                        removed: vec!["fixture credential".into()],
                        team_token_removed,
                        team_token_in_file,
                        ..Default::default()
                    })
                },
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap();
            assert!(report.leftovers.is_empty());
            assert_eq!(
                report
                    .notes
                    .iter()
                    .any(|note| note.contains("removed locally only")),
                team_token_removed || team_token_in_file
            );
        }
    }

    #[test]
    fn purge_undiscovered_custom_install_never_enables_global_credentials() {
        let env = crate::registry::DataDirTestEnv::new("purge_undiscovered");
        let selected = env.dir.join("config/Toolport");
        let custom = env.dir.join("data/toolport-work");
        for dir in [&selected, &custom] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            selected.join("registry.json"),
            r#"{"servers":[{"id":"mine"}]}"#,
        )
        .unwrap();
        std::fs::write(
            custom.join("registry.json"),
            r#"{"servers":[{"id":"work"}]}"#,
        )
        .unwrap();
        let candidates = install_candidates(&selected)
            .into_iter()
            .filter(|path| path.starts_with(&env.dir))
            .collect();
        let others = other_installs(&selected, candidates);
        assert!(!others.contains(&custom));
        let scope = credential_scope(&selected, &others, false).unwrap();
        assert!(!scope.all_installs, "discovery cannot prove sole ownership");
        assert_eq!(
            scope.server_ids,
            std::collections::HashSet::from([
                "mine".into(),
                crate::clients::backup_storage::install_account(&selected)
            ])
        );
    }

    #[test]
    fn purge_generated_path_collisions_preserve_user_bytes() {
        let env = crate::registry::DataDirTestEnv::new("purge_collisions");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let collisions = [
            "backups/tax.zip",
            "bin/personal-script",
            "logs/other-app.log",
            "daemon-notes.json",
            "broker/notes",
            "broker/nested/broker/notes",
            "children/notes.json",
            "exports/rules-notes.md",
            "oauth-locks/notes.lock",
            "search-vectors/notes",
            "daemon-runs/notes.json",
        ];
        for name in collisions {
            let path = env.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "user bytes").unwrap();
        }
        std::fs::write(env.dir.join("bin/toolport-gateway"), "gateway").unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        for name in collisions {
            assert_eq!(
                std::fs::read_to_string(env.dir.join(name)).unwrap(),
                "user bytes",
                "{name}"
            );
            // Unknown directories are reported as a whole and never traversed.
            let reported = if name == "broker/nested/broker/notes" {
                "broker/nested"
            } else {
                name
            };
            let reported_path = env.dir.join(reported);
            assert!(
                report.preserved.iter().any(|path| {
                    path.strip_suffix(" (not a Toolport-created entry)")
                        .is_some_and(|path| {
                            Path::new(path).components().eq(reported_path.components())
                        })
                }),
                "{name}: {:?}",
                report.preserved
            );
        }
        assert!(!env.dir.join("bin/toolport-gateway").exists());
        assert!(!env.dir.join("registry.json").exists());
    }

    #[test]
    fn purge_unreadable_other_registry_retains_ownership_then_retry_removes_tokens() {
        let env = crate::registry::DataDirTestEnv::new("purge_other_retry");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let selected = env.dir.join("registry.json");
        let original = br#"{"version":1,"servers":[{"id":"mine"}],"profiles":[]}"#;
        std::fs::write(&selected, original).unwrap();
        let other = env.dir.join("Toolport-dev");
        std::fs::create_dir(&other).unwrap();
        let other_registry = other.join("registry.json");
        std::fs::write(&other_registry, "broken").unwrap();
        let error = credential_scope(&env.dir, std::slice::from_ref(&other), false)
            .err()
            .expect("ownership failure must block cleanup");
        assert!(error.contains(other_registry.to_str().unwrap()), "{error}");
        assert!(error.contains("Credentials were not removed"), "{error}");
        assert_eq!(std::fs::read(&selected).unwrap(), original);
        std::fs::write(&other_registry, r#"{"servers":[{"id":"theirs"}]}"#).unwrap();
        let scope = credential_scope(&env.dir, &[other], false).unwrap();
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || {
                assert!(!scope.all_installs);
                assert!(scope.server_ids.contains("mine") && !scope.server_ids.contains("theirs"));
                Ok(crate::secrets::purge::remove_fixture_accounts(
                    &scope,
                    &["mine::TOKEN", "theirs::TOKEN"],
                ))
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.removed.contains(&"mine::TOKEN".into()));
        assert!(!report.removed.contains(&"theirs::TOKEN".into()));
        assert!(report.preserved.contains(&"theirs::TOKEN".into()));
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("may belong to another Toolport install")));
        assert!(report.leftovers.is_empty());
        assert!(!selected.exists());
    }

    #[test]
    fn purge_generated_names_match_writers_and_reject_loose_prefixes() {
        for path in ["daemon-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "daemon-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.lock", "oauth-refresh-mine.lock",
            "bin/toolport-gateway-2.0.0-beta.1", "bin/toolport-gateway-2.0.0-0123456789ab.exe",
            "bin/.toolport-gateway.123.456.tmp", "bin/toolport-gateway.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp",
            "bin/gateway-manifest.json.123.1.conduit-tmp", "tool-cache-v2-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.servers.json",
            "search-vectors/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.123.1.tmp",
            "backups/claude-code/123-claude-settings-aaaaaaaaaaaaaaaa.json.123.1.conduit-tmp"] {
            assert!(owned_data_entry(Path::new(path)).is_some(), "{path}");
        }
        for path in [
            "daemon-notes.json",
            "daemon-123.json",
            "team-activity-notes.json",
            "tool-cache-v2-work.json",
            "registry.json.notes.conduit-tmp",
            "bin/personal-script",
            "bin/toolport-gateway-notes",
            "bin/toolport-gateway.unknown.tmp",
            "backups/tax.zip",
            "backups/claude-code/tax.zip",
            "backups/personal/123-file.json",
            "logs/other-app.log",
            "broker/personal",
            "exports/rules-notes.md",
            "oauth-locks/notes.lock",
            "children/notes.json",
        ] {
            assert!(owned_data_entry(Path::new(path)).is_none(), "{path}");
        }
    }

    #[test]
    fn purge_directory_name_collisions_preserve_files_and_links() {
        let env = crate::registry::DataDirTestEnv::new("purge_directory_collisions");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        for name in ["bin", "backups"] {
            std::fs::write(env.dir.join(name), "user bytes").unwrap();
        }
        std::fs::create_dir(env.dir.join("gateway.log")).unwrap();
        std::fs::write(env.dir.join("gateway.log/personal.txt"), "nested bytes").unwrap();
        #[cfg(unix)]
        {
            std::fs::create_dir(env.dir.join("native")).unwrap();
            std::fs::write(env.dir.join("native/personal.txt"), "native bytes").unwrap();
            std::os::unix::fs::symlink(env.dir.join("native"), env.dir.join("exports")).unwrap();
            std::os::unix::fs::symlink(
                env.dir.join("native/personal.txt"),
                env.dir.join("secrets.enc"),
            )
            .unwrap();
        }
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
        for name in ["bin", "backups"] {
            assert_eq!(
                std::fs::read_to_string(env.dir.join(name)).unwrap(),
                "user bytes"
            );
            let path = env.dir.join(name).display().to_string();
            assert!(report
                .preserved
                .iter()
                .any(|entry| entry.starts_with(&path)));
        }
        assert_eq!(
            std::fs::read_to_string(env.dir.join("gateway.log/personal.txt")).unwrap(),
            "nested bytes"
        );
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(env.dir.join("exports"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(std::fs::symlink_metadata(env.dir.join("secrets.enc"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_to_string(env.dir.join("native/personal.txt")).unwrap(),
                "native bytes"
            );
        }
    }

    #[test]
    fn purge_r4_leftover_folder_and_case_insensitive_checkout_are_not_installs() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_candidates");
        let selected = env.dir.join("toolport-data");
        let dev = env.dir.join("Toolport-dev");
        let checkout = env.dir.join("toolport");
        for dir in [&selected, &dev, &checkout] {
            std::fs::create_dir(dir).unwrap();
        }
        std::fs::write(
            selected.join("registry.json"),
            r#"{"servers":[{"id":"mine"}]}"#,
        )
        .unwrap();
        std::fs::create_dir(dev.join("logs")).unwrap();
        std::fs::write(dev.join("logs/other-app.log"), "user bytes").unwrap();
        std::fs::write(checkout.join("Cargo.toml"), "git checkout").unwrap();
        // Supplying the lowercase candidate models case-insensitive discovery.
        let others = other_installs(&selected, vec![dev.clone(), checkout]);
        assert!(
            others.is_empty(),
            "leftovers and a checkout have no registry: {others:?}"
        );
        assert!(credential_scope(&selected, &others, false)
            .unwrap()
            .server_ids
            .contains("mine"));
        for evidence in [
            "registry.json.bak",
            "registry.json.123.1.conduit-tmp",
            "registry.json.unreadable-123",
        ] {
            std::fs::write(dev.join(evidence), "recovery bytes").unwrap();
            let others = other_installs(&selected, vec![dev.clone()]);
            assert_eq!(others.as_slice(), std::slice::from_ref(&dev));
            assert!(credential_scope(&selected, &others, false).is_err());
            std::fs::remove_file(dev.join(evidence)).unwrap();
        }
        std::fs::create_dir(dev.join("registry.json")).unwrap();
        assert_eq!(other_installs(&selected, vec![dev.clone()]), [dev]);
    }

    #[test]
    fn purge_r4_settings_plan_rejects_unreadable_ownership_without_changes() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_plan");
        let selected = env.dir.join("Toolport");
        let other = env.dir.join("Toolport-dev");
        for dir in [&selected, &other] {
            std::fs::create_dir(dir).unwrap();
        }
        let bytes = br#"{"version":1,"servers":[{"id":"mine"}],"profiles":[]}"#;
        std::fs::write(selected.join("registry.json"), bytes).unwrap();
        std::fs::write(selected.join("secrets.enc"), "encrypted fixture").unwrap();
        let broken = other.join("registry.json");
        std::fs::write(&broken, "unreadable registry").unwrap();
        let _selected = crate::registry::DataDirOverride::set(&selected);
        let error = plan().expect_err("both Settings shells must reject the plan before exit");
        assert!(error.contains(broken.to_str().unwrap()), "{error}");
        assert_eq!(
            std::fs::read(selected.join("registry.json")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read_to_string(selected.join("secrets.enc")).unwrap(),
            "encrypted fixture"
        );
        assert!(
            !selected.join("broker").exists(),
            "planning must not start cleanup"
        );
    }

    #[test]
    fn purge_r5_planning_checks_custom_directory_ownership_before_confirmation() {
        let env = crate::registry::DataDirTestEnv::new("purge_r5_plan_ownership");
        let registry = env.dir.join("registry.json");
        let bytes = br#"{"servers":[]}"#;
        std::fs::write(&registry, bytes).unwrap();
        std::fs::write(env.dir.join("secrets.enc"), "encrypted bytes").unwrap();
        let error = plan().expect_err("planning must reject a foreign registry");
        assert!(
            error.contains("ownership") && error.contains(&env.dir.display().to_string()),
            "{error}"
        );
        assert_eq!(std::fs::read(&registry).unwrap(), bytes);
        assert_eq!(
            std::fs::read(env.dir.join("secrets.enc")).unwrap(),
            b"encrypted bytes"
        );
        std::fs::remove_file(&registry).unwrap();
        for all_installs in [false, true] {
            assert!(plan_with_scope(all_installs)
                .err()
                .unwrap()
                .contains("ownership"));
        }
        // Explicit global recovery keeps the execution path's corrupt-file exception.
        std::fs::write(&registry, "{broken").unwrap();
        assert!(plan_with_scope(true).is_ok());
    }

    #[test]
    fn purge_r5_recovery_only_inventory_uses_newest_readable_registry_without_writes() {
        let env = crate::registry::DataDirTestEnv::new("purge_r5_recovery_inventory");
        std::fs::write(
            env.dir.join("registry.json"),
            r#"{"servers":[{"id":"mine"},{"id":"shared"}]}"#,
        )
        .unwrap();
        let other = env.dir.join("Toolport-dev");
        std::fs::create_dir(&other).unwrap();
        let old = other.join("registry.json.bak");
        let newest = other.join("registry.json.123.1.conduit-tmp");
        let corrupt = other.join("registry.json.unreadable-123");
        for (path, bytes, seconds) in [
            (&old, r#"{"servers":[{"id":"mine"}]}"#, 1),
            (&newest, r#"{"servers":[{"id":"shared"}]}"#, 2),
            (&corrupt, "broken", 3),
        ] {
            std::fs::write(path, bytes).unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                    ),
                )
                .unwrap();
        }
        for _ in 0..2 {
            let others = other_installs(&env.dir, vec![other.clone()]);
            let scope = credential_scope(&env.dir, &others, false)
                .expect("readable recovery ownership must converge");
            assert!(scope.server_ids.contains("mine") && !scope.server_ids.contains("shared"));
            assert!(!scope.all_installs);
            assert!(!other.join("registry.json").exists());
            assert_eq!(
                std::fs::read_to_string(&old).unwrap(),
                r#"{"servers":[{"id":"mine"}]}"#
            );
            assert_eq!(
                std::fs::read_to_string(&newest).unwrap(),
                r#"{"servers":[{"id":"shared"}]}"#
            );
            assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "broken");
        }
    }

    #[test]
    fn purge_r5_unreadable_recovery_names_existing_file_and_rename_allows_retry() {
        let env = crate::registry::DataDirTestEnv::new("purge_r5_recovery_retry");
        std::fs::write(
            env.dir.join("registry.json"),
            r#"{"servers":[{"id":"mine"}]}"#,
        )
        .unwrap();
        let other = env.dir.join("Toolport-dev");
        std::fs::create_dir(&other).unwrap();
        let leftover = other.join("registry.json.unreadable-123");
        std::fs::write(&leftover, "broken").unwrap();
        let others = other_installs(&env.dir, vec![other.clone()]);
        let error = credential_scope(&env.dir, &others, false).err().unwrap();
        assert!(
            error.contains(&leftover.display().to_string())
                && error.contains("rename")
                && !error.contains("--all-installs"),
            "{error}"
        );
        std::fs::rename(&leftover, other.join("saved-recovery.json")).unwrap();
        let others = other_installs(&env.dir, vec![other.clone()]);
        assert!(others.is_empty());
        assert!(credential_scope(&env.dir, &others, false)
            .unwrap()
            .server_ids
            .contains("mine"));
        assert_eq!(
            std::fs::read_to_string(other.join("saved-recovery.json")).unwrap(),
            "broken"
        );
    }

    #[test]
    fn purge_r5_same_plan_report_retry_preserves_spawn_failure_and_timeout_reports() {
        use std::io::Write;
        let env = crate::registry::DataDirTestEnv::new("purge_r5_report_retry");
        let path = env.dir.join("Toolport-removal-report-fixture.json");
        let report = open_removal_report(&path).unwrap();
        assert!(std::process::Command::new(env.dir.join("absent-gateway"))
            .stdout(report)
            .spawn()
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        let timeout_report = b"{\"leftovers\":[{\"error\":\"Toolport did not close\"}]}";
        let mut retry = open_removal_report(&path).expect("same-plan retry after spawn failure");
        retry.write_all(timeout_report).unwrap();
        let mut earlier_child = retry.try_clone().unwrap();
        drop(retry);
        let mut retry = open_removal_report(&path).expect("same-plan retry after child timeout");
        earlier_child.write_all(b"\n").unwrap();
        drop(earlier_child);
        retry.write_all(b"{\"removed\":[]}").unwrap();
        drop(retry);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"removed\":[]}");
        let mut archived: Vec<_> = std::fs::read_dir(&env.dir)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".previous-")
                    .then(|| std::fs::read(path).unwrap())
            })
            .collect();
        archived.sort();
        assert_eq!(
            archived,
            [b"".to_vec(), [timeout_report.as_slice(), b"\n"].concat()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn purge_r5_report_retry_refuses_symlink_without_changing_its_target() {
        let env = crate::registry::DataDirTestEnv::new("purge_r5_report_link");
        let target = env.dir.join("user.json");
        let report = env.dir.join("Toolport-removal-report-fixture.json");
        std::fs::write(&target, "user bytes").unwrap();
        std::os::unix::fs::symlink(&target, &report).unwrap();
        assert!(open_removal_report(&report).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "user bytes");
        assert!(std::fs::symlink_metadata(report)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn purge_r4_custom_directory_guidance_explains_post_removal_limit() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_guidance");
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || {
                Ok(crate::secrets::purge::Removal {
                    preserved: vec!["__conduit_master_key__".into()],
                    ..Default::default()
                })
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("custom data directory")
                    && note.contains("before")
                    && note.contains("credential manager")),
            "{:?}",
            report.notes
        );
        assert!(
            report.notes.iter().any(|note| note.contains("Toolport's")
                && note.contains("master key")
                && note.contains("HTTP")),
            "{:?}",
            report.notes
        );
        assert!(env.dir.exists());
    }

    #[test]
    fn purge_r4_timestamped_backups_and_legacy_store_collisions_preserve_bytes() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_patterns");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let collisions = [
            "backups/claude-code/2024-taxes.pdf",
            "backups/claude-code/1791750000000-taxes.pdf",
            "quarantine-notes.json",
            "tool-cache-notes.json",
        ];
        for name in collisions {
            let path = env.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "user bytes").unwrap();
        }
        let generated = [
            "backups/claude-code/1791750000000-.claude.json",
            "backups/claude-code/1791750000000-claude-0123456789abcdef.json",
            "backups/claude-code/1791750000000-claude-settings-0123456789abcdef.json",
            "quarantine-default.json",
            "tool-cache-default.json",
            "tool-pins-default.json",
        ];
        for name in generated {
            std::fs::write(env.dir.join(name), "generated fixture").unwrap();
        }
        let report = run_with(
            |_| Vec::new(),
            || Ok(Vec::new()),
            |_| Vec::new(),
            || Ok(crate::secrets::purge::Removal::default()),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(report.leftovers.is_empty());
        for name in collisions {
            assert_eq!(
                std::fs::read_to_string(env.dir.join(name)).unwrap(),
                "user bytes",
                "{name}"
            );
            let expected = env.dir.join(name);
            assert!(
                report.preserved.iter().any(|entry| {
                    entry
                        .strip_suffix(" (not a Toolport-created entry)")
                        .is_some_and(|entry| {
                            Path::new(entry).components().eq(expected.components())
                        })
                }),
                "{name}: {:?}",
                report.preserved
            );
        }
        for name in generated {
            assert!(!env.dir.join(name).exists(), "{name}");
        }
    }

    #[test]
    fn purge_r4_candidate_comparison_uses_the_selected_msix_view() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_msix");
        let real = env.dir.join("real/Toolport");
        let shadow = env.dir.join("virtualized/Toolport");
        let dev = env.dir.join("real/Toolport-dev");
        let dev_shadow = env.dir.join("virtualized/Toolport-dev");
        for dir in [&real, &shadow, &dev] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("registry.json"), r#"{"servers":[]}"#).unwrap();
        }
        let others = other_installs_with(
            &real,
            vec![shadow.clone(), dev_shadow, dev.clone()],
            |path| {
                if let Ok(relative) = path.strip_prefix(env.dir.join("virtualized")) {
                    env.dir.join("real").join(relative)
                } else {
                    path.to_path_buf()
                }
            },
        );
        assert_eq!(
            others,
            [dev],
            "compare and inventory every candidate in the selected UNC view"
        );
    }

    #[test]
    fn purge_r4_cursor_settings_backup_matches_shared_writer() {
        let env = crate::registry::DataDirTestEnv::new("purge_r4_cursor_backup");
        let config = env.dir.join("hooks.json");
        let original = r#"{"hooks":{}}"#;
        std::fs::write(&config, original).unwrap();
        crate::clients::write_settings_key_for(
            "cursor",
            &config,
            Some(original),
            &serde_json::json!({"hooks":{"fixture":[]}}),
            "hooks",
        )
        .unwrap();
        let backups: Vec<_> = std::fs::read_dir(env.dir.join("backups/cursor"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.contains("-claude-settings-") && name.ends_with(".json")
                    })
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "select the settings copy, not the original recovery snapshot"
        );
        let backup = &backups[0];
        assert_eq!(
            crate::clients::read_client_backup(backup, &config).unwrap(),
            original
        );
        assert!(
            owned_data_entry(backup.strip_prefix(&env.dir).unwrap()).is_some(),
            "{}",
            backup.display()
        );
    }
}
