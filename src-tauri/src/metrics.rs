//! Opt-in Prometheus `/metrics` for the HTTP gateway (SOU-347).
//!
//! Off by default (`TOOLPORT_METRICS=1` / legacy `CONDUIT_METRICS`). Scrapes the
//! same local audit + savings + quarantine files the desktop dashboards use.
//! Labels are bounded to ids only (server / tool / client / ok), never args.

use std::collections::BTreeMap;

use serde_json::Value;

/// Whether `GET /metrics` is served. Default off.
pub fn metrics_enabled() -> bool {
    crate::brand::env_flag("TOOLPORT_METRICS", "CONDUIT_METRICS")
}

/// Prometheus text exposition of current local stats.
///
/// `Err` when a local stat file exists but cannot be read. The caller answers
/// non-200 so the scrape fails loudly (`up` goes 0) instead of serving a body
/// that is indistinguishable from an idle instance with every series missing
/// (SBS-873). Same contract as the desktop readers: a missing file is empty,
/// an unreadable one is an error.
pub fn render() -> Result<String, String> {
    let entries =
        crate::audit::read_all().map_err(|e| format!("couldn't read the activity log: {e}"))?;
    let savings = crate::savings::try_summary()
        .map_err(|e| format!("couldn't read the savings logs: {e}"))?;
    let tokens_saved = savings
        .get("tokensSaved")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let quarantined = crate::integrity::all_quarantined()?.len() as u64;
    Ok(render_with_savings(
        &entries,
        tokens_saved,
        quarantined,
        &savings,
    ))
}

/// Pure renderer for tests (no disk).
pub fn render_from_parts(entries: &[Value], tokens_saved: u64, quarantined_tools: u64) -> String {
    render_with_savings(
        entries,
        tokens_saved,
        quarantined_tools,
        &serde_json::json!({}),
    )
}

fn render_with_savings(
    entries: &[Value],
    tokens_saved: u64,
    quarantined_tools: u64,
    savings: &Value,
) -> String {
    // key: (server, tool, client, ok) -> (calls, error_count_if_not_ok is redundant with ok label)
    let mut calls: BTreeMap<(String, String, String, bool), u64> = BTreeMap::new();
    let mut held: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    let mut dur_sum: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut dur_count: BTreeMap<(String, String), u64> = BTreeMap::new();

    for e in entries {
        let Some(ok) = crate::audit::tool_call_ok(e) else {
            continue;
        };
        let server = e
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let tool = e
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let client = e
            .get("client")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .unwrap_or("")
            .to_string();
        *calls
            .entry((server.clone(), tool.clone(), client.clone(), ok))
            .or_insert(0) += 1;
        if e.get("held").and_then(Value::as_bool) == Some(true) {
            *held
                .entry((server.clone(), tool.clone(), client.clone()))
                .or_insert(0) += 1;
        }
        if let Some(ms) = e.get("durationMs").and_then(Value::as_u64) {
            *dur_sum.entry((server.clone(), tool.clone())).or_insert(0) += ms;
            *dur_count.entry((server, tool)).or_insert(0) += 1;
        }
    }

    let mut out = String::with_capacity(2048);
    out.push_str("# HELP toolport_tool_calls_total Tool calls in the retained local audit log\n");
    out.push_str("# TYPE toolport_tool_calls_total counter\n");
    for ((server, tool, client, ok), n) in &calls {
        out.push_str(&format!(
            "toolport_tool_calls_total{{server=\"{}\",tool=\"{}\",client=\"{}\",ok=\"{}\"}} {}\n",
            esc_label(server),
            esc_label(tool),
            esc_label(client),
            if *ok { "true" } else { "false" },
            n
        ));
    }

    out.push_str(
        "# HELP toolport_held_calls_total Destructive calls held for confirmation (retained log)\n",
    );
    out.push_str("# TYPE toolport_held_calls_total counter\n");
    for ((server, tool, client), n) in &held {
        out.push_str(&format!(
            "toolport_held_calls_total{{server=\"{}\",tool=\"{}\",client=\"{}\"}} {}\n",
            esc_label(server),
            esc_label(tool),
            esc_label(client),
            n
        ));
    }

    out.push_str(
        "# HELP toolport_tool_call_duration_milliseconds_sum Sum of recorded call durations (ms)\n",
    );
    out.push_str("# TYPE toolport_tool_call_duration_milliseconds_sum counter\n");
    for ((server, tool), sum) in &dur_sum {
        out.push_str(&format!(
            "toolport_tool_call_duration_milliseconds_sum{{server=\"{}\",tool=\"{}\"}} {}\n",
            esc_label(server),
            esc_label(tool),
            sum
        ));
    }
    out.push_str(
        "# HELP toolport_tool_call_duration_milliseconds_count Timed tool calls in the retained log\n",
    );
    out.push_str("# TYPE toolport_tool_call_duration_milliseconds_count counter\n");
    for ((server, tool), n) in &dur_count {
        out.push_str(&format!(
            "toolport_tool_call_duration_milliseconds_count{{server=\"{}\",tool=\"{}\"}} {}\n",
            esc_label(server),
            esc_label(tool),
            n
        ));
    }

    out.push_str(
        "# HELP toolport_tokens_saved_total Deprecated compatibility estimate of catalog exposure avoided (legacy plus UTF-8 bytes / 4); not provider tokens\n",
    );
    out.push_str("# TYPE toolport_tokens_saved_total counter\n");
    out.push_str(&format!("toolport_tokens_saved_total {}\n", tokens_saved));

    for (metric, help, key) in [
        (
            "toolport_tool_definition_bytes_avoided_total",
            "Exact serialized MCP tool-definition array bytes avoided by v2 catalog loads",
            "avoidedSurfaceBytes",
        ),
        (
            "toolport_tool_definition_extra_exposure_bytes_total",
            "Exact extra serialized bytes exposed when a discovery list exceeds full mode",
            "extraExposedSurfaceBytes",
        ),
        (
            "toolport_tool_definition_tokens_estimated_avoided_total",
            "Estimated token equivalent of v2 avoided bytes (UTF-8 bytes / 4)",
            "estimatedTokensAvoided",
        ),
        (
            "toolport_tool_list_loads_total",
            "Non-full tool-list loads, including legacy rows",
            "listLoads",
        ),
        (
            "toolport_discovery_response_bytes_total",
            "Exact UTF-8 text bytes returned by v2 search responses",
            "discoveryResponseBytes",
        ),
        (
            "toolport_discovery_searches_total",
            "Measured v2 discovery search responses",
            "discoveryCount",
        ),
    ] {
        out.push_str(&format!(
            "# HELP {metric} {help}\n# TYPE {metric} counter\n{metric} {}\n",
            savings.get(key).and_then(Value::as_u64).unwrap_or(0)
        ));
    }

    out.push_str(
        "# HELP toolport_quarantined_tools Tools currently quarantined after high-risk drift\n",
    );
    out.push_str("# TYPE toolport_quarantined_tools gauge\n");
    out.push_str(&format!(
        "toolport_quarantined_tools {}\n",
        quarantined_tools
    ));

    out
}

/// Escape a Prometheus label value (backslash, newline, double-quote).
fn esc_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metrics_enabled_parses_truthy() {
        // Unit path: pure parser via env is process-global; just lock the pure bits here.
        assert_eq!(esc_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(esc_label("x\ny"), r#"x\ny"#);
    }

    #[test]
    fn render_groups_calls_and_held() {
        let entries = vec![
            json!({"server":"s1","tool":"echo","ok":true,"durationMs":10,"client":"web"}),
            json!({"server":"s1","tool":"echo","ok":false,"durationMs":20,"client":"web"}),
            json!({"server":"s1","tool":"wipe","ok":true,"held":true}),
        ];
        let text = render_from_parts(&entries, 42, 1);
        assert!(text.contains(
            "toolport_tool_calls_total{server=\"s1\",tool=\"echo\",client=\"web\",ok=\"true\"} 1"
        ));
        assert!(text.contains(
            "toolport_tool_calls_total{server=\"s1\",tool=\"echo\",client=\"web\",ok=\"false\"} 1"
        ));
        assert!(
            text.contains("toolport_held_calls_total{server=\"s1\",tool=\"wipe\",client=\"\"} 1")
        );
        assert!(text.contains(
            "toolport_tool_call_duration_milliseconds_sum{server=\"s1\",tool=\"echo\"} 30"
        ));
        assert!(text.contains(
            "toolport_tool_call_duration_milliseconds_count{server=\"s1\",tool=\"echo\"} 2"
        ));
        assert!(text.contains("toolport_tokens_saved_total 42"));
        assert!(text.contains("toolport_quarantined_tools 1"));
        assert!(text.contains("# TYPE toolport_tool_calls_total counter"));
    }

    #[test]
    fn exact_byte_counters_are_separate_from_legacy_alias() {
        let summary = json!({"avoidedSurfaceBytes":1200, "estimatedTokensAvoided":300,
            "listLoads":7, "discoveryResponseBytes":90, "discoveryCount":2});
        let text = render_with_savings(&[], 400, 0, &summary);
        assert!(text.contains("toolport_tokens_saved_total 400"));
        assert!(text.contains("toolport_tool_definition_bytes_avoided_total 1200"));
        assert!(text.contains("toolport_tool_definition_tokens_estimated_avoided_total 300"));
        assert!(text.contains("toolport_tool_list_loads_total 7"));
        assert!(text.contains("toolport_discovery_response_bytes_total 90"));
        assert!(text.contains("toolport_discovery_searches_total 2"));
    }

    /// An approved HITL writes decision + timed (2x). The decision is `ok:true`
    /// on purpose; it still must not increment successful tool calls. An
    /// advisor row that omits `ok` is unknown, not success (SBS-932).
    #[test]
    fn render_skips_hitl_and_omitted_ok_rows() {
        let entries = vec![
            json!({"server":"s1","tool":"echo","ok":true,"durationMs":10,"client":"web"}),
            json!({"server":"s1","tool":"wipe","ok":true,"kind":"approval","decision":"denied","held":true,"client":"web"}),
            json!({"server":"s1","tool":"wipe","ok":true,"kind":"approval","decision":"approved","held":false,"client":"web"}),
            json!({"server":"s1","tool":"wipe","ok":true,"durationMs":15,"client":"web"}),
            json!({"server":"toolport","tool":"routine.advisor.hint_shown","kind":"routine","action":"hint_shown"}),
        ];
        let text = render_from_parts(&entries, 0, 0);
        assert!(text.contains(
            r#"toolport_tool_calls_total{server="s1",tool="echo",client="web",ok="true"} 1"#
        ));
        assert!(text.contains(
            r#"toolport_tool_calls_total{server="s1",tool="wipe",client="web",ok="true"} 1"#
        ));
        assert!(!text.contains(r#"tool="routine.advisor.hint_shown""#));
        assert!(!text.contains(r#"ok="true"} 2"#));
        // HITL deny is not a confirm-destructive hold.
        assert!(!text.contains("toolport_held_calls_total{"));
        assert!(text.contains(
            r#"toolport_tool_call_duration_milliseconds_count{server="s1",tool="echo"} 1"#
        ));
        assert!(text.contains(
            r#"toolport_tool_call_duration_milliseconds_count{server="s1",tool="wipe"} 1"#
        ));
    }

    #[test]
    fn empty_log_still_emits_gauges() {
        let text = render_from_parts(&[], 0, 0);
        assert!(text.contains("toolport_tokens_saved_total 0"));
        assert!(text.contains("toolport_quarantined_tools 0"));
        assert!(!text.contains("toolport_tool_calls_total{"));
    }

    /// Scratch data dir for the disk-backed `render()` cases.
    fn scratch_data_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "toolport-metrics-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch data dir");
        path
    }

    /// A missing audit.jsonl is a real idle instance: render the gauges and
    /// answer 200, exactly like an empty log.
    #[test]
    fn missing_audit_log_renders_an_idle_instance() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch_data_dir("missing");
        let _override = crate::registry::DataDirOverride::set(&dir);
        let text = render().expect("a missing log is an idle instance, not a scrape failure");
        assert!(text.contains("toolport_tokens_saved_total"));
        assert!(text.contains("toolport_quarantined_tools"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An existing but unreadable audit.jsonl must be a FAILED scrape, not a
    /// 200 whose body is indistinguishable from `render_from_parts(&[], 0, 0)`
    /// (SBS-873). Otherwise a persistent permission error reads as an idle
    /// instance while Prometheus `up` stays 1.
    #[test]
    fn unreadable_audit_log_fails_the_scrape() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch_data_dir("unreadable");
        let _override = crate::registry::DataDirOverride::set(&dir);
        let path = crate::audit::audit_path().expect("audit path under override");
        // IsADirectory: the log path exists but cannot be read as a file.
        std::fs::create_dir_all(&path).expect("unreadable log fixture");
        let error = render().expect_err("an unreadable log must fail the scrape");
        assert!(
            error.contains("activity log"),
            "the scrape error must name what failed: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_savings_store_fails_the_scrape_for_both_eras() {
        let _lock = crate::registry::data_dir_test_lock();
        for name in ["savings.jsonl", "savings-v2.jsonl"] {
            let dir = scratch_data_dir(name);
            let _override = crate::registry::DataDirOverride::set(&dir);
            std::fs::create_dir_all(dir.join(name)).unwrap();
            let error = render().expect_err("an unreadable savings file must fail the scrape");
            assert!(error.contains("savings logs"), "{error}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The quarantine gauge carries the same contract as the audit log: an
    /// existing store that cannot be read is a FAILED scrape, not zero.
    #[test]
    fn unreadable_quarantine_store_fails_the_scrape() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = scratch_data_dir("unreadable-quarantine");
        let _override = crate::registry::DataDirOverride::set(&dir);
        let path = crate::registry::conduit_dir()
            .expect("data dir under override")
            .join("quarantine.json");
        // IsADirectory: the store path exists but cannot be read as a file.
        std::fs::create_dir_all(&path).expect("unreadable store fixture");
        let error = render().expect_err("an unreadable quarantine store must fail the scrape");
        assert!(
            error.contains("quarantine"),
            "the scrape error must name what failed: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
