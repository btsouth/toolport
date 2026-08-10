//! Windows Credential Manager backend with transparent large-value chunking.
//!
//! Generic credentials are limited to 2,560 bytes. The keyring crate encodes
//! passwords as UTF-16, so OAuth/JWT values can exceed that platform limit.

use keyring::Entry;
use sha2::{Digest, Sha256};

use super::{account, SERVICE};

const CHUNK_UTF16_UNITS: usize = 1_000;
const MANIFEST_PREFIX: &str = "toolport-chunked-v1:";

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

pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    let base = account(server_id, key);
    let previous_manifest = read_entry(&base)?
        .as_deref()
        .map(parse_manifest)
        .transpose()?
        .flatten();

    if value.encode_utf16().count() <= CHUNK_UTF16_UNITS {
        entry(&base)?
            .set_password(value)
            .map_err(|error| error.to_string())?;
        if let Some(previous) = previous_manifest {
            delete_chunks(&base, &previous)?;
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
                let _ = delete_entry(&chunk_account(&base, &manifest.generation, cleanup_index));
            }
            return Err(error);
        }
        written += 1;
    }

    if let Err(error) = entry(&base)?
        .set_password(&manifest_value(&manifest))
        .map_err(|error| error.to_string())
    {
        let _ = delete_chunks(&base, &manifest);
        return Err(error);
    }
    if let Some(previous) = previous_manifest {
        delete_chunks(&base, &previous)?;
    }
    Ok(())
}

pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
    let base = account(server_id, key);
    let Some(value) = read_entry(&base)? else {
        return Ok(None);
    };
    let Some(manifest) = parse_manifest(&value)? else {
        return Ok(Some(value));
    };
    let mut combined = String::new();
    for index in 0..manifest.count {
        let chunk = read_entry(&chunk_account(&base, &manifest.generation, index))?
            .ok_or_else(|| format!("Windows credential chunk {index} is missing"))?;
        combined.push_str(&chunk);
    }
    if checksum(&combined) != manifest.checksum {
        return Err("Windows credential chunks failed their integrity check".to_string());
    }
    Ok(Some(combined))
}

pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
    let base = account(server_id, key);
    let manifest = read_entry(&base)?
        .as_deref()
        .map(parse_manifest)
        .transpose()?
        .flatten();
    // Make the value unreadable first, then clean up its generation chunks.
    delete_entry(&base)?;
    if let Some(manifest) = manifest {
        delete_chunks(&base, &manifest)?;
    }
    Ok(())
}
