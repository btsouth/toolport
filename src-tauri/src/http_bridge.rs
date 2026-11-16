//! Shell-neutral lifecycle for Toolport's supervised HTTP gateway.
//!
//! Desktop shells own persistence and user-facing policy. This module owns the
//! child process, authenticated readiness check, and clean shutdown so every
//! shell supervises the same runtime implementation.
use crate::http_client::{RequestHeaderExt as _, ResponseResultExt as _};

use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
pub struct HttpBridge {
    pub(crate) child: Option<std::process::Child>,
    pub(crate) port: Option<u16>,
    pub(crate) token: Option<String>,
    pub(crate) proxy_mode: bool,
    /// The endpoint was left on, but restoring it at start would have had to
    /// ask to unlock the keyring for its token. It waits for an unlock or the
    /// user's Retry instead, and the shells show it as locked, not stopped.
    pub(crate) waiting_for_keyring: bool,
}

pub type HttpBridgeState = Mutex<HttpBridge>;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpBridgeStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub token: Option<String>,
    /// Left on, and waiting for the keyring to unlock before it starts.
    pub keyring_locked: bool,
}

impl HttpBridgeStatus {
    pub fn new(port: Option<u16>, token: Option<String>) -> Self {
        Self {
            running: port.is_some(),
            url: port.map(|port| format!("http://localhost:{port}")),
            port,
            token,
            keyring_locked: false,
        }
    }

    fn of(bridge: &HttpBridge) -> Self {
        let mut status = Self::new(bridge.port, bridge.token.clone());
        status.keyring_locked = !status.running && bridge.waiting_for_keyring;
        status
    }
}

/// How often a start held for the keyring checks, without asking, whether it
/// has been unlocked.
pub const KEYRING_POLL: Duration = Duration::from_secs(3);

/// At start, before the saved token is read: true when reading it, or saving
/// a new one, would ask to unlock the keyring. The state then shows as locked until the endpoint starts
/// or is turned off. A running endpoint is never held.
pub fn hold_for_keyring(state: &HttpBridgeState) -> bool {
    let running = {
        let mut bridge = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        alive(&mut bridge)
    };
    let hold = !running && crate::secrets::write_would_ask();
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .waiting_for_keyring = hold;
    hold
}

/// Show the endpoint as waiting (left on, not running yet) while a restore at
/// start is retried.
pub fn mark_waiting(state: &HttpBridgeState, waiting: bool) {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    bridge.waiting_for_keyring = waiting && !alive(&mut bridge);
}

/// One restore of a saved endpoint, without asking to unlock the keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restore {
    /// Not left on.
    Off,
    /// Running, or already was.
    Running,
    /// Held while the keyring is locked.
    Held,
    /// Left on, but it could not start (its token could not be read without
    /// asking, the keyring locked meanwhile, the port was busy).
    Failed(String),
}

/// How many times a restore at start is tried before it gives up and shows
/// the endpoint stopped. Holds for a locked keyring do not count.
pub const RESTORE_ATTEMPTS: u32 = 5;

/// Restore a saved endpoint at start and keep at it: held for the keyring,
/// it waits for an unlock; failed, it keeps showing the wait and tries again
/// after the next poll, up to [`RESTORE_ATTEMPTS`]. Turning it off ends the
/// wait. `attempt_once` must never ask to unlock the keyring.
pub fn restore_with_retries<H: Clone + Send + 'static>(
    handle: H,
    state_of: fn(&H) -> &HttpBridgeState,
    attempt_once: fn(&H) -> Restore,
    attempt: u32,
) -> Restore {
    let outcome = attempt_once(&handle);
    let next = match &outcome {
        Restore::Held => Some(attempt),
        Restore::Failed(_) if attempt < RESTORE_ATTEMPTS => {
            mark_waiting(state_of(&handle), true);
            Some(attempt + 1)
        }
        Restore::Failed(_) => {
            mark_waiting(state_of(&handle), false);
            None
        }
        // Turned off between an attempt and its wait: no wait left to show.
        Restore::Off => {
            mark_waiting(state_of(&handle), false);
            None
        }
        Restore::Running => None,
    };
    if let Some(next) = next {
        let waiting = handle.clone();
        restore_after_unlock(
            move || waiting_for_keyring(state_of(&waiting)),
            move || {
                if let Restore::Failed(error) =
                    restore_with_retries(handle, state_of, attempt_once, next)
                {
                    eprintln!("toolport: could not restore the saved HTTP endpoint: {error}");
                }
            },
        );
    }
    outcome
}

pub fn waiting_for_keyring(state: &HttpBridgeState) -> bool {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    !alive(&mut bridge) && bridge.waiting_for_keyring
}

/// Run `restore` once the keyring is unlocked (by anything: the first use of a
/// server, another app, the user), checking without asking. Gives up when
/// `still_waiting` turns false: the user started, retried or turned it off.
pub fn restore_after_unlock(
    still_waiting: impl Fn() -> bool + Send + 'static,
    restore: impl FnOnce() + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("toolport-bridge-unlock".into())
        .spawn(move || loop {
            std::thread::sleep(KEYRING_POLL);
            if !still_waiting() {
                return;
            }
            if !crate::secrets::write_would_ask() {
                restore();
                return;
            }
        });
    if let Err(error) = spawned {
        eprintln!("toolport: could not wait for the keyring to restore the HTTP endpoint: {error}");
    }
}

pub fn status(state: &HttpBridgeState) -> HttpBridgeStatus {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    alive(&mut bridge);
    HttpBridgeStatus::of(&bridge)
}

/// Reap the child if it has already exited; returns true if it is still alive.
pub fn alive(bridge: &mut HttpBridge) -> bool {
    let alive = match bridge.child.as_mut() {
        Some(child) => !matches!(child.try_wait(), Ok(Some(_))),
        None => false,
    };
    if !alive {
        bridge.child = None;
        bridge.port = None;
        bridge.token = None;
        bridge.proxy_mode = false;
    }
    alive
}

pub fn identity_ready(port: u16, token: &str) -> bool {
    use std::io::Read as _;

    let response = match crate::http_client::agent()
        .get(&format!("http://127.0.0.1:{port}/"))
        .config()
        .timeout_global(Some(Duration::from_millis(300)))
        .build()
        .set_header("Authorization", &format!("Bearer {token}"))
        .call()
        .retain_status_body()
    {
        Ok(response) if response.status().as_u16() == 200 => response,
        _ => return false,
    };
    let mut body = String::new();
    response
        .into_body()
        .into_reader()
        .take(4 * 1024)
        .read_to_string(&mut body)
        .is_ok()
        && body.starts_with("Toolport gateway (HTTP mode).")
}

pub fn start_with_token_at(
    state: &HttpBridgeState,
    port: Option<u16>,
    token: Option<String>,
) -> Result<HttpBridgeStatus, String> {
    let port = port.unwrap_or(8765);
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if alive(&mut bridge) {
        return Ok(HttpBridgeStatus::new(bridge.port, bridge.token.clone()));
    }
    if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        return Err(format!(
            "Port {port} is already in use. Stop whatever is using it, then try again."
        ));
    }
    let bin = crate::clients::resolve_gateway_path_for_launch()?
        .ok_or_else(|| "toolport-gateway binary not found next to the app".to_string())?;
    let token = match token {
        Some(token) => token,
        None => {
            let mut bytes = [0u8; 24];
            getrandom::getrandom(&mut bytes)
                .map_err(|error| format!("could not generate a token: {error}"))?;
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }
    };
    let mut command = std::process::Command::new(&bin);
    command
        .arg("--http-proxy")
        .arg(port.to_string())
        .env("TOOLPORT_HTTP_TOKEN", &token)
        .env("CONDUIT_HTTP_TOKEN", &token)
        // The proxy treats stdin EOF as the app's service lease ending.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the HTTP bridge: {error}"))?;
    let startup_timeout = 25;
    let deadline = std::time::Instant::now() + Duration::from_secs(startup_timeout);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "The HTTP endpoint exited on startup ({status}). Is port {port} already in use?"
            ));
        }
        if identity_ready(port, &token) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "The HTTP endpoint did not come up on port {port} within {startup_timeout}s."
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bridge.child = Some(child);
    bridge.port = Some(port);
    bridge.token = Some(token.clone());
    bridge.proxy_mode = true;
    bridge.waiting_for_keyring = false;
    Ok(HttpBridgeStatus::new(Some(port), Some(token)))
}

pub fn stop_with(
    bridge: &mut HttpBridge,
    kill_child: impl FnOnce(&mut std::process::Child) -> std::io::Result<()>,
) -> Result<HttpBridgeStatus, String> {
    // Turning it off ends any wait for the keyring.
    bridge.waiting_for_keyring = false;
    if let Some(mut child) = bridge.child.take() {
        let gracefully_exited = if bridge.proxy_mode {
            // Closing the parent-held pipe tells the proxy to release the
            // daemon lease and stop its public listener.
            drop(child.stdin.take());
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break true,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    _ => break false,
                }
            }
        } else {
            false
        };
        let stopped = if gracefully_exited {
            Ok(())
        } else {
            match kill_child(&mut child) {
                Ok(()) => child.wait().map(|_| ()),
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(kill_error),
                    Err(wait_error) => Err(wait_error),
                },
            }
        };
        if let Err(error) = stopped {
            bridge.child = Some(child);
            return Err(match bridge.port {
                Some(port) => format!("Toolport HTTP endpoint on port {port}: {error}"),
                None => format!("Toolport HTTP endpoint: {error}"),
            });
        }
    }
    bridge.port = None;
    bridge.token = None;
    bridge.proxy_mode = false;
    Ok(HttpBridgeStatus::new(None, None))
}

pub fn stop(state: &HttpBridgeState) -> Result<HttpBridgeStatus, String> {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_with(&mut bridge, std::process::Child::kill)
}

pub fn tracked_port_and_token(state: &HttpBridgeState) -> Option<(Option<u16>, Option<String>)> {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    alive(&mut bridge).then(|| (bridge.port, bridge.token.clone()))
}

pub fn kill_on_exit(state: &HttpBridgeState) {
    let mut bridge = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = stop_with(&mut bridge, std::process::Child::kill);
}

#[cfg(test)]
mod tests {
    #[derive(Clone)]
    struct Fixture(std::sync::Arc<(super::HttpBridgeState, std::sync::atomic::AtomicU32, u32)>);
    fn fixture_state(fixture: &Fixture) -> &super::HttpBridgeState {
        &fixture.0 .0
    }
    /// Fails its first `fixture.0.2` attempts, then runs.
    fn fixture_attempt(fixture: &Fixture) -> super::Restore {
        let tried = fixture
            .0
             .1
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if tried > fixture.0 .2 {
            super::Restore::Running
        } else {
            super::Restore::Failed("the token could not be read".into())
        }
    }

    /// Failure mode: the keyring check passed, the token read after it failed,
    /// and nothing tried again: the endpoint stayed stopped and showed neither
    /// running nor waiting.
    #[test]
    fn a_failed_restore_shows_the_wait_and_tries_again() {
        crate::secrets::tests::with_isolated_vault(|| {
            let fixture = Fixture(std::sync::Arc::new((
                super::HttpBridgeState::default(),
                std::sync::atomic::AtomicU32::new(0),
                1,
            )));
            let first =
                super::restore_with_retries(fixture.clone(), fixture_state, fixture_attempt, 1);
            assert!(matches!(first, super::Restore::Failed(_)), "{first:?}");
            assert!(
                super::status(&fixture.0 .0).keyring_locked,
                "shows the wait"
            );
            let deadline = std::time::Instant::now() + super::KEYRING_POLL * 3;
            while fixture.0 .1.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                assert!(std::time::Instant::now() < deadline, "never tried again");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }

    fn fixture_off(_: &Fixture) -> super::Restore {
        super::Restore::Off
    }

    /// Failure mode: a Stop between a failed attempt and its wait left the
    /// wait set with the setting off, so it showed "Keyring locked" for good.
    #[test]
    fn a_restore_that_finds_it_off_clears_the_wait() {
        let fixture = Fixture(std::sync::Arc::new((
            super::HttpBridgeState::default(),
            std::sync::atomic::AtomicU32::new(0),
            0,
        )));
        super::mark_waiting(&fixture.0 .0, true);
        let outcome = super::restore_with_retries(fixture.clone(), fixture_state, fixture_off, 2);
        assert_eq!(outcome, super::Restore::Off);
        assert!(!super::status(&fixture.0 .0).keyring_locked);
    }

    /// Failure mode: with no Toolport item in the store and its collection
    /// locked, the restore made a token it could not save, failed five times
    /// and stayed off after the unlock.
    #[test]
    fn a_locked_collection_holds_the_endpoint_for_the_unlock() {
        crate::secrets::tests::with_isolated_vault(|| {
            let state = super::HttpBridgeState::default();
            let prompts = crate::secrets::locked_store::write_prompts_during(|| {
                assert!(super::hold_for_keyring(&state));
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            assert!(super::status(&state).keyring_locked);
            assert!(!super::hold_for_keyring(&state), "open: nothing to hold");
        });
    }

    /// The retries are bounded: it then shows the endpoint stopped.
    #[test]
    fn a_restore_that_keeps_failing_gives_up() {
        crate::secrets::tests::with_isolated_vault(|| {
            let fixture = Fixture(std::sync::Arc::new((
                super::HttpBridgeState::default(),
                std::sync::atomic::AtomicU32::new(0),
                u32::MAX,
            )));
            super::restore_with_retries(fixture.clone(), fixture_state, fixture_attempt, 1);
            let deadline =
                std::time::Instant::now() + super::KEYRING_POLL * (super::RESTORE_ATTEMPTS + 2);
            while super::waiting_for_keyring(&fixture.0 .0) {
                assert!(std::time::Instant::now() < deadline, "kept waiting");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert_eq!(
                fixture.0 .1.load(std::sync::atomic::Ordering::SeqCst),
                super::RESTORE_ATTEMPTS
            );
            assert!(!super::status(&fixture.0 .0).keyring_locked);
        });
    }

    /// Turning a held endpoint off ends its wait: the first unlock after it
    /// never starts it.
    #[test]
    fn turning_off_a_held_endpoint_ends_its_wait_for_an_unlock() {
        crate::secrets::tests::with_isolated_vault(|| {
            let state = std::sync::Arc::new(super::HttpBridgeState::default());
            let (restored, restores) = std::sync::mpsc::channel();
            let prompts = crate::secrets::locked_store::prompts_during(|| {
                assert!(super::hold_for_keyring(&state));
                let waiting = state.clone();
                super::restore_after_unlock(
                    move || super::waiting_for_keyring(&waiting),
                    move || restored.send(()).unwrap(),
                );
                super::stop(&state).unwrap();
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            assert!(!super::status(&state).keyring_locked);
            let late = restores.recv_timeout(super::KEYRING_POLL * 2);
            assert!(late.is_err(), "restored after it was turned off");
        });
    }

    /// An endpoint held at start shows as locked, not stopped, until it is
    /// turned off, and its restore runs on the first unlock, never before.
    #[test]
    fn a_held_endpoint_shows_locked_and_restores_on_the_first_unlock() {
        crate::secrets::tests::with_isolated_vault(|| {
            let state = std::sync::Arc::new(super::HttpBridgeState::default());
            let (restored, restores) = std::sync::mpsc::channel();
            let prompts = crate::secrets::locked_store::prompts_during(|| {
                assert!(super::hold_for_keyring(&state));
                let status = super::status(&state);
                assert!(status.keyring_locked && !status.running, "{status:?}");
                let waiting = state.clone();
                super::restore_after_unlock(
                    move || super::waiting_for_keyring(&waiting),
                    move || restored.send(()).unwrap(),
                );
                // Still locked a full poll later: nothing restored.
                let early = restores.recv_timeout(super::KEYRING_POLL * 2);
                assert!(early.is_err(), "restored while the keyring was locked");
            });
            assert!(prompts.is_empty(), "{prompts:?}");
            // Unlocked: the next poll restores it.
            restores
                .recv_timeout(super::KEYRING_POLL * 3)
                .expect("restored after the unlock");

            assert!(
                !super::hold_for_keyring(&state),
                "unlocked: nothing to hold"
            );
            crate::secrets::locked_store::prompts_during(|| {
                assert!(super::hold_for_keyring(&state));
            });
            super::stop(&state).unwrap();
            let status = super::status(&state);
            assert!(!status.keyring_locked, "turned off: {status:?}");
            assert!(!super::waiting_for_keyring(&state));
        });
    }
}
