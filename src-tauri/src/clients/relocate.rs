//! Toolport entries left in files a client no longer reads.
//!
//! Clients move their MCP config, and older Toolport builds sometimes wrote to a
//! place the client never read: Goose on macOS reads `~/.config/goose`, never the
//! `Application Support` file Toolport used to write; Cline 4.1 moved to `~/.cline`
//! and ignores its old VS Code storage file; Devin Desktop reads the Devin file, not
//! `~/.codeium/windsurf`. An entry at the old place is invisible to the client, so the
//! user's connection quietly stops working.
//!
//! The repair pass moves that connection. The entry is written to the file the client
//! reads now, with its scope environment kept, and Toolport's own entry is then taken
//! out of the old file through the same per-path restore a Disconnect uses
//! ([`disconnect::restore_path`]). That honours the old file's original snapshot and
//! any "Move into gateway" record for it, changes nothing else in the file, and marks
//! the snapshot released so a later Disconnect or uninstall does not revisit it.
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
        ("claude-desktop", Platform::Windows) => {
            let mut paths = vec![roaming_config_dir(home, platform)
                .join("Claude")
                .join("claude_desktop_config.json")];
            paths.extend(claude_desktop_msix_configs(&local()));
            paths
        }
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
        if !target_has_gateway(&def, &target)? {
            // A Shared HTTP entry carries a bearer and endpoint the stdio repair
            // cannot rebuild. Leave it, and the old file, for an explicit reconnect.
            if server.url.is_some() || server.args.iter().any(|arg| arg == "mcp-remote") {
                return Err(format!(
                    "Toolport's Shared HTTP entry is in {}, which this client no longer reads. Reconnect the client to move it.",
                    legacy.display()
                ));
            }
            let mut entry = repair_gateway_entry(def.format, &legacy, &server.name, client_id)?;
            entry.command = Some(gateway.to_string());
            outcome.managed = install_or_remove(client_id, Some(&entry))?.managed;
        }
        // The record that made this entry ours, not the one just written.
        remove_from_legacy(&def, &legacy, managed)?;
        outcome.cleaned.push(legacy);
    }
    Ok(outcome)
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
/// Connect and Disconnect call this after changing the current file, so a client is
/// never left with a second, dead Toolport entry. Failures become warnings: the
/// current file, which the client reads, is already right.
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
