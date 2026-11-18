//! Shell-neutral local observability mutations.

pub fn clear_activity_logs() -> Result<(), String> {
    let mut failed = Vec::new();
    if crate::audit::try_clear().is_err() {
        failed.push("audit log");
    }
    if crate::searchtrace::try_clear().is_err() {
        failed.push("search traces");
    }
    if crate::inspect::try_clear().is_err() {
        failed.push("inspector captures");
    }
    if crate::savings::try_clear().is_err() {
        failed.push("savings");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("Couldn't clear: {}", failed.join(", ")))
    }
}

/// Stores that can change an Activity panel. Exclude locks and backups: reading
/// a store must not itself invalidate the page.
pub fn activity_file(name: &str) -> bool {
    matches!(
        name,
        "audit.jsonl"
            | "security.jsonl"
            | "inspect.jsonl"
            | "search-trace.jsonl"
            | "search-trace-v2.jsonl"
            | "savings.jsonl"
            | "savings-v2.jsonl"
            | "savings-v3.jsonl"
            | "savings-v4.jsonl"
            | "registry.json"
            | "client-locations.json"
            | "root-integrity-scopes.json"
    ) || (name.ends_with(".json")
        && (name.starts_with("tool-pins") || name.starts_with("quarantine")))
}

/// Live health is independent of retained stores. The daemon identity probe is
/// read-only and bounded to two seconds; this path never reads Activity logs.
pub fn activity_health() -> serde_json::Value {
    serde_json::json!({
        "telemetry": crate::telemetry::activity_health(),
        "gatewayNotes": crate::daemon::status_notes(),
    })
}

/// Revisions digest every byte, without parsing or rebuilding Activity panels.
/// Linux caches the digest behind a synchronous, nonblocking directory watcher.
/// A full digest every minute also catches writes outside that watcher (such as
/// remote filesystems or symlink targets). Other platforms, or failed watches,
/// digest on every check. Sampling and timestamps alone cannot detect rewrites.
pub fn activity_revision() -> Result<String, String> {
    activity_revision_for(activity_revision_generation())
}

/// Capture before queuing a worker, so release also cancels work not yet started.
pub fn activity_revision_generation() -> u64 {
    ACTIVITY_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn activity_revision_for(generation: u64) -> Result<String, String> {
    // Only workers wait here. Cache ownership is moved out of the short state
    // lock before any path resolution, metadata, watch setup or digest IO.
    let _reading = ACTIVITY_READING.lock().map_err(|error| error.to_string())?;
    let mut cache = {
        let mut state = ACTIVITY_REVISION
            .lock()
            .map_err(|error| error.to_string())?;
        if generation != activity_revision_generation() {
            return Err("Activity change check cancelled".into());
        }
        state.take()
    };
    let result = (|| {
        let dir = crate::registry::conduit_dir().ok_or("Could not resolve Activity directory")?;
        if cache.as_ref().is_none_or(|cache| cache.dir != dir) {
            cache = Some(RevisionCache::new(dir));
        }
        cache.as_mut().unwrap().revision()
    })();
    let mut state = ACTIVITY_REVISION
        .lock()
        .map_err(|error| error.to_string())?;
    if generation != activity_revision_generation() {
        // The worker owns the only in-flight watch. Drop it here, rather than
        // publishing cancelled state or scheduling cleanup on the UI thread.
        return Err("Activity change check cancelled".into());
    }
    *state = cache;
    result
}

/// Cancel immediately, without waiting for metadata or regular-file reads.
/// An in-flight owner drops its watch when IO returns; queued old generations
/// cannot create one. No watcher thread or deferred UI-thread wait is needed.
pub fn release_activity_revision() {
    let mut state = ACTIVITY_REVISION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ACTIVITY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *state = None;
}

static ACTIVITY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ACTIVITY_READING: std::sync::Mutex<()> = std::sync::Mutex::new(());
static ACTIVITY_REVISION: std::sync::Mutex<Option<RevisionCache>> = std::sync::Mutex::new(None);
const CONTENT_RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

struct RevisionCache {
    dir: std::path::PathBuf,
    cached: Option<(String, String, std::time::Instant)>,
    #[cfg(test)]
    digest_reads: usize,
    #[cfg(target_os = "linux")]
    watch: Option<RevisionWatch>,
}

impl RevisionCache {
    fn new(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            cached: None,
            #[cfg(test)]
            digest_reads: 0,
            #[cfg(target_os = "linux")]
            watch: None,
        }
    }

    fn revision(&mut self) -> Result<String, String> {
        #[cfg(target_os = "linux")]
        let unchanged = {
            // Watch before scanning. Directory identity also detects deletion,
            // recreation and an atomic replacement of the directory itself.
            if self
                .watch
                .as_ref()
                .is_none_or(|watch| !watch.matches(&self.dir))
            {
                self.cached = None;
                self.watch = RevisionWatch::new(&self.dir).ok();
            }
            self.watch.as_mut().is_some_and(|watch| !watch.changed())
        };
        #[cfg(not(target_os = "linux"))]
        let unchanged = false;

        let (metadata, paths) = match activity_metadata(&self.dir) {
            Ok(value) => value,
            Err(error) => {
                self.cached = None;
                return Err(error);
            }
        };
        if unchanged {
            if let Some((previous, digest, checked)) = &self.cached {
                if previous == &metadata && checked.elapsed() < CONTENT_RECHECK {
                    return Ok(digest.clone());
                }
            }
        }
        // Never reuse a successful revision after an unreadable store.
        self.cached = None;
        #[cfg(test)]
        {
            self.digest_reads += 1;
        }
        let digest = activity_digest(&paths)?;
        let revision = if metadata == "missing" {
            metadata.clone()
        } else {
            digest
        };
        #[cfg(target_os = "linux")]
        let stable = self.watch.as_mut().is_some_and(|watch| !watch.changed());
        #[cfg(not(target_os = "linux"))]
        let stable = false;
        // An event during the read leaves the next check dirty. No delayed
        // callback can publish a digest over a newer notification generation.
        if stable {
            self.cached = Some((metadata, revision.clone(), std::time::Instant::now()));
        }
        Ok(revision)
    }
}

fn activity_digest(paths: &[std::path::PathBuf]) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy();
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        let mut options = std::fs::File::options();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A concurrent replacement with a FIFO must not block a shell worker.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|error| {
            format!("Could not read Activity store {}: {error}", path.display())
        })?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err(format!(
                "Activity store {} is not a regular file",
                path.display()
            ));
        }
        digest.update(metadata.len().to_le_bytes());
        // Digest one finite snapshot, even if another process keeps appending.
        // Notifications during this read prevent caching it over newer bytes.
        let mut file = file.take(metadata.len());
        loop {
            let count = file.read(&mut buffer).map_err(|error| {
                format!("Could not read Activity store {}: {error}", path.display())
            })?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn activity_metadata(dir: &std::path::Path) -> Result<(String, Vec<std::path::PathBuf>), String> {
    use std::hash::{Hash, Hasher};
    // A missing directory is an empty store list; the resolved registry may still
    // exist elsewhere and decide what Activity shows.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not check Activity stores: {error}")),
    };
    let dir_missing = entries.is_none();
    let mut stores = Vec::new();
    for entry in entries.into_iter().flatten() {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if activity_file(&name) {
            let metadata = std::fs::metadata(entry.path()).map_err(|error| error.to_string())?;
            stores.push((entry.path(), metadata));
        }
    }
    // The resolved registry decides what Activity shows (its safety level), so
    // its changes count whatever its name or directory, such as an override at
    // <data-dir>/policy.json.
    if let Some(registry) = crate::registry::resolved_path()
        .filter(|path| !stores.iter().any(|(selected, _)| selected == path))
    {
        if let Ok(metadata) = std::fs::metadata(&registry) {
            stores.push((registry, metadata));
        }
    }
    if dir_missing && stores.is_empty() {
        return Ok(("missing".into(), Vec::new()));
    }
    stores.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let paths = stores.iter().map(|(path, _)| path.clone()).collect();
    for (path, metadata) in stores {
        let name = path.to_string_lossy();
        if !metadata.is_file() {
            return Err(format!("Activity store {name} is not a regular file"));
        }
        name.hash(&mut hash);
        metadata.len().hash(&mut hash);
        // Tests can stand in for a filesystem whose timestamps did not tick.
        #[cfg(test)]
        let times = !SAME_TICK.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let times = true;
        if times {
            metadata.modified().ok().hash(&mut hash);
            metadata.created().ok().hash(&mut hash);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino()).hash(&mut hash);
            if times {
                (metadata.ctime(), metadata.ctime_nsec()).hash(&mut hash);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };
            let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
            // SAFETY: the handle belongs to this open file and info is a valid output buffer.
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            (
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            )
                .hash(&mut hash);
        }
    }
    // A Safety change can keep the registry's length ("ask" to "off") and land in
    // one timestamp tick, and a registry outside the data directory has no
    // directory watch. Hash its content (capped, never blocking): any rewrite counts.
    crate::registry::registry_content_stamp().hash(&mut hash);
    Ok((format!("{:x}", hash.finish()), paths))
}

#[cfg(test)]
thread_local! {
    static SAME_TICK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(target_os = "linux")]
struct RevisionWatch {
    fd: std::os::fd::OwnedFd,
    identity: (u64, u64),
    valid: bool,
}

#[cfg(target_os = "linux")]
impl RevisionWatch {
    fn new(dir: &std::path::Path) -> std::io::Result<Self> {
        use std::os::{fd::FromRawFd, unix::ffi::OsStrExt, unix::fs::MetadataExt};
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
        let metadata = std::fs::metadata(dir)?;
        // SAFETY: inotify returns an owned descriptor, immediately wrapped for
        // RAII close on failure, directory change, hidden page or process exit.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        use std::os::fd::AsRawFd;
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_ATTRIB
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_ONLYDIR;
        // SAFETY: fd and the NUL-terminated path remain live for this call.
        if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            identity: (metadata.dev(), metadata.ino()),
            valid: true,
        })
    }

    fn matches(&self, dir: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        self.valid
            && std::fs::metadata(dir)
                .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
    }

    fn changed(&mut self) -> bool {
        use std::os::fd::AsRawFd;
        let mut changed = false;
        let mut buffer = [0u8; 8192];
        // Bound synchronous work even under sustained writes. Remaining events
        // invalidate the next check too; overflow always forces a full digest.
        for _ in 0..16 {
            // SAFETY: buffer is writable for its full length, fd is owned/live.
            let count = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return changed;
                }
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                self.valid = false;
                return true;
            }
            if count == 0 {
                self.valid = false;
                return true;
            }
            let mut offset = 0;
            let header = std::mem::size_of::<libc::inotify_event>();
            while offset + header <= count as usize {
                // SAFETY: the complete fixed header fits in the read buffer.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let end = offset + header + event.len as usize;
                if end > count as usize {
                    self.valid = false;
                    return true;
                }
                if event.mask
                    & (libc::IN_IGNORED
                        | libc::IN_DELETE_SELF
                        | libc::IN_MOVE_SELF
                        | libc::IN_UNMOUNT)
                    != 0
                {
                    self.valid = false;
                    changed = true;
                }
                if event.mask & libc::IN_Q_OVERFLOW != 0 {
                    changed = true;
                }
                let name = &buffer[offset + header..end];
                let name = &name[..name
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name.len())];
                if activity_file(&String::from_utf8_lossy(name)) {
                    changed = true;
                }
                offset = end;
            }
            if offset != count as usize {
                self.valid = false;
                return true;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_content_revision_covers_middle_bytes_and_preserved_metadata() {
        let data = crate::registry::DataDirTestEnv::new("activity-content-rewrite");
        let path = data.dir.join("audit.jsonl");
        let mut bytes = vec![b'a'; 512 * 1024];
        std::fs::write(&path, &bytes).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut cache = RevisionCache::new(data.dir.clone());
        let first = cache.revision().unwrap();
        bytes[257 * 1024] = b'b';
        std::fs::write(&path, &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        // Simulate a filesystem that also preserves ctime: present the cache
        // with the current metadata, so only the write notification can help.
        if let Some(cached) = &mut cache.cached {
            cached.0 = activity_metadata(&data.dir).unwrap().0;
        }
        let rewritten = cache.revision().unwrap();
        assert_ne!(rewritten, first);
        assert_eq!(cache.revision().unwrap(), rewritten);
        // The portable fallback hashes the same complete bytes, not samples.
        assert_eq!(activity_digest(&[path]).unwrap(), rewritten);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn activity_revision_idle_cache_and_missed_event_fallback_are_bounded() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-idle");
        let path = data.dir.join("savings-v4.jsonl");
        std::fs::write(&path, vec![b'a'; 1024 * 1024]).unwrap();
        let mut cache = RevisionCache::new(data.dir.clone());
        let first = cache.revision().unwrap();
        assert!(cache.watch.is_some());
        for _ in 0..20 {
            assert_eq!(cache.revision().unwrap(), first);
        }
        assert_eq!(
            cache.digest_reads, 1,
            "idle checks must not reread contents"
        );
        std::fs::write(data.dir.join("savings-v4.jsonl.lock"), "ignored").unwrap();
        std::fs::write(data.dir.join("savings-v4.jsonl.bak"), "ignored").unwrap();
        assert_eq!(cache.revision().unwrap(), first);
        assert_eq!(cache.digest_reads, 1);
        std::fs::write(&path, vec![b'b'; 1024 * 1024]).unwrap();
        assert!(cache.watch.as_mut().unwrap().changed()); // simulate a lost event
        let cached = cache.cached.as_mut().unwrap();
        cached.0 = activity_metadata(&data.dir).unwrap().0;
        cached.2 = std::time::Instant::now() - CONTENT_RECHECK;
        assert_ne!(cache.revision().unwrap(), first);
        assert_eq!(
            cache.digest_reads, 2,
            "full periodic audit catches missed notifications"
        );
    }

    #[test]
    #[cfg(unix)]
    fn activity_revision_audits_symlink_targets_outside_the_watched_directory() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-symlink");
        let watched = data.dir.join("watched");
        std::fs::create_dir(&watched).unwrap();
        let target = data.dir.join("target");
        std::fs::write(&target, "before").unwrap();
        std::os::unix::fs::symlink(&target, watched.join("audit.jsonl")).unwrap();
        let mut cache = RevisionCache::new(watched.clone());
        let before = cache.revision().unwrap();
        let modified = std::fs::metadata(&target).unwrap().modified().unwrap();
        std::fs::write(&target, "after!").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        if let Some(cached) = &mut cache.cached {
            cached.0 = activity_metadata(&watched).unwrap().0;
            cached.2 = std::time::Instant::now() - CONTENT_RECHECK;
        }
        assert_ne!(cache.revision().unwrap(), before);
        std::fs::remove_file(target).unwrap();
        assert!(
            cache.revision().is_err(),
            "broken targets must not reuse cached success"
        );
    }

    #[test]
    fn activity_revision_covers_a_registry_override_by_its_resolved_path() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-registry-override");
        let _env = crate::registry::REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let elsewhere = data.dir.with_extension("registry-override");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let registry = elsewhere.join("registry.json");
        std::fs::write(&registry, r#"{"safetyLevel":"off"}"#).unwrap();
        std::fs::write(data.dir.join("security.jsonl"), "{}\n").unwrap();
        struct Restore(Option<std::ffi::OsString>, std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("TOOLPORT_REGISTRY", value),
                    None => std::env::remove_var("TOOLPORT_REGISTRY"),
                }
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _restore = Restore(std::env::var_os("TOOLPORT_REGISTRY"), elsewhere.clone());
        // Outside the data dir, and inside it under a name the store scan skips.
        let policy = data.dir.join("policy.json");
        std::fs::write(&policy, r#"{"safetyLevel":"off"}"#).unwrap();
        for registry in [registry.clone(), policy] {
            std::env::set_var("TOOLPORT_REGISTRY", &registry);
            let mut cache = RevisionCache::new(data.dir.clone());
            let before = cache.revision().unwrap();
            assert_eq!(cache.revision().unwrap(), before, "{registry:?} unchanged");
            std::fs::write(&registry, r#"{"safetyLevel":"ask"}"#).unwrap();
            assert_ne!(cache.revision().unwrap(), before, "{registry:?}");
        }
        // A same-length rewrite in place, within one timestamp tick, still counts.
        std::env::set_var("TOOLPORT_REGISTRY", &registry);
        std::fs::write(&registry, r#"{"safetyLevel":"off"}"#).unwrap();
        SAME_TICK.with(|same| same.set(true));
        let mut cache = RevisionCache::new(data.dir.clone());
        let before = cache.revision().unwrap();
        std::fs::write(&registry, r#"{"safetyLevel":"ask"}"#).unwrap();
        let after = cache.revision();
        SAME_TICK.with(|same| same.set(false));
        assert_ne!(after.unwrap(), before, "same-length Safety change");
        // No Activity directory yet: the external registry still counts.
        std::env::set_var("TOOLPORT_REGISTRY", &registry);
        let mut absent = RevisionCache::new(data.dir.join("absent"));
        let before = absent.revision().unwrap();
        assert_ne!(before, "missing");
        std::fs::write(&registry, r#"{"safetyLevel":"off"}"#).unwrap();
        assert_ne!(absent.revision().unwrap(), before);
        std::fs::remove_file(&registry).unwrap();
        assert_eq!(absent.revision().unwrap(), "missing");
    }

    #[test]
    fn activity_revision_rejects_queued_generations_after_release() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-cancelled");
        std::fs::write(data.dir.join("audit.jsonl"), "before").unwrap();
        release_activity_revision();
        let generation = activity_revision_generation();
        let reading = ACTIVITY_READING.lock().unwrap();
        let worker = std::thread::spawn(move || activity_revision_for(generation));
        release_activity_revision();
        drop(reading);
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(ACTIVITY_REVISION.lock().unwrap().is_none());
        assert!(
            activity_revision().is_ok(),
            "new visible generation can read"
        );
        release_activity_revision();
    }

    #[test]
    fn activity_revision_releases_and_recreates_directory_state() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-lifecycle");
        std::fs::write(data.dir.join("audit.jsonl"), "before").unwrap();
        let before = activity_revision().unwrap();
        release_activity_revision();
        assert!(ACTIVITY_REVISION.lock().unwrap().is_none());
        std::fs::write(data.dir.join("audit.jsonl"), "after!").unwrap();
        let after = activity_revision().unwrap();
        assert_ne!(after, before);
        std::fs::remove_dir_all(&data.dir).unwrap();
        assert_eq!(activity_revision().unwrap(), "missing");
        std::fs::create_dir_all(&data.dir).unwrap();
        std::fs::write(data.dir.join("audit.jsonl"), "again!").unwrap();
        assert_ne!(activity_revision().unwrap(), after);
        release_activity_revision();
        let mut other = RevisionCache::new(data.dir.join("absent"));
        assert_eq!(other.revision().unwrap(), "missing");
        std::fs::create_dir_all(&other.dir).unwrap();
        std::fs::write(other.dir.join("savings-v4.jsonl"), "new root").unwrap();
        assert_ne!(other.revision().unwrap(), "missing");
        assert_ne!(other.revision().unwrap(), before);
    }

    #[test]
    fn activity_revision_errors_do_not_reuse_a_successful_revision() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision-errors");
        let path = data.dir.join("audit.jsonl");
        std::fs::write(&path, "good").unwrap();
        let mut cache = RevisionCache::new(data.dir.clone());
        let before = cache.revision().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(cache.revision().unwrap_err().contains("not a regular file"));
        assert!(cache.cached.is_none());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "back").unwrap();
        assert_ne!(cache.revision().unwrap(), before);
    }

    #[test]
    fn savings_v4_revision_observes_creation_append_replacement_and_removal() {
        let data = crate::registry::DataDirTestEnv::new("activity-v4-revision");
        let empty = activity_revision().unwrap();
        for name in ["savings-v4.jsonl.lock", "savings-v4.jsonl.bak"] {
            std::fs::write(data.dir.join(name), "ignored").unwrap();
        }
        assert_eq!(activity_revision().unwrap(), empty);
        let path = data.dir.join("savings-v4.jsonl");
        std::fs::write(&path, "one").unwrap();
        let created = activity_revision().unwrap();
        assert_ne!(created, empty);
        std::fs::write(&path, "one two").unwrap();
        let appended = activity_revision().unwrap();
        assert_ne!(appended, created);
        let replacement = data.dir.join("replacement");
        std::fs::write(&replacement, "new two").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(std::fs::metadata(&path).unwrap().modified().unwrap())
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(activity_revision().unwrap(), appended);
        std::fs::remove_file(path).unwrap();
        assert_eq!(activity_revision().unwrap(), empty);
    }

    #[test]
    fn activity_revision_observes_appends_rewrites_replacements_and_removal() {
        let data = crate::registry::DataDirTestEnv::new("activity-revision");
        let empty = activity_revision().unwrap();
        std::fs::write(data.dir.join("audit.jsonl.lock"), "lock").unwrap();
        std::fs::write(data.dir.join("gateway.log"), "background log").unwrap();
        assert_eq!(activity_revision().unwrap(), empty);
        let path = data.dir.join("audit.jsonl");
        std::fs::write(&path, "one").unwrap();
        let first = activity_revision().unwrap();
        assert_ne!(first, empty);
        assert_eq!(activity_revision().unwrap(), first);
        std::fs::write(&path, "one two").unwrap();
        let appended = activity_revision().unwrap();
        assert_ne!(appended, first);
        std::fs::write(&path, "new two").unwrap();
        assert_ne!(activity_revision().unwrap(), appended);
        std::fs::write(data.dir.join("replacement"), "changed").unwrap();
        std::fs::File::options()
            .write(true)
            .open(data.dir.join("replacement"))
            .unwrap()
            .set_modified(std::fs::metadata(&path).unwrap().modified().unwrap())
            .unwrap();
        let rewritten = activity_revision().unwrap();
        std::fs::rename(data.dir.join("replacement"), &path).unwrap();
        assert_ne!(activity_revision().unwrap(), rewritten);
        std::fs::remove_file(path).unwrap();
        assert_eq!(activity_revision().unwrap(), empty);
        for name in [
            "tool-pins-v2-profile.json",
            "quarantine-v2-profile.json",
            "security.jsonl",
            "savings-v3.jsonl",
            "client-locations.json",
        ] {
            assert!(activity_file(name));
            assert!(!activity_file(&format!("{name}.lock")));
            assert!(!activity_file(&format!("{name}.bak")));
        }
    }
}
