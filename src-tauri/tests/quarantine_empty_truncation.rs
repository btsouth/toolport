//! Repro: an empty quarantine store file must fail closed, not silently unblock.
//!
//! Pin baselines treat a present-but-empty file as a lost/corrupt baseline
//! (`PinsLoad::Corrupt`) because `atomic_write` never leaves an empty file.
//! Quarantine currently treats empty as "nothing blocked" (`Ok(empty set)`), which
//! drops every quarantine record and re-exposes tools that were deliberately held.

use serde_json::json;

/// `DataDirOverride` is process-global, so every test in this binary that resolves the
/// data dir has to serialize against every other one — otherwise a test reads a sibling's
/// scratch directory. The library's own `data_dir_test_lock` is `pub(crate)`, so an
/// integration test has to bring its own.
static DATA_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn data_dir_guard() -> std::sync::MutexGuard<'static, ()> {
    DATA_DIR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolport-q-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch data dir");
    dir
}

fn quarantine_a_destructive_tool(profile: Option<&str>) {
    let current = vec![json!({
        "name": "srv__wipe",
        "description": "Wipe everything.",
        "inputSchema": { "type": "object" },
        "annotations": { "destructiveHint": true }
    })];
    let events = vec![json!({
        "ts": 1,
        "type": "tool_drift",
        "server": "srv",
        "tool": "srv__wipe",
        "change": "changed",
        "severity": "high",
    })];
    assert!(
        conduit_lib::integrity::apply_quarantine(profile, &current, &events).unwrap(),
        "destructive change must quarantine"
    );
}

#[test]
fn empty_quarantine_file_must_not_silently_unblock() {
    let dir = scratch_dir("empty-trunc");
    let _lock = data_dir_guard();
    let _data_dir = conduit_lib::registry::DataDirOverride::set(&dir);

    let profile = Some("q-empty-trunc");
    quarantine_a_destructive_tool(profile);
    let blocked = conduit_lib::integrity::quarantined(profile).expect("readable store");
    assert!(
        blocked.contains("srv__wipe"),
        "tool must be quarantined before truncation"
    );

    // Present-but-empty is not a legitimate first-run state: a clean install has no
    // file at all, and a full release writes `{}`. Emptiness is truncation or wipe.
    let path = dir.join(format!(
        "quarantine-v2-{}.json",
        conduit_lib::registry::profile_store_key("q-empty-trunc")
    ));
    assert!(
        path.is_file(),
        "quarantine store should exist at {}",
        path.display()
    );
    std::fs::write(&path, "").expect("truncate quarantine store to empty");

    // Fail closed: same class of failure as corrupt JSON (SOU-320). Returning Ok({})
    // here permanently loses every quarantine record and unblocks the tool.
    let after = conduit_lib::integrity::quarantined(profile);
    assert!(
        after.is_err(),
        "empty quarantine file must fail closed like a corrupt store, got Ok({:?})",
        after.ok()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_quarantine_file_is_still_an_empty_set() {
    // First run has no file at all and the pin store is Fresh. That must stay a
    // legitimate empty set, or every clean install would refuse to serve tools.
    // SBS-871: missing + Loaded pins is a different case (Err, not Ok empty).
    let dir = scratch_dir("absent");
    let _lock = data_dir_guard();
    let _data_dir = conduit_lib::registry::DataDirOverride::set(&dir);

    let blocked = conduit_lib::integrity::quarantined(Some("q-absent"))
        .expect("a missing store is a fresh start, not a failure");
    assert!(blocked.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_store_also_blocks_rewrites_that_would_erase_it() {
    // `release` and `apply_quarantine` both refuse to act on a store they could not
    // load, so an empty file must not become a `{}` write that makes the loss permanent.
    let dir = scratch_dir("no-rewrite");
    let _lock = data_dir_guard();
    let _data_dir = conduit_lib::registry::DataDirOverride::set(&dir);

    let profile = Some("q-no-rewrite");
    quarantine_a_destructive_tool(profile);

    let path = dir.join(format!(
        "quarantine-v2-{}.json",
        conduit_lib::registry::profile_store_key("q-no-rewrite")
    ));
    std::fs::write(&path, "").expect("truncate quarantine store to empty");

    assert!(
        conduit_lib::integrity::release(profile, "srv__wipe").is_err(),
        "release must refuse while the store is unreadable"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("store still present"),
        "",
        "the broken store must be left for inspection, not overwritten"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
