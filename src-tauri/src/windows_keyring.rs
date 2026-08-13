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
pub(super) const READ_ATTEMPTS: usize = 3;

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

/// Test-only: script the next base-credential reads. `true` reports the target as
/// absent, standing in for the window in which a concurrent `CredWriteW` replace
/// makes Windows report a live credential as missing; `false` reads the real store.
/// Reads past the end of the script go to the real store too.
///
/// A script rather than a count because the interesting cases are not all-absent:
/// a chunk failure FOLLOWED by absences has to stay an error rather than decay
/// into `Ok(None)`.
///
/// Thread-local, so tests running in parallel cannot consume each other's script.
#[cfg(test)]
pub(super) fn script_base_reads(absent: &[bool]) {
    BASE_READ_SCRIPT.with(|queue| {
        *queue.borrow_mut() = absent.iter().copied().collect();
    });
}

#[cfg(test)]
thread_local! {
    static BASE_READ_SCRIPT: std::cell::RefCell<std::collections::VecDeque<bool>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// Pause between read attempts, escalating so the three attempts span ~5ms — far
/// wider than any single one of them.
///
/// This deliberately sleeps rather than yields. A `CredWriteW` replace window
/// outlives a bare `yield_now` on an idle multi-core machine, where yielding
/// returns immediately if any core is free, so every attempt lands inside the same
/// window and the retry buys almost nothing. Measured across 9,600 racing reads:
///
/// | backoff        | torn reads | hard errors |
/// |----------------|-----------:|------------:|
/// | none           |         37 |           0 |
/// | `yield_now`    |         26 |          16 |
/// | 250µs + 1ms    |         16 |           1 |
/// | 1ms + 4ms      |          4 |           1 |
///
/// Note what that table does NOT show: a zero. This is a mitigation, not a
/// guarantee — Windows offers no promise that a live credential always reads back,
/// and the curve is asymptotic, so a wider span trades latency for ever smaller
/// gains. That is why
/// `windows_chunk_reader_survives_concurrent_generation_swaps` asserts on the
/// integrity of whatever value comes back rather than on every read producing one,
/// and why the retry itself is pinned by a deterministic test instead of that race.
///
/// The cost lands on genuinely-absent secrets, which now take ~5ms rather than a
/// single read. Callers do check presence in loops over servers × keys, so that is
/// real, but it is bounded, off the per-call path, and cheap next to reporting a
/// live credential as missing.
fn read_backoff(attempt: usize) {
    const BACKOFF_MICROS: [u64; READ_ATTEMPTS - 1] = [1_000, 4_000];
    let micros = BACKOFF_MICROS[attempt];
    std::thread::sleep(std::time::Duration::from_micros(micros));
}

/// Read the base credential. Identical to [`read_entry`] outside tests; under
/// `cfg(test)` it honours [`script_base_reads`] so the retry below can be driven
/// deterministically instead of by racing a writer thread.
fn read_base(account: &str) -> Result<Option<String>, String> {
    #[cfg(test)]
    {
        let scripted = BASE_READ_SCRIPT.with(|queue| queue.borrow_mut().pop_front());
        if scripted == Some(true) {
            return Ok(None);
        }
    }
    read_entry(account)
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
    // Keep the most recent integrity error. A later transient absence must not
    // erase evidence that we read a base credential but could not assemble it.
    // `None` therefore means every attempt saw no base credential at all.
    let mut last_error = None;
    for attempt in 0..READ_ATTEMPTS {
        match read_base(&base)? {
            Some(value) => {
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
            }
            // `set_secret` never deletes the base credential — it replaces it with a
            // plain `CredWriteW` overwrite — so an absent base cannot mean "a write is
            // partway through" in our own code. Windows reports it as absent anyway for
            // the instant that replace is in flight, which made this a torn read: a
            // caller asking for a token while the app rewrote a refreshed one got
            // `Ok(None)`, indistinguishable from never having authenticated.
            //
            // So absence only counts once it survives the same retries a chunk race
            // gets. See `read_backoff` for the measured effect and the cost this
            // puts on genuinely-absent secrets.
            None => {}
        }
        if attempt + 1 < READ_ATTEMPTS {
            read_backoff(attempt);
        }
    }
    match last_error {
        Some(error) => Err(error),
        // Absent on every attempt: the credential really is gone.
        None => Ok(None),
    }
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

#[cfg(test)]
pub fn raw_entries_for_test(server_id: &str, key: &str) -> Result<Vec<String>, String> {
    let base = account(server_id, key);
    let mut accounts = vec![base.clone()];
    if let Some(raw) = read_entry(&base)? {
        if let Some(manifest) = parse_manifest(&raw)? {
            accounts.extend(
                (0..manifest.count)
                    .map(|index| chunk_account(&base, &manifest.generation, index)),
            );
        }
    }
    Ok(accounts)
}

#[cfg(test)]
pub fn raw_entry_exists_for_test(account: &str) -> Result<bool, String> {
    Ok(read_entry(account)?.is_some())
}
