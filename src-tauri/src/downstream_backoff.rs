//! Cross-process 429 backoff for downstream HTTP servers (issue #874).
//!
//! Every stdio client session runs its own gateway process (see
//! docs/design/one-gateway-per-host.md), so per-process backoff cannot protect
//! a provider's shared rate limit: each new session starts with fresh state and
//! immediately re-hits a provider that is already returning 429s. Until the
//! host daemon lands, gateway processes coordinate through a small JSON file in
//! the data dir (a sibling of rate_limit_counters.json): a 429 records a
//! per-provider retry-not-before timestamp, and every gateway consults it
//! before sending, failing fast while the window is open. The mitigation is
//! deliberately conservative: windows cap at HTTP_RETRY_CAP, keys carry no path
//! or query (so no endpoint token is ever persisted), and a missing or corrupt
//! file simply means no backoff.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::downstream::HTTP_RETRY_CAP;
use serde::{Deserialize, Serialize};

/// Backoff state file in the Toolport data dir, named like the counter file it
/// sits beside (rate_limit_counters.json).
const FILE_NAME: &str = "downstream_backoff.json";

#[derive(Default, Serialize, Deserialize)]
struct BackoffFile {
    /// provider origin (scheme://host:port) -> unix epoch millis before which
    /// the provider should not be contacted again
    not_before: HashMap<String, u64>,
}

struct BackoffState {
    path: Option<PathBuf>,
    not_before: HashMap<String, u64>,
}

static STATE: OnceLock<Mutex<BackoffState>> = OnceLock::new();

fn state_lock() -> &'static Mutex<BackoffState> {
    STATE.get_or_init(|| {
        Mutex::new(BackoffState {
            path: None,
            not_before: HashMap::new(),
        })
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Bind the backoff file to a data directory (the registry parent), like
/// rate_limits::bind_data_dir. Safe to call multiple times; the first bind wins
/// until process exit. All errors are swallowed: shared backoff is a mitigation
/// for busy hosts, never a load-bearing gate.
pub fn bind_data_dir(dir: &Path) {
    let path = dir.join(FILE_NAME);
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    let st = &mut *guard;
    if st.path.is_some() {
        return;
    }
    if let Ok(mut disk) = load_file(&path) {
        sanitize(&mut disk, now_ms());
        merge_max(&mut st.not_before, &disk.not_before);
    }
    st.path = Some(path);
}

/// Canonical key for a downstream endpoint: scheme://host:port via url::Url
/// origin serialization, so scheme/host case and explicit default ports
/// (HTTPS://Example.COM:443 vs https://example.com) collapse to one key. The
/// path, query, fragment, and userinfo are dropped on purpose, both so an
/// endpoint token in any of them can never be persisted and so two server
/// entries aimed at one provider (for example a work and a personal account)
/// share one window — the provider limits them together.
fn origin_key(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(parsed.origin().ascii_serialization()),
        _ => None,
    }
}

/// Merge incoming timestamps into kept, always retaining the later one: a
/// fresh 429 must never shorten a window another process already recorded.
fn merge_max(kept: &mut HashMap<String, u64>, incoming: &HashMap<String, u64>) {
    for (key, ts) in incoming {
        let entry = kept.entry(key.clone()).or_insert(0);
        if *entry < *ts {
            *entry = *ts;
        }
    }
}

/// Normalize persisted deadlines: drop ones that have already elapsed so the
/// file stays small, and cap survivors at one full window, so a hand-edited
/// file or a clock stepped backward cannot park a provider any longer than a
/// legitimate 429 would.
fn sanitize(file: &mut BackoffFile, now: u64) {
    let ceiling = now.saturating_add(HTTP_RETRY_CAP.as_millis() as u64);
    file.not_before.retain(|_, ts| {
        if *ts <= now {
            return false;
        }
        if *ts > ceiling {
            *ts = ceiling;
        }
        true
    });
}

fn load_file(path: &Path) -> Result<BackoffFile, String> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|err| err.to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BackoffFile::default()),
        Err(err) => Err(err.to_string()),
    }
}

fn save_file(path: &Path, file: &BackoffFile) -> Result<(), String> {
    let raw = serde_json::to_string(file).map_err(|err| err.to_string())?;
    crate::registry::atomic_write(path, &raw).map_err(|err| err.to_string())
}

/// Record that the provider behind url just returned 429. retry_after is the
/// server-advertised delay when the response carried Retry-After; without one
/// the full HTTP_RETRY_CAP window is used, since the provider gave no signal
/// and the point is to keep every session on the host from re-hitting it at
/// once. Best-effort: any failure leaves the window process-local.
pub fn record_rate_limited(url: &str, retry_after: Option<Duration>) {
    let Some(key) = origin_key(url) else {
        return;
    };
    let wait = retry_after.unwrap_or(HTTP_RETRY_CAP).min(HTTP_RETRY_CAP);
    let not_before = now_ms().saturating_add(wait.as_millis() as u64);
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    let st = &mut *guard;
    let entry = st.not_before.entry(key.clone()).or_insert(0);
    if *entry < not_before {
        *entry = not_before;
    }
    let Some(path) = st.path.clone() else {
        return;
    };
    let Ok(_lock) = crate::registry::lock_at(&path) else {
        return;
    };
    let mut disk = load_file(&path).unwrap_or_default();
    merge_max(&mut disk.not_before, &st.not_before);
    sanitize(&mut disk, now_ms());
    if save_file(&path, &disk).is_ok() {
        st.not_before = disk.not_before;
    }
}

/// Remaining backoff for the provider behind url, if a recorded window is
/// still open. None means "go ahead" — including when the state file is
/// missing, unreadable, or corrupt.
pub fn remaining_for_url(url: &str) -> Option<Duration> {
    let key = origin_key(url)?;
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    let st = &mut *guard;
    // Another gateway process may have recorded a window after this one
    // bound the file, so a consult reloads and merges persisted state rather
    // than trusting the startup snapshot. atomic_write keeps each read
    // complete, so a best-effort consult needs no cross-process lock.
    if let Some(path) = st.path.clone() {
        if let Ok(mut disk) = load_file(&path) {
            sanitize(&mut disk, now_ms());
            merge_max(&mut st.not_before, &disk.not_before);
        }
    }
    let not_before = st.not_before.get(&key).copied()?;
    let now = now_ms();
    let remaining = not_before.saturating_sub(now);
    (remaining > 0).then(|| Duration::from_millis(remaining).min(HTTP_RETRY_CAP))
}

#[cfg(test)]
pub fn reset_for_test() {
    *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = BackoffState {
        path: None,
        not_before: HashMap::new(),
    };
}

/// Backoff state is process-global, so tests that mutate it — here and in
/// downstream.rs's transport tests — must serialize against each other; the
/// harness runs test threads in parallel within one binary.
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Test-only: hold the backoff state lock across a whole test so a reset or
/// record from another test cannot wipe state mid-flight.
#[cfg(test)]
pub(crate) fn lock_state_for_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            loop {
                let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "toolport-downstream-backoff-{label}-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => {
                        panic!("failed to create test directory {}: {err}", path.display())
                    }
                }
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn origin_key_strips_path_query_and_fragment() {
        assert_eq!(
            origin_key("https://api.example.com/mcp?token=hunter2#frag"),
            Some("https://api.example.com".into())
        );
        assert_eq!(
            origin_key("http://localhost:8080"),
            Some("http://localhost:8080".into())
        );
        assert_eq!(origin_key("not a url"), None);
        assert_eq!(origin_key("ftp://example.com"), None);
        assert_eq!(origin_key("https://"), None);
    }

    #[test]
    fn origin_key_canonicalizes_case_and_default_port() {
        // One provider, one window, however the server entry spells the URL.
        assert_eq!(
            origin_key("HTTPS://API.Example.COM:443/mcp"),
            Some("https://api.example.com".into())
        );
        assert_eq!(
            origin_key("https://api.example.com/mcp"),
            Some("https://api.example.com".into())
        );
        assert_eq!(
            origin_key("http://Example.com:80"),
            Some("http://example.com".into())
        );
        assert_eq!(
            origin_key("http://example.com"),
            Some("http://example.com".into())
        );
        // Credentials in the URL never reach the key.
        assert_eq!(
            origin_key("https://user:secret@example.com/mcp"),
            Some("https://example.com".into())
        );
    }

    #[test]
    fn loaded_deadlines_are_capped_at_one_window() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("cap-on-load");
        let path = dir.0.join(FILE_NAME);
        let mut file = BackoffFile::default();
        file.not_before.insert(
            "https://api.example.com".into(),
            now_ms() + HTTP_RETRY_CAP.as_millis() as u64 * 10,
        );
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        bind_data_dir(&dir.0);

        let remaining = remaining_for_url("https://api.example.com").unwrap();
        assert!(
            remaining <= HTTP_RETRY_CAP,
            "a hand-edited or backward-clock deadline must not outlive one window"
        );
    }

    #[test]
    fn consult_picks_up_windows_recorded_after_bind() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("cross-process-pickup");
        let path = dir.0.join(FILE_NAME);
        bind_data_dir(&dir.0);
        assert_eq!(remaining_for_url("https://api.example.com"), None);

        // Another gateway process records a window after this one started.
        let mut disk = BackoffFile::default();
        disk.not_before
            .insert("https://api.example.com".into(), now_ms() + 2_000);
        fs::write(&path, serde_json::to_string(&disk).unwrap()).unwrap();

        let remaining = remaining_for_url("https://api.example.com").unwrap();
        assert!(remaining <= Duration::from_secs(2) && !remaining.is_zero());
    }

    #[test]
    fn record_persists_window_and_read_back() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("record-read-back");
        let path = dir.0.join(FILE_NAME);
        bind_data_dir(&dir.0);

        record_rate_limited("https://api.example.com/mcp", Some(Duration::from_secs(2)));

        let remaining = remaining_for_url("https://api.example.com/other-path").unwrap();
        assert!(remaining <= Duration::from_secs(2) && !remaining.is_zero());
        let file = load_file(&path).unwrap();
        let ts = *file.not_before.get("https://api.example.com").unwrap();
        let now = now_ms();
        assert!(
            ts > now && ts <= now + 2_000,
            "window should be ~2s from now, got {ts} vs {now}"
        );
    }

    #[test]
    fn record_caps_retry_after_at_http_retry_cap() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("cap-retry-after");
        let path = dir.0.join(FILE_NAME);
        bind_data_dir(&dir.0);

        record_rate_limited("https://api.example.com", Some(Duration::from_secs(3_600)));

        let file = load_file(&path).unwrap();
        let ts = *file.not_before.get("https://api.example.com").unwrap();
        let now = now_ms();
        assert!(
            ts > now && ts <= now + HTTP_RETRY_CAP.as_millis() as u64,
            "a hostile Retry-After must not exceed the cap"
        );
    }

    #[test]
    fn record_without_retry_after_uses_full_cap() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("default-cap");
        let path = dir.0.join(FILE_NAME);
        bind_data_dir(&dir.0);

        record_rate_limited("https://api.example.com", None);

        let file = load_file(&path).unwrap();
        let ts = *file.not_before.get("https://api.example.com").unwrap();
        let now = now_ms();
        assert!(
            ts > now && ts <= now + HTTP_RETRY_CAP.as_millis() as u64,
            "no Retry-After should default to the cap window"
        );
    }

    #[test]
    fn expired_window_means_no_backoff() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("expired");
        let path = dir.0.join(FILE_NAME);
        let mut file = BackoffFile::default();
        file.not_before
            .insert("https://api.example.com".into(), now_ms() - 1);
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        bind_data_dir(&dir.0);

        assert_eq!(remaining_for_url("https://api.example.com"), None);
    }

    #[test]
    fn missing_file_means_no_backoff() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("missing");
        bind_data_dir(&dir.0);

        assert_eq!(remaining_for_url("https://api.example.com"), None);
        assert!(!dir.0.join(FILE_NAME).exists());
    }

    #[test]
    fn corrupt_file_degrades_silently() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("corrupt");
        let path = dir.0.join(FILE_NAME);
        fs::write(&path, b"{ definitely not valid json").unwrap();

        bind_data_dir(&dir.0);

        // Reads treat corrupt state as "no backoff" without crashing...
        assert_eq!(remaining_for_url("https://api.example.com"), None);
        // ...and a later record self-heals the file instead of staying broken.
        record_rate_limited("https://api.example.com", Some(Duration::from_secs(2)));
        let file = load_file(&path).unwrap();
        assert!(file.not_before.contains_key("https://api.example.com"));
    }

    #[test]
    fn record_never_shortens_a_persisted_window() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dir = TestDir::new("keep-longest");
        let path = dir.0.join(FILE_NAME);
        let long = now_ms() + HTTP_RETRY_CAP.as_millis() as u64;
        let mut file = BackoffFile::default();
        file.not_before
            .insert("https://api.example.com".into(), long);
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        bind_data_dir(&dir.0);

        // A fresh 429 with a short Retry-After must not pull the window in.
        record_rate_limited("https://api.example.com", Some(Duration::from_secs(1)));

        let file = load_file(&path).unwrap();
        assert_eq!(
            file.not_before.get("https://api.example.com"),
            Some(&long),
            "a shorter fresh window must not shorten the persisted one"
        );
    }

    #[test]
    fn unbound_state_stays_in_memory_and_writes_no_file() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();

        record_rate_limited("https://api.example.com", Some(Duration::from_secs(2)));

        assert!(remaining_for_url("https://api.example.com").is_some());
        assert!(
            !Path::new("downstream_backoff.json").exists(),
            "unbound state must not write into the working directory"
        );
    }
}
