//! Lazy-discovery search traces: a bounded, local record of what the model actually
//! searched for and what the gateway handed back.
//!
//! This is the in-path answer to "how do I know lazy discovery is working, and what
//! is it costing me?" Because Toolport IS the gateway, it knows the ground truth a
//! post-hoc log reader cannot see: the returned content's exact UTF-8 byte count.
//! Token fields in older traces are bytes/4 estimates, not model usage.
//!
//! Kept lean and non-sensitive: the model-authored query is capped, and only tool
//! NAMES (never their schemas, arguments, or results) are stored. Like the audit and
//! savings logs it stays local. New traces use a versioned file and a
//! cross-process append/rotation lock; old gateways retain their legacy file.

use std::path::PathBuf;

use serde_json::{json, Value};

/// Trim the log once it passes this size. A line is a few hundred bytes and a search
/// happens per model turn, so this is a long, bounded window.
const MAX_TRACE_BYTES: u64 = 512 * 1024;
/// Recent lines kept on rotation; older lines are dropped (unlike savings, there is
/// no cumulative total to preserve, so trimming just discards the oldest traces).
const KEEP_LINES: usize = 500;
/// Cap the stored query so a pathological (model-authored) query can't bloat a line.
const MAX_QUERY_CHARS: usize = 200;
/// Cap how many matched tool names we store per trace.
const MAX_NAMES: usize = 25;

fn trace_path() -> Option<PathBuf> {
    // Same anchor as the registry/audit/savings logs, so the app and every
    // client-spawned gateway (some under MSIX virtualization) share one file.
    Some(crate::registry::conduit_dir()?.join("search-trace-v2.jsonl"))
}

fn legacy_trace_path() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("search-trace.jsonl"))
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Truncate `s` to at most `max` chars on a char boundary (never mid-codepoint).
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// One recorded search. `response_content_bytes` is exact returned text size;
/// `matched_schema_bytes` and `catalog_schema_bytes` are separately serialized
/// arrays. Legacy token fields remain bytes/4 estimates for reader compatibility.
///
/// `ranking` explains why each returned tool surfaced: one entry per result (in result
/// order) of `{ name, rank, matched, pinned, fallback }`, so the Discovery panel can
/// answer "why this tool" not just "which tools". `fallbacks` stores the uncapped
/// recovery-candidate count. `mode` is the ranker the search used (`lexical` or
/// `semantic`). These fields are additive; older traces may omit them.
#[allow(clippy::too_many_arguments)]
pub fn record_measured(
    client: Option<&str>,
    query: &str,
    server_filter: Option<&str>,
    top: &str,
    names: &[String],
    returned: usize,
    total: usize,
    fallbacks: usize,
    returned_tokens: u64,
    flat_tokens: u64,
    response_content_bytes: u64,
    matched_schema_bytes: u64,
    catalog_schema_bytes: u64,
    escalated: bool,
    ranking: &[Value],
    mode: &str,
) {
    let stored_names: Vec<&String> = names.iter().take(MAX_NAMES).collect();
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "query": cap_chars(query, MAX_QUERY_CHARS),
        "top": top,
        "names": stored_names,
        "returned": returned as u64,
        "total": total as u64,
        "fallbacks": fallbacks as u64,
        "returnedTokens": returned_tokens,
        "flatTokens": flat_tokens,
        "savedTokens": flat_tokens.saturating_sub(returned_tokens),
        "v": 2,
        "responseContentBytes": response_content_bytes,
        "matchedSchemaBytes": matched_schema_bytes,
        "catalogSchemaBytes": catalog_schema_bytes,
        "estimatedResponseTokens": crate::savings::estimated_tokens(response_content_bytes),
        "estimateMethod": crate::savings::ESTIMATE_METHOD,
        "escalated": escalated,
        "mode": mode,
    });
    if !ranking.is_empty() {
        let stored_ranking: Vec<&Value> = ranking.iter().take(MAX_NAMES).collect();
        entry["ranking"] = json!(stored_ranking);
    }
    if let Some(s) = server_filter.filter(|s| !s.trim().is_empty()) {
        entry["server"] = json!(s);
    }
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    write_line(&entry);
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn record(
    client: Option<&str>,
    query: &str,
    server_filter: Option<&str>,
    top: &str,
    names: &[String],
    returned: usize,
    total: usize,
    fallbacks: usize,
    returned_tokens: u64,
    flat_tokens: u64,
    escalated: bool,
    ranking: &[Value],
    mode: &str,
) {
    record_measured(
        client,
        query,
        server_filter,
        top,
        names,
        returned,
        total,
        fallbacks,
        returned_tokens,
        flat_tokens,
        0,
        0,
        0,
        escalated,
        ranking,
        mode,
    );
}

/// Append and rotate under the same cross-process lock as other local logs.
fn write_line(entry: &Value) {
    let Some(path) = trace_path() else {
        return;
    };
    let _ = crate::registry::append_line_locked(
        &path,
        &entry.to_string(),
        MAX_TRACE_BYTES,
        KEEP_LINES,
        None,
    );
}

/// The most recent `limit` traces, newest first.
///
/// A missing file is an empty trace log: nothing has searched yet. Any other
/// IO error is returned so a caller cannot treat an unreadable existing file as
/// "no traces" (SBS-873). Unparseable lines are skipped — a mid-write or
/// corrupt line is not an IO failure.
pub fn read_recent(limit: usize) -> std::io::Result<Vec<Value>> {
    let mut rows = Vec::new();
    for path in [legacy_trace_path(), trace_path()].into_iter().flatten() {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        rows.extend(
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok()),
        );
    }
    let mut indexed: Vec<(usize, Value)> = rows.into_iter().enumerate().collect();
    indexed.sort_by(|(ia, a), (ib, b)| {
        b.get("ts")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .cmp(&a.get("ts").and_then(Value::as_u64).unwrap_or(0))
            .then_with(|| ib.cmp(ia))
    });
    Ok(indexed
        .into_iter()
        .take(limit)
        .map(|(_, row)| row)
        .collect())
}

/// Delete the trace log. Returns `Err` only on a real removal failure; a missing file
/// (nothing to clear) is success, so a caller can honestly confirm it is gone.
pub fn try_clear() -> std::io::Result<()> {
    let mut first_error = None;
    for path in [legacy_trace_path(), trace_path()].into_iter().flatten() {
        let _lock = match crate::registry::lock_at(&path) {
            Ok(lock) => lock,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(std::io::Error::other(error));
                }
                continue;
            }
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Fire-and-forget clear (called when the user clears it from Activity, and from the
/// test resets that don't surface the outcome).
pub fn clear() {
    let _ = try_clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_read_returns_newest_first() {
        let _data_dir = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("record");
        clear();
        record(
            Some("cursor"),
            "list products",
            None,
            "stripe__list",
            &["stripe__list".into()],
            1,
            3,
            0,
            120,
            5000,
            false,
            &[],
            "lexical",
        );
        record(
            None,
            "send email",
            Some("resend"),
            "resend__send",
            &["resend__send".into()],
            1,
            1,
            0,
            90,
            5000,
            false,
            &[],
            "lexical",
        );
        let recent = read_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        // Newest first.
        assert_eq!(recent[0]["query"], "send email");
        assert_eq!(recent[0]["server"], "resend");
        assert_eq!(recent[0]["savedTokens"], 5000 - 90);
        assert_eq!(recent[1]["query"], "list products");
        assert_eq!(recent[1]["client"], "cursor");
        assert_eq!(recent[1]["total"], 3);
        clear();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ranking_and_mode_are_recorded_and_capped() {
        let _data_dir = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("ranking");
        clear();
        // Ranking with more than MAX_NAMES entries; only MAX_NAMES are stored.
        let ranking: Vec<Value> = (0..40)
            .map(|i| {
                json!({ "name": format!("srv__t{i}"), "rank": i + 1, "matched": ["list (name)"], "pinned": false })
            })
            .collect();
        record(
            None,
            "list",
            None,
            "srv__t0",
            &["srv__t0".into()],
            40,
            40,
            30,
            100,
            200,
            false,
            &ranking,
            "semantic",
        );
        let recent = read_recent(1).unwrap();
        let e = &recent[0];
        assert_eq!(e["mode"], "semantic");
        assert_eq!(e["fallbacks"], 30);
        let r = e["ranking"].as_array().unwrap();
        assert_eq!(r.len(), MAX_NAMES);
        assert_eq!(r[0]["name"], "srv__t0");
        assert_eq!(r[0]["rank"], 1);
        assert_eq!(r[0]["matched"][0], "list (name)");
        // Empty ranking is omitted entirely (older-style call), but mode still records.
        clear();
        record(
            None,
            "x",
            None,
            "",
            &[],
            0,
            0,
            0,
            0,
            5000,
            false,
            &[],
            "lexical",
        );
        let recent = read_recent(1).unwrap();
        let e = &recent[0];
        assert_eq!(e["mode"], "lexical");
        assert!(e.get("ranking").is_none());
        clear();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn query_is_capped_and_names_are_limited() {
        let _data_dir = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("capped");
        clear();
        let long_q = "x".repeat(500);
        let many: Vec<String> = (0..40).map(|i| format!("srv__t{i}")).collect();
        record(
            None,
            &long_q,
            None,
            "srv__t0",
            &many,
            40,
            40,
            0,
            100,
            200,
            false,
            &[],
            "lexical",
        );
        let recent = read_recent(1).unwrap();
        let e = &recent[0];
        let stored_q = e["query"].as_str().unwrap();
        // Capped to MAX_QUERY_CHARS (+ the ellipsis), never the full 500.
        assert!(
            stored_q.chars().count() <= MAX_QUERY_CHARS + 1,
            "query not capped: {}",
            stored_q.chars().count()
        );
        assert_eq!(e["names"].as_array().unwrap().len(), MAX_NAMES);
        clear();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_match_search_is_still_recorded() {
        let _data_dir = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("no-match");
        clear();
        record(
            None,
            "nonexistent capability",
            None,
            "",
            &[],
            0,
            0,
            0,
            0,
            5000,
            false,
            &[],
            "lexical",
        );
        let recent = read_recent(1).unwrap();
        let e = &recent[0];
        assert_eq!(e["returned"], 0);
        assert_eq!(e["top"], "");
        // A miss still shows what a full catalog would have cost.
        assert_eq!(e["flatTokens"], 5000);
        clear();
        let _ = std::fs::remove_dir_all(root);
    }

    fn isolated_data_dir(label: &str) -> (crate::registry::DataDirOverride, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "toolport-searchtrace-read-{label}-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch data dir");
        (crate::registry::DataDirOverride::set(&path), path)
    }

    /// Missing legacy and v2 files are an empty trace log, not a load failure.
    #[test]
    fn read_recent_missing_file_is_ok_empty() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("missing");
        let path = trace_path().expect("trace path under override");
        assert!(!path.exists(), "fixture must not create the log");
        let entries = read_recent(10).expect("missing file is Ok empty");
        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A readable JSONL returns newest-first parsed rows.
    #[test]
    fn read_recent_readable_jsonl_is_newest_first() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("readable");
        let path = trace_path().expect("trace path under override");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "{\"i\":1}\n{\"i\":2}\n").unwrap();
        let entries = read_recent(10).expect("readable fixture");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["i"], 2);
        assert_eq!(entries[1]["i"], 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn old_trace_rotation_cannot_replace_new_measured_traces() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("mixed-versions");
        let legacy = legacy_trace_path().unwrap();
        std::fs::write(&legacy, "{\"ts\":1,\"query\":\"old\"}\n").unwrap();
        record_measured(
            None,
            "new",
            None,
            "",
            &[],
            0,
            0,
            0,
            0,
            0,
            42,
            2,
            2,
            false,
            &[],
            "lexical",
        );
        crate::registry::atomic_write(&legacy, "{\"ts\":1,\"query\":\"old\"}\n").unwrap();
        let rows = read_recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["query"], "new");
        assert_eq!(rows[1]["query"], "old");
        try_clear().unwrap();
        assert!(!trace_path().unwrap().exists());
        assert!(!legacy.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An existing but unreadable search-trace.jsonl must not look like "no
    /// traces" (SBS-873).
    #[test]
    fn read_recent_unreadable_existing_path_is_err() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("unreadable");
        let path = trace_path().expect("trace path under override");
        std::fs::create_dir_all(&path).unwrap();
        let err = read_recent(10).expect_err("unreadable existing path must be Err");
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(root);
    }
}
