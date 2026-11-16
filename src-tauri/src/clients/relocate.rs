//! Toolport entries left in files a client no longer reads.
//!
//! Clients move their MCP config, and older Toolport builds sometimes wrote to a
//! place the client never read: Goose on macOS reads `~/.config/goose`, never the
//! `Application Support` file Toolport used to write; Cline 4.1 moved to `~/.cline`
//! and ignores its old VS Code storage file; Devin Desktop reads the Devin file, not
//! `~/.codeium/windsurf`. An entry at the old place is invisible to the client, so the
//! user's connection quietly stops working.
//!
//! A client that moved may still read the old place: an older install that was not
//! updated, a discovery setting, a second editor. So the old entry is removed only on
//! positive evidence that this machine's client no longer reads it ([`old_file_status`]:
//! the client never read that path, or a file or version only the new release writes).
//! Without that evidence both files keep a working Toolport entry: the repair pass
//! writes the new file and keeps the old entry current, and Connect writes both.
//! Disconnect, uninstall and "Remove from all clients" take Toolport out of both
//! through the same per-path restore ([`disconnect::restore_path`]), which puts each
//! file back byte for byte from its own snapshot. An old entry is also never removed
//! when the new file holds a different connection (another scope, a hand edit, a
//! Shared HTTP entry): removing it would change what the client is connected to.
//!
//! Only real location moves are listed. A location chosen by an env var
//! (`XDG_CONFIG_HOME`, `HERMES_HOME`, `CLINE_DIR`, ...) is per shell: the default file
//! is still read by every session started without it, so it is never cleaned.
use super::*;

/// Config files `client_id` used to read on `platform`, or that older Toolport wrote
/// for it, apart from the one it reads now. A candidate that turns out to be the
/// current file is skipped by the caller, so a client whose current file depends on
/// what exists (Cline, Claude Desktop) lists every place it can live.
pub(super) fn resolve_legacy_config_paths(
    client_id: &str,
    home: &Path,
    platform: Platform,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let local = || {
        absolute_env_dir(env("LOCALAPPDATA")).unwrap_or_else(|| home.join("AppData").join("Local"))
    };
    match (client_id, platform) {
        ("goose", Platform::MacOs) => vec![home
            .join("Library")
            .join("Application Support")
            .join("Block")
            .join("goose")
            .join("config.yaml")],
        ("windsurf", _) => vec![home
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json")],
        ("cline", _) => vec![cline_legacy_settings_path(home, platform, env)],
        // Crush now treats this file as machine-owned state.
        ("crush", Platform::Windows) => vec![local().join("crush").join("crush.json")],
        ("antigravity", _) => ["antigravity-ide", "antigravity"]
            .into_iter()
            .map(|dir| home.join(".gemini").join(dir).join("mcp_config.json"))
            .collect(),
        // The Windows build always reads %LOCALAPPDATA%\hermes.
        ("hermes", Platform::Windows) => vec![home.join(".hermes").join("config.yaml")],
        ("amazon-q", _) => vec![amazon_q_dir(home).join("mcp.json")],
        ("lm-studio", _) => lm_studio_default_homes(home)
            .into_iter()
            .map(|dir| dir.join("mcp.json"))
            .collect(),
        ("reasonix", _) if env("REASONIX_HOME").is_none() => {
            reasonix_legacy_configs(home, platform, env)
        }
        _ => Vec::new(),
    }
}

fn legacy_config_paths(client_id: &str) -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    resolve_legacy_config_paths(client_id, &home, Platform::current(), &client_path_env)
}

/// Every client that may have a legacy file, for the launch pass.
pub(super) fn relocatable_clients() -> Vec<&'static str> {
    defs()
        .into_iter()
        .map(|def| def.id)
        .filter(|id| !legacy_config_paths(id).is_empty())
        .collect()
}

/// Legacy files of a main client that hold an entry Toolport still owns.
fn stale_files(
    def: &ClientDef,
    target: &Path,
    managed: Option<&ManagedEntry>,
) -> Result<Vec<(PathBuf, McpServer)>, String> {
    let saved = locations::saved().unwrap_or_default();
    let mut found: Vec<(PathBuf, McpServer)> = Vec::new();
    for legacy in legacy_config_paths(def.id) {
        if locations::same_path(&legacy, target)
            || found
                .iter()
                .any(|(path, _)| locations::same_path(path, &legacy))
            // A file the user added as its own location is maintained there.
            || saved
                .iter()
                .any(|row| locations::same_path(&row.config_path, &legacy))
            || !std::fs::metadata(&legacy).is_ok_and(|meta| meta.is_file())
            || std::fs::symlink_metadata(&legacy).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            continue;
        }
        let Ok(text) = read_config_file(&legacy) else {
            continue;
        };
        // An unreadable file is left exactly as it is.
        let Ok(servers) = parse_client_content(def.format, &text) else {
            continue;
        };
        let Some(entry) = gateway_entry_for_client(def.id, &servers) else {
            continue;
        };
        // A hand-edited entry is the user's; it stays where they put it.
        if entry_state_for(Some(entry), managed) != GatewayEntryState::Managed {
            continue;
        }
        found.push((legacy, entry.clone()));
    }
    Ok(found)
}

fn target_has_gateway(def: &ClientDef, target: &Path) -> Result<bool, String> {
    if !mutation::exists(target) {
        return Ok(false);
    }
    let text = read_config_file(target)?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let servers = parse_client_content(def.format, &text)?;
    Ok(gateway_entry_for_client(def.id, &servers).is_some())
}

/// What one client's relocation did.
#[derive(Default)]
pub(super) struct Relocated {
    /// Ownership snapshot of the entry written to the current file, if one was.
    pub managed: Option<ManagedEntry>,
    /// Legacy files Toolport's entry was taken out of.
    pub cleaned: Vec<PathBuf>,
    /// Legacy files the client may still read, whose Toolport entry was kept.
    pub kept: Vec<PathBuf>,
}

/// Whether this machine's client still reads one of its old config files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OldFile {
    /// Positive evidence it is not read; the text names the evidence.
    Unread(&'static str),
    /// No such evidence: the client may still read it.
    MaybeRead,
}

/// Move `client_id`'s Toolport connection out of every legacy file into the file the
/// client reads now, using `gateway` as the command. The current file is written only
/// when it has no Toolport entry yet; an unreadable current file stops the move before
/// anything is touched, so the old entry is never removed without a replacement.
pub(super) fn relocate(
    client_id: &str,
    gateway: &str,
    managed: Option<&ManagedEntry>,
) -> Result<Relocated, String> {
    let mut outcome = Relocated::default();
    let Some(def) = find_def(client_id).filter(|def| def.location.is_none()) else {
        return Ok(outcome);
    };
    let target = resolved_definition_path(&def)?;
    for (legacy, server) in stale_files(&def, &target, managed)? {
        // A Shared HTTP entry carries an endpoint, args and a bearer the stdio
        // repair cannot rebuild. It stays exactly as it is, whatever the new file
        // holds, until the user reconnects the client.
        if !is_local_gateway(&server) {
            if !target_has_gateway(&def, &target)? {
                return Err(format!(
                    "Toolport's Shared HTTP entry is in {}, which this client may no longer read. Reconnect the client to move it.",
                    legacy.display()
                ));
            }
            outcome.kept.push(legacy);
            continue;
        }
        if !target_has_gateway(&def, &target)? {
            let mut entry = repair_gateway_entry(def.format, &legacy, &server.name, client_id)?;
            entry.command = Some(gateway.to_string());
            outcome.managed = install_or_remove(client_id, Some(&entry))?.managed;
        }
        let unread = matches!(old_file_status(def.id, &legacy), OldFile::Unread(_));
        if unread && same_connection(&def, &target, &legacy, &server.name, client_id)? {
            // The record that made this entry ours, not the one just written.
            remove_from_legacy(&def, &legacy, managed)?;
            outcome.cleaned.push(legacy);
        } else {
            // The client may still read it, or the new file holds a different
            // connection: keep the old entry, pointed at the current gateway.
            if server.command.as_deref() != Some(gateway) {
                refresh_legacy(&def, &legacy, &server.name, gateway)?;
            }
            outcome.kept.push(legacy);
        }
    }
    Ok(outcome)
}

/// The `TOOLPORT_PROFILE` (or legacy `CONDUIT_PROFILE`) scope of an entry.
fn scope_of(entry: &ServerEntry) -> Option<String> {
    ["TOOLPORT_PROFILE", "CONDUIT_PROFILE"]
        .iter()
        .find_map(|key| {
            entry
                .env
                .iter()
                .find(|var| var.key == *key)
                .and_then(|var| var.value.clone())
        })
}

/// Whether the current file's Toolport entry is the same connection as the old
/// file's: a local Toolport gateway with the same scope.
fn same_connection(
    def: &ClientDef,
    target: &Path,
    legacy: &Path,
    name: &str,
    client_id: &str,
) -> Result<bool, String> {
    if !target_has_gateway(def, target)? {
        return Ok(false);
    }
    let servers = parse_client_content(def.format, &read_config_file(target)?)?;
    let Some(current) = gateway_entry_for_client(def.id, &servers) else {
        return Ok(false);
    };
    if current.url.is_some() || !command_is_gateway_binary(current.command.as_deref().unwrap_or(""))
    {
        return Ok(false);
    }
    let current = repair_gateway_entry(def.format, target, &current.name, client_id)?;
    let old = repair_gateway_entry(def.format, legacy, name, client_id)?;
    Ok(scope_of(&current) == scope_of(&old))
}

/// Whether an entry is a local stdio Toolport gateway: a gateway binary command
/// with no URL and no `mcp-remote` bridge. Only such an entry may be rebuilt by
/// [`repair_gateway_entry`]; anything else would lose its endpoint and auth.
fn is_local_gateway(server: &McpServer) -> bool {
    server.url.is_none()
        && !server.args.iter().any(|arg| arg == "mcp-remote")
        && command_is_gateway_binary(server.command.as_deref().unwrap_or(""))
}

/// Point the old file's Toolport entry at `gateway`, keeping its scope. Under the
/// config lock the entry is read again, and anything but a local gateway entry is
/// left untouched.
fn refresh_legacy(def: &ClientDef, legacy: &Path, name: &str, gateway: &str) -> Result<(), String> {
    mutation::run(def.id, legacy, def.format, || {
        let servers = parse_client_content(def.format, &read_config_file(legacy)?)?;
        if !servers
            .iter()
            .find(|server| server.name == name)
            .is_some_and(is_local_gateway)
        {
            return Ok(());
        }
        backup_file(def.id, legacy)?;
        let mut entry = repair_gateway_entry(def.format, legacy, name, def.id)?;
        entry.command = Some(gateway.to_string());
        edit_format(def.format, legacy, Some(&entry), false)
    })
}

/// Take Toolport's entry out of one legacy file. The per-path restore a Disconnect
/// uses comes first, so the file's original snapshot and move record are honoured.
/// A snapshot already marked released makes that restore a no-op by design (it never
/// re-claims native entries), so an entry of ours that appeared there since is then
/// removed directly, and nothing else in the file changes.
fn remove_from_legacy(
    def: &ClientDef,
    legacy: &Path,
    managed: Option<&ManagedEntry>,
) -> Result<(), String> {
    super::disconnect::restore_path(def.id, legacy, def.format, managed)?;
    let still_ours = |text: &str| {
        parse_client_content(def.format, text).is_ok_and(|servers| {
            gateway_entry_for_client(def.id, &servers).is_some_and(|entry| {
                entry_state_for(Some(entry), managed) == GatewayEntryState::Managed
            })
        })
    };
    if !mutation::exists(legacy) || !still_ours(&read_config_file(legacy)?) {
        return Ok(());
    }
    mutation::run(def.id, legacy, def.format, || {
        mutation::disconnecting();
        if mutation::exists(legacy) && still_ours(&read_config_file(legacy)?) {
            backup_file(def.id, legacy)?;
            edit_format(def.format, legacy, None, true)?;
        }
        Ok(())
    })
}

/// [`relocate`] for every client that has legacy files, for the launch repair pass.
pub(super) fn relocate_all(
    gateway: &str,
    managed: &HashMap<String, ManagedEntry>,
) -> RepointOutcome {
    let mut outcome = RepointOutcome::default();
    for id in relocatable_clients() {
        match relocate(id, gateway, managed.get(id)) {
            Ok(moved) => {
                for path in &moved.cleaned {
                    let msg = format!(
                        "toolport: moved {id}'s Toolport entry out of {}, which it no longer reads",
                        path.display()
                    );
                    eprintln!("{msg}");
                    crate::gatewaylog::append(&msg);
                }
                if let Some(entry) = moved.managed {
                    outcome.repointed.push((id.to_string(), entry));
                }
            }
            // A backed-up write the locked keyring refused: not a failure. The
            // start-up waiter (or a Retry) runs this pass again once it opens.
            Err(error) if super::backup_vault::is_locked_error(&error) => {
                crate::gatewaylog::append(&format!(
                    "toolport: {id}'s old Toolport entry will be moved once the keyring is unlocked"
                ));
                outcome.deferred.push(id.to_string());
            }
            Err(error) => {
                let msg = format!("toolport: could not move {id}'s old Toolport entry: {error}");
                eprintln!("{msg}");
                crate::gatewaylog::append(&msg);
                outcome.failed.push((id.to_string(), error));
            }
        }
    }
    outcome
}

/// Take Toolport's entry out of every legacy file without writing the current one.
/// Disconnect calls this after changing the current file, so Toolport leaves no entry
/// behind in any file the client reads or used to read. Failures become warnings:
/// the current file is already right.
pub(super) fn clean(client_id: &str, managed: Option<&ManagedEntry>) -> Vec<String> {
    let Some(def) = find_def(client_id).filter(|def| def.location.is_none()) else {
        return Vec::new();
    };
    let Ok(target) = resolved_definition_path(&def) else {
        return Vec::new();
    };
    let files = match stale_files(&def, &target, managed) {
        Ok(files) => files,
        Err(error) => return vec![format!("could not check older config files: {error}")],
    };
    files
        .into_iter()
        .filter_map(|(legacy, _)| {
            remove_from_legacy(&def, &legacy, managed)
                .err()
                .map(|error| {
                    format!(
                        "could not remove Toolport from the old config {}: {error}",
                        legacy.display()
                    )
                })
        })
        .collect()
}

/// After Connect wrote `entry` to the current file: an old file the client no longer
/// reads loses Toolport's entry, and one it may still read gets the same entry, so
/// every install of the client is connected. Only existing files are written, a
/// hand-edited entry under Toolport's name is left alone, and failures are warnings.
pub(super) fn after_connect(client_id: &str, entry: &ServerEntry) -> Vec<String> {
    let Some(def) = find_def(client_id).filter(|def| def.location.is_none()) else {
        return Vec::new();
    };
    let Ok(target) = resolved_definition_path(&def) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    let saved = locations::saved().unwrap_or_default();
    let mut seen: Vec<PathBuf> = Vec::new();
    for legacy in legacy_config_paths(def.id) {
        if locations::same_path(&legacy, &target)
            || seen.iter().any(|path| locations::same_path(path, &legacy))
            || saved
                .iter()
                .any(|row| locations::same_path(&row.config_path, &legacy))
            || !std::fs::metadata(&legacy).is_ok_and(|meta| meta.is_file())
            // A link (Antigravity links its old files to the new one) is written
            // through its target, never replaced.
            || std::fs::symlink_metadata(&legacy).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            continue;
        }
        seen.push(legacy.clone());
        let Ok(text) = read_config_file(&legacy) else {
            continue;
        };
        let Ok(servers) = parse_client_content(def.format, &text) else {
            continue;
        };
        let existing = gateway_entry_for_client(def.id, &servers);
        let ours = existing.is_some_and(|server| {
            entry_state_for(Some(server), None) == GatewayEntryState::Managed
        });
        let result = match old_file_status(def.id, &legacy) {
            OldFile::Unread(_) if ours => remove_from_legacy(&def, &legacy, None),
            OldFile::Unread(_) => Ok(()),
            // A hand-edited entry under our name is the user's.
            OldFile::MaybeRead if existing.is_some() && !ours => Ok(()),
            OldFile::MaybeRead => mutation::run(def.id, &legacy, def.format, || {
                backup_file(def.id, &legacy)?;
                edit_format(def.format, &legacy, Some(entry), false)
            }),
        };
        if let Err(error) = result {
            warnings.push(format!(
                "could not update Toolport in the older config {}: {error}",
                legacy.display()
            ));
        }
    }
    warnings
}

#[cfg(test)]
thread_local! {
    /// Forces [`old_file_status`] for tests of what a verdict leads to.
    pub(super) static STATUS_OVERRIDE: std::cell::Cell<Option<OldFile>> =
        const { std::cell::Cell::new(None) };
}

/// [`resolve_old_file_status`] for this machine.
pub(super) fn old_file_status(client_id: &str, legacy: &Path) -> OldFile {
    #[cfg(test)]
    if let Some(status) = STATUS_OVERRIDE.with(std::cell::Cell::get) {
        return status;
    }
    resolve_old_file_status(client_id, legacy, Platform::current(), &client_path_env)
}

/// Whether `client_id` on `platform` still reads `legacy`, one of its old files.
/// `Unread` needs positive evidence, stated in the text; everything else is
/// `MaybeRead`. Evidence per client is in docs/clients.md.
pub(super) fn resolve_old_file_status(
    client_id: &str,
    _legacy: &Path,
    platform: Platform,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> OldFile {
    match (client_id, platform) {
        // goose v1.0.0 read ~/.config/goose; since 54af4c9 it uses etcetera's
        // choose_app_strategy, which is Xdg on macOS (paths.rs@v1.59.1). No release
        // read Application Support; only an older Toolport wrote there.
        ("goose", Platform::MacOs) => {
            OldFile::Unread("Goose has never read Application Support on macOS")
        }
        // The Windows installer sets HERMES_HOME (install.ps1@v2026.5.16), and Hermes
        // then reads only that folder (hermes_constants.py@v0.23.1).
        ("hermes", Platform::Windows) if env("HERMES_HOME").is_some_and(|v| !v.is_empty()) => {
            OldFile::Unread("HERMES_HOME is set, so Hermes reads only that folder")
        }
        // LM Studio's home pointer is written by lmstudio-js (the CLI and SDK);
        // the closed-source app may predate it and read ~/.lmstudio. Reasonix
        // reads only its home config.toml once it exists, but a rollback to a
        // release that knew only the legacy file reads that again. Cline 3.x reads
        // only the VS Code storage file, and a rollback from Cline 4 reads it again
        // even after Cline 4 recorded its migration. All three keep the old entry.
        // Devin Desktop still imports ~/.codeium/windsurf by default
        // (read_config_from.windsurf), Crush still merges %LOCALAPPDATA%\crush last,
        // Amazon Q still loads mcp.json while useLegacyMcpJson is on, and Antigravity
        // 1.x read its old folders: no evidence any of them stopped.
        _ => OldFile::MaybeRead,
    }
}

/// Cline 4's own record, in `~/.cline/data/globalState.json`, that it migrated the
/// VS Code storage file (mcp-settings-legacy-migration.ts@v4.1.26). It only picks
/// where Toolport writes; the old file always keeps its entry.
pub(super) fn cline_migrated(home: &Path) -> bool {
    std::fs::read_to_string(home.join(".cline").join("data").join("globalState.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|state| state.get("__vscodeLegacyMcpSettingsMigration").is_some())
}

/// Where to write Cline's entry: the new file once Cline 4 migrated or is present
/// in VS Code's extensions folder. The old file keeps its entry either way, since a
/// rollback to Cline 3 reads it again.
pub(super) fn cline_moved(home: &Path) -> bool {
    cline_migrated(home) || installed_cline_major(home).is_some_and(|major| major >= 4)
}

/// The newest `saoudrizwan.claude-dev-<version>` major in VS Code's extensions folder.
fn installed_cline_major(home: &Path) -> Option<u64> {
    std::fs::read_dir(home.join(".vscode").join("extensions"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = name.strip_prefix("saoudrizwan.claude-dev-")?;
            version.split('.').next()?.parse::<u64>().ok()
        })
        .max()
}
