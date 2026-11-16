//! Shared bulk operation for Settings and pre-uninstall CLI callers.
use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientResult {
    pub client_id: String,
    pub path: String,
    pub dry_run: bool,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Restore every client, then stop the daemons nothing connected still uses:
/// with Toolport out of every config, an idle daemon has no one left to serve.
pub fn all(dry_run: bool) -> Result<Vec<ClientResult>, String> {
    let results = all_with_registry(dry_run, false)?;
    if !dry_run {
        for line in crate::daemon::stop_when_idle_after_disconnect() {
            eprintln!("{line}");
        }
    }
    Ok(results)
}

pub(crate) fn for_purge(all_installs: bool) -> Result<Vec<ClientResult>, String> {
    all_with_registry(false, all_installs)
}

fn all_with_registry(dry_run: bool, allow_unreadable: bool) -> Result<Vec<ClientResult>, String> {
    all_with_clients(dry_run, allow_unreadable, detect_clients)
}

fn all_with_clients(
    dry_run: bool,
    allow_unreadable: bool,
    detect: impl FnOnce() -> Vec<DetectedClient>,
) -> Result<Vec<ClientResult>, String> {
    let (current, recovery_only) = match crate::registry_controller::registry_for_disconnect() {
        Ok(registry) => (registry, false),
        Err(_) if allow_unreadable => (crate::registry::Registry::default(), true),
        Err(error) => return Err(error),
    };
    let mut clients = detect();
    apply_entry_states(&mut clients, &current.client_managed_entries);
    // Keep the stored recovery path beside the display path before restoration
    // can remove a newly created file or directory.
    let recovery_paths: HashMap<_, _> = clients
        .iter()
        .map(|client| {
            let native = locations::lookup(&client.id)
                .map(|row| row.config_path)
                .unwrap_or_else(|| PathBuf::from(&client.config_path));
            (client.id.clone(), (client.config_path.clone(), native))
        })
        .collect();
    let targets = clients.into_iter().filter(|client| {
        let recovery_path = &recovery_paths[&client.id].1;
        client.error.is_some()
            || (client.gateway_installed
                && !restore::released(&client.id, recovery_path).unwrap_or(false))
            || current.client_managed_entries.contains_key(&client.id)
            || moved::has_record(&client.id)
    });
    let mut errors = Vec::new();
    let targets = targets.filter_map(|client| {
        if let Some(error) = client.error {
            errors.push(ClientResult {
                client_id: client.id,
                path: client.config_path,
                dry_run,
                error: Some(error),
                warnings: Vec::new(),
            });
            None
        } else {
            Some((client.id, client.config_path))
        }
    });
    let mut results = run(targets, dry_run, |id, path| {
        if recovery_only {
            // Global removal can use the independent client snapshots without
            // loading, migrating or overwriting a corrupt registry.
            let def = find_def(id).ok_or("Unknown client")?;
            let native = recovery_paths
                .get(id)
                .map(|(_, native)| native.as_path())
                .unwrap_or_else(|| Path::new(path));
            restore_path(id, native, def.format, None)
        } else {
            crate::registry_controller::disconnect_client(id).map(|result| result.outcome.warnings)
        }
    });
    results.extend(errors);
    // Secondary Claude profiles and settings have path-specific recovery records
    // too. They are restored even if the profile's config override has changed.
    let mut recorded = restore::recorded_paths();
    recorded.extend(moved::recorded_paths());
    for (id, path, format) in recorded {
        let format = match format {
            Ok(format) => format,
            Err(error) => {
                results.push(ClientResult {
                    client_id: id,
                    path: path.to_string_lossy().into_owned(),
                    dry_run,
                    error: Some(error),
                    warnings: Vec::new(),
                });
                continue;
            }
        };
        if results.iter().any(|result| {
            result.client_id == id && {
                let recovery_path = recovery_paths
                    .get(&id)
                    .filter(|(display, _)| display == &result.path)
                    .map(|(_, native)| native.as_path())
                    .unwrap_or_else(|| Path::new(&result.path));
                locations::same_path(recovery_path, &path)
            }
        }) {
            continue;
        }
        let result = if dry_run {
            Ok(Vec::new())
        } else {
            restore_path(&id, &path, format, current.client_managed_entries.get(&id))
        };
        results.push(ClientResult {
            client_id: id,
            path: path.to_string_lossy().into_owned(),
            dry_run,
            warnings: result.as_ref().cloned().unwrap_or_default(),
            error: result.err(),
        });
    }
    // A corrupt receipt or saved-location store must not prevent the valid
    // clients above from restoring, or disappear from noninteractive reports.
    for row in recovery::list()? {
        if let Some(error) = row.error {
            if !results.iter().any(|result| result.path == row.backup_path) {
                results.push(ClientResult {
                    client_id: row.client_id,
                    path: row.backup_path,
                    dry_run,
                    error: Some(error),
                    warnings: Vec::new(),
                });
            }
        }
    }
    Ok(results)
}

pub(super) fn restore_path(
    id: &str,
    path: &Path,
    format: Format,
    managed: Option<&ManagedEntry>,
) -> Result<Vec<String>, String> {
    backup_storage::reset_missing();
    recovery_values::reset_progress();
    let (revision, used_move_record) = restore::run(id, path, format, || {
        mutation::disconnecting();
        restore::prepare_disconnect(id, path)?;
        if !restore::pending_values(id, path)? {
            backup_file(id, path)?;
        }
        let (handled, adopted) = restore::apply_adopting(id, format, path)?;
        if !handled {
            restore::check_legacy_gateway(format, path, managed)?;
            moved::restore(id, format, path)?;
            edit_format(format, path, None, true)?;
        } else if adopted || restore::needs_moved(id, path)? {
            moved::restore(id, format, path)?;
        }
        let revision = mutation::read(path)
            .flatten()
            .as_deref()
            .map(crate::registry::sha256_hex);
        Ok((revision, moved::matches_path(id, path)?))
    })?;
    #[cfg(test)]
    AFTER_RESTORE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(path);
        }
    });
    let mut warnings = disconnect_warnings(format, path)
        .map_err(|error| {
            eprintln!(
                "toolport: could not inspect disconnect warnings at {}: {error}",
                path.display()
            )
        })
        .unwrap_or_default();
    let recovery_warnings = finish_uninstall_with_warnings(
        id,
        &WriteOutcome {
            existed_before: true,
            path: path.to_string_lossy().into_owned(),
            backup: None,
            managed: None,
            restored: Vec::new(),
            used_move_record,
            revision,
            warnings: Vec::new(),
            recovery_path: None,
        },
    )?;
    warnings.extend(recovery_warnings);
    Ok(warnings)
}

fn run(
    targets: impl IntoIterator<Item = (String, String)>,
    dry_run: bool,
    mut disconnect: impl FnMut(&str, &str) -> Result<Vec<String>, String>,
) -> Vec<ClientResult> {
    targets
        .into_iter()
        .map(|(client_id, path)| {
            let result = if dry_run {
                Ok(Vec::new())
            } else {
                disconnect(&client_id, &path)
            };
            ClientResult {
                client_id,
                path,
                dry_run,
                warnings: result.as_ref().cloned().unwrap_or_default(),
                error: result.err(),
            }
        })
        .collect()
}

#[cfg(test)]
type RestoreHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    static AFTER_RESTORE: std::cell::RefCell<Option<RestoreHook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bulk_disconnect_continues_after_one_client_fails_and_dry_run_writes_nothing() {
        let targets = vec![
            ("first".into(), "/first".into()),
            ("broken".into(), "/broken".into()),
            ("last".into(), "/last".into()),
        ];
        let mut called = Vec::new();
        let results = run(targets.clone(), false, |id, _| {
            called.push(id.to_string());
            if id == "broken" {
                Err("read-only config".into())
            } else {
                Ok(Vec::new())
            }
        });
        assert_eq!(called, ["first", "broken", "last"]);
        assert!(results[0].error.is_none());
        assert_eq!(results[1].error.as_deref(), Some("read-only config"));
        assert!(results[2].error.is_none());
        run(targets, true, |_, _| panic!("dry run must not mutate"));
    }
    #[test]
    fn bulk_disconnect_serializes_warnings_without_errors() {
        let result = run(vec![("client".into(), "/config".into())], false, |_, _| {
            Ok(vec!["keychain unreachable".into()])
        });
        assert!(result.iter().all(|client| client.error.is_none()));
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json[0]["warnings"][0], "keychain unreachable");
        assert!(json[0]["error"].is_null());
    }
    #[test]
    fn bulk_restore_reports_bad_config_and_restores_the_remaining_files() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-bulk-restore-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let fixtures = [
            ("first", Format::JsonMcpServers, "{ \"mcpServers\": {} }"),
            ("broken", Format::JsonMcpServers, "{\"mcpServers\":{}}"),
            (
                "last",
                Format::TomlMcpServers,
                "# user's config\nmodel = 'custom'",
            ),
        ];
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        let mut targets = Vec::new();
        for (id, format, original) in &fixtures {
            let path = dir.join(id);
            std::fs::write(&path, original).unwrap();
            mutation::run(id, &path, *format, || {
                edit_format(*format, &path, Some(&entry), true)
            })
            .unwrap();
            targets.push((id.to_string(), path.to_string_lossy().into_owned()));
        }
        let broken = "{ native write interrupted";
        std::fs::write(dir.join("broken"), broken).unwrap();
        let before: Vec<_> = targets
            .iter()
            .map(|(_, path)| std::fs::read(path).unwrap())
            .collect();
        run(targets.clone(), true, |_, _| {
            panic!("dry run mutated a client")
        });
        assert_eq!(
            targets
                .iter()
                .map(|(_, path)| std::fs::read(path).unwrap())
                .collect::<Vec<_>>(),
            before
        );
        let results = run(targets, false, |id, _| {
            let (_, format, _) = fixtures
                .iter()
                .find(|(client, _, _)| *client == id)
                .unwrap();
            let path = dir.join(id);
            mutation::run(id, &path, *format, || {
                restore::apply(id, *format, &path).map(|_| Vec::new())
            })
        });
        assert!(results[0].error.is_none());
        assert!(results[1].error.is_some());
        assert!(results[2].error.is_none());
        assert_eq!(
            std::fs::read_to_string(dir.join("first")).unwrap(),
            fixtures[0].2
        );
        assert_eq!(std::fs::read_to_string(dir.join("broken")).unwrap(), broken);
        assert_eq!(
            std::fs::read_to_string(dir.join("last")).unwrap(),
            fixtures[2].2
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_warning_read_does_not_abort_completed_restoration() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-warning-read-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("config.json");
        let original = r#"{"mcpServers":{"native":{"command":"native"}}}"#;
        std::fs::write(&path, original).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        mutation::run("fixture", &path, Format::JsonMcpServers, || {
            moved::record("fixture", Format::JsonMcpServers, &path)?;
            write_format(
                Format::JsonMcpServers,
                &path,
                std::slice::from_ref(&entry),
                true,
            )
        })
        .unwrap();
        AFTER_RESTORE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|path| {
                assert_eq!(
                    std::fs::read_to_string(path).unwrap(),
                    r#"{"mcpServers":{"native":{"command":"native"}}}"#
                );
                std::fs::write(path, "{ interrupted native save").unwrap();
            }));
        });
        assert!(restore_path("fixture", &path, Format::JsonMcpServers, None)
            .unwrap()
            .is_empty());
        assert!(!moved::has_record("fixture"));
        assert!(restore::released("fixture", &path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ interrupted native save"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn legacy_secondary_path_restores_without_the_apps_profile_environment() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-old-profile-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        let path = dir.join("old-profile.json");
        let native = r#"{"session":1,"mcpServers":{"native":{"command":"native","env":{"TOKEN":"fixture"}}}}"#;
        std::fs::write(&path, native).unwrap();
        moved::record("claude-code", Format::JsonMcpServers, &path).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        write_format(Format::JsonMcpServers, &path, &[entry], true).unwrap();
        assert!(moved::recorded_paths()
            .iter()
            .any(|(id, recorded, _)| id == "claude-code" && recorded == &path));
        restore_path("claude-code", &path, Format::JsonMcpServers, None).unwrap();
        let restored = read_config_file(&path).unwrap();
        assert_eq!(
            parse_json_value(&restored).unwrap(),
            parse_json_value(native).unwrap()
        );
        assert!(!moved::has_record("claude-code"));
        restore_path("claude-code", &path, Format::JsonMcpServers, None).unwrap();
        assert_eq!(read_config_file(&path).unwrap(), restored);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn purge_r5_corrupt_registry_restores_selected_nondefault_client_path() {
        let env = crate::registry::DataDirTestEnv::new("purge_r5_selected_client");
        let _env_lock = env_test_lock();
        let fallback_dir = env.dir.join("fallback-claude");
        std::fs::create_dir(&fallback_dir).unwrap();
        let _config_dir = EnvRestore::set("CLAUDE_CONFIG_DIR", &fallback_dir);
        let fallback = resolved_definition_path(&find_def("claude-code").unwrap()).unwrap();
        assert!(fallback.starts_with(&fallback_dir));
        let selected = env.dir.join("selected-profile.json");
        let native = r#"{"session":1,"mcpServers":{"native":{"command":"native"}}}"#;
        std::fs::write(&selected, native).unwrap();
        std::fs::write(&fallback, "{\"mcpServers\":{},\"fallback\":true}").unwrap();
        let fallback_bytes = std::fs::read(&fallback).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        mutation::run("claude-code", &selected, Format::JsonMcpServers, || {
            edit_format(Format::JsonMcpServers, &selected, Some(&entry), true)
        })
        .unwrap();
        let servers = parse_client_content(
            Format::JsonMcpServers,
            &std::fs::read_to_string(&selected).unwrap(),
        )
        .unwrap();
        let registry = env.dir.join("registry.json");
        std::fs::write(&registry, "broken").unwrap();
        let client = DetectedClient {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            uses_connectors: false,
            discovery: discovery::capabilities("claude-code"),
            config_path: selected.to_string_lossy().into_owned(),
            config_exists: true,
            app_present: true,
            servers,
            plugin_servers: Vec::new(),
            gateway_installed: true,
            entry_state: GatewayEntryState::Managed,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
            location_removable: false,
            location_kept_by_t3_code: false,
        };
        let results = all_with_clients(false, true, || vec![client]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, selected.to_string_lossy());
        assert!(results[0].error.is_none(), "{:?}", results[0]);
        assert_eq!(std::fs::read_to_string(&selected).unwrap(), native);
        assert_eq!(std::fs::read(&fallback).unwrap(), fallback_bytes);
        assert_eq!(std::fs::read_to_string(&registry).unwrap(), "broken");
    }

    /// Greptile P1 on #1370: with a corrupt registry, purge restores from the client's own
    /// records. An edited Toolport entry takes the merge path, which asks the registry
    /// whether the entry is v1's; a registry it can't read proves nothing, so the restore
    /// goes on: the moved server comes back and the edited entry stays.
    #[test]
    fn purge_with_a_corrupt_registry_restores_a_client_whose_entry_was_edited() {
        let env = crate::registry::DataDirTestEnv::new("purge_corrupt_registry_edited_entry");
        let _env_lock = env_test_lock();
        let path = env.dir.join("client.json");
        std::fs::write(&path, r#"{"mcpServers":{"native":{"command":"native"}}}"#).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        mutation::run("claude-code", &path, Format::JsonMcpServers, || {
            moved::record("claude-code", Format::JsonMcpServers, &path)?;
            write_format(
                Format::JsonMcpServers,
                &path,
                std::slice::from_ref(&entry),
                true,
            )
        })
        .unwrap();
        let mut edited = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        assert!(edited["mcpServers"].get("native").is_none());
        edited["mcpServers"]["toolport"]["args"] = serde_json::json!(["--custom"]);
        std::fs::write(&path, edited.to_string()).unwrap();
        let registry = env.dir.join("registry.json");
        std::fs::write(&registry, "broken").unwrap();
        let servers = parse_client_content(
            Format::JsonMcpServers,
            &std::fs::read_to_string(&path).unwrap(),
        )
        .unwrap();
        let client = DetectedClient {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            uses_connectors: false,
            discovery: discovery::capabilities("claude-code"),
            config_path: path.to_string_lossy().into_owned(),
            config_exists: true,
            app_present: true,
            servers,
            plugin_servers: Vec::new(),
            gateway_installed: true,
            entry_state: GatewayEntryState::Customized,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
            location_removable: false,
            location_kept_by_t3_code: false,
        };
        let results = all_with_clients(false, true, || vec![client]).unwrap();
        assert!(results.iter().all(|row| row.error.is_none()), "{results:?}");
        let after = parse_json_value(&read_config_file(&path).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"]["native"],
            serde_json::json!({"command": "native"})
        );
        assert_eq!(
            after["mcpServers"]["toolport"]["args"],
            serde_json::json!(["--custom"])
        );
        assert_eq!(std::fs::read_to_string(&registry).unwrap(), "broken");
    }

    #[test]
    fn purge_corrupt_registry_snapshot_restore_preserves_native_bytes() {
        let env = crate::registry::DataDirTestEnv::new("purge_global_disconnect");
        let path = env.dir.join("secondary-client.json");
        let native = r#"{"mcpServers":{"native":{"command":"native"}}}"#;
        std::fs::write(&path, native).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/fixture/toolport-gateway"})).unwrap();
        mutation::run("claude-code", &path, Format::JsonMcpServers, || {
            moved::record("claude-code", Format::JsonMcpServers, &path)?;
            write_format(
                Format::JsonMcpServers,
                &path,
                std::slice::from_ref(&entry),
                true,
            )
        })
        .unwrap();
        let registry = env.dir.join("registry.json");
        std::fs::write(&registry, "broken").unwrap();
        assert!(crate::registry_controller::registry_for_disconnect().is_err());
        assert!(
            restore_path("claude-code", &path, Format::JsonMcpServers, None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), native);
        assert_eq!(std::fs::read_to_string(&registry).unwrap(), "broken");
    }
}
