//! Shared recovery receipts, integrity states and explicit user actions.
use super::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRecovery {
    pub id: String,
    pub client_id: String,
    pub client_name: String,
    pub path: String,
    pub backup_path: String,
    pub missing: Vec<backup_storage::Reference>,
    pub dismissed: bool,
    pub message: String,
    pub description: String,
    pub value_lines: Vec<String>,
    pub discard_values: Vec<String>,
    pub instructions: String,
    pub retention: String,
    pub details: String,
    pub error: Option<String>,
    pub can_discard: bool,
    pub cleanup: bool,
}
const RETENTION: &str = crate::i18n::msg("{client} recovery backup is kept at:\n{path}\nIt is kept until recovery succeeds or you remove Toolport data. It does not expire automatically.");
fn heading(name: &str) -> String {
    crate::i18n::trf("{client} is disconnected.", &[("client", name)])
}
pub(super) fn reason(state: crate::secrets::BackupSecretError) -> (&'static str, &'static str) {
    use crate::secrets::BackupSecretError::*;
    match state {
        Locked=>(crate::i18n::msg("keyring locked"),crate::i18n::msg("Unlock your keyring, then choose {action}.")),
        Missing=>(crate::i18n::msg("saved value is gone"),crate::i18n::msg("The saved value is gone. Re-enter it in the client, or enter it in Toolport and choose {action}.")),
        Corrupt=>(crate::i18n::msg("saved copy failed its integrity check"),crate::i18n::msg("A saved copy failed its integrity check. Re-enter the value in the client, or enter it in Toolport and choose {action}.")),
        Ambiguous=>(crate::i18n::msg("multiple matching saved copies"),crate::i18n::msg("More than one saved copy matches. Resolve the duplicates in your credential store, then choose {action}.")),
        Unavailable=>(crate::i18n::msg("credential store unavailable"),crate::i18n::msg("Your credential store is unavailable. Start or reconnect it, then choose {action}.")),
        AuthenticationFailed=>(crate::i18n::msg("credential store rejected authentication"),crate::i18n::msg("Your credential store rejected authentication. Check its authentication and app access settings, then choose {action}.")),
        Cancelled=>(crate::i18n::msg("credential access cancelled"),crate::i18n::msg("Credential access was cancelled. Allow access in your credential store, then choose {action}.")),
        AccessDenied=>(crate::i18n::msg("permission to read saved value denied"),crate::i18n::msg("Toolport cannot read these saved values. Allow access in your credential store, then choose {action}.")),
        Unchecked=>(crate::i18n::msg("saved value not checked"),crate::i18n::msg("Choose Retry to check these saved values.")),
    }
}
pub(super) fn next_step(state: crate::secrets::BackupSecretError, discard: bool) -> String {
    crate::i18n::format_message(
        reason(state).1,
        &[(
            "action",
            &crate::i18n::tr(if discard {
                "Discard these saved values"
            } else {
                "Retry"
            }),
        )],
    )
}
pub(super) fn state_for_instructions(text: &str) -> Option<crate::secrets::BackupSecretError> {
    use crate::secrets::BackupSecretError::*;
    [
        Locked,
        Missing,
        Corrupt,
        Ambiguous,
        Unavailable,
        AuthenticationFailed,
        Cancelled,
        AccessDenied,
        Unchecked,
    ]
    .into_iter()
    .find(|&s| text == next_step(s, false))
}
fn progress_message(name: &str, restored: usize, accounted: usize, remaining: usize) -> String {
    crate::i18n::trf("{client}: saved values restored: {restored}; already resolved or removed: {accounted}; still missing: {remaining}.",&[("client",name),("restored",&restored.to_string()),("accounted",&accounted.to_string()),("remaining",&remaining.to_string())])
}
pub(super) fn warnings(id: &str, path: &Path) -> Vec<String> {
    let (restored, accounted) = recovery_values::progress();
    let rows = list()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.client_id == id && r.path == path.to_string_lossy())
        .collect::<Vec<_>>();
    if rows.is_empty() && restored + accounted > 0 {
        let name = find_def(id)
            .map(|d| d.name.to_string())
            .unwrap_or_else(|| id.into());
        return vec![progress_message(&name, restored, accounted, 0)];
    }
    rows.into_iter()
        .map(|row| {
            let text = format!(
                "{}\n\n{}\n\n{}",
                row.message, row.instructions, row.retention
            );
            if restored + accounted > 0 {
                format!(
                    "{}\n\n{text}",
                    progress_message(&row.client_name, restored, accounted, row.missing.len())
                )
            } else {
                text
            }
        })
        .collect()
}

struct Receipt {
    notice: ClientRecovery,
    file: PathBuf,
    format: Format,
}
fn error_receipt(
    client_id: &str,
    name: &str,
    file: PathBuf,
    format: Format,
    reason: &str,
) -> Receipt {
    let backup_path = file.to_string_lossy().into_owned();
    Receipt { notice:ClientRecovery {
        id:crate::registry::sha256_hex(&format!("{client_id}:{backup_path}")),client_id:client_id.into(),client_name:name.into(),path:String::new(),backup_path:backup_path.clone(),missing:Vec::new(),dismissed:false,
        message:format!("{}\n\n{}",crate::i18n::trf("{client} recovery needs attention.",&[("client",name)]),crate::i18n::lookup(reason)),description:crate::i18n::lookup(reason),value_lines:Vec::new(),discard_values:Vec::new(),instructions:crate::i18n::tr("Repair the saved recovery file or its permissions, then choose Retry. Other recoveries are unaffected."),
        retention:crate::i18n::trf("Saved recovery information:\n{path}",&[("path",&backup_path)]),details:crate::i18n::trf("Saved recovery information:\n{path}",&[("path",&backup_path)]),error:Some(crate::i18n::lookup(reason)),can_discard:false,cleanup:false,
    },file,format }
}
fn read_receipt(id: &str, name: &str, file: &Path) -> Result<Option<Receipt>, &'static str> {
    let meta = std::fs::symlink_metadata(file).map_err(|_| {
        crate::i18n::msg("Toolport could not inspect this recovery file. Check its permissions.")
    })?;
    if !meta.is_file() || crate::purge::is_link(&meta) {
        return Err(crate::i18n::msg(
            "This recovery file is a link or is not a regular file. Toolport will not follow it.",
        ));
    }
    let raw = std::fs::read_to_string(file).map_err(|_| {
        crate::i18n::msg("Toolport could not read this recovery file. Check its permissions.")
    })?;
    let (record, _) = backup_storage::raw_metadata(&raw).map_err(|_| {
        crate::i18n::msg("This recovery file is malformed. Its saved values have been kept.")
    })?;
    let path = record["configPath"].as_str().ok_or(crate::i18n::msg(
        "This recovery file has no valid config path. Its saved values have been kept.",
    ))?;
    if record["version"] != 1 {
        return Err(crate::i18n::msg("This recovery file uses an unsupported version. Open it with a compatible Toolport version."));
    }
    if !Path::new(path).is_absolute()
        || restore::record_path(id, Path::new(path))
            .map_err(|_| crate::i18n::msg("The recovery file location could not be verified."))?
            != file
    {
        return Err(crate::i18n::msg("This recovery file does not match its original config path. Toolport will not write or discard it."));
    }
    let format:Format=serde_json::from_value(record["format"].clone()).map_err(|_|crate::i18n::msg("This recovery file uses an unsupported config format. Open it with a compatible Toolport version."))?;
    let markers = backup_storage::raw_markers(&raw).map_err(|_| {
        crate::i18n::msg("This recovery file is malformed. Its saved values have been kept.")
    })?;
    if record["disconnected"] != true || markers.is_empty() {
        return Ok(None);
    }
    let statuses:Vec<backup_storage::RecoveryIssue>=serde_json::from_value(record.get("recoveryIssues").cloned().unwrap_or_else(||serde_json::json!([]))).map_err(|_|crate::i18n::msg("This recovery file uses an unsupported recovery status. Open it with a compatible Toolport version."))?;
    let current=match std::fs::read_to_string(path){Ok(s)=>Some(s),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>None,Err(_)=>return Err(crate::i18n::msg("Toolport could not read this client config. Check its permissions; the saved values have been kept."))};
    let mut discard_values = Vec::new();
    for (r, _) in &markers {
        let name = crate::i18n::trf(
            "{key} for {server}",
            &[("key", &r.key), ("server", &r.server)],
        );
        if !discard_values.contains(&name) {
            discard_values.push(name);
        }
    }
    let mut missing = Vec::new();
    let mut value_lines = Vec::new();
    let mut instructions = Vec::new();
    let mut config_error = None;
    for (reference, marker) in markers {
        let present = match current
            .as_deref()
            .map(|s| recovery_values::present(format, s, &marker))
            .transpose()
        {
            Ok(v) => v.unwrap_or(false),
            Err(error) => {
                config_error = Some(diagnostics::at(Path::new(path), error));
                true
            }
        };
        if !present {
            continue;
        }
        if missing.iter().any(|r: &backup_storage::Reference| {
            r.server == reference.server && r.key == reference.key
        }) {
            continue;
        }
        let state = statuses
            .iter()
            .find(|v| {
                v.marker == marker
                    && v.reference.server == reference.server
                    && v.reference.key == reference.key
            })
            .map_or(crate::secrets::BackupSecretError::Unchecked, |v| v.state);
        let (why, _) = reason(state);
        value_lines.push(crate::i18n::trf(
            "{key} for {server}: {reason}",
            &[
                ("key", &reference.key),
                ("server", &reference.server),
                ("reason", &crate::i18n::lookup(why)),
            ],
        ));
        let next = next_step(state, false);
        if !instructions.contains(&next) {
            instructions.push(next);
        }
        missing.push(reference);
    }
    let cleanup = missing.is_empty();
    let description = crate::i18n::tr(if cleanup {
        crate::i18n::msg("The config has no remaining placeholders. Toolport still needs to remove its saved recovery copies.")
    } else {
        crate::i18n::msg("Its original config is back. These values still have placeholders:")
    });
    if cleanup {
        let state = record.get("recoveryCleanupState").and_then(|v| {
            serde_json::from_value::<crate::secrets::BackupSecretError>(v.clone()).ok()
        });
        instructions=vec![state.map(|s|next_step(s,false)).unwrap_or_else(||crate::i18n::tr("Unlock your credential store, then choose Retry to finish removing the saved copies."))];
    }
    let message = format!(
        "{}\n\n{}{}",
        heading(name),
        description,
        if value_lines.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n{}",
                value_lines
                    .iter()
                    .map(|v| format!("• {v}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    );
    let backup_path = file.to_string_lossy().into_owned();
    Ok(Some(Receipt {notice:ClientRecovery {
        id:crate::registry::sha256_hex(&format!("{id}:{backup_path}")),client_id:id.into(),client_name:name.into(),path:path.into(),backup_path:backup_path.clone(),missing,dismissed:record["recoveryDismissed"].as_bool().unwrap_or(false),message,description,value_lines,discard_values,instructions:instructions.join("\n\n"),retention:crate::i18n::format_message(RETENTION,&[("client",name),("path",&backup_path)]),details:crate::i18n::trf("Config file:\n{config}\n\nRecovery backup:\n{backup}\n\nTemporary placeholder format:\nTOOLPORT_MISSING_CREDENTIAL_*\n\nTo retry from a terminal:\ntoolport-gateway --disconnect-all",&[("config",path),("backup",&backup_path)]),error:config_error,can_discard:true,cleanup,
    },file:file.into(),format}))
}
// Inventory each trusted root independently. Errors are records, never a reason
// to hide another client's recovery or silently forget saved locations.
fn receipts() -> Result<Vec<Receipt>, String> {
    let mut records = Vec::new();
    if locations::saved().is_err() {
        let file = crate::registry::conduit_dir()
            .ok_or("Could not resolve data dir")?
            .join("client-locations.json");
        records.push(error_receipt("saved-locations",&crate::i18n::tr("Saved client locations"),file,Format::JsonMcpServers,crate::i18n::msg("Toolport could not read saved client locations. Repair that file or its permissions. Built-in client recoveries are still listed.")));
    }
    let Some(root) = crate::registry::conduit_dir().map(|d| d.join("backups")) else {
        return Ok(records);
    };
    match std::fs::symlink_metadata(&root) {
        Ok(meta) if !meta.is_dir() || crate::purge::is_link(&meta) => {
            records.push(error_receipt("backup-root",&crate::i18n::tr("Client config recovery"),root,Format::JsonMcpServers,crate::i18n::msg("This recovery folder is a link or is not a directory. Toolport will not follow it.")));
            return Ok(records);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
        Err(_) => {
            records.push(error_receipt(
                "backup-root",
                &crate::i18n::tr("Client config recovery"),
                root,
                Format::JsonMcpServers,
                crate::i18n::msg(
                    "Toolport could not inspect the recovery folder. Check its permissions.",
                ),
            ));
            return Ok(records);
        }
        _ => {}
    }
    let dirs = match std::fs::read_dir(&root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(records),
        Err(_) => {
            records.push(error_receipt(
                "backup-root",
                &crate::i18n::tr("Client config recovery"),
                root,
                Format::JsonMcpServers,
                crate::i18n::msg(
                    "Toolport could not read the recovery folder. Check its permissions.",
                ),
            ));
            return Ok(records);
        }
    };
    for dir in dirs {
        let dir = match dir {
            Ok(dir) => dir,
            Err(_) => {
                records.push(error_receipt(
                    "backup-index",
                    &crate::i18n::tr("Client config recovery"),
                    root.clone(),
                    Format::JsonMcpServers,
                    crate::i18n::msg(
                        "Toolport could not inspect a recovery file. Check the folder permissions.",
                    ),
                ));
                continue;
            }
        };
        let id = dir.file_name().to_string_lossy().into_owned();
        let Some(base) = defs()
            .into_iter()
            .find(|d| d.id == id.split('@').next().unwrap_or(""))
        else {
            records.push(error_receipt(&id,&crate::i18n::tr("Client config recovery"),dir.path(),Format::JsonMcpServers,crate::i18n::msg("This recovery belongs to an unsupported client. Use a compatible Toolport version to recover it.")));
            continue;
        };
        if id != base.id && !locations::valid_id(&id) {
            continue;
        }
        let meta = match std::fs::symlink_metadata(dir.path()) {
            Ok(meta) => meta,
            Err(_) => {
                records.push(error_receipt(
                    &id,
                    base.name,
                    dir.path(),
                    base.format,
                    crate::i18n::msg(
                        "Toolport could not inspect the recovery folder. Check its permissions.",
                    ),
                ));
                continue;
            }
        };
        if !meta.is_dir() || crate::purge::is_link(&meta) {
            records.push(error_receipt(&id,base.name,dir.path(),base.format,crate::i18n::msg("This recovery folder is a link or is not a directory. Toolport will not follow it.")));
            continue;
        }
        let entries = match std::fs::read_dir(dir.path()) {
            Ok(v) => v,
            Err(_) => {
                records.push(error_receipt(
                    &id,
                    base.name,
                    dir.path(),
                    base.format,
                    crate::i18n::msg(
                        "Toolport could not read the recovery folder. Check its permissions.",
                    ),
                ));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    records.push(error_receipt(&id,base.name,dir.path(),base.format,crate::i18n::msg("Toolport could not inspect a recovery file. Check the folder permissions.")));
                    continue;
                }
            };
            if !entry.file_name().to_string_lossy().starts_with("original-") {
                continue;
            }
            match read_receipt(&id, base.name, &entry.path()) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(reason) => records.push(error_receipt(
                    &id,
                    base.name,
                    entry.path(),
                    base.format,
                    reason,
                )),
            }
        }
    }
    records.sort_by(|a, b| a.notice.id.cmp(&b.notice.id));
    records.dedup_by(|a, b| a.notice.id == b.notice.id);
    Ok(records)
}

pub fn list() -> Result<Vec<ClientRecovery>, String> {
    Ok(receipts()?.into_iter().map(|r| r.notice).collect())
}
fn find(id: &str) -> Result<Receipt, String> {
    receipts()?
        .into_iter()
        .find(|r| r.notice.id == id)
        .ok_or_else(|| {
            crate::i18n::tr("Client recovery is no longer pending. Refresh the Clients page.")
        })
}
pub fn dismiss(id: &str, dismissed: bool) -> Result<(), String> {
    let receipt = find(id)?;
    if !receipt.notice.can_discard {
        return Err(receipt
            .notice
            .error
            .unwrap_or_else(|| crate::i18n::tr("Repair this recovery record before continuing.")));
    }
    let _auth = crate::registry_controller::acquire_auth_lock(&format!(
        "client-config:{}",
        receipt.notice.client_id
    ))?;
    let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
    let _mutation = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    let receipt = find(id)?;
    backup_storage::set_dismissed(&receipt.file, dismissed)
}
#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_RETRY_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub fn retry(id: &str) -> Result<Option<ClientRecovery>, String> {
    let receipt = find(id)?;
    if !receipt.notice.can_discard {
        return Err(receipt
            .notice
            .error
            .unwrap_or_else(|| crate::i18n::tr("Repair this recovery record before continuing.")));
    }
    #[cfg(test)]
    BEFORE_RETRY_LOCK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
    let _lock = crate::registry_controller::acquire_auth_lock(&format!(
        "client-config:{}",
        receipt.notice.client_id
    ))?;
    // Connect can replace the receipt while Retry waits for this client lock.
    // Never restore the stale revision against a newly connected config.
    let receipt = find(id)?;
    let registry = crate::registry_controller::registry_for_disconnect()?;
    disconnect::restore_path(
        &receipt.notice.client_id,
        Path::new(&receipt.notice.path),
        receipt.format,
        registry
            .client_managed_entries
            .get(&receipt.notice.client_id),
    )
    .map_err(|error| diagnostics::at(Path::new(&receipt.notice.path), error))?;
    Ok(list()?.into_iter().find(|r| r.id == id))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryAttempt {
    pub id: String,
    pub client_id: String,
    pub client_name: String,
    pub restored: usize,
    pub accounted: usize,
    pub remaining: usize,
    pub message: String,
    pub pending: Option<ClientRecovery>,
}
pub fn retry_with_report(id: &str) -> Result<RecoveryAttempt, String> {
    let before = find(id)?.notice;
    let pending = retry(id)?;
    let remaining = pending.as_ref().map_or(0, |r| r.missing.len());
    let (restored, accounted) = recovery_values::progress();
    let message = progress_message(&before.client_name, restored, accounted, remaining);
    Ok(RecoveryAttempt {
        id: before.id,
        client_id: before.client_id,
        client_name: before.client_name,
        restored,
        accounted,
        remaining,
        message,
        pending,
    })
}
/// Explicitly destructive, unlike Dismiss. Never touches the native config.
pub fn discard(id: &str) -> Result<(), String> {
    let receipt = find(id)?;
    if !receipt.notice.can_discard {
        return Err(crate::i18n::tr(
            "This recovery record cannot be safely discarded. Repair its provenance first.",
        ));
    }
    let _auth = crate::registry_controller::acquire_auth_lock(&format!(
        "client-config:{}",
        receipt.notice.client_id
    ))?;
    let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
    let _mutation = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    let receipt = find(id)?;
    if !receipt.notice.can_discard {
        return Err(crate::i18n::tr(
            "This recovery record cannot be safely discarded. Repair its provenance first.",
        ));
    }
    let copies =
        restore::recovery_copies(&receipt.notice.client_id, Path::new(&receipt.notice.path))?;
    for file in &copies {
        if let Err(error) = backup_storage::release_for_discard(file) {
            if let Some(state) = state_for_instructions(&error) {
                backup_storage::set_cleanup_state(&receipt.file, state)?;
            }
            if let Some(state) = state_for_instructions(&error) {
                return Err(next_step(state, true));
            }
            return Err(crate::i18n::tr("Toolport could not delete all saved copies. Unlock or reconnect your credential store, then try Discard again. The recovery record has been kept."));
        }
    }
    for file in copies {
        match std::fs::remove_file(file){Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(_)=>return Err(crate::i18n::tr("Toolport could not remove a saved recovery file. Check its permissions, then try Discard again."))}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(path: &Path) -> ClientRecovery {
        pending_for("cursor", path)
    }
    fn pending_for(client: &str, path: &Path) -> ClientRecovery {
        let original="{\n \"mcpServers\":{\"native\":{\"command\":\"echo\",\"env\":{\"API_KEY\":\"sk-recovery-state-fixture\"}}}\n}\n";
        std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
        crate::secrets::set_secret("native", "API_KEY", "sk-recovery-state-fixture").unwrap();
        backup_storage::register("native", "API_KEY", "sk-recovery-state-fixture");
        std::fs::write(path, original).unwrap();
        mutation::run(client, path, Format::JsonMcpServers, || {
            backup_file(client, path)?;
            write_format(Format::JsonMcpServers, path, &[], true)
        })
        .unwrap();
        backup_storage::clear();
        std::env::set_var("TOOLPORT_SECRET_KEY", "locked-state-fixture");
        disconnect::restore_path(client, path, Format::JsonMcpServers, None).unwrap();
        list()
            .unwrap()
            .into_iter()
            .find(|r| r.path == path.to_string_lossy())
            .unwrap()
    }
    #[test]
    fn shared_values_in_two_clients_keep_independent_recovery_payloads() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let a = pending_for("cursor", &data.join("shared-cursor.json"));
            let b = pending_for("claude-code", &data.join("shared-claude.json"));
            let second_before = std::fs::read(&b.path).unwrap();
            let edited = std::fs::read_to_string(&a.path).unwrap().replacen(
                '{',
                "{\n \"userSetting\":42,",
                1,
            );
            std::fs::write(&a.path, edited).unwrap();
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let done = retry_with_report(&a.id).unwrap();
            assert!(done.pending.is_none());
            assert_eq!(done.restored, 1);
            assert_eq!(done.remaining, 0);
            assert!(std::fs::read_to_string(&a.path)
                .unwrap()
                .contains("\"userSetting\":42"));
            assert_eq!(std::fs::read(&b.path).unwrap(), second_before);
            assert!(Path::new(&b.backup_path).exists());
            let (account, key, _) = backup_storage::test_recovery_key(Path::new(&b.backup_path));
            assert!(crate::secrets::backup_secret_status(&account, &key)
                .unwrap()
                .is_some());
            assert!(retry_with_report(&b.id).unwrap().pending.is_none());
            assert!(std::fs::read_to_string(&b.path)
                .unwrap()
                .contains("sk-recovery-state-fixture"));
        });
    }
    #[test]
    fn bad_recovery_records_and_saved_locations_do_not_hide_good_records() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let good = pending(&data.join("good.json"));
            let dir = backup_dir("cursor").unwrap();
            std::fs::write(
                dir.join("original-malformed.json"),
                "{broken sk-do-not-display",
            )
            .unwrap();
            std::fs::write(dir.join("original-unreadable.json"), [0xff]).unwrap();
            let mut future: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&good.backup_path).unwrap()).unwrap();
            let future_path = data.join("future.json");
            future["configPath"] = future_path.to_string_lossy().to_string().into();
            future["version"] = 99.into();
            std::fs::write(
                restore::record_path("cursor", &future_path).unwrap(),
                future.to_string(),
            )
            .unwrap();
            std::fs::create_dir(data.join("client-locations.json")).unwrap();
            let rows = list().unwrap();
            assert_eq!(rows.len(), 5);
            assert!(rows.iter().any(|r| r.id == good.id && r.can_discard));
            assert_eq!(
                rows.iter()
                    .filter(|r| !r.can_discard && r.error.is_some())
                    .count(),
                4
            );
            assert!(!serde_json::to_string(&rows)
                .unwrap()
                .contains("sk-do-not-display"));
            assert!(rows.iter().any(|r| r.client_id == "saved-locations"));
            let report = disconnect::all(true).unwrap();
            assert!(report
                .iter()
                .any(|r| r.path.ends_with("client-locations.json") && r.error.is_some()));
            assert!(report
                .iter()
                .any(|r| r.path.ends_with("original-malformed.json") && r.error.is_some()));
            assert!(!serde_json::to_string(&report)
                .unwrap()
                .contains("sk-do-not-display"));
        });
    }
    #[test]
    fn discard_releases_only_this_path_and_keeps_receipts_after_midway_failure() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let a = data.join("a").join("mcp.json");
            let b = data.join("b").join("mcp.json");
            std::fs::create_dir_all(a.parent().unwrap()).unwrap();
            std::fs::create_dir_all(b.parent().unwrap()).unwrap();
            let first = pending(&a);
            let second = pending(&b);
            let original_bytes = std::fs::read(&a).unwrap();
            let other_bytes = std::fs::read(&b).unwrap();
            assert!(discard(&first.id).is_err());
            assert!(Path::new(&first.backup_path).exists());
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let copies = restore::recovery_copies("cursor", &a).unwrap();
            let last = copies
                .iter()
                .find(|p| p.to_string_lossy() == first.backup_path)
                .unwrap();
            let (_, key, _) = backup_storage::test_recovery_key(last);
            let failure_key = Box::leak(format!("{key}-credential-0").into_boxed_str());
            crate::secrets::tests::with_failed_write(failure_key, || {
                assert!(discard(&first.id).is_err())
            });
            for copy in &copies {
                assert!(
                    copy.exists(),
                    "retain the entire deletion inventory after failure"
                );
            }
            assert!(Path::new(&second.backup_path).exists());
            assert_eq!(std::fs::read(&b).unwrap(), other_bytes);
            discard(&first.id).unwrap();
            assert_eq!(
                std::fs::read(&a).unwrap(),
                original_bytes,
                "discard never edits native config"
            );
            for copy in &copies {
                assert!(!copy.exists());
            }
            let other = restore::recovery_copies("cursor", &b).unwrap();
            assert!(!other.is_empty());
            assert!(retry(&second.id).unwrap().is_none());
        });
    }
    #[test]
    fn corrupted_or_swapped_copies_never_enter_the_client_config() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let path = data.join("corrupt.json");
            let row = pending(&path);
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            let (account, key, _) = backup_storage::test_recovery_key(Path::new(&row.backup_path));
            crate::secrets::set_secret(&account, &key, "corrupted full payload").unwrap();
            crate::secrets::set_secret(
                &account,
                &format!("{key}-credential-0"),
                "sk-swapped-copy-wrong-value",
            )
            .unwrap();
            crate::secrets::delete_secret("native", "API_KEY").unwrap();
            let attempt = retry_with_report(&row.id).unwrap();
            assert_eq!(attempt.remaining, 1);
            assert_eq!(attempt.restored, 0);
            let pending = attempt.pending.unwrap();
            assert!(pending.value_lines[0].contains("integrity"));
            assert!(Path::new(&row.backup_path).exists());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("TOOLPORT_MISSING"));
            assert!(!text.contains("sk-swapped"));
            crate::secrets::set_secret("native", "API_KEY", "sk-recovery-state-fixture").unwrap();
            let done = retry_with_report(&row.id).unwrap();
            assert_eq!(done.restored, 1);
            assert_eq!(done.remaining, 0);
            assert!(done.pending.is_none());
        });
    }
}

pub fn worker_error() -> String {
    crate::i18n::tr(
        "Recovery did not finish reporting its result. Choose Retry to check the current status.",
    )
}
pub fn safe_action_error(id: &str, error: &str) -> String {
    let row = find(id).ok();
    let path = row
        .as_ref()
        .map(|r| r.notice.path.as_str())
        .unwrap_or("client config");
    for format in ["JSON", "TOML", "YAML"] {
        if error.contains(&format!("{format} syntax error in ")) {
            if let Some((_, position)) = error.rsplit_once(" at line ") {
                if let Some((line, column)) = position.split_once(" column ") {
                    if let (Ok(line), Ok(column)) = (
                        line.parse::<usize>(),
                        column.split(';').next().unwrap_or("").parse::<usize>(),
                    ) {
                        return crate::i18n::trf("{format} syntax error in {path}, line {line}, column {column}. Fix the syntax, then choose Retry. The saved values are kept.",&[("format",format),("path",path),("line",&line.to_string()),("column",&column.to_string())]);
                    }
                }
            }
        }
    }
    for state in [
        crate::secrets::BackupSecretError::Locked,
        crate::secrets::BackupSecretError::Unavailable,
        crate::secrets::BackupSecretError::Ambiguous,
        crate::secrets::BackupSecretError::AccessDenied,
        crate::secrets::BackupSecretError::AuthenticationFailed,
        crate::secrets::BackupSecretError::Cancelled,
    ] {
        if error == next_step(state, true) || error == next_step(state, false) {
            return error.to_string();
        }
    }
    for message in [crate::i18n::msg("Toolport could not delete all saved copies. Unlock or reconnect your credential store, then try Discard again. The recovery record has been kept."),crate::i18n::msg("Toolport could not remove a saved recovery file. Check its permissions, then try Discard again."),crate::i18n::msg("This recovery record cannot be safely discarded. Repair its provenance first.")] {if error==crate::i18n::lookup(message){return error.to_string();}}
    if let Some(row) = row {
        if !row.notice.can_discard {
            return row.notice.error.unwrap_or_else(worker_error);
        }
    }
    crate::i18n::tr("Recovery could not finish. Check the client config and credential-store access, then choose Retry. The recovery record is kept.")
}
