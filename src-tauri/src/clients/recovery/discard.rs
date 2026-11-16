//! Explicit, confirmed deletion of one config's recovery copies.
//!
//! The user reviews an exact inventory (`prepare_discard`) and confirms its
//! manifest. Under the client and mutation locks the inventory is compared again,
//! the receipt records `started` before any vault release and `valuesRemoved`
//! after every release, other files are unlinked first and the receipt last.
//! A phase only restricts restoration and display; deletion always needs a
//! fresh confirmed manifest. Every error is composed from typed categories.
use super::*;
use backup_storage::DiscardPhase;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardConfirmation {
    /// Fingerprint of the exact files shown; Discard refuses any other inventory.
    pub manifest: String,
    pub heading: String,
    pub description: String,
    /// One bounded list: values first, then the exact current backup files.
    pub lines: Vec<String>,
    pub location: String,
    pub action_label: String,
}

// Receipt status writes that must not revoke consent. Everything else, including
// the immutable vaultRecovery payload, source binding and published revision, counts.
const MUTABLE_STATUS: [&str; 6] = [
    "recoveryIssues",
    "recoveryResolved",
    "recoveryAvailable",
    "recoveryCleanupState",
    "recoveryDismissed",
    "discardPhase",
];
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
/// Only the canonical Original Snapshot receipt receives Toolport's status
/// writes, so only it may ignore those fields. Any other file (timestamped or
/// legacy native backups, the move record) is hashed byte for byte, even if a
/// user setting happens to share a status field's name.
fn file_digest(file: &Path, receipt: &Path) -> Result<String, ()> {
    let raw = std::fs::read(file).map_err(|_| ())?;
    if file == receipt {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(&raw) {
            let canonical = map.get("toolportBackupVersion").and_then(|v| v.as_u64()) == Some(1)
                || (map.get("version").and_then(|v| v.as_u64()) == Some(1)
                    && map.get("configPath").is_some_and(|v| v.is_string()));
            if canonical {
                // Sorted keys, so removing a status field cannot reorder the rest.
                let kept: std::collections::BTreeMap<_, _> = map
                    .into_iter()
                    .filter(|(key, _)| !MUTABLE_STATUS.contains(&key.as_str()))
                    .collect();
                let text = serde_json::to_string(&kept).map_err(|_| ())?;
                return Ok(format!("receipt:{}", digest(text.as_bytes())));
            }
        }
    }
    Ok(format!("raw:{}", digest(&raw)))
}
pub(super) fn fingerprint(files: &[PathBuf], receipt: &Path) -> Result<String, ()> {
    let mut text = String::new();
    for file in files {
        text.push_str(&file.to_string_lossy());
        text.push('\0');
        text.push_str(&file_digest(file, receipt)?);
        text.push('\n');
    }
    Ok(digest(text.as_bytes()))
}

#[derive(Clone, Copy)]
enum Done {
    Nothing,
    Partial,
    ValuesGone,
}
enum Repair<'a> {
    Changed,
    Folder(&'a Path),
    List(&'a Path),
    File(&'a Path),
    Store(crate::secrets::BackupSecretError),
    StoreUnknown,
    Unknown,
}
impl Done {
    fn of(phase: Option<DiscardPhase>) -> Self {
        match phase {
            None => Self::Nothing,
            Some(DiscardPhase::Started) => Self::Partial,
            Some(DiscardPhase::ValuesRemoved) => Self::ValuesGone,
        }
    }
    fn action(self) -> &'static str {
        match self {
            Self::Nothing => DISCARD_ACTION,
            _ => FINISH_ACTION,
        }
    }
}
// Messages this process composed from typed categories. Only these (plus the
// static refusals below) pass through `safe_discard_error` unchanged.
static ISSUED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
fn issue(message: String) -> String {
    let mut issued = ISSUED.lock().unwrap_or_else(|e| e.into_inner());
    if !issued.contains(&message) {
        if issued.len() >= 64 {
            issued.remove(0);
        }
        issued.push(message.clone());
    }
    message
}
fn failure(client: &str, done: Done, repair: Repair<'_>) -> String {
    let status = match done {
        Done::Nothing => crate::i18n::trf("Nothing was deleted. The saved values and recovery files for {client} are kept.", &[("client", client)]),
        Done::Partial => crate::i18n::trf("Discard needs to finish. Some saved copies for {client} may already be gone. The recovery files are kept.", &[("client", client)]),
        Done::ValuesGone => crate::i18n::trf("Discard needs to finish. The saved values for {client} were deleted, but some recovery files remain.", &[("client", client)]),
    };
    let action = crate::i18n::format_message(done.action(), &[]);
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let repair = match repair {
        Repair::Changed => crate::i18n::tr("The recovery files changed since you reviewed them. Review the list again, then confirm."),
        Repair::Folder(dir) => crate::i18n::trf("Toolport could not update the recovery folder. Check that it is writable, then choose {action}:\n{path}", &[("action", &action), ("path", &path(dir))]),
        Repair::List(dir) => crate::i18n::trf("Toolport could not list the recovery files. Check the recovery folder and its permissions, then choose {action}:\n{path}", &[("action", &action), ("path", &path(dir))]),
        Repair::File(file) => crate::i18n::trf("Toolport could not delete a recovery file. Check its permissions and the folder's, then choose {action}:\n{path}", &[("action", &action), ("path", &path(file))]),
        Repair::Store(state) => action_step(state, done.action()),
        Repair::StoreUnknown => crate::i18n::trf("Toolport could not delete the saved copies. Check the recovery files and your credential store, then choose {action}.", &[("action", &action)]),
        Repair::Unknown => crate::i18n::trf("Discard did not finish reporting its result. Check the recovery files and your credential store, then choose {action} to review what remains.", &[("action", &action)]),
    };
    issue(format!("{status}\n\n{repair}"))
}
fn untrusted() -> String {
    crate::i18n::tr("This recovery record cannot be safely discarded. Repair its provenance first.")
}
fn not_pending() -> String {
    crate::i18n::tr("Client recovery is no longer pending. Refresh the Clients page.")
}
fn folder(receipt: &Receipt) -> &Path {
    receipt.file.parent().unwrap_or(&receipt.file)
}
fn shared_tail(path: &str) -> String {
    crate::i18n::trf("The current client config at {path} will not change. Any temporary text for missing values will stay there; re-enter those values in the client if needed. Toolport's server credentials will not change.", &[("path", path)])
}

/// Lazy, read-only review of exactly what Discard (or Finish) would delete.
/// Reads only recovery metadata and file bytes; never contacts a vault.
pub fn prepare_discard(id: &str) -> Result<DiscardConfirmation, String> {
    let receipt = find(id)?;
    if receipt.notice.can_discard_file {
        return prepare_file_discard(&receipt);
    }
    if !receipt.notice.can_discard {
        return Err(untrusted());
    }
    let notice = &receipt.notice;
    let done = Done::of(receipt.phase);
    let files = restore::recovery_copies(&notice.client_id, Path::new(&notice.path))
        .map_err(|_| failure(&notice.client_name, done, Repair::List(folder(&receipt))))?;
    let manifest = fingerprint(&files, &receipt.file)
        .map_err(|_| failure(&notice.client_name, done, Repair::List(folder(&receipt))))?;
    let values: &[String] = if matches!(done, Done::ValuesGone) {
        &[]
    } else {
        &notice.discard_values
    };
    let (value_count, file_count) = (values.len().to_string(), files.len().to_string());
    let counts = [
        ("client", notice.client_name.as_str()),
        ("value_count", value_count.as_str()),
        ("file_count", file_count.as_str()),
        ("path", notice.path.as_str()),
    ];
    let (heading, description) = match done {
        Done::Nothing => (
            crate::i18n::trf("Discard saved values for {client}?", &counts),
            crate::i18n::trf("Toolport will permanently delete recovery copies for {client}. Saved values: {value_count}. Backup files: {file_count}.\n\nThe current client config at {path} will not change. Any temporary text for missing values will stay there; re-enter those values in the client if needed. Toolport's server credentials will not change.", &counts),
        ),
        Done::Partial => (
            crate::i18n::trf("Finish discarding saved values for {client}?", &counts),
            format!(
                "{}\n\n{}",
                crate::i18n::trf("Toolport already started deleting recovery copies for {client}, so some may already be gone. Saved values to finish discarding: {value_count}. Backup files left to delete: {file_count}.", &counts),
                shared_tail(&notice.path)
            ),
        ),
        Done::ValuesGone => (
            crate::i18n::trf("Finish discarding saved values for {client}?", &counts),
            format!(
                "{}\n\n{}",
                crate::i18n::trf("The saved values for {client} were already deleted. Backup files left to delete: {file_count}.", &counts),
                shared_tail(&notice.path)
            ),
        ),
    };
    let mut lines: Vec<String> = values
        .iter()
        .map(|value| crate::i18n::trf("Value: {value}", &[("value", value)]))
        .collect();
    lines.extend(files.iter().map(|file| {
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string_lossy().into_owned());
        crate::i18n::trf("Backup file: {file}", &[("file", &name)])
    }));
    Ok(DiscardConfirmation {
        manifest,
        heading,
        description,
        lines,
        location: crate::i18n::format_message(
            DISCARD_LOCATION,
            &[("path", &folder(&receipt).to_string_lossy())],
        ),
        action_label: crate::i18n::format_message(done.action(), &[]),
    })
}

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_REMOVE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}
fn remove(file: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_REMOVE.with(|slot| slot.borrow().as_deref() == Some(file)) {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    std::fs::remove_file(file)
}

/// Deletes exactly the reviewed inventory. Returns the shared success prose.
pub fn discard_confirmed(id: &str, manifest: &str) -> Result<String, String> {
    let receipt = find(id)?;
    if !receipt.notice.can_discard && !receipt.notice.can_discard_file {
        return Err(untrusted());
    }
    let _auth = crate::registry_controller::acquire_auth_lock(&format!(
        "client-config:{}",
        receipt.notice.client_id
    ))?;
    let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
    let _mutation = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    // Revalidate provenance, path binding and phase under the locks.
    let receipt = find(id)?;
    if receipt.notice.can_discard_file {
        return discard_file(&receipt, manifest);
    }
    if !receipt.notice.can_discard {
        return Err(untrusted());
    }
    let name = receipt.notice.client_name.as_str();
    let done = Done::of(receipt.phase);
    let files =
        restore::recovery_copies(&receipt.notice.client_id, Path::new(&receipt.notice.path))
            .map_err(|_| failure(name, done, Repair::List(folder(&receipt))))?;
    if fingerprint(&files, &receipt.file).ok().as_deref() != Some(manifest) {
        return Err(failure(name, done, Repair::Changed));
    }
    if receipt.phase != Some(DiscardPhase::ValuesRemoved) {
        // Intent first: a failure here stops before any vault deletion.
        backup_storage::set_discard_phase(&receipt.file, DiscardPhase::Started)
            .map_err(|_| failure(name, done, Repair::Folder(folder(&receipt))))?;
        crate::secrets::with_backup_batch(|| {
            for file in &files {
                backup_storage::release_for_discard(file).map_err(|error| {
                    failure(
                        name,
                        Done::Partial,
                        state_for_instructions(&error).map_or(Repair::StoreUnknown, Repair::Store),
                    )
                })?;
            }
            Ok::<(), String>(())
        })?;
        backup_storage::set_discard_phase(&receipt.file, DiscardPhase::ValuesRemoved)
            .map_err(|_| failure(name, Done::ValuesGone, Repair::Folder(folder(&receipt))))?;
    }
    // Preserve only credential-free ownership before removing the receipt.
    // Other locations must never gain an ambiguous legacy backup by elimination.
    restore::remember_discarded_path(
        &receipt.notice.client_id,
        Path::new(&receipt.notice.path),
        receipt.format,
    )
    .map_err(|_| failure(name, Done::ValuesGone, Repair::Folder(folder(&receipt))))?;
    // The receipt carries the phase, so it is unlinked last.
    let (last, others): (Vec<_>, Vec<_>) = files.iter().partition(|file| **file == receipt.file);
    for file in others.into_iter().chain(last) {
        match remove(file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure(name, Done::ValuesGone, Repair::File(file))),
        }
    }
    Ok(crate::i18n::trf("{client}'s saved recovery copies were deleted. The current config at {path} was not changed. Any temporary text for missing values is still there; re-enter those values in the client if needed.", &[("client", name), ("path", &receipt.notice.path)]))
}

fn is_move_record(receipt: &Receipt) -> bool {
    receipt
        .file
        .file_name()
        .is_some_and(|name| name == moved::RECORD_NAME)
}

/// Review for a receipt that cannot be parsed: exactly that one file. Its vault
/// entries cannot be named from damaged bytes, so they are left for data removal.
fn prepare_file_discard(receipt: &Receipt) -> Result<DiscardConfirmation, String> {
    let notice = &receipt.notice;
    let name = notice.client_name.as_str();
    let files = [receipt.file.clone()];
    let manifest = fingerprint(&files, &receipt.file).map_err(|_| {
        file_failure(name, crate::i18n::trf("Toolport could not list the recovery files. Check the recovery folder and its permissions, then choose {action}:\n{path}", &[("action", &crate::i18n::format_message(DISCARD_FILE_ACTION, &[])), ("path", &folder(receipt).to_string_lossy())]))
    })?;
    let file = receipt
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if is_move_record(receipt) {
        return Ok(DiscardConfirmation {
            manifest,
            heading: crate::i18n::trf("Discard the unreadable record of servers moved from {client}?", &[("client", name)]),
            description: crate::i18n::trf("Toolport cannot read its record of the servers it moved from {client} into Toolport. Without it, Disconnect cannot put those servers back in the client config. Discard deletes only this record. The servers stay in Toolport; to use one in {client} without Toolport, add it there again.\n\nThe client config will not change.", &[("client", name)]),
            lines: vec![crate::i18n::trf("Backup file: {file}", &[("file", &file)])],
            location: crate::i18n::format_message(
                DISCARD_LOCATION,
                &[("path", &folder(receipt).to_string_lossy())],
            ),
            action_label: crate::i18n::format_message(DISCARD_FILE_ACTION, &[]),
        });
    }
    Ok(DiscardConfirmation {
        manifest,
        heading: crate::i18n::trf("Discard the unreadable recovery file for {client}?", &[("client", name)]),
        description: crate::i18n::trf("Toolport cannot read this recovery file, so it cannot put its saved values back. Discard deletes only this file. Saved values it referred to stay in Toolport's credential store until you remove Toolport's data.\n\nThe client config will not change. Any temporary text for missing values will stay there; re-enter those values in the client if needed.", &[("client", name)]),
        lines: vec![crate::i18n::trf("Backup file: {file}", &[("file", &file)])],
        location: crate::i18n::format_message(
            DISCARD_LOCATION,
            &[("path", &folder(receipt).to_string_lossy())],
        ),
        action_label: crate::i18n::format_message(DISCARD_FILE_ACTION, &[]),
    })
}
fn file_failure(client: &str, repair: String) -> String {
    issue(format!(
        "{}\n\n{repair}",
        crate::i18n::trf(
            "Nothing was deleted. The saved values and recovery files for {client} are kept.",
            &[("client", client)]
        )
    ))
}
/// Deletes only the reviewed unreadable receipt, under the caller's locks.
fn discard_file(receipt: &Receipt, manifest: &str) -> Result<String, String> {
    let name = receipt.notice.client_name.as_str();
    let files = [receipt.file.clone()];
    if fingerprint(&files, &receipt.file).ok().as_deref() != Some(manifest) {
        return Err(failure(name, Done::Nothing, Repair::Changed));
    }
    match remove(&receipt.file) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(file_failure(name, crate::i18n::trf("Toolport could not delete a recovery file. Check its permissions and the folder's, then choose {action}:\n{path}", &[("action", &crate::i18n::format_message(DISCARD_FILE_ACTION, &[])), ("path", &receipt.file.to_string_lossy())]))),
    }
    let done = if is_move_record(receipt) {
        crate::i18n::msg("Toolport's damaged record of the servers it moved from {client} was deleted. The servers stay in Toolport, and the client config was not changed.")
    } else {
        crate::i18n::msg(
            "{client}'s unreadable recovery file was deleted. The client config was not changed.",
        )
    };
    Ok(issue(crate::i18n::format_message(
        done,
        &[("client", name)],
    )))
}

/// Only typed Discard results pass unchanged. Anything else becomes a
/// phase-accurate message that neither echoes the error nor claims a rollback.
pub fn safe_discard_error(id: &str, error: &str) -> String {
    let issued = ISSUED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|message| message == error);
    if issued || error == untrusted() || error == not_pending() {
        return error.to_string();
    }
    discard_worker_error(id)
}
/// For a worker that panicked or never reported: describes what is on disk now.
pub fn discard_worker_error(id: &str) -> String {
    match find(id) {
        Ok(receipt) if !receipt.notice.can_discard => {
            receipt.notice.error.unwrap_or_else(untrusted)
        }
        Ok(receipt) => failure(
            &receipt.notice.client_name,
            Done::of(receipt.phase),
            Repair::Unknown,
        ),
        Err(_) => not_pending(),
    }
}
