//! Public, vault-free recovery receipts shared by both shells and uninstall CLI.
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
    pub instructions: String,
    pub retention: String,
    pub details: String,
}
const MESSAGE: &str = crate::i18n::msg("{client} is disconnected.\n\nIts original config is back, except for values Toolport could not read because your keyring is locked or unavailable:\n\n{values}\n\nThe config has placeholders for these values for now.");
const INSTRUCTIONS: &str =
    crate::i18n::msg("Unlock your keyring, then choose Retry to put the values back.");
const RETENTION: &str = crate::i18n::msg("{client} recovery backup is kept at:\n{path}\nIt is kept until recovery succeeds or you remove Toolport data. It does not expire automatically.");
fn message(name: &str, values: &[String]) -> String {
    crate::i18n::format_message(
        MESSAGE,
        &[
            ("client", name),
            (
                "values",
                &values
                    .iter()
                    .map(|value| format!("• {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ],
    )
}
pub(super) fn warnings(id: &str, path: &Path) -> Vec<String> {
    let values = backup_storage::warnings();
    if values.is_empty() {
        return Vec::new();
    }
    let name = find_def(id)
        .map(|d| d.name.to_string())
        .unwrap_or_else(|| id.into());
    let mut parts = vec![message(&name, &values), crate::i18n::lookup(INSTRUCTIONS)];
    if let Ok(backup) = restore::record_path(id, path) {
        parts.push(crate::i18n::format_message(
            RETENTION,
            &[("client", &name), ("path", &backup.to_string_lossy())],
        ));
    }
    vec![parts.join("\n\n")]
}
struct Receipt {
    notice: ClientRecovery,
    file: PathBuf,
    format: Format,
}
// Inventory the trusted backup roots. Never call restore::load/recorded_paths:
// those intentionally decode vaulted payloads, unlike a notice read.
fn receipts() -> Result<Vec<Receipt>, String> {
    let mut notices = Vec::new();
    for def in machine_defs(false) {
        let Some(dir) = backup_dir(def.client_id()) else {
            continue;
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries {
            let file = entry.map_err(|e| e.to_string())?.path();
            if !file
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("original-"))
                || file.extension().is_none_or(|e| e != "json")
            {
                continue;
            }
            let meta = std::fs::symlink_metadata(&file).map_err(|e| e.to_string())?;
            if !meta.is_file() || crate::purge::is_link(&meta) {
                continue;
            }
            let raw = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
            let (record, missing) = backup_storage::raw_metadata(&raw)?;
            let Some(path) = record["configPath"].as_str() else {
                continue;
            };
            // A path must match its existing provenance filename. Callers cannot
            // supply a filesystem path through the retry or dismissal API.
            if record["version"] != 1
                || restore::record_path(def.client_id(), Path::new(path))? != file
            {
                continue;
            }
            if record["disconnected"] != true || missing.is_empty() {
                continue;
            }
            let format =
                serde_json::from_value(record["format"].clone()).map_err(|e| e.to_string())?;
            let values: Vec<_> = missing
                .iter()
                .map(|r| {
                    crate::i18n::trf(
                        "{key} for {server}",
                        &[("key", &r.key), ("server", &r.server)],
                    )
                })
                .collect();
            let backup_path = file.to_string_lossy().into_owned();
            let id = crate::registry::sha256_hex(&format!("{}:{backup_path}", def.client_id()));
            let notice = ClientRecovery {
                id,
                client_id: def.client_id().into(),
                client_name: def.name.into(),
                path: path.into(),
                backup_path: backup_path.clone(),
                missing,
                dismissed: record["recoveryDismissed"].as_bool().unwrap_or(false),
                message: message(def.name, &values),
                instructions: crate::i18n::lookup(INSTRUCTIONS),
                retention: crate::i18n::format_message(
                    RETENTION,
                    &[("client", def.name), ("path", &backup_path)],
                ),
                details: crate::i18n::trf("Config file:\n{config}\n\nRecovery backup:\n{backup}\n\nTemporary placeholder format:\nTOOLPORT_MISSING_CREDENTIAL_*\n\nTo retry from a terminal:\ntoolport-gateway --disconnect-all", &[("config", path), ("backup", &backup_path)]),
            };
            notices.push(Receipt {
                notice,
                file,
                format,
            });
        }
    }
    notices.sort_by(|a, b| a.notice.id.cmp(&b.notice.id));
    notices.dedup_by(|a, b| a.notice.id == b.notice.id);
    Ok(notices)
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
    )?;
    Ok(list()?.into_iter().find(|r| r.id == id))
}
