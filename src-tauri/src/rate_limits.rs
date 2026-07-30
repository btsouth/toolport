//! Cooperative tool-call rate limits (SOU-340 / Batch B).
//!
//! Caps are authored in Teams, distributed on config pull (`rateLimits`), and enforced
//! here in the local gateway. This is cooperative (not a cloud proxy boundary): a
//! member can bypass by disconnecting from Teams. Counters are local and calendar-
//! windowed (UTC day / UTC month). They persist across restarts in the app data dir
//! so a restart mid-window does not reset the budget.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One resolved cap from the team config pull (member-scoped on the server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Cap {
    pub id: String,
    /// `day` | `month`
    pub window: String,
    pub max_calls: u64,
    /// When set, only this original tool name (or `server/tool`) counts toward the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// Parse `rateLimits` from a team config JSON blob. Unknown entries are skipped.
pub fn parse_caps(team_cfg: &Value) -> Vec<Cap> {
    let Some(arr) = team_cfg.get("rateLimits").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in arr {
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let window = v
            .get("window")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(window.as_str(), "day" | "month") {
            continue;
        }
        let max = v
            .get("maxCalls")
            .or_else(|| v.get("max_calls"))
            .and_then(Value::as_u64)
            .or_else(|| {
                v.get("maxCalls")
                    .or_else(|| v.get("max_calls"))
                    .and_then(Value::as_i64)
                    .filter(|n| *n > 0)
                    .map(|n| n as u64)
            })
            .unwrap_or(0);
        if max == 0 {
            continue;
        }
        let tool = v
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(Cap {
            id: if id.is_empty() {
                format!("anon-{}-{}-{}", window, max, tool.as_deref().unwrap_or("*"))
            } else {
                id
            },
            window,
            max_calls: max,
            tool,
        });
    }
    out
}

/// Whether `cap.tool` matches this call's original tool name (and optional server id).
fn tool_matches(cap_tool: &str, server_id: &str, orig_tool: &str) -> bool {
    if cap_tool == orig_tool {
        return true;
    }
    // `server/tool` or `server__tool` forms from admin authoring.
    let slash = format!("{server_id}/{orig_tool}");
    let dunder = format!("{server_id}__{orig_tool}");
    cap_tool == slash || cap_tool == dunder || cap_tool.ends_with(&format!("/{orig_tool}"))
}

fn utc_ymd() -> (i64, u32, u32) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    ymd_from_ms(ms)
}

fn ymd_from_ms(ms: i64) -> (i64, u32, u32) {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

fn window_key(window: &str) -> String {
    let (y, m, d) = utc_ymd();
    match window {
        "month" => format!("{y:04}-{m:02}"),
        _ => format!("{y:04}-{m:02}-{d:02}"),
    }
}

/// Counter key: `{window_kind}:{window_key}:{tool_or_*}` so day and month roll independently.
fn counter_key(window: &str, tool: Option<&str>) -> String {
    format!(
        "{}:{}:{}",
        window,
        window_key(window),
        tool.unwrap_or("*")
    )
}

#[derive(Default, Serialize, Deserialize)]
struct CounterFile {
    /// key -> call count in that window
    counts: HashMap<String, u64>,
}

struct CounterState {
    path: Option<PathBuf>,
    file: CounterFile,
}

static STATE: OnceLock<Mutex<Option<CounterState>>> = OnceLock::new();

fn state_lock() -> &'static Mutex<Option<CounterState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// Bind counters to a data directory (registry parent). Safe to call multiple times;
/// the first successful bind wins until process exit.
pub fn bind_data_dir(dir: &Path) {
    let path = dir.join("rate_limit_counters.json");
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(st) if st.path.is_some() => return,
        Some(st) => {
            let mut file = load_file(&path);
            for (key, pending) in &st.file.counts {
                let persisted = file.counts.entry(key.clone()).or_insert(0);
                *persisted = persisted.saturating_add(*pending);
            }
            st.path = Some(path);
            st.file = file;
            save_file(st);
        }
        None => {
            let file = load_file(&path);
            *guard = Some(CounterState {
                path: Some(path),
                file,
            });
        }
    }
}

fn load_file(path: &Path) -> CounterFile {
    let Ok(raw) = fs::read_to_string(path) else {
        return CounterFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_file(st: &CounterState) {
    let Some(path) = st.path.as_ref() else {
        return;
    };
    if let Ok(raw) = serde_json::to_string(&st.file) {
        let _ = fs::write(path, raw);
    }
}

/// Prune counters for windows that are no longer current (keep file small).
fn prune(file: &mut CounterFile) {
    let day = window_key("day");
    let month = window_key("month");
    file.counts.retain(|k, _| {
        if let Some(rest) = k.strip_prefix("day:") {
            rest.starts_with(&day)
        } else if let Some(rest) = k.strip_prefix("month:") {
            rest.starts_with(&month)
        } else {
            false
        }
    });
}

/// Check every applicable cap for this tool call. On allow, increments counters
/// (so a denied call does not consume budget). Returns `Err(message)` when blocked.
pub fn check_and_count(caps: &[Cap], server_id: &str, orig_tool: &str) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }
    let applicable: Vec<&Cap> = caps
        .iter()
        .filter(|c| match c.tool.as_deref() {
            None => true,
            Some(t) => tool_matches(t, server_id, orig_tool),
        })
        .collect();
    if applicable.is_empty() {
        return Ok(());
    }

    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    // In-memory fallback when bind_data_dir was never called (tests / early calls).
    if guard.is_none() {
        *guard = Some(CounterState {
            path: None,
            file: CounterFile::default(),
        });
    }
    let st = guard.as_mut().expect("just set");
    prune(&mut st.file);

    // Fail closed: if any cap is already at max, deny without incrementing.
    for cap in &applicable {
        let key = counter_key(&cap.window, cap.tool.as_deref());
        let used = st.file.counts.get(&key).copied().unwrap_or(0);
        if used >= cap.max_calls {
            let win = if cap.window == "month" {
                "this month"
            } else {
                "today"
            };
            let scope = match cap.tool.as_deref() {
                Some(t) => format!("tool `{t}`"),
                None => "all tools".into(),
            };
            return Err(format!(
                "Toolport: org rate limit reached for {scope} ({}/{} calls {win}). \
                 Ask a team admin to raise the cap, or wait for the next window.",
                used, cap.max_calls
            ));
        }
    }

    // All clear: increment every applicable counter once.
    for cap in &applicable {
        let key = counter_key(&cap.window, cap.tool.as_deref());
        *st.file.counts.entry(key).or_insert(0) += 1;
    }
    save_file(st);
    Ok(())
}

/// Current usage for tests / diagnostics.
#[cfg(test)]
pub fn peek(window: &str, tool: Option<&str>) -> u64 {
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = guard.as_mut() else {
        return 0;
    };
    let key = counter_key(window, tool);
    st.file.counts.get(&key).copied().unwrap_or(0)
}

#[cfg(test)]
pub fn reset_for_test() {
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(CounterState {
        path: None,
        file: CounterFile::default(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// Counter state is process-global; serialize tests that mutate it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static TEST_DIR_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            loop {
                let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "toolport-rate-limits-{label}-{}-{sequence}",
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

    struct UnexpectedFile {
        path: PathBuf,
        existed: bool,
    }

    impl UnexpectedFile {
        fn new(path: impl Into<PathBuf>) -> Self {
            let path = path.into();
            let existed = path.exists();
            Self { path, existed }
        }
    }

    impl Drop for UnexpectedFile {
        fn drop(&mut self) {
            if !self.existed {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    #[test]
    fn parse_caps_reads_camel_case_and_skips_bad() {
        let cfg = json!({
            "rateLimits": [
                { "id": "a", "window": "day", "maxCalls": 10 },
                { "id": "b", "window": "month", "maxCalls": 100, "tool": "list_issues" },
                { "id": "bad", "window": "hour", "maxCalls": 5 },
                { "id": "zero", "window": "day", "maxCalls": 0 },
            ]
        });
        let caps = parse_caps(&cfg);
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].max_calls, 10);
        assert_eq!(caps[1].tool.as_deref(), Some("list_issues"));
    }

    #[test]
    fn enforces_team_cap_then_blocks() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let caps = vec![Cap {
            id: "t".into(),
            window: "day".into(),
            max_calls: 2,
            tool: None,
        }];
        assert!(check_and_count(&caps, "srv", "echo").is_ok());
        assert!(check_and_count(&caps, "srv", "add").is_ok());
        let err = check_and_count(&caps, "srv", "echo").unwrap_err();
        assert!(err.contains("rate limit"), "{err}");
        assert_eq!(peek("day", None), 2);
    }

    #[test]
    fn unbound_states_do_not_write_counter_files() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fallback_artifact = UnexpectedFile::new("rate_limit_counters.json");
        let test_artifact = UnexpectedFile::new("rate_limit_counters.test.json");
        assert!(
            !fallback_artifact.existed && !test_artifact.existed,
            "counter artifact already exists in the test working directory"
        );
        let caps = vec![Cap {
            id: "t".into(),
            window: "day".into(),
            max_calls: 2,
            tool: None,
        }];

        *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = None;
        assert!(check_and_count(&caps, "srv", "echo").is_ok());
        let fallback_path = state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|st| st.path.clone());
        let fallback_wrote_file = fallback_artifact.path.exists();

        reset_for_test();
        assert!(check_and_count(&caps, "srv", "echo").is_ok());
        let test_path = state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|st| st.path.clone());
        let test_reset_wrote_file = test_artifact.path.exists();

        assert_eq!(
            (fallback_path, test_path),
            (None, None),
            "unbound counter states must not acquire filesystem paths"
        );
        assert!(
            !fallback_wrote_file,
            "the production fallback must remain in memory"
        );
        assert!(
            !test_reset_wrote_file,
            "test counter state must remain in memory"
        );
    }

    #[test]
    fn binding_before_calls_persists_across_restart() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new("bound-persistence");
        let path = dir.0.join("rate_limit_counters.json");
        let caps = vec![Cap {
            id: "day".into(),
            window: "day".into(),
            max_calls: 2,
            tool: None,
        }];

        *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = None;
        bind_data_dir(&dir.0);
        assert!(check_and_count(&caps, "srv", "echo").is_ok());

        let (bound_path, key, count) = {
            let guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
            let st = guard.as_ref().unwrap();
            assert_eq!(st.file.counts.len(), 1);
            let (key, count) = st.file.counts.iter().next().unwrap();
            (st.path.clone(), key.clone(), *count)
        };
        assert!(key.starts_with("day:"));
        assert_eq!(
            (
                bound_path.as_deref(),
                count,
                load_file(&path).counts.get(&key).copied(),
            ),
            (Some(path.as_path()), 1, Some(1)),
            "binding must set the path and persist the first call"
        );

        *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = None;
        bind_data_dir(&dir.0);
        let rebound = {
            let guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
            let st = guard.as_ref().unwrap();
            (st.path.clone(), st.file.counts.get(&key).copied())
        };
        assert_eq!(rebound, (Some(path), Some(1)));
    }

    #[test]
    fn late_binding_merges_and_persists_pending_counters() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let legacy_artifact = UnexpectedFile::new("rate_limit_counters.json");
        assert!(
            !legacy_artifact.existed,
            "counter artifact already exists in the test working directory"
        );
        let first_dir = TestDir::new("late-bind-first");
        let second_dir = TestDir::new("late-bind-second");
        let first_path = first_dir.0.join("rate_limit_counters.json");
        let second_path = second_dir.0.join("rate_limit_counters.json");
        let caps = vec![
            Cap {
                id: "day".into(),
                window: "day".into(),
                max_calls: 10,
                tool: None,
            },
            Cap {
                id: "month".into(),
                window: "month".into(),
                max_calls: u64::MAX,
                tool: None,
            },
            Cap {
                id: "echo".into(),
                window: "day".into(),
                max_calls: 10,
                tool: Some("echo".into()),
            },
        ];

        *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = None;
        assert!(check_and_count(&caps, "srv", "echo").is_ok());
        assert!(check_and_count(&caps, "srv", "echo").is_ok());
        let (day_key, month_key, echo_key, pending_counts) = {
            let guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
            let counts = &guard.as_ref().unwrap().file.counts;
            let day = counts
                .keys()
                .find(|key| key.starts_with("day:") && key.ends_with(":*"))
                .unwrap()
                .clone();
            let month = counts
                .keys()
                .find(|key| key.starts_with("month:"))
                .unwrap()
                .clone();
            let echo = counts
                .keys()
                .find(|key| key.starts_with("day:") && key.ends_with(":echo"))
                .unwrap()
                .clone();
            let observed = (
                counts.get(&day).copied(),
                counts.get(&month).copied(),
                counts.get(&echo).copied(),
            );
            (day, month, echo, observed)
        };
        assert_eq!(pending_counts, (Some(2), Some(2), Some(2)));
        assert!(
            !legacy_artifact.path.exists(),
            "unbound calls must not write the legacy fallback file"
        );

        let day_prefix = day_key.rsplit_once(':').unwrap().0;
        let persisted_only_key = format!("{day_prefix}:persisted-only");
        let mut first_file = CounterFile::default();
        first_file.counts.insert(day_key.clone(), 2);
        first_file.counts.insert(month_key.clone(), u64::MAX);
        first_file.counts.insert(persisted_only_key.clone(), 5);
        fs::write(&first_path, serde_json::to_string(&first_file).unwrap()).unwrap();

        let mut second_file = CounterFile::default();
        second_file.counts.insert(day_key.clone(), 1);
        fs::write(&second_path, serde_json::to_string(&second_file).unwrap()).unwrap();
        let second_before = fs::read(&second_path).unwrap();

        let values = |file: &CounterFile| {
            (
                file.counts.get(&day_key).copied(),
                file.counts.get(&month_key).copied(),
                file.counts.get(&echo_key).copied(),
                file.counts.get(&persisted_only_key).copied(),
            )
        };
        let state_values = || {
            let guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
            let st = guard.as_ref().unwrap();
            (st.path.clone(), values(&st.file))
        };
        let expected = (Some(4), Some(u64::MAX), Some(2), Some(5));

        bind_data_dir(&first_dir.0);
        assert_eq!(
            state_values(),
            (Some(first_path.clone()), expected),
            "late binding must merge every pending and persisted counter"
        );
        assert_eq!(
            values(&load_file(&first_path)),
            expected,
            "the merged state must be persisted immediately"
        );

        bind_data_dir(&second_dir.0);
        assert_eq!(state_values(), (Some(first_path.clone()), expected));
        assert_eq!(fs::read(&second_path).unwrap(), second_before);

        *state_lock().lock().unwrap_or_else(|e| e.into_inner()) = None;
        bind_data_dir(&first_dir.0);
        assert_eq!(state_values(), (Some(first_path), expected));
    }

    #[test]
    fn tool_scoped_cap_does_not_count_other_tools() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let caps = vec![Cap {
            id: "t".into(),
            window: "day".into(),
            max_calls: 1,
            tool: Some("list_issues".into()),
        }];
        assert!(check_and_count(&caps, "linear", "create_issue").is_ok());
        assert!(check_and_count(&caps, "linear", "create_issue").is_ok());
        assert!(check_and_count(&caps, "linear", "list_issues").is_ok());
        assert!(check_and_count(&caps, "linear", "list_issues").is_err());
    }

    #[test]
    fn tool_matches_server_slash_form() {
        assert!(tool_matches("linear/list_issues", "linear", "list_issues"));
        assert!(tool_matches("list_issues", "linear", "list_issues"));
        assert!(!tool_matches("create_issue", "linear", "list_issues"));
    }
}
