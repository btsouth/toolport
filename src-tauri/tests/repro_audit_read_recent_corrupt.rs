//! Repro: audit::read_recent takes `limit` lines before skipping unparseable ones,
//! so a corrupt line inside the window shrinks the returned page. inspect::read_recent
//! was already fixed to filter first; audit still takes first.

use conduit_lib::audit::{audit_path, read_recent};
use conduit_lib::registry::DataDirOverride;
use std::fs;
use std::path::PathBuf;

/// `DataDirOverride` is process-global, so tests in this binary must serialize.
static DATA_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn data_dir_guard() -> std::sync::MutexGuard<'static, ()> {
    DATA_DIR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn read_recent_skips_corrupt_lines_without_shrinking_page() {
    let dir = unique_temp_dir("audit-read-recent-corrupt");
    let _lock = data_dir_guard();
    let _override = DataDirOverride::set(&dir);

    let path = audit_path().expect("audit path under override");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create data dir");
    }

    // Four valid entries and one corrupt line near the end (newest side when reversed).
    // Requesting limit=3 should still yield 3 valid newest entries (4,3,2), not 2.
    fs::write(
        &path,
        r#"{"server":"s","tool":"t","ok":true,"i":1}
{"server":"s","tool":"t","ok":true,"i":2}
{"server":"s","tool":"t","ok":true,"i":3}
not json
{"server":"s","tool":"t","ok":true,"i":4}
"#,
    )
    .expect("write fixture audit log");

    let recent = read_recent(3).unwrap();
    assert_eq!(
        recent.len(),
        3,
        "corrupt lines must not shrink the page; got {recent:?}"
    );
    assert_eq!(recent[0].get("i").and_then(|v| v.as_u64()), Some(4));
    assert_eq!(recent[1].get("i").and_then(|v| v.as_u64()), Some(3));
    assert_eq!(recent[2].get("i").and_then(|v| v.as_u64()), Some(2));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_fully_valid_log_is_unchanged() {
    // Filtering before taking must not disturb ordering or counts on the ordinary path.
    let dir = unique_temp_dir("audit-read-recent-valid");
    let _lock = data_dir_guard();
    let _override = DataDirOverride::set(&dir);

    let path = audit_path().expect("audit path under override");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create data dir");
    }
    fs::write(
        &path,
        r#"{"i":1}
{"i":2}
{"i":3}
"#,
    )
    .expect("write fixture audit log");

    let recent = read_recent(2).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].get("i").and_then(|v| v.as_u64()), Some(3));
    assert_eq!(recent[1].get("i").and_then(|v| v.as_u64()), Some(2));

    // A limit past the end returns everything, newest first.
    let all = read_recent(99).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].get("i").and_then(|v| v.as_u64()), Some(3));

    let _ = fs::remove_dir_all(&dir);
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "toolport-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&path).expect("temp dir");
    path
}
