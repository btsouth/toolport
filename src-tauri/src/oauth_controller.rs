//! Cross-process OAuth flow ownership shared by desktop shells.

use std::io::ErrorKind;
use std::sync::{Arc, Mutex, OnceLock};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use fs2::FileExt;

use crate::registry;

const OAUTH_LOCK_LEASE_SECS: u64 = 180;
const OAUTH_LOCK_WAIT_SECS: u64 = 30;
pub(crate) const OAUTH_LOCK_POLL_MS: u64 = 250;

pub(crate) struct OAuthFlowLock {
    path: std::path::PathBuf,
    pub(crate) attempt_id: String,
    succeeded: bool,
    // Never unlink the advisory-lock inode: the OS releases it even on hard quit.
    _owner: std::fs::File,
}

impl OAuthFlowLock {
    pub(crate) fn mark_succeeded(&mut self) {
        self.succeeded = true;
    }
}

impl Drop for OAuthFlowLock {
    fn drop(&mut self) {
        let completion = oauth_completion_path(&self.path, &self.attempt_id);
        let status = if self.succeeded { "ok" } else { "failed" };
        let _ = registry::atomic_write(
            &completion,
            &format!(
                "status={status}\ndone={}\npid={}\n",
                now_unix_secs(),
                std::process::id()
            ),
        );
        // An older lease holder must never remove a newer attempt's metadata.
        if read_oauth_lock_snapshot(&self.path)
            .ok()
            .flatten()
            .and_then(|snapshot| snapshot.attempt_id)
            .as_deref()
            == Some(&self.attempt_id)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone)]
pub(crate) struct OAuthLockSnapshot {
    modified: SystemTime,
    content: String,
    attempt_id: Option<String>,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn oauth_attempt_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

pub(crate) fn oauth_completion_path(
    path: &std::path::Path,
    attempt_id: &str,
) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("oauth.lock");
    path.with_file_name(format!("{name}.{attempt_id}.done"))
}

pub(crate) fn oauth_lock_contents(attempt_id: &str) -> String {
    format!(
        "attempt_id={attempt_id}\npid={}\nstarted={}\nlease_secs={}\nownership=os-lock-v1\n",
        std::process::id(),
        now_unix_secs(),
        OAUTH_LOCK_LEASE_SECS
    )
}

fn parse_lock_attempt_id(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.strip_prefix("attempt_id=")
            .or_else(|| line.strip_prefix("nonce="))
            .map(ToOwned::to_owned)
    })
}

pub(crate) fn read_oauth_lock_snapshot(
    path: &std::path::Path,
) -> Result<Option<OAuthLockSnapshot>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not stat oauth lock file: {error}")),
    };
    let modified = metadata
        .modified()
        .map_err(|error| format!("could not read oauth lock timestamp: {error}"))?;
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        // The owner finished and removed the lock between the stat and the read.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read oauth lock file: {error}")),
    };
    let attempt_id = parse_lock_attempt_id(&content);
    Ok(Some(OAuthLockSnapshot {
        modified,
        content,
        attempt_id,
    }))
}

fn lock_snapshot_is_expired(snapshot: &OAuthLockSnapshot) -> bool {
    snapshot
        .modified
        .elapsed()
        .is_ok_and(|elapsed| elapsed.as_secs() >= OAUTH_LOCK_LEASE_SECS)
}

#[cfg(all(test, feature = "desktop"))]
pub(crate) fn completion_exists(path: &std::path::Path, attempt_id: &str) -> bool {
    oauth_completion_path(path, attempt_id).exists()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OAuthCompletion {
    Succeeded,
    Failed,
}

pub(crate) fn read_oauth_completion(
    path: &std::path::Path,
    attempt_id: &str,
) -> Option<OAuthCompletion> {
    let content = std::fs::read_to_string(oauth_completion_path(path, attempt_id)).ok()?;
    if content.lines().any(|line| line.trim() == "status=failed") {
        Some(OAuthCompletion::Failed)
    } else if content.lines().any(|line| line.trim() == "status=ok") || content.contains("done=") {
        Some(OAuthCompletion::Succeeded)
    } else {
        None
    }
}

pub(crate) fn oauth_waiter_outcome(
    path: &std::path::Path,
    attempt_id: &str,
) -> Option<Result<(), String>> {
    match read_oauth_completion(path, attempt_id)? {
        OAuthCompletion::Succeeded => Some(Ok(())),
        OAuthCompletion::Failed => Some(Err(
            "another Toolport process failed to complete OAuth for this server".into(),
        )),
    }
}

pub(crate) fn oauth_lock_key(server_id: &str, url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(server_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn oauth_lock_path(server_id: &str, url: &str) -> Result<std::path::PathBuf, String> {
    let directory = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    let locks = directory.join("oauth-locks");
    std::fs::create_dir_all(&locks)
        .map_err(|error| format!("could not create oauth lock directory: {error}"))?;
    Ok(locks.join(format!("{}.lock", oauth_lock_key(server_id, url))))
}

/// A Windows process object may outlive the process while another handle is
/// open. Legacy lease recovery needs execution liveness, not object existence.
#[cfg(windows)]
fn legacy_owner_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE,
    };
    unsafe {
        let process = OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            // Only a missing PID proves the owner gone; access denial remains
            // conservative so a live legacy owner is never displaced.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        // Zero timeout never waits. A terminated process is signaled; timeout
        // or an unexpected query failure still counts as potentially running.
        let running = WaitForSingleObject(process, 0) != WAIT_OBJECT_0;
        CloseHandle(process);
        running
    }
}

#[cfg(not(windows))]
fn legacy_owner_is_running(pid: u32) -> bool {
    crate::gateway_publish::pid_is_running(pid)
}

pub(crate) fn try_acquire_oauth_lock(
    path: &std::path::Path,
) -> Result<Option<OAuthFlowLock>, String> {
    let owner = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("owner"))
        .map_err(|error| format!("could not open oauth ownership lock: {error}"))?;
    // Windows reports ERROR_LOCK_VIOLATION rather than WouldBlock.
    match owner.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            return Ok(None)
        }
        Err(error) => return Err(format!("could not lock oauth ownership: {error}")),
    }
    // Compatibility with older shells that only write a lease. For new shells,
    // ownership of the OS lock proves that no previous holder can still commit.
    if let Some(snapshot) = read_oauth_lock_snapshot(path)? {
        let os_owned = snapshot
            .content
            .lines()
            .any(|line| line == "ownership=os-lock-v1");
        let owner_running = snapshot
            .content
            .lines()
            .find_map(|line| line.strip_prefix("pid="))
            .and_then(|pid| pid.parse::<u32>().ok())
            .map(legacy_owner_is_running);
        if !os_owned
            && (owner_running == Some(true)
                || (owner_running.is_none() && !lock_snapshot_is_expired(&snapshot)))
        {
            return Ok(None);
        }
    }
    let attempt_id = oauth_attempt_id();
    registry::atomic_write(path, &oauth_lock_contents(&attempt_id))
        .map_err(|error| format!("could not write oauth lock file: {error}"))?;
    Ok(Some(OAuthFlowLock {
        path: path.to_path_buf(),
        attempt_id,
        succeeded: false,
        _owner: owner,
    }))
}

#[cfg(all(test, feature = "desktop"))]
pub(crate) fn acquire_or_wait_oauth_lock_at(
    path: &std::path::Path,
) -> Result<Option<OAuthFlowLock>, String> {
    acquire_or_wait_oauth_lock_cancellable(path, &crate::oauth::Cancellation::default())
}

fn acquire_or_wait_oauth_lock_cancellable(
    path: &std::path::Path,
    cancellation: &crate::oauth::Cancellation,
) -> Result<Option<OAuthFlowLock>, String> {
    let mut observed_attempt_id: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(OAUTH_LOCK_WAIT_SECS);
    loop {
        cancellation.check()?;
        if let Some(lock) = try_acquire_oauth_lock(path)? {
            if let Some(attempt_id) = &observed_attempt_id {
                if let Some(outcome) = oauth_waiter_outcome(path, attempt_id) {
                    drop(lock);
                    return outcome.map(|()| None);
                }
            }
            return Ok(Some(lock));
        }
        if let Some(snapshot) = read_oauth_lock_snapshot(path)? {
            if let Some(attempt_id) = snapshot.attempt_id {
                observed_attempt_id = Some(attempt_id);
            }
        }
        if let Some(attempt_id) = &observed_attempt_id {
            if let Some(outcome) = oauth_waiter_outcome(path, attempt_id) {
                return outcome.map(|()| None);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "another Toolport process is already running OAuth for this server; timed out waiting for it to finish"
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(OAUTH_LOCK_POLL_MS));
    }
}

/// A shell owns an opaque attempt id before dispatching blocking work, so closing
/// a panel can cancel even if its worker has not started yet.
#[derive(Default)]
struct Attempt {
    cancellation: crate::oauth::Cancellation,
    flow_lock: Mutex<Option<OAuthFlowLock>>,
    started: std::sync::atomic::AtomicBool,
    committed: std::sync::atomic::AtomicBool,
}

fn attempts() -> &'static Mutex<std::collections::HashMap<String, Arc<Attempt>>> {
    static ATTEMPTS: OnceLock<Mutex<std::collections::HashMap<String, Arc<Attempt>>>> =
        OnceLock::new();
    ATTEMPTS.get_or_init(Mutex::default)
}

pub(crate) fn start_attempt() -> String {
    let id = oauth_attempt_id();
    attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id.clone(), Arc::new(Attempt::default()));
    id
}

/// Mark cancellation on the shell/IPC thread before scheduling cleanup. This is
/// nonblocking even if a keychain commit already owns the cancellation gate.
pub(crate) fn request_cancel_attempt(attempt_id: &str) {
    if let Some(attempt) = attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(attempt_id)
    {
        attempt.cancellation.request_cancel();
    }
}

pub(crate) fn cancel_attempt(attempt_id: &str) -> bool {
    let attempt = attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(attempt_id);
    if let Some(attempt) = attempt {
        attempt.cancellation.cancel();
        // Release cross-process ownership immediately, even during slow discovery
        // or token exchange. The cancelled worker can no longer launch or store.
        attempt
            .flow_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        return !attempt.committed.load(std::sync::atomic::Ordering::SeqCst);
    }
    false
}

pub(crate) fn cancel_all_attempts() {
    let ids: Vec<_> = attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .cloned()
        .collect();
    for id in &ids {
        request_cancel_attempt(id);
    }
    for id in ids {
        cancel_attempt(&id);
    }
}

struct FinishAttempt(String);
impl Drop for FinishAttempt {
    fn drop(&mut self) {
        cancel_attempt(&self.0);
    }
}

pub(crate) fn authenticate_with(
    server_id: &str,
    url: &str,
    attempt_id: &str,
    bump_generation: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let attempt = attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(attempt_id)
        .cloned()
        .ok_or("Browser sign-in cancelled. Try again.")?;
    if attempt
        .started
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Err("Browser sign-in attempt already started.".into());
    }
    let _finish = FinishAttempt(attempt_id.to_string());
    run_attempt(
        &attempt,
        &oauth_lock_path(server_id, url)?,
        || crate::oauth::authenticate_cancellable(url, None, &attempt.cancellation),
        |result| {
            let _mutation = crate::registry_controller::acquire_auth_lock(server_id)?;
            crate::remote::store_oauth_state(
                server_id,
                Some(result.issuer),
                &result.token_endpoint,
                &result.client_id,
                result.refresh_token,
                Some(url.to_string()),
                result.scope,
                result.issued_at,
                result.expires_at,
            )
            .map_err(|error| could_not_finish_sign_in(&error))?;
            crate::secrets::set_secret(
                server_id,
                crate::secrets::HTTP_AUTH_KEY,
                &result.access_token,
            )
            .map_err(|error| crate::registry_controller::could_not_store_token(&error))?;
            Ok(())
        },
        bump_generation,
    )
}

/// The same cancellation gate protects storing refresh metadata and access tokens.
/// If commit wins the race, cancellation waits for it; if cancellation wins,
/// neither write is allowed. Tests inject simulated authorization and storage.
fn run_attempt<T>(
    attempt: &Attempt,
    path: &std::path::Path,
    authorize: impl FnOnce() -> Result<T, String>,
    store: impl FnOnce(T) -> Result<(), String>,
    bump_generation: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let flow_lock = acquire_or_wait_oauth_lock_cancellable(path, &attempt.cancellation)?;
    let Some(flow_lock) = flow_lock else {
        return attempt.cancellation.check();
    };
    attempt.cancellation.with_active(|| {
        *attempt
            .flow_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(flow_lock);
        Ok(())
    })?;
    let result = authorize()?;
    attempt.cancellation.with_active(|| {
        store(result)?;
        attempt
            .committed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(lock) = attempt
            .flow_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            lock.mark_succeeded();
        }
        bump_generation().map_err(|error| stored_sign_in_token_but_reload_failed(&error))
    })
}

pub(crate) fn stored_sign_in_token_but_reload_failed(error: &str) -> String {
    format!("The sign-in token was stored in the keychain, but {error}")
}

pub(crate) fn could_not_finish_sign_in(error: &str) -> String {
    format!("Could not finish sign-in: {error}")
}

#[cfg_attr(not(feature = "gtk-desktop"), allow(dead_code))]
pub(crate) fn authenticate(server_id: &str, url: &str, attempt_id: &str) -> Result<(), String> {
    authenticate_with(server_id, url, attempt_id, || {
        registry::update(|registry| {
            registry.secrets_generation = registry.secrets_generation.wrapping_add(1);
            Ok(())
        })
        .map(|_| ())
        .map_err(|error| {
            format!("could not reload the running gateway after the secret change: {error}")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("toolport-oauth-test-{}", oauth_attempt_id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    // Simulated authorization/storage. Exercises the production commit gate and
    // OS lock without launching a browser, using a provider, or touching a vault.
    #[test]
    fn cancelled_authorization_cannot_store_or_replace_a_successful_retry() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let id = start_attempt();
        let old = attempts().lock().unwrap().get(&id).unwrap().clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let stored = Arc::new(Mutex::new(Vec::new()));
        let old_stored = stored.clone();
        let old_path = path.clone();
        let worker = std::thread::spawn(move || {
            run_attempt(
                &old,
                &old_path,
                || {
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok("late-old-token")
                },
                |token| {
                    old_stored.lock().unwrap().push(token);
                    Ok(())
                },
                || Ok(()),
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cancel_attempt(&id);
        let retry = Attempt::default();
        let mut reloaded = false;
        run_attempt(
            &retry,
            &path,
            || Ok("new-token"),
            |token| {
                stored.lock().unwrap().push(token);
                Ok(())
            },
            || {
                reloaded = true;
                Ok(())
            },
        )
        .unwrap();
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        assert_eq!(*stored.lock().unwrap(), vec!["new-token"]);
        assert!(reloaded);
        // The retired attempt must not remove the newer owner's lock.
        assert!(try_acquire_oauth_lock(&path).unwrap().is_none());
        let retry_id = retry
            .flow_lock
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .attempt_id
            .clone();
        drop(retry);
        assert_eq!(
            read_oauth_completion(&path, &retry_id),
            Some(OAuthCompletion::Succeeded)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn abandoned_authorization_records_failure_and_allows_retry() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let attempt = Attempt::default();
        let result = run_attempt(
            &attempt,
            &path,
            || Err::<(), _>("browser callback timed out".into()),
            |_| panic!("must not store abandoned authorization"),
            || panic!("must not reload"),
        );
        assert!(result.unwrap_err().contains("timed out"));
        let id = attempt
            .flow_lock
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .attempt_id
            .clone();
        drop(attempt);
        assert_eq!(
            read_oauth_completion(&path, &id),
            Some(OAuthCompletion::Failed)
        );
        assert!(try_acquire_oauth_lock(&path).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancellation_reports_when_credentials_already_committed() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let id = start_attempt();
        let attempt = attempts().lock().unwrap().get(&id).unwrap().clone();
        run_attempt(&attempt, &path, || Ok(()), |_| Ok(()), || Ok(())).unwrap();
        assert!(!cancel_attempt(&id));
        assert!(try_acquire_oauth_lock(&path).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancellation_before_worker_dispatch_is_terminal() {
        let id = start_attempt();
        cancel_attempt(&id);
        let error = authenticate_with("unused", "https://example.invalid", &id, || {
            panic!("must not reload")
        })
        .unwrap_err();
        assert!(error.contains("cancelled"));
    }

    #[test]
    fn cancellation_interrupts_cross_process_waiting() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let owner = try_acquire_oauth_lock(&path).unwrap().unwrap();
        let cancellation = Arc::new(crate::oauth::Cancellation::default());
        let signal = cancellation.clone();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            acquire_or_wait_oauth_lock_cancellable(&worker_path, &signal).map(|_| ())
        });
        cancellation.cancel();
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // A separate OS process owns the real file lock. Parent kills and waits for
    // it to exit, leaving a fresh lease file, then immediately starts a retry.
    #[test]
    fn real_process_exit_releases_ownership_without_waiting_for_lease() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "oauth_controller::tests::oauth_lock_child_process",
                "--ignored",
            ])
            .env("TOOLPORT_TEST_OAUTH_LOCK", &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !path.with_extension("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !path.with_extension("ready").exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not acquire ownership");
        }
        assert!(try_acquire_oauth_lock(&path).unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        // Keep Child's handle open: on Windows the exited process object can
        // still be opened by PID. The legacy-owner check must detect its exit.
        let snapshot = read_oauth_lock_snapshot(&path).unwrap().unwrap();
        assert!(!lock_snapshot_is_expired(&snapshot));
        let retry = try_acquire_oauth_lock(&path)
            .unwrap()
            .expect("retry immediately after real quit");
        drop(retry);
        std::fs::write(
            &path,
            snapshot.content.replace("ownership=os-lock-v1\n", ""),
        )
        .unwrap();
        assert!(
            try_acquire_oauth_lock(&path).unwrap().is_some(),
            "dead legacy owner also recovers immediately"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "helper subprocess for real_process_exit_releases_ownership_without_waiting_for_lease"]
    fn oauth_lock_child_process() {
        let Some(path) = std::env::var_os("TOOLPORT_TEST_OAUTH_LOCK") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let _lock = try_acquire_oauth_lock(&path).unwrap().unwrap();
        std::fs::write(path.with_extension("ready"), "ready").unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn expired_metadata_cannot_replace_an_os_lock_owner() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let owner = try_acquire_oauth_lock(&path).unwrap().unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .unwrap();
        assert!(try_acquire_oauth_lock(&path).unwrap().is_none());
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_lease_of_a_live_process_is_preserved() {
        let dir = scratch();
        let path = dir.join("oauth.lock");
        let contents = oauth_lock_contents("legacy").replace("ownership=os-lock-v1\n", "");
        std::fs::write(&path, contents).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .unwrap();
        assert!(try_acquire_oauth_lock(&path).unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
