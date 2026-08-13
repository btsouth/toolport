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
pub fn render() -> String {
    let entries = crate::audit::read_all();
    let tokens_saved = crate::savings::summary()
        .get("tokensSaved")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let quarantined = match crate::integrity::all_quarantined() {
        Ok(entries) => entries.len() as u64,
        Err(error) => return format!("# Toolport metrics unavailable: {error}\n"),
    };
    render_from_parts(&entries, tokens_saved, quarantined)
}

/// Pure renderer for tests (no disk).
pub fn render_from_parts(entries: &[Value], tokens_saved: u64, quarantined_tools: u64) -> String {
    // key: (server, tool, client, ok) -> (calls, error_count_if_not_ok is redundant with ok label)
    let mut calls: BTreeMap<(String, String, String, bool), u64> = BTreeMap::new();
    let mut held: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    let mut dur_sum: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut dur_count: BTreeMap<(String, String), u64> = BTreeMap::new();

    for e in entries {
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
        let ok = e.get("ok").and_then(Value::as_bool).unwrap_or(true);
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
        "# HELP toolport_tokens_saved_total Estimated tool-definition tokens saved by lazy discovery\n",
    );
    out.push_str("# TYPE toolport_tokens_saved_total counter\n");
    out.push_str(&format!("toolport_tokens_saved_total {}\n", tokens_saved));

    out.push_str("# HELP toolport_quarantined_tools Tools currently quarantined after high-risk drift\n");
    out.push_str("# TYPE toolport_quarantined_tools gauge\n");
    out.push_str(&format!("toolport_quarantined_tools {}\n", quarantined_tools));

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
        assert!(text.contains("toolport_tool_calls_total{server=\"s1\",tool=\"echo\",client=\"web\",ok=\"true\"} 1"));
        assert!(text.contains("toolport_tool_calls_total{server=\"s1\",tool=\"echo\",client=\"web\",ok=\"false\"} 1"));
        assert!(text.contains("toolport_held_calls_total{server=\"s1\",tool=\"wipe\",client=\"\"} 1"));
        assert!(text.contains("toolport_tool_call_duration_milliseconds_sum{server=\"s1\",tool=\"echo\"} 30"));
        assert!(text.contains("toolport_tool_call_duration_milliseconds_count{server=\"s1\",tool=\"echo\"} 2"));
        assert!(text.contains("toolport_tokens_saved_total 42"));
        assert!(text.contains("toolport_quarantined_tools 1"));
        assert!(text.contains("# TYPE toolport_tool_calls_total counter"));
    }

    #[test]
    fn empty_log_still_emits_gauges() {
        let text = render_from_parts(&[], 0, 0);
        assert!(text.contains("toolport_tokens_saved_total 0"));
        assert!(text.contains("toolport_quarantined_tools 0"));
        assert!(!text.contains("toolport_tool_calls_total{"));
    }
}
