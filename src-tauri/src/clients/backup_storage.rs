//! Vault-backed recovery payloads. The fallback retains formatting and non-secret
//! data, so a locked/lost vault never prevents recovering the user's config.
use super::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub server: String,
    pub key: String,
}
thread_local! {
    static IMPORTS: RefCell<Vec<(Reference, String)>> = const { RefCell::new(Vec::new()) };
    static MISSING: RefCell<bool> = const { RefCell::new(false) };
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}
pub(crate) fn register(server: &str, key: &str, value: &str) {
    IMPORTS.with(|v| {
        v.borrow_mut().push((
            Reference {
                server: server.into(),
                key: key.into(),
            },
            value.into(),
        ))
    });
}
pub(crate) fn clear() {
    IMPORTS.with(|v| v.borrow_mut().clear());
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    toolport_backup_version: u32,
    account: String,
    hash: String,
    key: String,
    #[serde(default)]
    generations: Vec<(String, usize)>,
    fallback: String,
    references: Vec<Reference>,
    #[serde(default)]
    reference_markers: Vec<usize>,
    #[serde(default)]
    value_hashes: Vec<String>,
    #[serde(default)]
    source_config_path: Option<String>,
    #[serde(default)]
    recovery_dismissed: Option<bool>,
    #[serde(default)]
    published_hash: Option<String>,
    #[serde(default)]
    exact_eligible: Option<bool>,
}
fn envelope(text: &str) -> Option<Envelope> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("toolportBackupVersion")?.as_u64()? != 1 {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// A timestamped backup with nothing to vault. Its text is the exact config
/// bytes that were already written unvaulted; the wrapper only adds the same
/// path provenance an Envelope has, so two same-named configs never share it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlainBackup {
    toolport_plain_backup_version: u32,
    source_config_path: String,
    binding: String,
    text: String,
}
fn plain_binding(path: &Path, source: &str, text: &str) -> String {
    format!(
        "{}-{}-{}",
        crate::registry::sha256_hex(&path.to_string_lossy()),
        crate::registry::sha256_hex(source),
        crate::registry::sha256_hex(text)
    )
}
const PLAIN_UNSUPPORTED: &str =
    "This backup uses an unsupported format. Open it with a compatible Toolport version; it was left unchanged.";
const PLAIN_MALFORMED: &str = "This backup file is malformed. Toolport left it unchanged.";
/// `Ok(None)` only for unmarked text (legacy raw backups and native configs).
/// Anything carrying the wrapper marker is either a valid version 1 wrapper
/// or refused: a future or damaged wrapper is never treated as config bytes.
fn plain_backup(text: &str) -> Result<Option<PlainBackup>, String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else {
        return Ok(None);
    };
    let Some(version) = map.get("toolportPlainBackupVersion") else {
        return Ok(None);
    };
    if version.as_u64() != Some(1) {
        return Err(PLAIN_UNSUPPORTED.into());
    }
    serde_json::from_value(serde_json::Value::Object(map))
        .map(Some)
        .map_err(|_| PLAIN_MALFORMED.into())
}
fn wrap_plain(path: &Path, source: &str, text: &str) -> Result<String, String> {
    serde_json::to_string(&PlainBackup {
        toolport_plain_backup_version: 1,
        source_config_path: source.into(),
        binding: plain_binding(path, source, text),
        text: text.into(),
    })
    .map_err(|e| diagnostics::json(&e))
}

// Rewrite string values, never syntax, field names, hashes or recovery paths.
// Recurse through serialized config strings without reformatting their bytes.
fn substitute(text: &str, from: &str, to: &str) -> String {
    if from
        .strip_prefix("TOOLPORT_MISSING_CREDENTIAL_")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        recovery_values::replace_markers(text, &[(from.into(), to.into())])
    } else {
        text.replace(from, to)
    }
}
fn redact(text: &str, secret: &str, marker: &str, depth: usize) -> String {
    if depth > 8 {
        return text.into();
    }
    let bytes = text.as_bytes();
    let mut output = String::new();
    let mut cursor = 0;
    let mut index = 0;
    let mut key = String::new();
    while index < bytes.len() {
        let quote = bytes[index];
        // YAML permits bare strings in block and flow mappings, including a
        // trailing comment. Only replace a complete scalar, never a field name
        // or a substring of another value. JSON strings are handled below.
        if quote != b'"'
            && quote != b'\''
            && secret.len() >= 8
            && secret.bytes().any(|byte| byte.is_ascii_alphabetic())
            && bytes[index..].starts_with(secret.as_bytes())
        {
            let end = index + secret.len();
            let before = index == 0
                || bytes[index - 1].is_ascii_whitespace()
                || b":=,[{".contains(&bytes[index - 1]);
            let after = end == bytes.len()
                || bytes[end].is_ascii_whitespace()
                || b",]}#;".contains(&bytes[end]);
            if before && after {
                output.push_str(&text[cursor..index]);
                output.push_str(marker);
                cursor = end;
                index = end;
                continue;
            }
        }
        // Client JSONC/TOML/YAML comments can repeat a credential. They are
        // recovery data too, and must not retain an unquoted plaintext copy.
        let line_comment = quote == b'#' || bytes[index..].starts_with(b"//");
        let block_comment = bytes[index..].starts_with(b"/*");
        if line_comment || block_comment {
            let start = index;
            if block_comment {
                index += 2;
                while index < bytes.len() && !bytes[index..].starts_with(b"*/") {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            } else {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            let comment = &text[start..index];
            let replacement = substitute(comment, secret, marker);
            if replacement != comment {
                output.push_str(&text[cursor..start]);
                output.push_str(&replacement);
                cursor = index;
            }
            continue;
        }
        if quote != b'"' && quote != b'\'' {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' && quote == b'"' {
                index += 2;
                continue;
            }
            if bytes[index] == quote {
                break;
            }
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        index += 1;
        let token = &text[start..index];
        let value = if quote == b'"' {
            serde_json::from_str::<String>(token).ok()
        } else {
            Some(token[1..token.len() - 1].to_string())
        };
        let next = text[index..].trim_start().as_bytes().first().copied();
        if let Some(value) = value {
            if matches!(next, Some(b':' | b'=')) {
                key = value;
                continue;
            }
            if matches!(
                key.as_str(),
                "configPath"
                    | "originalHash"
                    | "lastWrittenHash"
                    | "publishedHash"
                    | "format"
                    | "toolportVersion"
                    | "vaultRecovery"
            ) {
                continue;
            }
            let replacement = if matches!(
                key.as_str(),
                "original" | "lastWritten" | "disconnectBefore" | "text"
            ) {
                redact(&value, secret, marker, depth + 1)
            } else if value == secret || secret.len() >= 8 && value.contains(secret) {
                substitute(&value, secret, marker)
            } else {
                value.clone()
            };
            if replacement != value {
                output.push_str(&text[cursor..start]);
                if quote == b'"' {
                    output.push_str(&serde_json::to_string(&replacement).unwrap());
                } else {
                    output.push('\'');
                    output.push_str(&replacement);
                    output.push('\'');
                }
                cursor = index;
            }
        }
    }
    output.push_str(&text[cursor..]);
    // Plain YAML scalars are the remaining supported unquoted string values.
    output
        .split_inclusive('\n')
        .map(|line| {
            for separator in [": ", "= "] {
                if let Some((_, right)) = line.split_once(separator) {
                    if right.trim() == secret {
                        return line.replacen(right, &right.replace(secret, marker), 1);
                    }
                }
            }
            line.to_string()
        })
        .collect()
}

pub(super) fn encode(path: &Path, text: &str) -> Result<String, String> {
    encode_recovery(path, text, false)
}

pub(super) fn encode_recovery(
    path: &Path,
    text: &str,
    disconnecting: bool,
) -> Result<String, String> {
    encode_internal(path, text, disconnecting, None)
}
pub(super) fn encode_backup(
    path: &Path,
    text: &str,
    disconnecting: bool,
    source: &Path,
) -> Result<String, String> {
    encode_internal(path, text, disconnecting, Some(source))
}
fn encode_internal(
    path: &Path,
    text: &str,
    disconnecting: bool,
    source: Option<&Path>,
) -> Result<String, String> {
    let mut values = IMPORTS.with(|v| v.borrow().clone());
    if !missing() || !disconnecting {
        values.extend(vaulted_credentials(text)?);
    }
    let mut generations = Vec::new();
    // Rewrites of an existing snapshot must retain its protection even outside
    // the original import transaction.
    if let Ok(previous) = std::fs::read_to_string(path) {
        let prior = envelope(&previous).or_else(|| {
            serde_json::from_str::<serde_json::Value>(&previous)
                .ok()
                .and_then(|record| record["vaultRecovery"].as_str().and_then(envelope))
        });
        if let Some(previous) = prior {
            generations = previous.generations;
            if !generations.iter().any(|(key, _)| key == &previous.key) {
                generations.push((previous.key.clone(), previous.references.len()));
            }
            for (index, reference) in previous.references.into_iter().enumerate() {
                if let Some(value) = crate::secrets::get_backup_secret_result(
                    &previous.account,
                    &format!("{}-credential-{index}", previous.key),
                )
                .ok()
                .flatten()
                {
                    values.push((reference, value));
                }
            }
        }
    }
    let mut fallback = text.to_string();
    let mut references = Vec::new();
    let mut retained_values = Vec::new();
    let mut reference_markers = Vec::new();
    for (reference, value) in values {
        if value.is_empty() {
            continue;
        }
        if references.iter().zip(&retained_values).any(
            |(saved, saved_value): (&Reference, &String)| {
                saved.server == reference.server
                    && saved.key == reference.key
                    && saved_value == &value
            },
        ) {
            continue;
        }
        if let Some(index) = retained_values.iter().position(|saved| saved == &value) {
            reference_markers.push(reference_markers[index]);
            references.push(reference);
            retained_values.push(value);
            continue;
        }
        let mut marker_index = references.len();
        while text.contains(&format!("TOOLPORT_MISSING_CREDENTIAL_{marker_index}"))
            || reference_markers.contains(&marker_index)
        {
            marker_index += 1;
        }
        let marker = format!("TOOLPORT_MISSING_CREDENTIAL_{marker_index}");
        let before = fallback.clone();
        fallback = redact(&fallback, &value, &marker, 0);
        if value.len() >= 8
            && value.bytes().any(|byte| byte.is_ascii_alphabetic())
            && fallback.contains(&value)
        {
            return Err("Could not protect every credential occurrence in client recovery. Client config unchanged.".into());
        }
        if before != fallback {
            references.push(reference);
            retained_values.push(value);
            reference_markers.push(marker_index);
        }
    }
    if references.is_empty() {
        return match source {
            Some(source) => wrap_plain(path, &source.to_string_lossy(), text),
            None => Ok(text.into()),
        };
    }
    let hash = crate::registry::sha256_hex(text);
    let account = install_account(
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or("Invalid backup path")?,
    );
    let key = format!(
        "{}-{}{hash}",
        crate::registry::sha256_hex(&path.to_string_lossy()),
        source
            .map(|p| format!("{}-", crate::registry::sha256_hex(&p.to_string_lossy())))
            .unwrap_or_default()
    );
    // The complete byte spelling lives only in the vault. Publish the fallback
    // only after a verified vault write, before modifying the client's file.
    for (index, value) in retained_values.iter().enumerate() {
        crate::secrets::set_secret(&account, &format!("{key}-credential-{index}"), value)?;
    }
    crate::secrets::set_secret(&account, &key, text)?;
    if crate::secrets::get_backup_secret_result(&account, &key)?.as_deref() != Some(text) {
        return Err("Could not verify vaulted client backup. Client config unchanged.".into());
    }
    serde_json::to_string(&Envelope {
        toolport_backup_version: 1,
        account,
        hash,
        key,
        generations,
        fallback,
        references,
        reference_markers,
        source_config_path: source.map(|p| p.to_string_lossy().into_owned()),
        value_hashes: retained_values
            .iter()
            .map(|value| crate::registry::sha256_hex(value))
            .collect(),
        recovery_dismissed: None,
        published_hash: None,
        exact_eligible: None,
    })
    .map_err(|e| e.to_string())
}

// Notice operations inspect only public fallback metadata. They must never
// contact a vault (even an unlocked one) or change a client config.
pub(super) fn raw_metadata(text: &str) -> Result<(serde_json::Value, Vec<Reference>), String> {
    let mut text = text.to_owned();
    let mut refs = Vec::new();
    let mut dismissed = None;
    let mut issues = None;
    let mut cleanup_state = None;
    let mut available = None;
    let mut phase = None;
    for _ in 0..8 {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            issues = issues.or_else(|| value.get("recoveryIssues").cloned());
            cleanup_state = cleanup_state.or_else(|| value.get("recoveryCleanupState").cloned());
            available = available.or_else(|| value.get("recoveryAvailable").cloned());
            phase = phase.or_else(|| value.get("discardPhase").cloned());
        }
        if let Some(saved) = envelope(&text) {
            dismissed = dismissed.or(saved.recovery_dismissed);
            refs.extend(saved.references);
            text = saved.fallback;
            continue;
        }
        let mut value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| diagnostics::json(&e))?;
        dismissed = dismissed.or(value["recoveryDismissed"].as_bool());
        if let Some(recovery) = value["vaultRecovery"].as_str() {
            if let Some(saved) = envelope(recovery) {
                refs.extend(saved.references);
            }
        }
        value["recoveryDismissed"] = dismissed.unwrap_or(false).into();
        if let Some(issues) = issues {
            value["recoveryIssues"] = issues;
        }
        if let Some(state) = cleanup_state {
            value["recoveryCleanupState"] = state;
        }
        if let Some(available) = available {
            value["recoveryAvailable"] = available;
        }
        if let Some(phase) = phase {
            value["discardPhase"] = phase;
        }
        refs.sort_by(|a, b| (&a.server, &a.key).cmp(&(&b.server, &b.key)));
        refs.dedup_by(|a, b| a.server == b.server && a.key == b.key);
        return Ok((value, refs));
    }
    Err("Client recovery metadata is nested too deeply".into())
}
fn merge_recovery_metadata(
    payload: String,
    dismissed: Option<bool>,
    published: Option<&str>,
    eligible: Option<bool>,
) -> Result<String, String> {
    if dismissed.is_none() && published.is_none() && eligible.is_none() {
        return Ok(payload);
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return Ok(payload);
    };
    if value.get("originalHash").is_none() {
        return Ok(payload);
    }
    if let Some(dismissed) = dismissed {
        value["recoveryDismissed"] = dismissed.into();
    }
    if let Some(published) = published {
        value["publishedHash"] = published.into();
    }
    if let Some(eligible) = eligible {
        value["exactEligible"] = eligible.into();
    }
    serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))
}
pub(super) fn set_dismissed(path: &Path, dismissed: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| diagnostics::json(&e))?;
    value["recoveryDismissed"] = dismissed.into();
    crate::registry::atomic_write(
        path,
        &serde_json::to_string(&value).map_err(|e| e.to_string())?,
    )
}

pub(super) fn decode(text: &str) -> Result<String, String> {
    if let Some(plain) = plain_backup(text)? {
        // Path ownership is checked by `source_path`, like an Envelope's key;
        // reading only requires the stored bytes to be the ones recorded.
        let hash = format!("-{}", crate::registry::sha256_hex(&plain.text));
        if !plain.binding.ends_with(&hash) {
            return Err("Saved backup copy failed its integrity check".into());
        }
        return Ok(plain.text);
    }
    let Some(saved) = envelope(text) else {
        return Ok(text.into());
    };
    match crate::secrets::get_backup_secret_result(&saved.account, &saved.key) {
        Ok(Some(payload)) if crate::registry::sha256_hex(&payload) == saved.hash => {
            merge_recovery_metadata(
                payload,
                saved.recovery_dismissed,
                saved.published_hash.as_deref(),
                saved.exact_eligible,
            )
        }
        result => {
            // A missing/damaged full payload can still be reconstructed exactly
            // from its independent copies. The full hash authenticates legacy
            // envelopes too; no partial unauthenticated copy is ever accepted.
            if matches!(result, Ok(None) | Ok(Some(_))) {
                if let Some(payload) = verified_copy_payload(&saved) {
                    return merge_recovery_metadata(
                        payload,
                        saved.recovery_dismissed,
                        saved.published_hash.as_deref(),
                        saved.exact_eligible,
                    );
                }
            }
            MISSING.with(|v| *v.borrow_mut() = true);
            for reference in &saved.references {
                let warning = crate::i18n::trf(
                    "{key} for {server}",
                    &[("key", &reference.key), ("server", &reference.server)],
                );
                WARNINGS.with(|v| {
                    let mut v = v.borrow_mut();
                    if !v.contains(&warning) {
                        v.push(warning);
                    }
                });
            }
            // Snapshot hashes authenticate the recovered bytes, including the
            // explicit missing-value markers when the vault is unavailable.
            let mut fallback = saved.fallback;
            if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&fallback) {
                if record.get("originalHash").is_some() {
                    record["vaultRecovery"] = serde_json::Value::String(text.into());
                    for (field, hash) in [
                        ("original", "originalHash"),
                        ("lastWritten", "lastWrittenHash"),
                    ] {
                        record[hash] = record[field]
                            .as_str()
                            .map(crate::registry::sha256_hex)
                            .map(serde_json::Value::String)
                            .unwrap_or(serde_json::Value::Null);
                    }
                    fallback = serde_json::to_string(&record).map_err(|e| e.to_string())?;
                }
            }
            merge_recovery_metadata(
                fallback,
                saved.recovery_dismissed,
                saved.published_hash.as_deref(),
                saved.exact_eligible,
            )
        }
    }
}
fn verified_copy_payload(saved: &Envelope) -> Option<String> {
    let mut payload = saved.fallback.clone();
    for (index, reference) in saved.references.iter().enumerate() {
        let copy = crate::secrets::backup_secret_status(
            &saved.account,
            &format!("{}-credential-{index}", saved.key),
        )
        .ok()
        .flatten();
        let value = copy
            .filter(|value| {
                saved
                    .value_hashes
                    .get(index)
                    .is_none_or(|hash| crate::registry::sha256_hex(value) == *hash)
            })
            .or_else(|| {
                crate::secrets::backup_secret_status(&reference.server, &reference.key)
                    .ok()
                    .flatten()
                    .filter(|v| {
                        saved
                            .value_hashes
                            .get(index)
                            .is_some_and(|h| crate::registry::sha256_hex(v) == *h)
                    })
            })?;
        payload = redact(&payload, &marker(saved, index), &value, 0);
    }
    (crate::registry::sha256_hex(&payload) == saved.hash).then_some(payload)
}

pub(super) fn read(path: &Path) -> Result<String, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    decode(&text).map_err(std::io::Error::other)
}

/// Rollback may publish only the exact pre-operation bytes, never a recovery
/// template with missing-value markers. Keep the current config if the saved
/// payload cannot be authenticated and recovered in full.
pub(super) fn read_exact(path: &Path, source: &Path) -> Result<String, String> {
    let invalid = || {
        crate::i18n::tr("This recovery file does not match its original config path. Toolport will not write or discard it.")
    };
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || crate::purge::is_link(&meta) {
        return Err(invalid());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if source_path_in(path, &raw)?.is_some_and(|saved| Path::new(&saved) != source) {
        return Err(invalid());
    }
    let Some(saved) = envelope(&raw) else {
        if serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .is_some_and(|v| v.get("toolportBackupVersion").is_some())
        {
            return Err(crate::i18n::tr("This recovery file uses an unsupported version. Open it with a compatible Toolport version."));
        }
        return decode(&raw);
    };
    crate::secrets::with_backup_batch(|| {
        use crate::secrets::BackupSecretError;
        let result = crate::secrets::backup_secret_status(&saved.account, &saved.key);
        if let Ok(Some(payload)) = &result {
            if crate::registry::sha256_hex(payload) == saved.hash {
                return Ok(payload.clone());
            }
        }
        if let Some(payload) = verified_copy_payload(&saved) {
            return Ok(payload);
        }
        let state = match result {
            Ok(None) => BackupSecretError::Missing,
            Ok(Some(_)) => BackupSecretError::Corrupt,
            Err(state) => state,
        };
        Err(recovery::next_step(state, false))
    })
}
pub(super) fn missing() -> bool {
    MISSING.with(|v| *v.borrow())
}
pub(super) fn reset_missing() {
    MISSING.with(|v| *v.borrow_mut() = false);
    WARNINGS.with(|v| v.borrow_mut().clear());
}
#[cfg(test)]
pub(super) fn warnings() -> Vec<String> {
    WARNINGS.with(|v| v.borrow().clone())
}

pub(crate) fn install_account(dir: &Path) -> String {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.into());
    format!(
        "client-backup-{}",
        crate::registry::sha256_hex(&dir.to_string_lossy())
    )
}
pub(super) fn release(path: &Path) -> Result<(), String> {
    release_owned(path, false)
}
pub(super) fn release_for_discard(path: &Path) -> Result<(), String> {
    release_owned(path, true)
}
fn release_owned(path: &Path, discard_all: bool) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Could not inspect the recovery file; saved copies retained".into()),
    };
    if !metadata.is_file() || crate::purge::is_link(&metadata) {
        return Err("Recovery file is not a regular file; saved copies retained".into());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|_| "Could not read the recovery file; saved copies retained")?;
    fn release_text(
        path: &Path,
        text: &str,
        depth: usize,
        discard_all: bool,
    ) -> Result<(), String> {
        if depth > 8 {
            return Err("Client recovery metadata is nested too deeply".into());
        }
        if let Some(mut saved) = envelope(text) {
            let fallback = saved.fallback.clone();
            let Some(dir) = path.parent().and_then(Path::parent).and_then(Path::parent) else {
                return Ok(());
            };
            let prefix = format!("{}-", crate::registry::sha256_hex(&path.to_string_lossy()));
            // A copied backup can refer to another install or config's vault.
            // Restoring a copy never grants permission to delete its source.
            if saved.account != install_account(dir) || !saved.key.starts_with(&prefix) {
                return Ok(());
            }
            saved.generations.push((saved.key, saved.references.len()));
            let mut generations = std::collections::BTreeMap::<String, usize>::new();
            for (key, count) in saved.generations {
                generations
                    .entry(key)
                    .and_modify(|old| *old = (*old).max(count))
                    .or_insert(count);
            }
            for (key, count) in generations {
                if !key.starts_with(&prefix) {
                    continue;
                }
                crate::secrets::delete_backup_secret_copies_status(
                    &saved.account,
                    &key,
                    discard_all,
                )
                .map_err(|e| recovery::next_step(e, discard_all))?;
                for index in 0..count {
                    crate::secrets::delete_backup_secret_copies_status(
                        &saved.account,
                        &format!("{key}-credential-{index}"),
                        discard_all,
                    )
                    .map_err(|e| recovery::next_step(e, discard_all))?;
                }
            }
            release_text(path, &fallback, depth + 1, discard_all)?;
        } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(inner) = value["vaultRecovery"].as_str() {
                release_text(path, inner, depth + 1, discard_all)?;
            }
        }
        Ok(())
    }
    release_text(path, &text, 0, discard_all)
}

fn owned_backup_paths() -> Result<Vec<PathBuf>, String> {
    let Some(root) = crate::registry::conduit_dir().map(|dir| dir.join("backups")) else {
        return Ok(Vec::new());
    };
    let ownership = super::purge_backup_inventory();
    let clients = match std::fs::read_dir(root) {
        Ok(clients) => clients,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut paths = Vec::new();
    for client in clients {
        let client = client.map_err(|e| e.to_string())?;
        if !client.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let component = client.file_name();
        let Some(owner) = component.to_str().and_then(|name| ownership.get(name)) else {
            continue;
        };
        for file in std::fs::read_dir(client.path()).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if !file.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            if !file
                .file_name()
                .to_str()
                .is_some_and(|name| crate::purge::owned_backup_file(owner, name))
            {
                continue;
            }
            paths.push(file.path());
        }
    }
    Ok(paths)
}

fn native_value(text: &str) -> Option<serde_json::Value> {
    super::parse_json_value(text)
        .ok()
        .or_else(|| {
            toml::from_str::<toml::Value>(text)
                .ok()
                .and_then(|v| serde_json::to_value(v).ok())
        })
        .or_else(|| serde_yaml::from_str::<serde_json::Value>(text).ok())
}

fn legacy_credentials(text: &str, client: &str) -> Vec<(Reference, String)> {
    use serde_json::Value;
    fn add(values: &mut Vec<(Reference, String)>, source: &str, key: &str, value: &str) {
        if crate::import_credentials::provided(value)
            && !value
                .strip_prefix("TOOLPORT_MISSING_CREDENTIAL_")
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
            && !values.iter().any(|(reference, saved)| {
                reference.server == source && reference.key == key && saved == value
            })
        {
            values.push((
                Reference {
                    server: source.into(),
                    key: key.into(),
                },
                value.into(),
            ));
        }
    }
    fn environment(values: &mut Vec<(Reference, String)>, source: &str, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if let Some(value) = value.as_str() {
                        if crate::import_credentials::secret_env(key, Some(value)) {
                            add(values, source, key, value);
                        }
                    }
                }
            }
            Value::Array(entries) => {
                for entry in entries {
                    if let (Some(key), Some(value)) = (
                        entry
                            .get("key")
                            .or_else(|| entry.get("name"))
                            .and_then(Value::as_str),
                        entry.get("value").and_then(Value::as_str),
                    ) {
                        if crate::import_credentials::secret_env(key, Some(value)) {
                            add(values, source, key, value);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn walk(value: &Value, source: &str, depth: usize, values: &mut Vec<(Reference, String)>) {
        if depth > 32 {
            return;
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    let source = format!("{source}/{key}");
                    if matches!(
                        key.as_str(),
                        "env" | "environment" | "envs" | "headers" | "http_headers"
                    ) {
                        environment(values, &source, value);
                    }
                    if key == "args" {
                        if let Some(args) = value.as_array().and_then(|args| {
                            args.iter()
                                .map(|v| v.as_str().map(String::from))
                                .collect::<Option<Vec<_>>>()
                        }) {
                            for (index, (arg, secret)) in args
                                .iter()
                                .zip(crate::registry::secret_arg_mask(&args))
                                .enumerate()
                            {
                                if secret {
                                    add(values, &source, &format!("argument-{index}"), arg);
                                }
                            }
                        }
                    }
                    if matches!(key.as_str(), "url" | "httpUrl" | "serverUrl") {
                        if let Some(text) = value.as_str() {
                            if url::Url::parse(text).is_ok_and(|url| {
                                !url.username().is_empty()
                                    || url.password().is_some()
                                    || url.query().is_some()
                                    || crate::import_credentials::secret_url_path(&url)
                            }) {
                                add(values, &source, "URL", text);
                            }
                        }
                    }
                    if matches!(
                        key.as_str(),
                        "original" | "lastWritten" | "disconnectBefore" | "text"
                    ) {
                        if let Some(parsed) = value.as_str().and_then(native_value) {
                            walk(&parsed, &source, depth + 1, values);
                        }
                    }
                    walk(value, &source, depth + 1, values);
                }
            }
            Value::Array(entries) => {
                for value in entries {
                    walk(value, source, depth + 1, values);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    if let Some(value) = native_value(text) {
        walk(&value, client, 0, &mut values);
    }
    values
}

// Import transactions register new values before the registry is saved. Later
// connects must also protect credentials already moved into this install's vault.
fn vaulted_credentials(text: &str) -> Result<Vec<(Reference, String)>, String> {
    let registry = crate::registry::resolved_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|raw| serde_json::from_slice::<crate::registry::Registry>(&raw).ok());
    let mut values = Vec::new();
    let credentials_visible = !legacy_credentials(text, "client").is_empty();
    for server in registry.into_iter().flat_map(|registry| registry.servers) {
        let keys = server
            .env
            .iter()
            .filter(|env| env.secret)
            .map(|env| &env.key)
            .chain(
                server
                    .launch
                    .iter()
                    .flat_map(|launch| &launch.inputs)
                    .filter(|input| input.secret)
                    .map(|input| &input.key),
            );
        for key in keys {
            if !text.contains(key) && !credentials_visible {
                continue;
            }
            if let Some(value) = crate::secrets::get_backup_secret_result(&server.id, key)? {
                values.push((
                    Reference {
                        server: server.id.clone(),
                        key: key.clone(),
                    },
                    value,
                ));
            }
        }
    }
    Ok(values)
}

fn discovered_credentials(text: &str, client: &str) -> Vec<(Reference, String)> {
    let nonsecret_values: std::collections::HashSet<String> = crate::registry::resolved_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|raw| serde_json::from_slice::<crate::registry::Registry>(&raw).ok())
        .into_iter()
        .flat_map(|registry| registry.servers)
        .flat_map(|server| server.env)
        .filter(|env| !env.secret)
        .filter_map(|env| env.value)
        .collect();
    let mut values = legacy_credentials(text, client);
    values.retain(|(_, value)| !nonsecret_values.contains(value));
    values
}

/// Upgrade old plaintext recovery files before either desktop shell repoints
/// clients. Publish only after verified vault storage; an unavailable vault
/// leaves the original recovery copy untouched and surfaces a startup failure.
pub(crate) fn upgrade_legacy() -> Result<usize, String> {
    let Some(dir) = crate::registry::conduit_dir().filter(|dir| dir.join("backups").exists())
    else {
        return Ok(0);
    };
    let _lock = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    struct RestoreImports(Vec<(Reference, String)>);
    impl Drop for RestoreImports {
        fn drop(&mut self) {
            IMPORTS.with(|slot| *slot.borrow_mut() = std::mem::take(&mut self.0));
        }
    }
    let mut upgraded = 0;
    for path in owned_backup_paths()? {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("Could not inspect recovery file {}: {e}", path.display()))?;
        if envelope(&raw).is_some() {
            continue;
        }
        // A plain wrapper's credential scan and re-encode use its exact config
        // text. Only a binding that validates for this path is kept; a moved or
        // copied file is re-encoded unbound rather than gaining provenance.
        // Refuse a future or malformed wrapper before any scan or rewrite.
        let plain = plain_backup(&raw).map_err(|e| format!("{e} ({})", path.display()))?;
        let source = source_path(&path).ok().flatten();
        let text = match plain {
            Some(_) => decode(&raw)?,
            None => raw.clone(),
        };
        let client = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or("client");
        let values = discovered_credentials(&text, client);
        if values.is_empty() {
            continue;
        }
        let _restore =
            RestoreImports(IMPORTS.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), values)));
        let encoded = encode_internal(&path, &text, false, source.as_deref().map(Path::new)).map_err(|e| format!("Could not secure legacy recovery file {}: {e}. Original retained; unlock the vault and restart Toolport.", path.display()))?;
        if encoded != raw {
            crate::registry::atomic_write(&path, &encoded)?;
            upgraded += 1;
        }
    }
    Ok(upgraded)
}

pub(crate) fn protect_existing() -> Result<(), String> {
    for path in owned_backup_paths()? {
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        // Refuse a future or malformed wrapper before any rewrite.
        let plain = plain_backup(&raw)?;
        let payload = match envelope(&raw) {
            Some(saved) => crate::secrets::get_backup_secret_result(&saved.account, &saved.key)?
                .filter(|payload| crate::registry::sha256_hex(payload) == saved.hash)
                .ok_or("Unlock the original backup vault before importing more credentials. Client config unchanged.")?,
            None if plain.is_some() => decode(&raw)?,
            None => raw.clone(),
        };
        // Re-encoding keeps a backup's validated path provenance, never a
        // binding copied from another path.
        let source = source_path(&path).ok().flatten();
        let encoded = encode_internal(&path, &payload, false, source.as_deref().map(Path::new))?;
        if encoded != raw {
            crate::registry::atomic_write(&path, &encoded)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rollback_reads_exact_plain_and_vaulted_bytes_but_never_missing_templates() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            reset_missing();
            let dir = crate::registry::conduit_dir()
                .unwrap()
                .join("backups/cursor");
            std::fs::create_dir_all(&dir).unwrap();
            let source = dir
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("native/mcp.json");
            let plain = dir.join("1-mcp.json");
            let bytes = "{ \"mcpServers\": {} }\r\n";
            std::fs::write(
                &plain,
                encode_backup(&plain, bytes, false, &source).unwrap(),
            )
            .unwrap();
            assert_eq!(
                super::super::read_client_backup(&plain, &source).unwrap(),
                bytes
            );
            assert!(
                super::super::read_client_backup(&plain, &source.with_file_name("other.json"))
                    .is_err()
            );
            #[cfg(unix)]
            {
                let link = dir.join("link-mcp.json");
                std::os::unix::fs::symlink(&plain, &link).unwrap();
                assert!(super::super::read_client_backup(&link, &source).is_err());
            }
            let secret = "sk-exact-rollback-fixture";
            let bytes = format!("{{\n  \"mcpServers\": {{\"native\": {{\"command\": \"echo\", \"env\": {{\"API_KEY\": \"{secret}\"}}}}}},\n  \"originalHash\": \"a native setting, not receipt metadata\"\n}}\r\n");
            register("native", "API_KEY", secret);
            let protected = dir.join("2-mcp.json");
            let encoded = encode_backup(&protected, &bytes, false, &source).unwrap();
            std::fs::write(&protected, &encoded).unwrap();
            clear();
            assert_eq!(
                super::super::read_client_backup(&protected, &source).unwrap(),
                bytes
            );
            let unlock = std::env::var("TOOLPORT_SECRET_KEY").unwrap();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-exact-rollback");
            let error = super::super::read_client_backup(&protected, &source).unwrap_err();
            assert!(!error.contains(secret));
            assert!(!error.contains("TOOLPORT_MISSING_CREDENTIAL_"));
            assert!(read(&protected)
                .unwrap()
                .contains("TOOLPORT_MISSING_CREDENTIAL_"));
            assert_eq!(std::fs::read_to_string(&protected).unwrap(), encoded);
            std::env::set_var("TOOLPORT_SECRET_KEY", unlock);
            reset_missing();
            let (account, key, _) = test_recovery_key(&protected);
            crate::secrets::delete_backup_secret(&account, &key).unwrap();
            assert_eq!(
                super::super::read_client_backup(&protected, &source).unwrap(),
                bytes,
                "independent copies must reconstruct the exact payload"
            );
            crate::secrets::delete_backup_secret(&account, &format!("{key}-credential-0")).unwrap();
            assert!(super::super::read_client_backup(&protected, &source).is_err());
            assert_eq!(std::fs::read_to_string(&protected).unwrap(), encoded);
            clear();
            reset_missing();
        });
    }

    #[test]
    fn future_or_malformed_plain_wrappers_are_refused_and_never_rewritten() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            reset_missing();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-plain-backup-fixture");
            let dir = crate::registry::conduit_dir()
                .unwrap()
                .join("backups")
                .join("cursor");
            std::fs::create_dir_all(&dir).unwrap();
            let source = Path::new("/fixture/a/mcp.json");
            let valid_path = dir.join("1-mcp.json");
            let valid: serde_json::Value = serde_json::from_str(
                &encode_backup(&valid_path, "{\"mcpServers\":{}}", false, source).unwrap(),
            )
            .unwrap();
            let mut future = valid.clone();
            future["toolportPlainBackupVersion"] = 2.into();
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove("binding");
            let mut wrong_type = valid.clone();
            wrong_type["text"] = 7.into();
            let mut text_version = valid.clone();
            text_version["toolportPlainBackupVersion"] = "1".into();
            for (name, value, expected) in [
                ("2-mcp.json", future, PLAIN_UNSUPPORTED),
                ("3-mcp.json", text_version, PLAIN_UNSUPPORTED),
                ("4-mcp.json", missing, PLAIN_MALFORMED),
                ("5-mcp.json", wrong_type, PLAIN_MALFORMED),
            ] {
                let raw = value.to_string();
                let file = dir.join(name);
                std::fs::write(&file, &raw).unwrap();
                assert_eq!(decode(&raw).unwrap_err(), expected, "{name}");
                assert!(read(&file).is_err(), "{name}: never read as config");
                assert!(read_config_file(&file).is_err());
                assert_eq!(
                    source_path(&file).unwrap_err(),
                    expected,
                    "{name}: no ownership"
                );
                assert!(protect_existing().is_err(), "{name}: never reprotected");
                // A credential-looking import must not turn it into a sealed config.
                register("native", "API_KEY", "sk-plain-wrapper-fixture-value");
                assert!(upgrade_legacy().is_err(), "{name}: never upgraded");
                clear();
                assert_eq!(
                    std::fs::read_to_string(&file).unwrap(),
                    raw,
                    "{name}: untouched"
                );
                std::fs::remove_file(&file).unwrap();
            }
            // Unmarked legacy raw backups stay supported byte for byte.
            let legacy = dir.join("6-mcp.json");
            let raw = "{ \"mcpServers\": {} , \"note\": \"keep\" }\r\n";
            std::fs::write(&legacy, raw).unwrap();
            assert_eq!(read(&legacy).unwrap(), raw);
            assert_eq!(source_path(&legacy).unwrap(), None);
            assert!(plain_backup("[1,2]").unwrap().is_none());
            assert!(plain_backup("not json").unwrap().is_none());
            protect_existing().unwrap();
            assert_eq!(std::fs::read_to_string(&legacy).unwrap(), raw);
        });
    }
    #[test]
    fn plain_backups_round_trip_exact_bytes_without_a_vault_and_bind_their_source() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            reset_missing();
            // No vault is usable: plain backups must never need one.
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-plain-backup-fixture");
            let dir = crate::registry::conduit_dir()
                .unwrap()
                .join("backups")
                .join("cursor");
            std::fs::create_dir_all(&dir).unwrap();
            let source = Path::new("/fixture/a/mcp.json");
            for text in [
                "{ // é comment\r\n \"mcpServers\": {\"native\":{\"command\":\"native\"}},\r\n\t\"recoveryIssues\": [1] \r\n}\r\n",
                "# toml \u{1F600}\n[mcp_servers.native]\ncommand = 'native'   ",
                "",
            ] {
                let dest = dir.join("1-mcp.json");
                let encoded = encode_backup(&dest, text, false, source).unwrap();
                assert!(
                    plain_backup(&encoded).unwrap().is_some(),
                    "new plain backups are bound"
                );
                assert!(envelope(&encoded).is_none());
                std::fs::write(&dest, &encoded).unwrap();
                assert_eq!(decode(&encoded).unwrap(), text, "byte-exact");
                assert_eq!(read(&dest).unwrap(), text);
                assert_eq!(read_config_file(&dest).unwrap(), text);
                assert_eq!(
                    source_path(&dest).unwrap().as_deref(),
                    Some("/fixture/a/mcp.json")
                );
                assert!(raw_markers(&encoded).unwrap().is_empty());
                release_for_discard(&dest).unwrap();
                // Toolport's own re-encode keeps the same bytes and binding.
                protect_existing().unwrap();
                assert_eq!(std::fs::read_to_string(&dest).unwrap(), encoded);
                // A copy at another path never claims the original's provenance.
                let copy = dir.join("2-mcp.json");
                std::fs::write(&copy, &encoded).unwrap();
                assert!(source_path(&copy).is_err());
                std::fs::remove_file(&copy).unwrap();
                // Tampered text is refused, never returned as wrapper JSON.
                let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
                value["text"] = "tampered".into();
                assert!(decode(&value.to_string()).is_err());
                // Snapshots and move records (no source) keep their spelling.
                assert_eq!(encode(&dir.join("moved-servers.json"), text).unwrap(), text);
            }
        });
    }
    // Runs in Headless Rust tests (macos-latest, core) and
    // Headless Rust tests (windows-latest, core), ci.yml. The portable
    // vault checks backup byte spelling on both; Windows also uses Credential
    // Manager, and signed macOS test apps exercise the data-protection keychain.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn platform_backup_round_trip() -> Result<(), String> {
        clear();
        reset_missing();
        let dir = crate::registry::conduit_dir().unwrap();
        let path = dir.join("backups/cursor/1-mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = format!("{{\r\n \"env\":{{\"API_KEY\":\"sk-native-backup-fixture\"}},\r\n \"padding\":\"{}\"\r\n}}\r\n", "preserve byte spelling ".repeat(500));
        register(
            "native-backup-fixture",
            "API_KEY",
            "sk-native-backup-fixture",
        );
        let encoded = encode(&path, &original);
        clear();
        let encoded = encoded?;
        std::fs::write(&path, &encoded).unwrap();
        let saved = envelope(&encoded).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = release(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        assert!(!encoded.contains("sk-native-backup-fixture"));
        assert_eq!(read(&path).unwrap().as_bytes(), original.as_bytes());
        release(&path)?;
        assert!(crate::secrets::get_vault_secret_result(&saved.account, &saved.key)?.is_none());
        for index in 0..saved.references.len() {
            assert!(crate::secrets::get_vault_secret_result(
                &saved.account,
                &format!("{}-credential-{index}", saved.key)
            )?
            .is_none());
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_encrypted_backup_vault_round_trip() {
        crate::secrets::tests::with_isolated_vault(|| platform_backup_round_trip().unwrap());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn platform_native_backup_vault_round_trip() {
        crate::secrets::tests::with_isolated_vault(|| {
            // The surrounding helper serializes environment changes and owns a
            // temporary data directory. Never fall back to a file vault here.
            struct RestoreEnv(Option<std::ffi::OsString>);
            impl Drop for RestoreEnv {
                fn drop(&mut self) {
                    if let Some(value) = &self.0 {
                        std::env::set_var("CONDUIT_SECRET_KEY", value);
                    } else {
                        std::env::remove_var("CONDUIT_SECRET_KEY");
                    }
                }
            }
            let _restore = RestoreEnv(std::env::var_os("CONDUIT_SECRET_KEY"));
            std::env::remove_var("TOOLPORT_SECRET_KEY");
            std::env::remove_var("CONDUIT_SECRET_KEY");
            let result = platform_backup_round_trip();
            #[cfg(target_os = "windows")]
            result.expect("Credential Manager backup must round-trip, including chunked payloads");
            #[cfg(target_os = "macos")]
            if let Err(error) = result {
                // Unsigned PR runners cannot access the restricted production
                // group. Assert that refusal instead of silently skipping it.
                assert!(
                    error.contains("-34018"),
                    "unexpected Keychain failure: {error}"
                );
                let path = crate::registry::conduit_dir()
                    .unwrap()
                    .join("backups/cursor/1-mcp.json");
                assert!(
                    !path.exists(),
                    "unsigned refusal must not publish plaintext recovery"
                );
            }
        });
    }

    #[test]
    fn fresh_backups_protect_credentials_after_import_context_is_cleared() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            let root = crate::registry::conduit_dir().unwrap();
            let path = root.join("backups/cursor/1-mcp.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original =
                r#"{"mcpServers":{"native":{"env":{"API_KEY":"sk-reconnect-fixture"}}}}"#;
            let registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_KEY","secret":true}]}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            crate::secrets::set_secret("native", "API_KEY", "sk-reconnect-fixture").unwrap();
            let raw = encode(&path, original).unwrap();
            assert!(!raw.contains("sk-reconnect-fixture"));
            assert_eq!(decode(&raw).unwrap(), original);
        });
    }
    #[test]
    fn shared_credentials_report_every_missing_reference_with_the_same_marker() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            reset_missing();
            let path = crate::registry::conduit_dir()
                .unwrap()
                .join("backups/cursor/1-mcp.json");
            let original = r#"{"mcpServers":{"a":{"env":{"API_KEY":"sk-shared-fixture"}},"b":{"env":{"API_KEY":"sk-shared-fixture"}}}}"#;
            register("a", "API_KEY", "sk-shared-fixture");
            register("b", "API_KEY", "sk-shared-fixture");
            let raw = encode(&path, original).unwrap();
            clear();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-backup-vault");
            assert_eq!(
                decode(&raw).unwrap(),
                original.replace("sk-shared-fixture", "TOOLPORT_MISSING_CREDENTIAL_0")
            );
            let warnings = warnings().join("\n");
            assert!(warnings.contains("API_KEY for a"));
            assert!(warnings.contains("API_KEY for b"));
            assert!(!warnings.contains("CREDENTIAL_1"));
            reset_missing();
        });
    }
    #[test]
    fn pruning_releases_vaulted_payloads_and_keeps_envelopes_when_locked() {
        crate::secrets::tests::with_isolated_vault(|| {
            clear();
            let dir = crate::registry::conduit_dir()
                .unwrap()
                .join("backups/cursor");
            std::fs::create_dir_all(&dir).unwrap();
            let original = r#"{"env":{"API_KEY":"sk-pruning-fixture"}}"#;
            register("native", "API_KEY", "sk-pruning-fixture");
            let mut first = None;
            for index in 0..=CONFIG_BACKUP_GENERATIONS {
                let path = dir.join(format!("{index}-mcp.json"));
                let raw = encode(&path, original).unwrap();
                if index == 0 {
                    first = envelope(&raw);
                }
                std::fs::write(path, raw).unwrap();
            }
            clear();
            std::env::set_var("TOOLPORT_SECRET_KEY", "locked-backup-vault");
            prune_backups(&dir, "mcp.json");
            assert!(dir.join("0-mcp.json").exists());
            std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
            prune_backups(&dir, "mcp.json");
            assert!(!dir.join("0-mcp.json").exists());
            let saved = first.unwrap();
            assert!(
                crate::secrets::get_vault_secret_result(&saved.account, &saved.key)
                    .unwrap()
                    .is_none()
            );
            for index in 0..saved.references.len() {
                assert!(crate::secrets::get_vault_secret_result(
                    &saved.account,
                    &format!("{}-credential-{index}", saved.key)
                )
                .unwrap()
                .is_none());
            }
        });
    }
    #[test]
    fn copied_backups_do_not_delete_the_source_vault_payload() {
        crate::secrets::tests::with_isolated_vault(|| {
            let root = crate::registry::conduit_dir().unwrap();
            let source = root.join("source/backups/cursor/1-mcp.json");
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            register("native", "API_KEY", "sk-copy-fixture-value");
            let raw = encode(&source, r#"{"env":{"API_KEY":"sk-copy-fixture-value"}}"#).unwrap();
            clear();
            std::fs::write(&source, &raw).unwrap();
            let saved = envelope(&raw).unwrap();
            for copy in [
                root.join("copy/backups/cursor/1-mcp.json"),
                root.join("source/backups/cursor/2-mcp.json"),
            ] {
                std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
                std::fs::write(&copy, &raw).unwrap();
                release(&copy).unwrap();
                assert!(
                    crate::secrets::get_vault_secret_result(&saved.account, &saved.key)
                        .unwrap()
                        .is_some()
                );
            }
            release(&source).unwrap();
            assert!(
                crate::secrets::get_vault_secret_result(&saved.account, &saved.key)
                    .unwrap()
                    .is_none()
            );
        });
    }

    #[test]
    fn legacy_upgrade_honors_explicit_nonsecret_settings() {
        crate::secrets::tests::with_isolated_vault(|| {
            let root = crate::registry::conduit_dir().unwrap();
            let path = root.join("backups/cursor/1-mcp.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let raw = r#"{"mcpServers":{"native":{"env":{"API_KEY":"public-long-config-value"}}}}"#;
            std::fs::write(&path, raw).unwrap();
            let registry: crate::registry::Registry = serde_json::from_value(serde_json::json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","command":"echo","env":[{"key":"API_KEY","secret":false,"value":"public-long-config-value"}]}]})).unwrap();
            crate::registry::save(&registry).unwrap();
            assert_eq!(upgrade_legacy().unwrap(), 0);
            assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        });
    }
    #[test]
    fn legacy_credentials_cover_native_json_toml_yaml_and_provenance_strings() {
        for text in [
            r#"{"mcpServers":{"native":{"env":{"API_KEY":"sk-legacy-fixture"}}}}"#,
            "[mcp_servers.native.env]\nAPI_KEY = 'sk-legacy-fixture'\n",
            "mcpServers:\n  native:\n    env: { API_KEY: sk-legacy-fixture }\n",
        ] {
            for record in [
                text.to_string(),
                serde_json::json!({"original":text}).to_string(),
                serde_json::json!({"entries":[{"format":"toml","text":text}]}).to_string(),
            ] {
                let values = legacy_credentials(&record, "fixture");
                assert!(
                    values
                        .iter()
                        .any(|(reference, value)| reference.key == "API_KEY"
                            && value == "sk-legacy-fixture"),
                    "{record}"
                );
            }
        }
    }
    #[test]
    fn historical_backups_stay_protected_after_rotation_and_another_import() {
        crate::secrets::tests::with_isolated_vault(|| {
            let path = crate::registry::conduit_dir()
                .unwrap()
                .join("backups/cursor/1-mcp.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = r#"{"mcpServers":{"a":{"env":{"API_KEY":"sk-original-fake-value","TOKEN":"sk-second-fake-value"}}}}"#;
            register("a", "API_KEY", "sk-original-fake-value");
            crate::secrets::set_secret("a", "API_KEY", "sk-original-fake-value").unwrap();
            std::fs::write(&path, encode(&path, original).unwrap()).unwrap();
            clear();
            crate::secrets::set_secret("a", "API_KEY", "rotated-key").unwrap();
            register("a", "TOKEN", "sk-second-fake-value");
            let note = path.parent().unwrap().join("personal-notes.txt");
            let binary = path.parent().unwrap().join("personal.bin");
            std::fs::write(&note, b"sk-original-fake-value sk-second-fake-value").unwrap();
            std::fs::write(&binary, [0xff, 0xfe]).unwrap();
            protect_existing().unwrap();
            assert_eq!(
                std::fs::read(&note).unwrap(),
                b"sk-original-fake-value sk-second-fake-value"
            );
            assert_eq!(std::fs::read(&binary).unwrap(), [0xff, 0xfe]);
            clear();
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(!raw.contains("sk-original-fake-value"));
            assert!(!raw.contains("sk-second-fake-value"));
            assert_eq!(decode(&raw).unwrap(), original);
            crate::secrets::delete_secret("a", "API_KEY").unwrap();
            std::fs::write(&path, encode(&path, original).unwrap()).unwrap();
            assert!(!std::fs::read_to_string(&path)
                .unwrap()
                .contains("sk-original-fake-value"));
            assert_eq!(read(&path).unwrap(), original);
            let first = envelope(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let rewritten = format!("{original}\n");
            std::fs::write(&path, encode(&path, &rewritten).unwrap()).unwrap();
            let last = envelope(&std::fs::read_to_string(&path).unwrap()).unwrap();
            release(&path).unwrap();
            for key in [first.key, last.key] {
                assert!(crate::secrets::get_vault_secret_result(&last.account, &key)
                    .unwrap()
                    .is_none());
                assert!(crate::secrets::get_vault_secret_result(
                    &last.account,
                    &format!("{key}-credential-0")
                )
                .unwrap()
                .is_none());
            }
        });
    }
    #[test]
    fn credential_copies_in_comments_are_not_kept_in_plaintext() {
        for original in [
            "{ // sk-fake-comment-secret\n \"env\":{\"API_KEY\":\"sk-fake-comment-secret\"}\n}",
            "# sk-fake-comment-secret\nenv = { API_KEY = 'sk-fake-comment-secret' }\n",
            "/* sk-fake-comment-secret */ {\"env\":{\"API_KEY\":\"sk-fake-comment-secret\"}}",
            "env:\n  API_KEY: sk-fake-comment-secret # explanation\n",
            "env: { API_KEY: sk-fake-comment-secret, OTHER: unchanged }\n",
        ] {
            let result = redact(original, "sk-fake-comment-secret", "missing", 0);
            assert!(!result.contains("sk-fake-comment-secret"), "{result}");
            assert_eq!(
                result,
                original.replace("sk-fake-comment-secret", "missing")
            );
        }
    }
    #[test]
    fn redaction_preserves_syntax_short_values_and_serialized_config_bytes() {
        let original = "{\r\n \"version\":1, \"env\":{\"API_KEY\":\"1\"}\r\n}\r\n";
        let redacted = redact(original, "1", "missing", 0);
        assert_eq!(redacted, original.replace("\"1\"", "\"missing\""));
        let snapshot =
            serde_json::json!({"original":original,"originalHash":"keep1","configPath":"/keep1"});
        let result: serde_json::Value =
            serde_json::from_str(&redact(&snapshot.to_string(), "1", "missing", 0)).unwrap();
        assert_eq!(result["original"], redacted);
        assert_eq!(result["originalHash"], "keep1");
        assert_eq!(result["configPath"], "/keep1");
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryIssue {
    pub reference: Reference,
    pub marker: String,
    pub state: crate::secrets::BackupSecretError,
}
pub(super) struct RecoveryValue {
    pub reference: Reference,
    pub marker: String,
    pub secret: Option<String>,
    pub state: crate::secrets::BackupSecretError,
}
fn marker(saved: &Envelope, index: usize) -> String {
    format!(
        "TOOLPORT_MISSING_CREDENTIAL_{}",
        saved.reference_markers.get(index).copied().unwrap_or(index)
    )
}
fn envelopes(text: &str, out: &mut Vec<Envelope>, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("Client recovery metadata is nested too deeply".into());
    }
    if let Some(saved) = envelope(text) {
        let fallback = saved.fallback.clone();
        out.push(saved);
        envelopes(&fallback, out, depth + 1)?;
    } else {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| diagnostics::json(&e))?;
        if let Some(inner) = value["vaultRecovery"].as_str() {
            envelopes(inner, out, depth + 1)?;
        }
    }
    Ok(())
}
pub(super) fn raw_markers(text: &str) -> Result<Vec<(Reference, String)>, String> {
    let mut saved = Vec::new();
    envelopes(text, &mut saved, 0)?;
    Ok(saved
        .iter()
        .flat_map(|saved| {
            saved
                .references
                .iter()
                .enumerate()
                .map(|(index, r)| (r.clone(), marker(saved, index)))
        })
        .collect())
}
// Infer a missing duplicate only from the verified full payload, never from an
// unauthenticated independent copy. Nested config strings retain their spelling.
fn infer_value(fallback: &str, payload: &str, marker: &str) -> Option<String> {
    fn strings(f: &str, p: &str, m: &str) -> Option<String> {
        if f == p || !recovery_values::contains_marker(f, m) {
            return None;
        }
        let ranges = recovery_values::marker_ranges(f, m);
        let mut pieces = Vec::new();
        let mut cursor = 0;
        for range in ranges {
            pieces.push(&f[cursor..range.start]);
            cursor = range.end;
        }
        pieces.push(&f[cursor..]);
        let n = pieces.len() - 1;
        let fixed: usize = pieces.iter().map(|s| s.len()).sum();
        if let Some(size) = p
            .len()
            .checked_sub(fixed)
            .filter(|nbytes| nbytes % n == 0)
            .map(|nbytes| nbytes / n)
        {
            let start = pieces[0].len();
            if let Some(value) = p.get(start..start + size) {
                if substitute(f, m, value) == p {
                    return Some(value.into());
                }
            }
        }
        for parse in [0, 1, 2] {
            let parse_value = |text: &str| -> Option<serde_json::Value> {
                match parse {
                    0 => json5::from_str(text).ok(),
                    1 => serde_json::to_value(toml::from_str::<toml::Value>(text).ok()?).ok(),
                    _ => {
                        serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(text).ok()?)
                            .ok()
                    }
                }
            };
            if let (Some(f), Some(p)) = (parse_value(f), parse_value(p)) {
                if f.is_object() || f.is_array() {
                    if let Some(v) = walk(&f, &p, m) {
                        return Some(v);
                    }
                }
            }
        }
        None
    }
    fn walk(f: &serde_json::Value, p: &serde_json::Value, m: &str) -> Option<String> {
        match (f, p) {
            (serde_json::Value::String(f), serde_json::Value::String(p)) => strings(f, p, m),
            (serde_json::Value::Object(f), serde_json::Value::Object(p)) => {
                f.iter().find_map(|(k, v)| walk(v, p.get(k)?, m))
            }
            (serde_json::Value::Array(f), serde_json::Value::Array(p)) => {
                f.iter().zip(p).find_map(|(f, p)| walk(f, p, m))
            }
            _ => None,
        }
    }
    strings(fallback, payload, marker)
}
pub(super) fn recovery_values(path: &Path) -> Result<Vec<RecoveryValue>, String> {
    use crate::secrets::{backup_secret_status as get, BackupSecretError as State};
    let text =
        std::fs::read_to_string(path).map_err(|_| "Could not read the saved recovery file")?;
    let mut saved = Vec::new();
    envelopes(&text, &mut saved, 0)?;
    let mut values: Vec<RecoveryValue> = Vec::new();
    for saved in saved {
        let payload = get(&saved.account, &saved.key);
        let valid = payload
            .as_ref()
            .ok()
            .and_then(|s| s.as_ref())
            .filter(|s| crate::registry::sha256_hex(s) == saved.hash);
        let full_state = match &payload {
            Ok(Some(_)) => State::Corrupt,
            Ok(None) => State::Missing,
            Err(state) => *state,
        };
        let mut copies = Vec::new();
        for (i, _) in saved.references.iter().enumerate() {
            copies.push(get(
                &saved.account,
                &format!("{}-credential-{i}", saved.key),
            ));
        }
        // Old envelopes had no individual hashes. Accept their copies together
        // only if they reconstruct the authenticated payload exactly.
        let legacy_valid = if saved.value_hashes.is_empty() {
            let mut recovered = saved.fallback.clone();
            let mut all = true;
            for (i, copy) in copies.iter().enumerate() {
                if let Ok(Some(value)) = copy {
                    recovered = redact(&recovered, &marker(&saved, i), value, 0);
                } else {
                    all = false;
                }
            }
            all && crate::registry::sha256_hex(&recovered) == saved.hash
        } else {
            false
        };
        for (i, reference) in saved.references.iter().enumerate() {
            let m = marker(&saved, i);
            let inferred = valid.and_then(|p| infer_value(&saved.fallback, p, &m));
            let copy = copies[i].as_ref().ok().and_then(|v| v.clone());
            let authenticated = |v: &String| {
                saved
                    .value_hashes
                    .get(i)
                    .is_some_and(|h| crate::registry::sha256_hex(v) == *h)
                    || legacy_valid
            };
            let source = if inferred.is_none() && copy.as_ref().is_none_or(|v| !authenticated(v)) {
                get(&reference.server, &reference.key)
                    .ok()
                    .flatten()
                    .filter(authenticated)
            } else {
                None
            };
            let secret = inferred.or_else(|| copy.filter(authenticated)).or(source);
            let state = if secret.is_some() {
                State::Unchecked
            } else {
                match &copies[i] {
                    Err(state) => *state,
                    Ok(Some(_)) => State::Corrupt,
                    Ok(None) => full_state,
                }
            };
            values.push(RecoveryValue {
                reference: reference.clone(),
                marker: m,
                secret,
                state,
            });
        }
    }
    // Shared values may have several references. One authenticated copy is
    // sufficient for their shared marker; conflicting authenticated copies aren't.
    for i in 0..values.len() {
        let candidates: Vec<_> = values
            .iter()
            .filter(|v| v.marker == values[i].marker)
            .filter_map(|v| v.secret.clone())
            .collect();
        if candidates.windows(2).any(|pair| pair[0] != pair[1]) {
            values[i].secret = None;
            values[i].state = State::Ambiguous;
        } else if let Some(secret) = candidates.first() {
            values[i].secret = Some(secret.clone());
        }
    }
    Ok(values)
}
pub(super) fn set_issues(path: &Path, issues: &[RecoveryIssue]) -> Result<(), String> {
    // A restore attempt checked every remaining value, so an older Scan's
    // "ready" result no longer describes them.
    set_status(path, issues, &[])
}
/// A value a noninteractive Scan authenticated. Never contains value text.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReady {
    pub reference: Reference,
    pub marker: String,
}
/// Status-only receipt update. Captured content, hashes and provenance stay as written.
pub(super) fn set_status(
    path: &Path,
    issues: &[RecoveryIssue],
    ready: &[RecoveryReady],
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|_| "Could not update recovery status")?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| diagnostics::json(&e))?;
    value["recoveryIssues"] = serde_json::to_value(issues).map_err(|e| diagnostics::json(&e))?;
    if ready.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.remove("recoveryAvailable");
        }
    } else {
        value["recoveryAvailable"] =
            serde_json::to_value(ready).map_err(|e| diagnostics::json(&e))?;
    }
    crate::registry::atomic_write(
        path,
        &serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))?,
    )
}

/// Explicit Discard progress. It only restricts restoration and display; it
/// never authorizes deleting anything without a confirmed Discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum DiscardPhase {
    Started,
    ValuesRemoved,
}
/// Reads the phase from any metadata layer without contacting a vault. An
/// unrecognized phase (for example from a newer Toolport) counts as started.
pub(super) fn discard_phase(raw: &str) -> Option<DiscardPhase> {
    let mut text = raw.to_owned();
    for _ in 0..8 {
        let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        if let Some(phase) = value.get("discardPhase").filter(|v| !v.is_null()) {
            return Some(serde_json::from_value(phase.clone()).unwrap_or(DiscardPhase::Started));
        }
        if let Some(saved) = envelope(&text) {
            text = saved.fallback;
            continue;
        }
        let inner = value["vaultRecovery"].as_str()?;
        text = inner.to_owned();
    }
    None
}
pub(super) fn set_discard_phase(path: &Path, phase: DiscardPhase) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|_| "Could not update recovery status")?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "Could not update recovery status")?;
    value["discardPhase"] = serde_json::to_value(phase).map_err(|e| diagnostics::json(&e))?;
    crate::registry::atomic_write(
        path,
        &serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))?,
    )
}

#[cfg(test)]
pub(super) fn test_recovery_key(path: &Path) -> (String, String, usize) {
    let raw = std::fs::read_to_string(path).unwrap();
    let mut records = Vec::new();
    envelopes(&raw, &mut records, 0).unwrap();
    let record = records.last().unwrap();
    (
        record.account.clone(),
        record.key.clone(),
        record.references.len(),
    )
}

pub(super) fn source_path(path: &Path) -> Result<Option<String>, String> {
    let raw = std::fs::read_to_string(path).map_err(|_| "Could not inspect saved recovery copy")?;
    source_path_in(path, &raw)
}
fn source_path_in(path: &Path, raw: &str) -> Result<Option<String>, String> {
    if let Some(plain) = plain_backup(raw)? {
        if plain.binding != plain_binding(path, &plain.source_config_path, &plain.text) {
            return Err("Saved recovery copy has conflicting path provenance".into());
        }
        return Ok(Some(plain.source_config_path));
    }
    let Some(saved) = envelope(raw) else {
        return Ok(None);
    };
    if let Some(source) = saved.source_config_path {
        let expected = format!(
            "{}-{}-{}",
            crate::registry::sha256_hex(&path.to_string_lossy()),
            crate::registry::sha256_hex(&source),
            saved.hash
        );
        if saved.key != expected {
            return Err("Saved recovery copy has conflicting path provenance".into());
        }
        Ok(Some(source))
    } else {
        Ok(None)
    }
}

pub(super) fn set_cleanup_state(
    path: &Path,
    state: crate::secrets::BackupSecretError,
) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|_| "Could not update saved recovery status")?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| diagnostics::json(&e))?;
    value["recoveryCleanupState"] =
        serde_json::to_value(state).map_err(|e| diagnostics::json(&e))?;
    crate::registry::atomic_write(
        path,
        &serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))?,
    )
}

pub(super) fn remember_pending_revision(
    path: &Path,
    raw: &str,
    published: Option<&str>,
    eligible: bool,
) -> Result<(), String> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| diagnostics::json(&e))?;
    value["publishedHash"] = published
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null);
    value["exactEligible"] = eligible.into();
    crate::registry::atomic_write(
        path,
        &serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))?,
    )
}
