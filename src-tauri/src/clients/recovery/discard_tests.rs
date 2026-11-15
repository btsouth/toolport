//! Regressions for confirmed Discard, its phases, refresh feedback and Scan.
use super::*;
use backup_storage::DiscardPhase;

const SECRET: &str = "sk-discard-phase-fixture";
fn unlock() {
    std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
}
fn lock() {
    std::env::set_var("TOOLPORT_SECRET_KEY", "locked-discard-fixture");
}
/// A disconnected cursor config whose credential could not be read back.
fn pending(path: &Path) -> ClientRecovery {
    let original = format!(
        "{{\n \"mcpServers\":{{\"native\":{{\"command\":\"echo\",\"env\":{{\"API_KEY\":\"{SECRET}\"}}}}}}\n}}\n"
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    unlock();
    crate::secrets::set_secret("native", "API_KEY", SECRET).unwrap();
    backup_storage::register("native", "API_KEY", SECRET);
    std::fs::write(path, original).unwrap();
    mutation::run("cursor", path, Format::JsonMcpServers, || {
        backup_file("cursor", path)?;
        write_format(Format::JsonMcpServers, path, &[], true)
    })
    .unwrap();
    backup_storage::clear();
    lock();
    disconnect::restore_path("cursor", path, Format::JsonMcpServers, None).unwrap();
    notice(path)
}
fn notice(path: &Path) -> ClientRecovery {
    list()
        .unwrap()
        .into_iter()
        .find(|r| r.path == path.to_string_lossy())
        .unwrap()
}
fn copies(path: &Path) -> Vec<PathBuf> {
    restore::recovery_copies("cursor", path).unwrap()
}
fn phase(row: &ClientRecovery) -> Option<DiscardPhase> {
    backup_storage::discard_phase(&std::fs::read_to_string(&row.backup_path).unwrap())
}
fn vault_payload(row: &ClientRecovery) -> Option<String> {
    let (account, key, _) = backup_storage::test_recovery_key(Path::new(&row.backup_path));
    crate::secrets::backup_secret_status(&account, &key).unwrap()
}
#[cfg(unix)]
struct Mode(PathBuf, u32);
#[cfg(unix)]
impl Mode {
    fn set(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let old = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        Self(path.into(), old)
    }
}
#[cfg(unix)]
impl Drop for Mode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

#[cfg(unix)]
#[test]
fn file_error_replaced_by_folder_error_is_focused_without_success() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = backup_dir("cursor").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("original-broken.json");
        std::fs::write(&file, "{broken sk-do-not-display").unwrap();
        let before = list().unwrap();
        let old = before
            .iter()
            .find(|r| r.backup_path == file.to_string_lossy())
            .unwrap();
        assert!(old.error.is_some());
        let _mode = Mode::set(&dir, 0o000);
        let update = refresh_notice(&old.id, "cursor", &old.backup_path).unwrap();
        let folder = update
            .rows
            .iter()
            .find(|r| r.backup_path == dir.to_string_lossy())
            .expect("the folder error replaces the file error");
        assert!(folder.error.is_some());
        assert!(!update.rows.iter().any(|r| r.id == old.id));
        assert_eq!(update.focus_id.as_deref(), Some(folder.id.as_str()));
        assert_eq!(
            update.message, None,
            "an error is never reported as success"
        );
        assert!(!serde_json::to_string(&update)
            .unwrap()
            .contains("sk-do-not-display"));
    });
}

#[test]
fn deleted_error_record_reports_a_neutral_refresh_and_same_error_keeps_focus() {
    crate::secrets::tests::with_isolated_vault(|| {
        let dir = backup_dir("cursor").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("original-broken.json");
        std::fs::write(&file, "{broken").unwrap();
        let old = list()
            .unwrap()
            .into_iter()
            .find(|r| r.backup_path == file.to_string_lossy())
            .unwrap();
        let same = refresh_notice(&old.id, "cursor", &old.backup_path).unwrap();
        assert_eq!(same.focus_id.as_deref(), Some(old.id.as_str()));
        assert_eq!(
            same.message, None,
            "the same unreadable file is still an error"
        );
        std::fs::remove_file(&file).unwrap();
        let gone = refresh_notice(&old.id, "cursor", &old.backup_path).unwrap();
        assert_eq!(gone.focus_id, None);
        let message = gone.message.unwrap();
        assert_eq!(
            message,
            crate::i18n::trf(
                "{client}: recovery listing refreshed.",
                &[("client", &old.client_name)]
            )
        );
        assert!(!message.contains("read"), "a deleted file was never read");
    });
}

#[test]
fn prepared_manifest_lists_exact_files_and_ignores_status_only_changes() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("manifest").join("mcp.json");
        let row = pending(&path);
        assert!(row.can_retry && !row.discard_started);
        assert_eq!(
            row.discard_action,
            crate::i18n::format_message(DISCARD_ACTION, &[])
        );
        let files = copies(&path);
        assert!(files.len() >= 2, "receipt plus a timestamped copy");
        let first = prepare_discard(&row.id).unwrap();
        let mut expected = vec![crate::i18n::trf(
            "Value: {value}",
            &[("value", "API_KEY for native")],
        )];
        for file in &files {
            let name = file.file_name().unwrap().to_string_lossy();
            expected.push(crate::i18n::trf("Backup file: {file}", &[("file", &name)]));
        }
        assert_eq!(first.lines, expected);
        assert!(first
            .description
            .contains(&format!("Saved values: 1. Backup files: {}.", files.len())));
        assert!(first.description.contains(&row.path));
        assert!(!first.description.contains("does not expire"));
        assert_eq!(
            first.action_label,
            crate::i18n::format_message(DISCARD_ACTION, &[])
        );
        // Dismiss, status writes and Scan must not revoke consent.
        dismiss(&row.id, true).unwrap();
        backup_storage::set_cleanup_state(
            Path::new(&row.backup_path),
            crate::secrets::BackupSecretError::Locked,
        )
        .unwrap();
        unlock();
        refresh_status().unwrap();
        dismiss(&row.id, false).unwrap();
        assert_eq!(prepare_discard(&row.id).unwrap().manifest, first.manifest);
        // A user file beside the copies is not inventory; a new copy is.
        let extra = backup_dir("cursor").unwrap().join("notes.txt");
        std::fs::write(&extra, "not a recovery copy").unwrap();
        assert_eq!(prepare_discard(&row.id).unwrap().manifest, first.manifest);
        let stamp = backup_dir("cursor").unwrap().join("1-mcp.json");
        std::fs::write(&stamp, "{}").unwrap();
        assert_ne!(prepare_discard(&row.id).unwrap().manifest, first.manifest);
    });
}

#[test]
fn changed_inventory_is_refused_before_any_deletion() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("changed").join("mcp.json");
        let row = pending(&path);
        unlock();
        let reviewed = prepare_discard(&row.id).unwrap();
        let stamp = backup_dir("cursor").unwrap().join("1-mcp.json");
        std::fs::write(&stamp, "{}").unwrap();
        let config = std::fs::read(&path).unwrap();
        let error = discard_confirmed(&row.id, &reviewed.manifest).unwrap_err();
        assert!(error.contains("Nothing was deleted"));
        assert!(error.contains("changed since you reviewed"));
        assert_eq!(safe_discard_error(&row.id, &error), error);
        assert_eq!(phase(&row), None);
        assert!(vault_payload(&row).is_some(), "no value released");
        for file in copies(&path) {
            assert!(file.exists());
        }
        assert_eq!(std::fs::read(&path).unwrap(), config);
        // A fresh review of the current inventory is accepted.
        let current = prepare_discard(&row.id).unwrap();
        assert!(current.lines.contains(&crate::i18n::trf(
            "Backup file: {file}",
            &[("file", "1-mcp.json")]
        )));
        discard_confirmed(&row.id, &current.manifest).unwrap();
        assert!(!stamp.exists());
    });
}

#[test]
fn release_for_discard_names_the_discard_action() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("flag").join("mcp.json");
        let row = pending(&path);
        lock();
        let error = backup_storage::release_for_discard(Path::new(&row.backup_path)).unwrap_err();
        let state = state_for_instructions(&error).expect("typed store category");
        assert_eq!(error, next_step(state, true));
        assert!(error.contains(&crate::i18n::format_message(DISCARD_ACTION, &[])));
        let retry = backup_storage::release(Path::new(&row.backup_path)).unwrap_err();
        assert_eq!(retry, next_step(state, false));
    });
}

#[cfg(unix)]
#[test]
fn readonly_recovery_folder_stops_before_any_value_is_released() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("readonly").join("mcp.json");
        let row = pending(&path);
        unlock();
        let reviewed = prepare_discard(&row.id).unwrap();
        let config = std::fs::read(&path).unwrap();
        let dir = backup_dir("cursor").unwrap();
        let error = {
            let _mode = Mode::set(&dir, 0o555);
            discard_confirmed(&row.id, &reviewed.manifest).unwrap_err()
        };
        assert!(error.contains("Nothing was deleted"), "{error}");
        assert!(error.contains("writable"));
        assert!(error.contains(&dir.to_string_lossy().into_owned()));
        assert!(error.contains(&crate::i18n::format_message(DISCARD_ACTION, &[])));
        assert_eq!(safe_discard_error(&row.id, &error), error);
        assert_eq!(phase(&row), None);
        assert!(vault_payload(&row).is_some());
        for file in copies(&path) {
            assert!(file.exists());
        }
        assert_eq!(std::fs::read(&path).unwrap(), config);
        assert!(notice(&path).can_retry, "restoration is still available");
    });
}

#[test]
fn partial_release_requires_finish_and_never_restores() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("partial").join("mcp.json");
        let row = pending(&path);
        unlock();
        let reviewed = prepare_discard(&row.id).unwrap();
        let config = std::fs::read(&path).unwrap();
        let files = copies(&path);
        let (_, key, _) = backup_storage::test_recovery_key(Path::new(&row.backup_path));
        let failure_key = Box::leak(format!("{key}-credential-0").into_boxed_str());
        let error = crate::secrets::tests::with_failed_write(failure_key, || {
            discard_confirmed(&row.id, &reviewed.manifest).unwrap_err()
        });
        assert!(error.contains("may already be gone"), "{error}");
        assert!(error.contains(&crate::i18n::format_message(FINISH_ACTION, &[])));
        assert!(!error.contains("Nothing was deleted"));
        assert_eq!(safe_discard_error(&row.id, &error), error);
        for file in &files {
            assert!(file.exists(), "every file survives a release failure");
        }
        assert_eq!(phase(&row), Some(DiscardPhase::Started));
        let started = notice(&path);
        assert!(started.discard_started && !started.can_retry);
        assert_eq!(
            started.discard_action,
            crate::i18n::format_message(FINISH_ACTION, &[])
        );
        assert!(started.message.contains("may already be gone"));
        assert!(!started.retention.contains("does not expire"));
        assert_eq!(retry(&started.id).unwrap_err(), discard_started_refusal());
        assert_eq!(std::fs::read(&path).unwrap(), config);
        let finish = prepare_discard(&started.id).unwrap();
        assert_eq!(
            finish.action_label,
            crate::i18n::format_message(FINISH_ACTION, &[])
        );
        assert!(finish.description.contains("may already be gone"));
        let done = discard_confirmed(&started.id, &finish.manifest).unwrap();
        assert!(done.contains("were deleted"));
        assert!(done.contains(&row.path));
        for file in &files {
            assert!(!file.exists());
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            config,
            "never edits native config"
        );
        assert!(!list().unwrap().iter().any(|r| r.id == row.id));
    });
}

#[test]
fn file_unlink_failure_keeps_receipt_last_with_values_removed_phase() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("unlink").join("mcp.json");
        let row = pending(&path);
        unlock();
        let reviewed = prepare_discard(&row.id).unwrap();
        let config = std::fs::read(&path).unwrap();
        let files = copies(&path);
        let stuck = files
            .iter()
            .find(|f| f.to_string_lossy() != row.backup_path)
            .unwrap()
            .clone();
        discard::FAIL_REMOVE.with(|slot| *slot.borrow_mut() = Some(stuck.clone()));
        let error = discard_confirmed(&row.id, &reviewed.manifest).unwrap_err();
        discard::FAIL_REMOVE.with(|slot| *slot.borrow_mut() = None);
        assert!(
            error.contains("were deleted, but some recovery files remain"),
            "{error}"
        );
        assert!(error.contains(&stuck.to_string_lossy().into_owned()));
        assert_eq!(safe_discard_error(&row.id, &error), error);
        assert!(stuck.exists());
        assert!(
            Path::new(&row.backup_path).exists(),
            "receipt is unlinked last"
        );
        assert_eq!(phase(&row), Some(DiscardPhase::ValuesRemoved));
        assert_eq!(vault_payload(&row), None, "values were released");
        let remaining = notice(&path);
        assert!(remaining.discard_started && !remaining.can_retry);
        assert!(remaining.message.contains("were deleted"));
        // Finish needs no vault: values are already gone.
        lock();
        let finish = prepare_discard(&remaining.id).unwrap();
        assert!(!finish.lines.iter().any(|l| l.starts_with("Value:")));
        assert!(finish.description.contains("already deleted"));
        discard_confirmed(&remaining.id, &finish.manifest).unwrap();
        for file in &files {
            assert!(!file.exists());
        }
        assert_eq!(std::fs::read(&path).unwrap(), config);
    });
}

#[test]
fn started_phases_block_every_restore_entrypoint_without_deleting() {
    for started in [DiscardPhase::Started, DiscardPhase::ValuesRemoved] {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let path = data.join("refuse").join("mcp.json");
            let row = pending(&path);
            unlock();
            backup_storage::set_discard_phase(Path::new(&row.backup_path), started).unwrap();
            let config = std::fs::read(&path).unwrap();
            let files = copies(&path);
            let refusal = discard_started_refusal();
            assert_eq!(retry(&row.id).unwrap_err(), refusal);
            assert_eq!(dismiss(&row.id, true).unwrap_err(), refusal);
            assert_eq!(
                disconnect::restore_path("cursor", &path, Format::JsonMcpServers, None)
                    .unwrap_err(),
                refusal
            );
            assert_eq!(
                restore::apply("cursor", Format::JsonMcpServers, &path).unwrap_err(),
                refusal
            );
            assert_eq!(restore::finish("cursor", &path, None).unwrap_err(), refusal);
            assert_eq!(restore::released("cursor", &path).unwrap_err(), refusal);
            assert_eq!(
                restore::prepare_disconnect("cursor", &path).unwrap_err(),
                refusal
            );
            let receipt = Path::new(&row.backup_path);
            assert_eq!(
                restore::after_rollback(receipt, &path, None).unwrap_err(),
                refusal
            );
            assert_eq!(
                restore::after_capture_conflict(receipt, &path, None).unwrap_err(),
                refusal
            );
            assert_eq!(phase(&row), Some(started), "no rewrite dropped the phase");
            // Read-only listing still reports the config itself, not a receipt error.
            let recorded = restore::recorded_paths();
            assert!(recorded.iter().any(|(id, recorded, format)| id == "cursor"
                && recorded == &path
                && matches!(format, Ok(Format::JsonMcpServers))));
            assert!(!recorded.iter().any(|(_, _, format)| format.is_err()));
            assert_eq!(safe_action_error(&row.id, &refusal), refusal);
            refresh_status().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), config);
            for file in &files {
                assert!(file.exists(), "a phase never authorizes deletion");
            }
            assert!(vault_payload(&row).is_some(), "nothing was released");
            assert!(notice(&path).discard_started);
        });
    }
}

#[test]
fn scan_probes_without_touching_bytes_and_keeps_distinct_states() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("scan").join("mcp.json");
        let row = pending(&path);
        let locked_line = row.value_lines[0].clone();
        assert!(!locked_line.contains(&crate::i18n::format_message(READY_REASON, &[])));
        let reviewed = prepare_discard(&row.id).unwrap();
        let config = std::fs::read(&path).unwrap();
        let others: Vec<_> = copies(&path)
            .into_iter()
            .filter(|f| f.to_string_lossy() != row.backup_path)
            .map(|f| (f.clone(), std::fs::read(f).unwrap()))
            .collect();
        let check = |label: &str| {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                config,
                "{label}: config bytes"
            );
            for (file, bytes) in &others {
                assert_eq!(
                    &std::fs::read(file).unwrap(),
                    bytes,
                    "{label}: backup bytes"
                );
            }
            assert_eq!(
                prepare_discard(&row.id).unwrap().manifest,
                reviewed.manifest,
                "{label}: status-only"
            );
            let text = std::fs::read_to_string(&row.backup_path).unwrap();
            assert!(!text.contains(SECRET), "{label}: value text never stored");
        };
        // Still locked: same category, nothing claimed ready.
        refresh_status().unwrap();
        check("locked");
        assert_eq!(notice(&path).value_lines[0], locked_line);
        // Unlocked: ready to put back, with a Retry instruction, not Unchecked.
        unlock();
        refresh_status().unwrap();
        check("ready");
        let ready = notice(&path);
        assert!(ready.value_lines[0].contains(&crate::i18n::format_message(READY_REASON, &[])));
        assert!(ready
            .instructions
            .contains(&crate::i18n::format_message(READY_STEP, &[])));
        assert!(!ready.value_lines[0].contains(&crate::i18n::lookup(
            reason(crate::secrets::BackupSecretError::Unchecked).0
        )));
        // Corrupted copies are still rejected by their hashes.
        let (account, key, _) = backup_storage::test_recovery_key(Path::new(&row.backup_path));
        crate::secrets::set_secret(&account, &key, "corrupted full payload").unwrap();
        crate::secrets::set_secret(&account, &format!("{key}-credential-0"), "sk-swapped").unwrap();
        crate::secrets::delete_secret("native", "API_KEY").unwrap();
        refresh_status().unwrap();
        check("corrupt");
        assert!(notice(&path).value_lines[0].contains("integrity"));
        // Missing everywhere is reported as gone, distinct from corrupt.
        crate::secrets::delete_backup_secret(&account, &key).unwrap();
        crate::secrets::delete_backup_secret(&account, &format!("{key}-credential-0")).unwrap();
        refresh_status().unwrap();
        check("missing");
        let gone = crate::i18n::lookup(reason(crate::secrets::BackupSecretError::Missing).0);
        assert!(notice(&path).value_lines[0].contains(&gone));
        // Consent from before the scans still deletes the unchanged inventory.
        discard_confirmed(&row.id, &reviewed.manifest).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), config);
    });
}

#[test]
fn unknown_discard_errors_never_echo_and_follow_the_phase() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("unknown").join("mcp.json");
        let row = pending(&path);
        let leaked = "JSON parse error near \"sk-leak\"";
        let safe = safe_discard_error(&row.id, leaked);
        assert!(!safe.contains("sk-leak"));
        assert!(safe.contains("Nothing was deleted"));
        assert!(!safe.to_lowercase().contains("rolled back"));
        backup_storage::set_discard_phase(Path::new(&row.backup_path), DiscardPhase::Started)
            .unwrap();
        let safe = safe_discard_error(&row.id, leaked);
        assert!(safe.contains("may already be gone"));
        assert!(safe.contains(&crate::i18n::format_message(FINISH_ACTION, &[])));
        assert_eq!(discard_worker_error(&row.id), safe);
    });
}

#[test]
fn only_the_canonical_receipt_ignores_status_named_fields() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let path = data.join("legacy").join("mcp.json");
        let row = pending(&path);
        unlock();
        // A legacy/native timestamped backup whose user settings share the
        // names of Toolport's receipt status fields.
        let legacy = backup_dir("cursor").unwrap().join("1-mcp.json");
        let user = |issues: &str, ready: &str| {
            serde_json::json!({
                "mcpServers": {},
                "recoveryIssues": issues,
                "recoveryAvailable": ready,
                "recoveryDismissed": false,
                "discardPhase": "user value"
            })
            .to_string()
        };
        std::fs::write(&legacy, user("user", "user")).unwrap();
        let reviewed = prepare_discard(&row.id).unwrap();
        assert!(reviewed.lines.contains(&crate::i18n::trf(
            "Backup file: {file}",
            &[("file", "1-mcp.json")]
        )));
        for edited in [user("edited", "user"), user("user", "edited")] {
            std::fs::write(&legacy, &edited).unwrap();
            assert_ne!(
                prepare_discard(&row.id).unwrap().manifest,
                reviewed.manifest,
                "an edited backup file is a different inventory"
            );
        }
        let error = discard_confirmed(&row.id, &reviewed.manifest).unwrap_err();
        assert!(error.contains("changed since you reviewed"));
        assert!(legacy.exists());
        assert_eq!(phase(&row), None);
        // Toolport's own status writes to the receipt still keep consent.
        let current = prepare_discard(&row.id).unwrap();
        backup_storage::set_issues(Path::new(&row.backup_path), &[], &[]).unwrap();
        backup_storage::set_dismissed(Path::new(&row.backup_path), true).unwrap();
        assert_eq!(prepare_discard(&row.id).unwrap().manifest, current.manifest);
        discard_confirmed(&row.id, &current.manifest).unwrap();
        assert!(!legacy.exists());
    });
}

#[test]
fn started_record_stays_discoverable_beside_a_good_record() {
    crate::secrets::tests::with_isolated_vault(|| {
        let data = crate::registry::conduit_dir().unwrap();
        let started_path = data.join("started").join("mcp.json");
        let good_path = data.join("good").join("mcp.json");
        let started = pending(&started_path);
        let good = pending(&good_path);
        backup_storage::set_discard_phase(Path::new(&started.backup_path), DiscardPhase::Started)
            .unwrap();
        let started_files = copies(&started_path);
        let started_config = std::fs::read(&started_path).unwrap();
        // Inventory lists both; Finish stays reachable for the started one.
        let rows = list().unwrap();
        let listed = rows.iter().find(|r| r.id == started.id).unwrap();
        assert!(listed.discard_started && listed.error.is_none());
        assert!(rows.iter().any(|r| r.id == good.id && r.can_retry));
        let finish = prepare_discard(&started.id).unwrap();
        assert_eq!(
            finish.action_label,
            crate::i18n::format_message(FINISH_ACTION, &[])
        );
        // Read-only recorded paths keep both configs, with no receipt errors.
        let recorded = restore::recorded_paths();
        for path in [&started_path, &good_path] {
            assert!(recorded.iter().any(|(id, recorded, format)| id == "cursor"
                && recorded == path
                && format.is_ok()));
        }
        assert!(!recorded.iter().any(|(_, _, format)| format.is_err()));
        // Restoring the good record is unaffected; the started one is refused.
        unlock();
        assert_eq!(
            disconnect::restore_path("cursor", &started_path, Format::JsonMcpServers, None)
                .unwrap_err(),
            discard_started_refusal()
        );
        // Both configs share the basename mcp.json. Plain timestamped copies have
        // no source binding, so neither inventory may claim the other's.
        let good_files = copies(&good_path);
        assert!(
            good_files.iter().all(|f| !started_files.contains(f)),
            "inventories of two configs never overlap"
        );
        let dir = backup_dir("cursor").unwrap();
        let before: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        disconnect::restore_path("cursor", &good_path, Format::JsonMcpServers, None).unwrap();
        for file in before.iter().filter(|f| !good_files.contains(f)) {
            assert!(
                file.exists(),
                "restoring one config kept {}",
                file.display()
            );
        }
        assert!(std::fs::read_to_string(&good_path)
            .unwrap()
            .contains(SECRET));
        assert!(!list().unwrap().iter().any(|r| r.id == good.id));
        assert_eq!(std::fs::read(&started_path).unwrap(), started_config);
        for file in &started_files {
            assert!(file.exists());
        }
        assert!(list().unwrap().iter().any(|r| r.id == started.id));
    });
}

#[test]
fn discarding_one_location_keeps_legacy_ambiguity_for_the_other() {
    for discard_second in [false, true] {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let a = data.join("a/mcp.json");
            let b = data.join("b/mcp.json");
            let first = pending(&a);
            let second = pending(&b);
            let legacy = backup_dir("cursor").unwrap().join("5-mcp.json");
            let bytes = b"{\"legacy\":\"ambiguous original history\"}\n";
            std::fs::write(&legacy, bytes).unwrap();
            assert!(!copies(&a).contains(&legacy));
            assert!(!copies(&b).contains(&legacy));
            unlock();
            discard(&first.id).unwrap();
            let guards: Vec<_> = std::fs::read_dir(backup_dir("cursor").unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("released-"))
                .collect();
            assert_eq!(guards.len(), 1);
            let guard = std::fs::read_to_string(guards[0].path()).unwrap();
            assert!(!guard.contains(SECRET));
            assert!(!guard.contains("TOOLPORT_MISSING_CREDENTIAL_"));
            let guard: serde_json::Value = serde_json::from_str(&guard).unwrap();
            assert_eq!(guard["configPath"], a.to_string_lossy().as_ref());
            assert!(guard["original"].is_null() && guard["baseline"].is_null());
            assert!(guard.get("vaultRecovery").is_none());
            assert!(legacy.exists());
            assert!(
                !copies(&b).contains(&legacy),
                "discarding A must not authorize B to claim their ambiguous legacy copy"
            );
            if discard_second {
                discard(&second.id).unwrap();
            } else {
                disconnect::restore_path("cursor", &b, Format::JsonMcpServers, None).unwrap();
                assert!(std::fs::read_to_string(&b).unwrap().contains(SECRET));
            }
            assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
        });
    }
}
