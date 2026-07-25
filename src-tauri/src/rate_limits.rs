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
    path: PathBuf,
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
    if guard.is_some() {
        return;
    }
    let file = load_file(&path);
    *guard = Some(CounterState { path, file });
}

fn load_file(path: &Path) -> CounterFile {
    let Ok(raw) = fs::read_to_string(path) else {
        return CounterFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_file(st: &CounterState) {
    if let Ok(raw) = serde_json::to_string(&st.file) {
        let _ = fs::write(&st.path, raw);
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
            path: PathBuf::from("rate_limit_counters.json"),
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
        path: PathBuf::from("rate_limit_counters.test.json"),
        file: CounterFile::default(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Counter state is process-global; serialize tests that mutate it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

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
