//! Read-only listing refresh feedback and the explicit Scan status probe.
use super::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryListingUpdate {
    pub rows: Vec<ClientRecovery>,
    /// The row that should receive focus: the same record, or the error that replaced it.
    pub focus_id: Option<String>,
    /// Neutral result line. Never set while a related error is listed.
    pub message: Option<String>,
}

fn display_name(client_id: &str, rows: &[ClientRecovery]) -> String {
    if let Some(row) = rows.iter().find(|row| row.client_id == client_id) {
        return row.client_name.clone();
    }
    match client_id {
        "saved-locations" => crate::i18n::tr("Saved client locations"),
        "backup-root" | "backup-index" => crate::i18n::tr("Client config recovery"),
        _ => find_def(client_id)
            .map(|def| def.name.to_string())
            .unwrap_or_else(|| client_id.into()),
    }
}

/// Classifies a refresh after an action on `previous_id` against the complete
/// current inventory. Inventory never contacts a vault.
pub fn refresh_notice(
    previous_id: &str,
    client_id: &str,
    backup_path: &str,
) -> Result<RecoveryListingUpdate, String> {
    let rows = list()?;
    let refreshed = |rows: &[ClientRecovery]| {
        crate::i18n::trf(
            "{client}: recovery listing refreshed.",
            &[("client", &display_name(client_id, rows))],
        )
    };
    if let Some(row) = rows.iter().find(|row| row.id == previous_id) {
        let message = row.error.is_none().then(|| refreshed(&rows));
        return Ok(RecoveryListingUpdate {
            focus_id: Some(row.id.clone()),
            message,
            rows,
        });
    }
    // The old record can be replaced by a different error ID: the same file
    // under a folder-level error, or another error for this client. That is
    // still an error, so it takes focus and no success is reported.
    let old = Path::new(backup_path);
    let covers = |row: &ClientRecovery| {
        let path = Path::new(&row.backup_path);
        path == old || (old.starts_with(path) && path != old)
    };
    let errors = || rows.iter().filter(|row| row.error.is_some());
    let replacement = errors()
        .find(|row| row.client_id == client_id && covers(row))
        .or_else(|| errors().find(|row| covers(row)))
        .or_else(|| errors().find(|row| row.client_id == client_id))
        .map(|row| row.id.clone());
    let message = replacement.is_none().then(|| refreshed(&rows));
    Ok(RecoveryListingUpdate {
        rows,
        focus_id: replacement,
        message,
    })
}

/// Explicit Scan: noninteractively checks each trusted record's saved values
/// and stores only status categories. Client configs and captured recovery
/// content are never written; value text is never stored.
pub fn refresh_status() -> Result<(), String> {
    let rows = receipts()?;
    let mut failed = None;
    crate::secrets::with_backup_batch(|| {
        for row in rows {
            let notice = &row.notice;
            if !notice.can_discard || row.phase.is_some() || notice.missing.is_empty() {
                continue;
            }
            if probe(&notice.id).is_err() && failed.is_none() {
                failed = Some(notice.client_name.clone());
            }
        }
    });
    match failed {
        None => Ok(()),
        Some(client) => Err(crate::i18n::trf("Toolport could not check the saved values for {client}. Check the recovery folder and its permissions, then scan again.", &[("client", &client)])),
    }
}

fn probe(id: &str) -> Result<(), ()> {
    let receipt = find(id).map_err(|_| ())?;
    let _auth = crate::registry_controller::acquire_auth_lock(&format!(
        "client-config:{}",
        receipt.notice.client_id
    ))
    .map_err(|_| ())?;
    let dir = crate::registry::conduit_dir().ok_or(())?;
    let _mutation =
        crate::registry::lock_at(&dir.join("client-config-mutation")).map_err(|_| ())?;
    // Retry, Connect or Discard may have changed the record while we waited.
    let Ok(receipt) = find(id) else {
        return Ok(());
    };
    if !receipt.notice.can_discard || receipt.phase.is_some() {
        return Ok(());
    }
    let config = Path::new(&receipt.notice.path);
    let current = match std::fs::read_to_string(config) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(()),
    };
    // Authenticates each copy against its stored hash; corruption stays Corrupt.
    let values = backup_storage::recovery_values(&receipt.file).map_err(|_| ())?;
    let mut issues = Vec::new();
    let mut ready = Vec::new();
    for value in values {
        // Same rule as the listing: an unparsable config keeps every value owed.
        let owed = current.as_deref().is_some_and(|text| {
            recovery_values::present(receipt.format, text, &value.marker).unwrap_or(true)
        });
        if !owed {
            continue;
        }
        if value.secret.is_some() {
            ready.push(backup_storage::RecoveryReady {
                reference: value.reference,
                marker: value.marker,
            });
        } else {
            issues.push(backup_storage::RecoveryIssue {
                reference: value.reference,
                marker: value.marker,
                state: value.state,
            });
        }
    }
    backup_storage::set_status(&receipt.file, &issues, &ready).map_err(|_| ())
}
