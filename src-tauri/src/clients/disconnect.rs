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

pub fn all(dry_run: bool) -> Result<Vec<ClientResult>, String> {
    let current = crate::registry_controller::registry_for_disconnect()?;
    let mut clients = detect_clients();
    apply_entry_states(&mut clients, &current.client_managed_entries);
    let targets = clients.into_iter().filter(|client| {
        (client.gateway_installed
            && !restore::released(&client.id, Path::new(&client.config_path)).unwrap_or(false))
            || current.client_managed_entries.contains_key(&client.id)
            || moved::has_record(&client.id)
    });
    let mut results = run(
        targets.map(|client| (client.id, client.config_path)),
        dry_run,
        |id| {
            crate::registry_controller::disconnect_client(id).map(|result| result.outcome.warnings)
        },
    );
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
        if results
            .iter()
            .any(|result| result.client_id == id && result.path == path.to_string_lossy())
        {
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
    Ok(results)
}

fn restore_path(
    id: &str,
    path: &Path,
    format: Format,
    managed: Option<&ManagedEntry>,
) -> Result<Vec<String>, String> {
    let (revision, used_move_record) = restore::run(id, path, format, || {
        mutation::disconnecting();
        backup_file(id, path)?;
        if !restore::apply(id, format, path)? {
            restore::check_legacy_gateway(format, path, managed)?;
            moved::restore(id, format, path)?;
            edit_format(format, path, None, true)?;
        } else if restore::needs_moved(id, path)? {
            moved::restore(id, format, path)?;
        }
        let revision = mutation::read(path)
            .flatten()
            .as_deref()
            .map(crate::registry::sha256_hex);
        Ok((revision, moved::matches_path(id, path)?))
    })?;
    let warnings = disconnect_warnings(format, path)?;
    finish_uninstall(
        id,
        &WriteOutcome {
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
    Ok(warnings)
}

fn run(
    targets: impl IntoIterator<Item = (String, String)>,
    dry_run: bool,
    mut disconnect: impl FnMut(&str) -> Result<Vec<String>, String>,
) -> Vec<ClientResult> {
    targets
        .into_iter()
        .map(|(client_id, path)| {
            let result = if dry_run {
                Ok(Vec::new())
            } else {
                disconnect(&client_id)
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
        let results = run(targets.clone(), false, |id| {
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
        run(targets, true, |_| panic!("dry run must not mutate"));
    }
    #[test]
    fn bulk_disconnect_serializes_warnings_without_errors() {
        let result = run(vec![("client".into(), "/config".into())], false, |_| {
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
        run(targets.clone(), true, |_| {
            panic!("dry run mutated a client")
        });
        assert_eq!(
            targets
                .iter()
                .map(|(_, path)| std::fs::read(path).unwrap())
                .collect::<Vec<_>>(),
            before
        );
        let results = run(targets, false, |id| {
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
}
