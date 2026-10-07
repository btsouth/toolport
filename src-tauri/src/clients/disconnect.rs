//! Shared bulk operation for Settings and pre-uninstall CLI callers.
use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientResult {
    pub client_id: String,
    pub path: String,
    pub dry_run: bool,
    pub error: Option<String>,
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
        |id| crate::registry_controller::disconnect_client(id).map(|_| ()),
    );
    // Secondary Claude profiles and settings have path-specific recovery records
    // too. They are restored even if the profile's config override has changed.
    for (id, path, format) in restore::recorded_paths() {
        let format = match format {
            Ok(format) => format,
            Err(error) => {
                results.push(ClientResult {
                    client_id: id,
                    path: path.to_string_lossy().into_owned(),
                    dry_run,
                    error: Some(error),
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
            Ok(())
        } else {
            restore::run(&id, &path, format, || {
                mutation::disconnecting();
                backup_file(&id, &path)?;
                restore::apply(&id, format, &path)?;
                Ok(())
            })
            .and_then(|()| {
                let revision = if mutation::exists(&path) {
                    Some(crate::registry::sha256_hex(&read_config_file(&path)?))
                } else {
                    None
                };
                let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
                let _lock = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
                restore::finish(&id, &path, revision.as_deref())
            })
        };
        results.push(ClientResult {
            client_id: id,
            path: path.to_string_lossy().into_owned(),
            dry_run,
            error: result.err(),
        });
    }
    Ok(results)
}

fn run(
    targets: impl IntoIterator<Item = (String, String)>,
    dry_run: bool,
    mut disconnect: impl FnMut(&str) -> Result<(), String>,
) -> Vec<ClientResult> {
    targets
        .into_iter()
        .map(|(client_id, path)| {
            let error = if dry_run {
                None
            } else {
                disconnect(&client_id).err()
            };
            ClientResult {
                client_id,
                path,
                dry_run,
                error,
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
                Ok(())
            }
        });
        assert_eq!(called, ["first", "broken", "last"]);
        assert!(results[0].error.is_none());
        assert_eq!(results[1].error.as_deref(), Some("read-only config"));
        assert!(results[2].error.is_none());
        run(targets, true, |_| panic!("dry run must not mutate"));
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
                restore::apply(id, *format, &path).map(|_| ())
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
}
