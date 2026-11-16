//! Every credential-store call client recovery makes goes through here.
//!
//! Reads use `secrets::backup_secret_status`, which never asks for an unlock.
//! Writes go through the ordinary store, which does ask on Linux when the
//! keyring is locked, so work nobody asked for (the start-up client refresh, a
//! page listing) runs inside [`noninteractive`] and refuses a write to a locked
//! keyring instead. The one place an unlock prompt is wanted is
//! [`request_unlock`], called only from a user's Retry.
use crate::secrets::BackupSecretError;
use std::cell::Cell;

thread_local! {
    static NONINTERACTIVE: Cell<usize> = const { Cell::new(0) };
}

/// Run `f` with every store write in it refused while the keyring is locked.
pub(crate) fn noninteractive<T>(f: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            NONINTERACTIVE.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    NONINTERACTIVE.with(|depth| depth.set(depth.get() + 1));
    let _leave = Leave;
    // Also the store's own gate, so any write it reaches here waits too.
    crate::secrets::without_prompts(f)
}

pub(crate) fn is_noninteractive() -> bool {
    NONINTERACTIVE.with(|depth| depth.get() > 0)
}

/// The error text a locked keyring leaves on a failed recovery or refresh.
pub(crate) fn locked_error() -> String {
    BackupSecretError::Locked.to_string()
}

/// True when `error` came from a locked keyring rather than a real failure.
/// Undo any enclosing [`noninteractive`] for `f`, a step the user asked for.
pub(crate) fn interactive<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            NONINTERACTIVE.with(|depth| depth.set(self.0));
        }
    }
    let _restore = Restore(NONINTERACTIVE.with(|depth| depth.replace(0)));
    crate::secrets::with_prompts(f)
}

pub(crate) fn is_locked_error(error: &str) -> bool {
    // The store's own refusal of a background write while it would ask.
    const STORE_WAITS: &str = "the keyring is locked; this background write waits for it";
    // Release and cleanup failures carry the Locked repair step instead.
    error.contains(&locked_error())
        || error.contains(STORE_WAITS)
        || super::recovery::state_for_instructions(error) == Some(BackupSecretError::Locked)
}

pub(crate) fn status(server: &str, key: &str) -> Result<Option<String>, BackupSecretError> {
    #[cfg(test)]
    if let Some(state) = fake::read(server, key) {
        return state;
    }
    crate::secrets::backup_secret_status(server, key)
}

pub(crate) fn read(server: &str, key: &str) -> Result<Option<String>, String> {
    #[cfg(test)]
    if let Some(state) = fake::read(server, key) {
        return state.map_err(|error| error.to_string());
    }
    crate::secrets::get_backup_secret_result(server, key)
}

pub(crate) fn write(server: &str, key: &str, value: &str) -> Result<(), String> {
    #[cfg(test)]
    if let Some(result) = fake::write(server, key, is_noninteractive()) {
        return result;
    }
    // A new item can go into a locked default collection even when no saved
    // item is locked, so ask the store's write check, not only the read one.
    if is_noninteractive() && (store_locked() || crate::secrets::write_would_ask()) {
        return Err(locked_error());
    }
    crate::secrets::set_backup_secret(server, key, value)
}

pub(crate) fn delete(server: &str, key: &str, discard_all: bool) -> Result<(), BackupSecretError> {
    #[cfg(test)]
    if let Some(result) = fake::delete(server, key) {
        return result;
    }
    crate::secrets::delete_backup_secret_copies_status(server, key, discard_all)
}

/// Whether using the store now would ask to unlock it; never asks itself.
pub(crate) fn store_locked() -> bool {
    #[cfg(test)]
    if let Some(locked) = fake::locked() {
        return locked;
    }
    crate::secrets::store_would_prompt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unlock {
    /// Nothing was locked, or the user unlocked it.
    Open,
    /// The prompt was cancelled or failed; the keyring is still locked.
    StillLocked,
}

/// Ask the desktop to unlock the keyring. Blocks while its prompt is open, so
/// call it off the UI thread, and only for an action the user chose.
pub(crate) fn request_unlock() -> Unlock {
    #[cfg(test)]
    if let Some(answer) = fake::unlock() {
        return answer;
    }
    // Trust the unlock session's own answer: a separate store check can time
    // out behind another keyring user and would call an open keyring locked.
    match secret_service::unlock() {
        Ok(()) => Unlock::Open,
        Err(()) => Unlock::StillLocked,
    }
}

/// This process's start-up client refresh put clients off for the keyring.
/// Only the start-up callers set it, after their own guards (a data-dir
/// override instance, a debug GTK build), so a later refresh never runs where
/// the start-up one would not.
static STARTUP_DEFERRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REFRESHING: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg_attr(
    not(any(feature = "desktop", feature = "gtk-desktop")),
    allow(dead_code)
)]
static WAITER: std::sync::Once = std::sync::Once::new();

fn startup_deferred() -> bool {
    #[cfg(test)]
    if let Some(deferred) = fake::startup_deferred() {
        return deferred;
    }
    STARTUP_DEFERRED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Called by the start-up refresh's callers. When it put clients off, keep
/// one waiter that finishes them once the keyring opens (by itself or by a
/// user action elsewhere), checking without asking every few seconds.
#[cfg_attr(
    not(any(feature = "desktop", feature = "gtk-desktop")),
    allow(dead_code)
)]
pub(crate) fn note_startup_deferred(waiting: bool) {
    if !waiting {
        return;
    }
    STARTUP_DEFERRED.store(true, std::sync::atomic::Ordering::SeqCst);
    WAITER.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("toolport-client-refresh".into())
            .spawn(|| {
                wait_for_open(
                    crate::http_bridge::KEYRING_POLL,
                    crate::secrets::write_would_ask,
                    || {
                        refresh_deferred();
                    },
                    startup_deferred,
                )
            });
    });
}

/// Polls until nothing is waiting; refreshes whenever the store is open.
#[cfg_attr(
    not(any(feature = "desktop", feature = "gtk-desktop")),
    allow(dead_code)
)]
pub(super) fn wait_for_open(
    poll: std::time::Duration,
    would_ask: impl Fn() -> bool,
    mut refresh: impl FnMut(),
    waiting: impl Fn() -> bool,
) {
    while waiting() {
        if !would_ask() {
            refresh();
            if !waiting() {
                break;
            }
        }
        std::thread::sleep(poll);
    }
}

/// Finish the client refreshes this process's start-up put off for the
/// keyring, if it put any off. Never asks to unlock; one run at a time.
/// True when a refresh ran.
pub(crate) fn refresh_deferred() -> bool {
    if !startup_deferred() {
        return false;
    }
    #[cfg(test)]
    if fake::record("refresh deferred client configs") {
        // A real refresh can restore other clients, which resets progress.
        super::recovery_values::reset_progress();
        return true;
    }
    let Ok(_one) = REFRESHING.try_lock() else {
        return false;
    };
    let outcome = super::refresh_stale_client_configs();
    if outcome.deferred.is_empty() {
        STARTUP_DEFERRED.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    true
}

#[cfg(target_os = "linux")]
mod secret_service {
    use dbus_secret_service::{EncryptionType, SecretService};

    /// How long a Retry waits on the unlock prompt before giving up on it.
    const PROMPT_SECONDS: u64 = 300;
    /// Service attribute the vault stores Toolport's items under.
    const SERVICE: &str = "conduit-mcp";

    /// A plain session: no secret crosses it and no DH negotiation runs, so it
    /// cannot race the encrypted sessions `secrets` serializes. The file lock
    /// still keeps it from overlapping the gateway's session setup.
    fn connect(prompt_seconds: u64) -> Option<SecretService> {
        let _across_processes = crate::registry::conduit_dir().and_then(|dir| {
            crate::registry::lock_at_for(
                &dir.join("secret-service"),
                std::time::Duration::from_millis(2500),
            )
            .ok()
        });
        SecretService::connect_with_max_prompt_timeout(EncryptionType::Plain, prompt_seconds).ok()
    }

    pub(super) fn unlock() -> Result<(), ()> {
        let service = connect(PROMPT_SECONDS).ok_or(())?;
        if let Ok(collection) = service.get_default_collection() {
            if collection.is_locked().map_err(|_| ())? {
                collection.unlock().map_err(|_| ())?;
            }
        }
        // Saved copies can also sit in another collection.
        let found = service
            .search_items(std::collections::HashMap::from([("service", SERVICE)]))
            .map_err(|_| ())?;
        if !found.locked.is_empty() {
            service
                .unlock_all(&found.locked.iter().collect::<Vec<_>>())
                .map_err(|_| ())?;
        }
        // The same session's search says whether anything is still locked.
        let after = service
            .search_items(std::collections::HashMap::from([("service", SERVICE)]))
            .map_err(|_| ())?;
        if after.locked.is_empty() {
            Ok(())
        } else {
            Err(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod secret_service {
    pub(super) fn unlock() -> Result<(), ()> {
        Ok(())
    }
}

/// A locked keyring for tests. Inside [`fake::locked_store`] reads answer
/// `Locked` without touching the store, writes fail as a locked store would
/// (and say whether they would have prompted), and every call is recorded.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Store {
        locked: bool,
        unlock_answer: Option<Unlock>,
        calls: Vec<String>,
        startup_deferred: Option<bool>,
        check_times_out: bool,
    }
    thread_local! {
        static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
    }

    fn with<T>(f: impl FnOnce(&mut Store) -> T) -> Option<T> {
        STORE.with(|slot| slot.borrow_mut().as_mut().map(f))
    }

    /// Runs `test` against a locked store; `unlock` is what the unlock prompt
    /// answers (`Open` unlocks the fake). Returns the calls made.
    pub(crate) fn locked_store(unlock: Unlock, test: impl FnOnce()) -> Vec<String> {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                STORE.with(|slot| *slot.borrow_mut() = None);
            }
        }
        STORE.with(|slot| {
            *slot.borrow_mut() = Some(Store {
                locked: true,
                unlock_answer: Some(unlock),
                calls: Vec::new(),
                startup_deferred: Some(true),
                check_times_out: false,
            })
        });
        let _clear = Clear;
        test();
        with(|store| std::mem::take(&mut store.calls)).unwrap_or_default()
    }

    /// [`locked_store`] inside `secrets::locked_store`: also returns every
    /// stored-value read that would have asked to unlock the keyring.
    pub(crate) fn locked_everywhere(test: impl FnOnce()) -> (Vec<String>, Vec<String>) {
        let mut prompts = Vec::new();
        let calls = locked_store(Unlock::StillLocked, || {
            prompts = crate::secrets::locked_store::prompts_during(test);
        });
        (calls, prompts)
    }

    pub(super) fn read(
        _server: &str,
        key: &str,
    ) -> Option<Result<Option<String>, BackupSecretError>> {
        with(|store| {
            store.calls.push(format!("read {key}"));
            store.locked.then_some(Err(BackupSecretError::Locked))
        })
        .flatten()
    }

    pub(super) fn write(_server: &str, key: &str, quiet: bool) -> Option<Result<(), String>> {
        with(|store| {
            if !store.locked {
                store.calls.push(format!("write {key}"));
                return None;
            }
            if quiet {
                store.calls.push(format!("refused write {key}"));
            } else {
                // A real write to a locked Secret Service raises the prompt.
                store.calls.push(format!("prompting write {key}"));
            }
            Some(Err(locked_error()))
        })
        .flatten()
    }

    pub(super) fn delete(_server: &str, key: &str) -> Option<Result<(), BackupSecretError>> {
        with(|store| {
            store.calls.push(format!("delete {key}"));
            store.locked.then_some(Err(BackupSecretError::Locked))
        })
        .flatten()
    }

    pub(super) fn record(call: &str) -> bool {
        with(|store| store.calls.push(call.into())).is_some()
    }

    pub(super) fn startup_deferred() -> Option<bool> {
        with(|store| store.startup_deferred).flatten()
    }

    /// Inside [`locked_store`]: whether start-up put a client refresh off
    /// (true unless a test says otherwise).
    pub(crate) fn set_startup_deferred(deferred: bool) {
        with(|store| store.startup_deferred = Some(deferred));
    }

    /// An open store for tests that need the fake's records without a lock.
    pub(crate) fn open_store(test: impl FnOnce()) -> Vec<String> {
        locked_store(Unlock::Open, || {
            with(|store| store.locked = false);
            test();
        })
    }

    /// Inside [`locked_store`]: the store check answers "would prompt" (as a
    /// timed-out check does) while reads and writes reach the real vault.
    pub(crate) fn set_check_times_out() {
        with(|store| store.check_times_out = true);
    }

    /// Inside [`locked_store`]: reads and writes reach the real vault, while
    /// the whole-store check still says locked (another collection is).
    pub(crate) fn open_but_store_check_locked() {
        with(|store| {
            store.locked = false;
            store.check_times_out = true;
        });
    }

    pub(super) fn locked() -> Option<bool> {
        with(|store| store.locked || store.check_times_out)
    }

    pub(super) fn unlock() -> Option<Unlock> {
        with(|store| {
            store.calls.push("unlock prompt".into());
            let answer = store.unlock_answer.unwrap_or(Unlock::StillLocked);
            if answer == Unlock::Open {
                store.locked = false;
            }
            answer
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release or cleanup refused by the locked keyring reports the Locked
    /// repair step, which counts as locked too (Retry then asks to unlock).
    #[test]
    fn the_locked_repair_step_counts_as_a_locked_error() {
        for discard_all in [false, true] {
            let step = super::super::recovery::next_step(BackupSecretError::Locked, discard_all);
            assert!(is_locked_error(&step), "{step}");
        }
        let other = super::super::recovery::next_step(BackupSecretError::Corrupt, false);
        assert!(!is_locked_error(&other), "{other}");
    }

    /// K2: the start-up waiter checks without asking, refreshes once the
    /// store opens, and stops when nothing is waiting any more.
    #[test]
    fn the_waiter_refreshes_once_the_store_opens_and_then_stops() {
        use std::cell::Cell;
        let checks = Cell::new(0);
        let refreshes = Cell::new(0);
        let waiting = Cell::new(true);
        wait_for_open(
            std::time::Duration::ZERO,
            || {
                checks.set(checks.get() + 1);
                checks.get() < 3
            },
            || {
                refreshes.set(refreshes.get() + 1);
                waiting.set(false);
            },
            || waiting.get(),
        );
        assert_eq!((checks.get(), refreshes.get()), (3, 1));
        // Still waiting after a refresh (the keyring locked again): keep polling.
        let refreshes = Cell::new(0);
        let left = Cell::new(2);
        wait_for_open(
            std::time::Duration::ZERO,
            || false,
            || {
                refreshes.set(refreshes.get() + 1);
                left.set(left.get() - 1);
            },
            || left.get() > 0,
        );
        assert_eq!(refreshes.get(), 2);
    }
}
