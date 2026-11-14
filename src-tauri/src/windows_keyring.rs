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

/// Serializes this backend across processes.
///
/// The app and the gateway are separate processes using the same credentials. A
/// chunked value is several Credential Manager entries, so without this a reader
/// in one process can read a manifest whose chunks another process is replacing
/// or deleting ("chunk N is missing"), or see the base entry as absent mid-write.
/// A writer that misreads the previous manifest that way also leaks its chunks,
/// and enough leaked chunks make every later `CredWriteW` fail. The Linux
/// backend serializes Secret Service access across processes the same way.
///
/// The vault belongs to the user, not the session or the integrity level, so the
/// lock does too: a `Global\` name carrying the user SID (OpenSSH and scheduled
/// tasks run in other sessions), a DACL granting only that SID the rights a lock
/// needs, and a medium mandatory label so an elevated creator does not lock out
/// the app through no-write-up. SIDs are not secret, so another account could
/// pre-create the name; `open_lock_mutex` refuses a mutex this user does not own.
struct LockIdentity {
    sid: String,
    name: Vec<u16>,
    sddl: Vec<u16>,
}

fn lock_identity() -> Option<&'static LockIdentity> {
    static LOCK: std::sync::OnceLock<Option<LockIdentity>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| {
        let sid = user_sid()?;
        let wide = |text: String| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
        Some(LockIdentity {
            name: wide(format!("Global\\toolport-conduit-mcp-credentials-{sid}")),
            // Owner pinned to the user (an elevated token would otherwise make
            // Administrators the owner). 0x100001 is SYNCHRONIZE |
            // MUTEX_MODIFY_STATE. Medium label: Toolport runs no low-integrity
            // process, and an elevated creator must not lock out the app.
            sddl: wide(format!("O:{sid}D:P(A;;0x100001;;;{sid})S:(ML;;NW;;;ME)")),
            sid,
        })
    })
    .as_ref()
}

/// The current process's user SID, which is the same with or without elevation.
fn user_sid() -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut len = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    // u64 storage keeps TOKEN_USER suitably aligned.
    let mut buffer = vec![0u64; (len as usize).div_ceil(8).max(1)];
    let ok =
        unsafe { GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len) };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return None;
    }
    let sid = unsafe { wide_string(text) };
    unsafe { LocalFree(text.cast()) };
    sid
}

/// Read a NUL-terminated UTF-16 string.
unsafe fn wide_string(mut ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut units = Vec::new();
    while unsafe { *ptr } != 0 {
        units.push(unsafe { *ptr });
        ptr = unsafe { ptr.add(1) };
    }
    String::from_utf16(&units).ok()
}

/// Bounded like the Linux lock: a stuck peer must not hang credential access
/// forever. Past the deadline the operation runs unlocked, as it did before.
const LOCK_TIMEOUT_MS: u32 = 5_000;

/// Held for one whole backend operation. A Windows mutex is owned by a thread, so
/// this also serializes threads in one process, and re-entry on the owning thread
/// is allowed.
struct CredentialLock(windows_sys::Win32::Foundation::HANDLE);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Open (or create) the per-user mutex with only the rights a lock needs, and
/// refuse it unless this user owns it. The handle is not waited on, so callers
/// decide how long to wait.
fn open_lock_mutex() -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    let identity = lock_identity().ok_or("could not read the user SID")?;
    open_owned_mutex(&identity.name, &identity.sddl, &identity.sid)
}

/// Open or create `name` with `sddl`, and keep it only if `owner` owns it.
fn open_owned_mutex(
    name: &[u16],
    sddl: &[u16],
    owner: &str,
) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::System::Threading::{
        CreateMutexExW, MUTEX_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
    };
    // READ_CONTROL lets us read the owner. The owner gets it implicitly, so a
    // mutex this user created always grants it.
    const READ_CONTROL: u32 = 0x0002_0000;
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // CreateMutexW would ask for MUTEX_ALL_ACCESS, which the DACL above denies to
    // every opener after the creator.
    let handle = unsafe {
        CreateMutexExW(
            &attributes,
            name.as_ptr(),
            0,
            SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE | READ_CONTROL,
        )
    };
    let error = std::io::Error::last_os_error();
    unsafe { LocalFree(descriptor.cast()) };
    if handle.is_null() {
        return Err(error.to_string());
    }
    match mutex_owner(handle) {
        Some(found) if found == owner => Ok(handle),
        found => {
            unsafe { CloseHandle(handle) };
            Err(format!(
                "the lock name is held by another account ({})",
                found.as_deref().unwrap_or("unknown owner")
            ))
        }
    }
}

/// The owner SID of a kernel object, as a string.
fn mutex_owner(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    } != 0
    {
        return None;
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    let sid = if unsafe { ConvertSidToStringSidW(owner, &mut text) } != 0 {
        let sid = unsafe { wide_string(text) };
        unsafe { LocalFree(text.cast()) };
        sid
    } else {
        None
    };
    // `owner` points into `descriptor`.
    unsafe { LocalFree(descriptor.cast()) };
    sid
}

fn credential_lock_for(timeout_ms: u32) -> Option<CredentialLock> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    let handle = match open_lock_mutex() {
        Ok(handle) => handle,
        Err(error) => {
            // Once per process: every credential call would otherwise repeat it.
            static REPORTED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                eprintln!("toolport: credential lock unavailable ({error}); continuing unlocked");
            }
            return None;
        }
    };
    // An abandoned mutex means a peer exited mid-operation. Its writes are either
    // committed or unreachable, so taking ownership is safe.
    match unsafe { WaitForSingleObject(handle, timeout_ms) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Some(CredentialLock(handle)),
        _ => {
            eprintln!("toolport: credential lock timed out; continuing unlocked");
            unsafe { CloseHandle(handle) };
            None
        }
    }
}

fn credential_lock() -> Option<CredentialLock> {
    credential_lock_for(LOCK_TIMEOUT_MS)
}

/// Test-only: as `open_lock_mutex`, but for another name, so a test can stand in
/// for an account that pre-created the lock name.
#[cfg(test)]
pub fn open_lock_mutex_named_for_test(name: &str) -> Result<(), String> {
    let identity = lock_identity().ok_or("could not read the user SID")?;
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let handle = open_owned_mutex(&name, &identity.sddl, &identity.sid)?;
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    Ok(())
}

#[cfg(test)]
pub fn lock_mutex_opens_for_test() -> Result<(), String> {
    let handle = open_lock_mutex()?;
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    Ok(())
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

/// Generations younger than this are never swept. A process that predates the
/// lock can be partway through a write, with chunks stored but no manifest yet.
const ORPHAN_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Bounds on one sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SweepLimits {
    /// Longest one batch holds the lock after its snapshot. Peers wait at most
    /// `LOCK_TIMEOUT_MS`, so a big sweep hands the lock back in between.
    batch: std::time::Duration,
    /// Hard caps for the whole sweep. Whatever is left waits for the next process.
    total: std::time::Duration,
    max_batches: usize,
    /// A base missing from the vault must stay missing this long, across two
    /// snapshots, before its chunks count as orphaned. One listing could show the
    /// same false absence a read can.
    absence_gap: std::time::Duration,
}

const SWEEP_LIMITS: SweepLimits = SweepLimits {
    batch: std::time::Duration::from_millis(200),
    total: std::time::Duration::from_secs(10),
    max_batches: 100,
    absence_gap: std::time::Duration::from_secs(1),
};

/// Any chunked manifest format, including versions this build does not know.
const CHUNKED_FAMILY_PREFIX: &str = "toolport-chunked-";

/// Split a chunk account into its base account and generation, but only when it
/// has exactly the shape `chunk_account` produces.
fn parse_chunk_account(account: &str) -> Option<(&str, &str)> {
    let (rest, index) = account.rsplit_once("::")?;
    let (base, generation) = rest.rsplit_once("::toolport-chunk::")?;
    (!base.is_empty()
        && !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && generation.len() == 32
        && generation
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some((base, generation))
}

#[cfg(test)]
thread_local! {
    /// Test-only: make the sweep's base reads report absent, the false absence
    /// Windows shows while a peer rewrites the base.
    static SWEEP_READS_ABSENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn with_sweep_reads_absent<T>(test: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SWEEP_READS_ABSENT.with(|absent| absent.set(false));
        }
    }
    SWEEP_READS_ABSENT.with(|absent| absent.set(true));
    let _reset = Reset;
    test()
}

/// Base reads for the sweep. Plain `read_entry`, so a sweep never consumes a
/// test's scripted `read_base` results.
fn read_sweep_base(base: &str) -> Result<Option<String>, String> {
    #[cfg(test)]
    if SWEEP_READS_ABSENT.with(std::cell::Cell::get) {
        return Ok(None);
    }
    read_entry(base)
}

/// What the sweep may do with one base's chunk generations.
enum BaseVerdict {
    /// Delete every generation except this one (`None`: the base names none).
    Live(Option<String>),
    /// Missing from this snapshot and from every read. Orphaned only if a later
    /// snapshot, at least `absence_gap` later, agrees.
    Absent,
    /// Not provably orphaned. Keep all of them.
    Keep,
}

/// Decide from positive evidence only. Windows can report a present credential
/// as absent, and an unlocked peer (elevated, pre-lock, timed out, or another
/// app) can be rewriting it, so "absent" alone never condemns a generation that
/// the base might still name.
fn base_verdict(base: &str, in_snapshot: bool) -> BaseVerdict {
    for attempt in 0..READ_ATTEMPTS {
        match read_sweep_base(base) {
            Err(_) => return BaseVerdict::Keep,
            Ok(Some(value)) if value.starts_with(CHUNKED_FAMILY_PREFIX) => {
                return match parse_manifest(&value) {
                    Ok(Some(manifest)) => BaseVerdict::Live(Some(manifest.generation)),
                    // Damaged, or a manifest version newer than this build.
                    _ => BaseVerdict::Keep,
                };
            }
            // A direct value names no generation.
            Ok(Some(_)) => return BaseVerdict::Live(None),
            Ok(None) => {}
        }
        if attempt + 1 < READ_ATTEMPTS {
            read_backoff(attempt);
        }
    }
    if in_snapshot {
        BaseVerdict::Keep
    } else {
        BaseVerdict::Absent
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: extra time each sweep snapshot takes, standing in for a vault
    /// so full of leaked chunks that listing it is slow.
    static SLOW_SNAPSHOT: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

#[cfg(test)]
pub fn with_slow_sweep_snapshots<T>(delay: std::time::Duration, test: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SLOW_SNAPSHOT.with(|slow| slow.set(std::time::Duration::ZERO));
        }
    }
    SLOW_SNAPSHOT.with(|slow| slow.set(delay));
    let _reset = Reset;
    test()
}

type ChunkGenerations = std::collections::HashMap<String, (Vec<String>, bool)>;

/// One read-only pass over the vault: which base accounts exist, and each base's
/// chunk generations with their accounts and whether every chunk is old enough.
/// `scope` limits the pass to targets starting with it. `None` if enumeration
/// failed for any reason other than an empty result.
fn chunk_snapshot(
    min_age: std::time::Duration,
    scope: Option<&str>,
) -> Option<(
    std::collections::HashSet<String>,
    std::collections::HashMap<String, ChunkGenerations>,
)> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredEnumerateW, CredFree, CREDENTIALW, CRED_TYPE_GENERIC,
    };
    // FILETIME counts 100ns ticks since 1601.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64 / 100 + 116_444_736_000_000_000)
        .ok()?;
    let min_ticks = min_age.as_nanos() as u64 / 100;
    #[cfg(test)]
    std::thread::sleep(SLOW_SNAPSHOT.with(std::cell::Cell::get));
    let filter: Option<Vec<u16>> =
        scope.map(|prefix| format!("{prefix}*").encode_utf16().chain([0]).collect());
    let mut count = 0;
    let mut entries: *mut *mut CREDENTIALW = std::ptr::null_mut();
    let mut bases = std::collections::HashSet::new();
    let mut chunks: std::collections::HashMap<String, ChunkGenerations> =
        std::collections::HashMap::new();
    if unsafe {
        CredEnumerateW(
            filter
                .as_ref()
                .map_or(std::ptr::null(), |filter| filter.as_ptr()),
            0,
            &mut count,
            &mut entries,
        )
    } == 0
    {
        return (unsafe { GetLastError() } == ERROR_NOT_FOUND).then_some((bases, chunks));
    }
    for entry in unsafe { std::slice::from_raw_parts(entries, count as usize) } {
        let entry = unsafe { &**entry };
        if entry.Type != CRED_TYPE_GENERIC {
            continue;
        }
        let target = unsafe { wide_string(entry.TargetName) };
        let user = unsafe { wide_string(entry.UserName) };
        let (Some(target), Some(user)) = (target, user) else {
            continue;
        };
        if target != format!("{user}.{SERVICE}") {
            continue;
        }
        let Some((base, generation)) = parse_chunk_account(&user) else {
            bases.insert(user);
            continue;
        };
        let written = (u64::from(entry.LastWritten.dwHighDateTime) << 32)
            | u64::from(entry.LastWritten.dwLowDateTime);
        let slot = chunks
            .entry(base.to_string())
            .or_default()
            .entry(generation.to_string())
            .or_insert_with(|| (Vec::new(), true));
        slot.0.push(user.clone());
        slot.1 &= now.saturating_sub(written) >= min_ticks;
    }
    unsafe { CredFree(entries.cast()) };
    Some((bases, chunks))
}

/// Delete Toolport chunk generations that no base credential names any more.
///
/// They come from writes that crashed between storing chunks and committing the
/// manifest, cleanup that failed, and (before the lock) processes racing on one
/// credential. They are unreachable, and enough of them fill the per-user store
/// until every `CredWriteW` fails, for Toolport and for other apps.
///
/// Only generic `conduit-mcp` credentials whose account has the exact chunk shape
/// are considered, and only under the credential lock: if the lock cannot be
/// taken, nothing is swept. Each batch starts from a fresh snapshot, then works
/// for at most `limits.batch` but always finishes a base or deletes an entry, so
/// a slow snapshot cannot stall it. The whole sweep stops at `limits.total` or
/// `limits.max_batches`. Returns the number of entries removed.
fn sweep_orphaned_chunks(
    min_age: std::time::Duration,
    scope: Option<&str>,
    limits: SweepLimits,
) -> usize {
    use std::time::Instant;
    let deadline = Instant::now() + limits.total;
    let mut finished = std::collections::HashSet::new();
    // First time each base was seen missing from both the vault and its reads.
    let mut absent_since: std::collections::HashMap<String, Instant> =
        std::collections::HashMap::new();
    let mut removed = 0;
    for _ in 0..limits.max_batches {
        if Instant::now() >= deadline {
            break;
        }
        let mut waiting_until = None;
        {
            let Some(_lock) = credential_lock() else {
                break;
            };
            let Some((bases, chunks)) = chunk_snapshot(min_age, scope) else {
                break;
            };
            // The batch budget covers work, not the listing that precedes it.
            let started = Instant::now();
            let mut pending: Vec<_> = chunks
                .into_iter()
                .filter(|(base, _)| !finished.contains(base))
                .collect();
            if pending.is_empty() {
                break;
            }
            // Same order every batch, so a resumed base is reached first.
            pending.sort_by(|left, right| left.0.cmp(&right.0));
            let mut progressed = false;
            'batch: for (base, generations) in pending {
                if progressed && started.elapsed() >= limits.batch {
                    break;
                }
                let live = match base_verdict(&base, bases.contains(&base)) {
                    BaseVerdict::Keep => {
                        absent_since.remove(&base);
                        finished.insert(base);
                        progressed = true;
                        continue;
                    }
                    BaseVerdict::Live(live) => {
                        absent_since.remove(&base);
                        live
                    }
                    BaseVerdict::Absent => {
                        let first = *absent_since
                            .entry(base.clone())
                            .or_insert_with(Instant::now);
                        let confirmed_at = first + limits.absence_gap;
                        if Instant::now() < confirmed_at {
                            waiting_until = Some(
                                waiting_until
                                    .map_or(confirmed_at, |at: Instant| at.min(confirmed_at)),
                            );
                            continue;
                        }
                        None
                    }
                };
                for (generation, (accounts, old)) in generations {
                    if !old || live.as_deref() == Some(generation.as_str()) {
                        continue;
                    }
                    for account in accounts {
                        if progressed && started.elapsed() >= limits.batch {
                            // Resume this base from the next snapshot.
                            break 'batch;
                        }
                        match delete_entry(&account) {
                            Ok(()) => removed += 1,
                            Err(error) => {
                                eprintln!(
                                    "toolport: orphaned credential chunk not removed: {error}"
                                )
                            }
                        }
                        progressed = true;
                    }
                }
                absent_since.remove(&base);
                finished.insert(base);
                progressed = true;
            }
            if progressed {
                continue;
            }
        }
        // Only absences awaiting confirmation are left. Wait without the lock.
        match waiting_until {
            Some(at) if at < deadline => {
                std::thread::sleep(at.saturating_duration_since(Instant::now()))
            }
            _ => break,
        }
    }
    removed
}

/// What the once-per-process sweep runs with: production age and limits, the
/// whole vault.
fn production_sweep() -> (std::time::Duration, Option<&'static str>, SweepLimits) {
    (ORPHAN_MIN_AGE, None, SWEEP_LIMITS)
}

/// Sweep once per process, on a background thread started by the first write:
/// writes are what add chunks, and a sweep, however long, must never hold up a
/// save. Test builds skip it, so tests never sweep a developer's real vault.
fn sweep_once() {
    #[cfg(not(test))]
    {
        static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let spawned = std::thread::Builder::new()
                .name("toolport-credential-sweep".into())
                .spawn(|| {
                    let (min_age, scope, limits) = production_sweep();
                    let removed = sweep_orphaned_chunks(min_age, scope, limits);
                    if removed > 0 {
                        eprintln!(
                            "toolport: removed {removed} orphaned Windows credential chunk(s)"
                        );
                    }
                });
            if let Err(error) = spawned {
                eprintln!("toolport: credential sweep not started: {error}");
            }
        }
    }
}

#[cfg(test)]
pub fn production_sweep_is_whole_vault_with_production_limits() -> bool {
    production_sweep() == (ORPHAN_MIN_AGE, None, SWEEP_LIMITS)
        && ORPHAN_MIN_AGE == std::time::Duration::from_secs(10 * 60)
        && SWEEP_LIMITS.batch < std::time::Duration::from_millis(LOCK_TIMEOUT_MS as u64)
}

pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    sweep_once();
    let _lock = credential_lock();
    let base = account(server_id, key);
    let previous_manifest =
        read_entry(&base)?
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
    let _lock = credential_lock();
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
    let _lock = credential_lock();
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
pub fn sweep_orphaned_chunks_for_test(min_age: std::time::Duration, scope: &str) -> usize {
    sweep_orphaned_chunks(min_age, Some(scope), SWEEP_LIMITS)
}

/// Store one raw chunk entry, as a crashed or racing writer would leave it.
#[cfg(test)]
pub fn plant_chunk_for_test(base: &str, generation: &str, index: usize) -> Result<String, String> {
    let account = chunk_account(base, generation, index);
    entry(&account)?
        .set_password("orphaned chunk")
        .map_err(|error| error.to_string())?;
    Ok(account)
}

#[cfg(test)]
pub fn set_raw_account_for_test(account: &str, value: &str) -> Result<(), String> {
    entry(account)?
        .set_password(value)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub fn delete_raw_account_for_test(account: &str) -> Result<(), String> {
    delete_entry(account)
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
                (0..manifest.count).map(|index| chunk_account(&base, &manifest.generation, index)),
            );
        }
    }
    Ok(accounts)
}

#[cfg(test)]
pub fn raw_entry_exists_for_test(account: &str) -> Result<bool, String> {
    Ok(read_entry(account)?.is_some())
}
