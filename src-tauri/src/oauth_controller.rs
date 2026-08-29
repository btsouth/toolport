//! Cross-process OAuth flow ownership shared by desktop shells.

use std::io::ErrorKind;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::registry;

const OAUTH_LOCK_LEASE_SECS: u64 = 180;
const OAUTH_LOCK_WAIT_SECS: u64 = 30;
pub(crate) const OAUTH_LOCK_POLL_MS: u64 = 250;

pub(crate) struct OAuthFlowLock {
    path: std::path::PathBuf,
    pub(crate) attempt_id: String,
    succeeded: bool,
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
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone)]
pub(crate) struct OAuthLockSnapshot {
    modified: SystemTime,
    content: String,
    attempt_id: Option<String>,
}

impl OAuthLockSnapshot {
    fn instance_key(&self) -> String {
        let modified = self
            .modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{modified}:{}", self.content)
    }
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
        "attempt_id={attempt_id}\npid={}\nstarted={}\nlease_secs={}\n",
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
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read oauth lock file: {error}"))?;
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

pub(crate) fn try_replace_stale_lock(
    path: &std::path::Path,
    observed: &OAuthLockSnapshot,
    contender_contents: &str,
    contender_attempt_id: &str,
) -> Result<bool, String> {
    let Some(current) = read_oauth_lock_snapshot(path)? else {
        return Ok(false);
    };
    if current.instance_key() != observed.instance_key() {
        return Ok(false);
    }
    let _ = std::fs::remove_file(oauth_completion_path(path, contender_attempt_id));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("could not rewrite stale oauth lock file: {error}"))?;
    use std::io::Write as _;
    file.write_all(contender_contents.as_bytes())
        .map_err(|error| format!("could not write oauth lock file: {error}"))?;
    file.flush()
        .map_err(|error| format!("could not flush oauth lock file: {error}"))?;
    Ok(true)
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

pub(crate) fn try_acquire_oauth_lock(
    path: &std::path::Path,
) -> Result<Option<OAuthFlowLock>, String> {
    let attempt_id = oauth_attempt_id();
    let contents = oauth_lock_contents(&attempt_id);
    let _ = std::fs::remove_file(oauth_completion_path(path, &attempt_id));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(contents.as_bytes())
                .map_err(|error| format!("could not write oauth lock file: {error}"))?;
            Ok(Some(OAuthFlowLock {
                path: path.to_path_buf(),
                attempt_id,
                succeeded: false,
            }))
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let Some(observed) = read_oauth_lock_snapshot(path)? else {
                return Ok(None);
            };
            if lock_snapshot_is_expired(&observed)
                && try_replace_stale_lock(path, &observed, &contents, &attempt_id)?
            {
                return Ok(Some(OAuthFlowLock {
                    path: path.to_path_buf(),
                    attempt_id,
                    succeeded: false,
                }));
            }
            Ok(None)
        }
        Err(error) => Err(format!("could not create oauth lock file: {error}")),
    }
}

pub(crate) fn acquire_or_wait_oauth_lock(
    server_id: &str,
    url: &str,
) -> Result<Option<OAuthFlowLock>, String> {
    acquire_or_wait_oauth_lock_at(&oauth_lock_path(server_id, url)?)
}

pub(crate) fn acquire_or_wait_oauth_lock_at(
    path: &std::path::Path,
) -> Result<Option<OAuthFlowLock>, String> {
    let mut observed_attempt_id: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(OAUTH_LOCK_WAIT_SECS);
    loop {
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

pub(crate) fn authenticate_with(
    server_id: &str,
    url: &str,
    bump_generation: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let Some(mut flow_lock) = acquire_or_wait_oauth_lock(server_id, url)? else {
        return Ok(());
    };
    let result = crate::oauth::authenticate(url)?;
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
    flow_lock.mark_succeeded();
    bump_generation().map_err(|error| stored_sign_in_token_but_reload_failed(&error))
}

pub(crate) fn stored_sign_in_token_but_reload_failed(error: &str) -> String {
    format!("The sign-in token was stored in the keychain, but {error}")
}

pub(crate) fn could_not_finish_sign_in(error: &str) -> String {
    format!("Could not finish sign-in: {error}")
}

#[cfg_attr(not(feature = "gtk-desktop"), allow(dead_code))]
pub(crate) fn authenticate(server_id: &str, url: &str) -> Result<(), String> {
    authenticate_with(server_id, url, || {
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
