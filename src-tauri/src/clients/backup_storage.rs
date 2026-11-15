//! Vault-backed recovery payloads. The fallback retains formatting and non-secret
//! data, so a locked/lost vault never prevents recovering the user's config.
use super::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Reference {
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
}
fn envelope(text: &str) -> Option<Envelope> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("toolportBackupVersion")?.as_u64()? != 1 {
        return None;
    }
    serde_json::from_value(value).ok()
}

// Rewrite string values, never syntax, field names, hashes or recovery paths.
// Recurse through serialized config strings without reformatting their bytes.
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
            let replacement = comment.replace(secret, marker);
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
                value.replace(secret, marker)
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
    let mut values = IMPORTS.with(|v| v.borrow().clone());
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
                if let Some(value) = crate::secrets::get_vault_secret_result(
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
    for (reference, value) in values {
        if value.is_empty() {
            continue;
        }
        let marker = format!("TOOLPORT_MISSING_CREDENTIAL_{}", references.len());
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
        }
    }
    if references.is_empty() {
        return Ok(text.into());
    }
    let hash = crate::registry::sha256_hex(text);
    let account = install_account(
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or("Invalid backup path")?,
    );
    let key = format!(
        "{}-{hash}",
        crate::registry::sha256_hex(&path.to_string_lossy())
    );
    // The complete byte spelling lives only in the vault. Publish the fallback
    // only after a verified vault write, before modifying the client's file.
    for (index, value) in retained_values.iter().enumerate() {
        crate::secrets::set_secret(&account, &format!("{key}-credential-{index}"), value)?;
    }
    crate::secrets::set_secret(&account, &key, text)?;
    if crate::secrets::get_vault_secret_result(&account, &key)?.as_deref() != Some(text) {
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
    })
    .map_err(|e| e.to_string())
}

pub(super) fn decode(text: &str) -> Result<String, String> {
    let Some(saved) = envelope(text) else {
        return Ok(text.into());
    };
    match crate::secrets::get_vault_secret_result(&saved.account, &saved.key) {
        Ok(Some(payload)) if crate::registry::sha256_hex(&payload) == saved.hash => Ok(payload),
        _ => {
            MISSING.with(|v| *v.borrow_mut() = true);
            for (index, reference) in saved.references.iter().enumerate() {
                let warning = format!("Client config recovery: could not re-insert {}:{}; left TOOLPORT_MISSING_CREDENTIAL_{index}. Unlock the original vault and retry. Recovery backup retained.", reference.server, reference.key);
                WARNINGS.with(|v| {
                    let mut v = v.borrow_mut();
                    if !v.contains(&warning) {
                        eprintln!("{warning}");
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
            Ok(fallback)
        }
    }
}
pub(super) fn read(path: &Path) -> Result<String, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    decode(&text).map_err(std::io::Error::other)
}
pub(super) fn missing() -> bool {
    MISSING.with(|v| *v.borrow())
}
pub(super) fn reset_missing() {
    MISSING.with(|v| *v.borrow_mut() = false);
    WARNINGS.with(|v| v.borrow_mut().clear());
}
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
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some(mut saved) = envelope(&text) {
            saved.generations.push((saved.key, saved.references.len()));
            let mut generations = std::collections::BTreeMap::<String, usize>::new();
            for (key, count) in saved.generations {
                generations
                    .entry(key)
                    .and_modify(|old| *old = (*old).max(count))
                    .or_insert(count);
            }
            for (key, count) in generations {
                crate::secrets::delete_secret(&saved.account, &key)?;
                for index in 0..count {
                    crate::secrets::delete_secret(
                        &saved.account,
                        &format!("{key}-credential-{index}"),
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn protect_existing() -> Result<(), String> {
    let Some(root) = crate::registry::conduit_dir().map(|dir| dir.join("backups")) else {
        return Ok(());
    };
    let clients = match std::fs::read_dir(root) {
        Ok(clients) => clients,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for client in clients {
        let client = client.map_err(|e| e.to_string())?;
        if !client.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(client.path()).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if !file.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            let path = file.path();
            let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let payload = match envelope(&raw) {
                Some(saved) => crate::secrets::get_vault_secret_result(&saved.account, &saved.key)?
                    .filter(|payload| crate::registry::sha256_hex(payload) == saved.hash)
                    .ok_or("Unlock the original backup vault before importing more credentials. Client config unchanged.")?,
                None => raw.clone(),
            };
            let encoded = encode(&path, &payload)?;
            if encoded != raw {
                crate::registry::atomic_write(&path, &encoded)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            protect_existing().unwrap();
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
