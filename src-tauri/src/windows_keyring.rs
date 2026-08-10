//! Windows Credential Manager backend with transparent large-value chunking.
//!
//! Generic credentials are limited to 2,560 bytes. The keyring crate encodes
//! passwords as UTF-16, so OAuth/JWT values can exceed that platform limit.

use keyring::Entry;
use sha2::{Digest, Sha256};

use super::{account, SERVICE};

const CHUNK_UTF16_UNITS: usize = 1_000;
const MANIFEST_PREFIX: &str = "toolport-chunked-v1:";
const DIRECT_PREFIX: &str = "toolport-direct-v1:";
const READ_ATTEMPTS: usize = 3;

struct ChunkManifest {
    generation: String,
    count: usize,
    checksum: String,
}

fn entry(account: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, account).map_err(|e| e.to_string())
}

fn read_entry(account: &str) -> Result<Option<String>, String> {
    match entry(account)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn delete_entry(account: &str) -> Result<(), String> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn split_utf16(value: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (offset, ch) in value.char_indices() {
        let char_units = ch.len_utf16();
        if units + char_units > CHUNK_UTF16_UNITS {
            chunks.push(value[start..offset].to_string());
            start = offset;
            units = 0;
        }
        units += char_units;
    }
    chunks.push(value[start..].to_string());
    chunks
}

fn checksum(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn new_generation() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn chunk_account(base: &str, generation: &str, index: usize) -> String {
    format!("{base}::toolport-chunk::{generation}::{index}")
}

fn manifest_value(manifest: &ChunkManifest) -> String {
    format!(
        "{MANIFEST_PREFIX}{}:{}:{}",
        manifest.generation, manifest.count, manifest.checksum
    )
}

fn direct_value(value: &str) -> String {
    if value.starts_with(MANIFEST_PREFIX) || value.starts_with(DIRECT_PREFIX) {
        format!("{DIRECT_PREFIX}{}:{value}", checksum(value))
    } else {
        value.to_string()
    }
}

fn parse_direct_value(value: &str) -> Option<String> {
    let rest = value.strip_prefix(DIRECT_PREFIX)?;
    let (expected, direct) = rest.split_once(':')?;
    if expected.len() == 64
        && expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        && checksum(direct) == expected
    {
        Some(direct.to_string())
    } else {
        // Backward compatibility: a legacy raw secret may begin with our newly
        // reserved prefix. Only unwrap the envelope when its checksum validates.
        None
    }
}

fn parse_manifest(value: &str) -> Result<Option<ChunkManifest>, String> {
    let Some(rest) = value.strip_prefix(MANIFEST_PREFIX) else {
        return Ok(None);
    };
    let fields: Vec<_> = rest.split(':').collect();
    if fields.len() != 3
        || fields[0].len() != 32
        || !fields[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        || fields[2].len() != 64
        || !fields[2].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Windows credential chunk manifest is invalid".to_string());
    }
    let count = fields[1]
        .parse::<usize>()
        .map_err(|_| "Windows credential chunk manifest has an invalid count".to_string())?;
    if count == 0 {
        return Err("Windows credential chunk manifest has no chunks".to_string());
    }
    Ok(Some(ChunkManifest {
        generation: fields[0].to_string(),
        count,
        checksum: fields[2].to_string(),
    }))
}

fn delete_chunks(base: &str, manifest: &ChunkManifest) -> Result<(), String> {
    let mut errors = Vec::new();
    for index in 0..manifest.count {
        if let Err(error) = delete_entry(&chunk_account(base, &manifest.generation, index)) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "could not delete {} Windows credential chunk(s): {}",
            errors.len(),
            errors.join("; ")
        ))
    }
}

fn cleanup_chunks(base: &str, manifest: &ChunkManifest) {
    if let Err(error) = delete_chunks(base, manifest) {
        // The base credential is the commit point. Once it names the new value (or
        // has been deleted), stale chunks are unreachable and cleanup failure must
        // not make callers believe the primary write failed.
        eprintln!("toolport: Windows credential chunk cleanup deferred: {error}");
    }
}

pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    let base = account(server_id, key);
    let previous_manifest = read_entry(&base)?
        .as_deref()
        .and_then(|value| match parse_manifest(value) {
            Ok(manifest) => manifest,
            Err(error) => {
                // A damaged manifest must not permanently block reauthentication.
                // Its chunks may be unreachable, but replacing the base repairs the
                // user-visible credential and future writes.
                eprintln!("toolport: replacing invalid Windows credential manifest: {error}");
                None
            }
        });

    let direct = direct_value(value);
    if direct.encode_utf16().count() <= CHUNK_UTF16_UNITS {
        entry(&base)?
            .set_password(&direct)
            .map_err(|error| error.to_string())?;
        if let Some(previous) = previous_manifest {
            cleanup_chunks(&base, &previous);
        }
        return Ok(());
    }

    let chunks = split_utf16(value);
    let manifest = ChunkManifest {
        generation: new_generation()?,
        count: chunks.len(),
        checksum: checksum(value),
    };
    let mut written = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let result = entry(&chunk_account(&base, &manifest.generation, index))?
            .set_password(chunk)
            .map_err(|error| error.to_string());
        if let Err(error) = result {
            for cleanup_index in 0..written {
                if let Err(cleanup_error) =
                    delete_entry(&chunk_account(&base, &manifest.generation, cleanup_index))
                {
                    eprintln!(
                        "toolport: Windows credential partial-write cleanup deferred: {cleanup_error}"
                    );
                }
            }
            return Err(error);
        }
        written += 1;
    }

    if let Err(error) = entry(&base)?
        .set_password(&manifest_value(&manifest))
        .map_err(|error| error.to_string())
    {
        cleanup_chunks(&base, &manifest);
        return Err(error);
    }
    if let Some(previous) = previous_manifest {
        cleanup_chunks(&base, &previous);
    }
    Ok(())
}

pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
    let base = account(server_id, key);
    let mut last_error = None;
    for attempt in 0..READ_ATTEMPTS {
        let Some(value) = read_entry(&base)? else {
            return Ok(None);
        };
        if let Some(direct) = parse_direct_value(&value) {
            return Ok(Some(direct));
        }
        let Some(manifest) = parse_manifest(&value)? else {
            return Ok(Some(value));
        };
        let mut combined = String::new();
        let mut failed = None;
        for index in 0..manifest.count {
            match read_entry(&chunk_account(&base, &manifest.generation, index))? {
                Some(chunk) => combined.push_str(&chunk),
                None => {
                    failed = Some(format!("Windows credential chunk {index} is missing"));
                    break;
                }
            }
        }
        if failed.is_none() && checksum(&combined) == manifest.checksum {
            return Ok(Some(combined));
        }
        last_error = failed.or_else(|| {
            Some("Windows credential chunks failed their integrity check".to_string())
        });
        if attempt + 1 < READ_ATTEMPTS {
            std::thread::yield_now();
        }
    }
    Err(last_error.unwrap_or_else(|| "Windows credential read failed".to_string()))
}

pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
    let base = account(server_id, key);
    let manifest = read_entry(&base)?
        .as_deref()
        .and_then(|value| parse_manifest(value).ok().flatten());
    // Make the value unreadable first, then clean up its generation chunks.
    delete_entry(&base)?;
    if let Some(manifest) = manifest {
        cleanup_chunks(&base, &manifest);
    }
    Ok(())
}

#[cfg(test)]
pub fn set_raw_for_test(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    entry(&account(server_id, key))?
        .set_password(value)
        .map_err(|error| error.to_string())
}
