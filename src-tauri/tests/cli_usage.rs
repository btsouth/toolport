//! `toolport usage` over fixture Claude Code and Codex transcripts and a Toolport audit log.
mod cli_support;

use cli_support::{iso, now_ms, stdout, Fixture};
use serde_json::json;

fn usage(input: u64, read: u64, w5: u64, w1: u64, out: u64) -> serde_json::Value {
    json!({
        "input_tokens": input,
        "cache_read_input_tokens": read,
        "cache_creation_input_tokens": w5 + w1,
        "output_tokens": out,
        "cache_creation": {"ephemeral_5m_input_tokens": w5, "ephemeral_1h_input_tokens": w1}
    })
}

fn assistant(ts: u64, id: &str, usage: serde_json::Value, content: serde_json::Value) -> String {
    json!({
        "type": "assistant",
        "timestamp": iso(ts),
        "sessionId": "s1",
        "message": {"id": id, "model": "claude-opus-5-5", "role": "assistant", "usage": usage, "content": content}
    })
    .to_string()
}

fn tool_result(ts: u64, id: &str, content: serde_json::Value) -> String {
    json!({
        "type": "user",
        "timestamp": iso(ts),
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": content}]}
    })
    .to_string()
}

/// One Claude Code session: a Bash call with big output, the same file read twice, an MCP
/// call through the Toolport gateway, a screenshot, and a cache break inside the TTL.
fn claude_session(t0: u64) -> String {
    let png = {
        use std::io::Write as _;
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.write_all(&1200u32.to_be_bytes()).unwrap();
        b.write_all(&800u32.to_be_bytes()).unwrap();
        b.extend_from_slice(&[0u8; 32]);
        base64_encode(&b)
    };
    let lines = vec![
        // Not a usage line: ignored.
        json!({"type": "custom-title", "title": "x"}).to_string(),
        // A torn write: counted as malformed, never fatal.
        "{\"type\":\"assistant\",\"message\":{\"id\":".to_string(),
        assistant(
            t0,
            "m1",
            usage(10, 0, 30_000, 0, 50),
            json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}}]),
        ),
        // The same message again (Claude Code splits blocks across lines): counted once.
        assistant(
            t0,
            "m1",
            usage(10, 0, 30_000, 0, 80),
            json!([{"type": "text", "text": "running"}]),
        ),
        tool_result(t0 + 1000, "t1", json!("x".repeat(40_000))),
        assistant(
            t0 + 2000,
            "m2",
            usage(10, 30_000, 10_000, 0, 50),
            json!([{"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/p/src/main.rs"}}]),
        ),
        tool_result(t0 + 3000, "t2", json!("y".repeat(8_000))),
        assistant(
            t0 + 4000,
            "m3",
            usage(10, 40_000, 2_000, 0, 50),
            json!([{"type": "tool_use", "id": "t3", "name": "Read", "input": {"file_path": "/p/src/main.rs"}}]),
        ),
        tool_result(t0 + 5000, "t3", json!("y".repeat(8_000))),
        assistant(
            t0 + 6000,
            "m4",
            usage(10, 42_000, 2_000, 0, 50),
            json!([{"type": "tool_use", "id": "t4", "name": "mcp__toolport__toolport_call_tool", "input": {"name": "github__search_issues", "arguments": {}}}]),
        ),
        tool_result(
            t0 + 7000,
            "t4",
            json!([{"type": "text", "text": "z".repeat(4_000)}, {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]),
        ),
        // A cache break inside the 5 minute TTL: nothing read back, everything rewritten.
        assistant(
            t0 + 60_000,
            "m5",
            usage(10, 0, 47_000, 0, 50),
            json!([{"type": "text", "text": "done"}]),
        ),
    ];
    lines.join("\n") + "\n"
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn codex_session(t0: u64) -> String {
    let tc = |ts: u64, input: u64, cached: u64, out: u64, total: u64| {
        json!({"timestamp": iso(ts), "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0, "total_tokens": total},
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": cached, "output_tokens": out, "total_tokens": input + out}
        }}})
        .to_string()
    };
    [
        json!({"timestamp": iso(t0), "type": "session_meta", "payload": {"id": "c1"}}).to_string(),
        json!({"timestamp": iso(t0), "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}).to_string(),
        tc(t0 + 1, 20_000, 0, 100, 20_100),
        json!({"timestamp": iso(t0 + 2), "type": "response_item", "payload": {"type": "function_call", "name": "exec_command", "arguments": "{\"cmd\":\"ls\"}", "call_id": "c1"}}).to_string(),
        json!({"timestamp": iso(t0 + 3), "type": "response_item", "payload": {"type": "function_call_output", "call_id": "c1", "output": "f".repeat(12_000)}}).to_string(),
        tc(t0 + 4, 23_100, 20_000, 100, 43_300),
        // The same cumulative total again: Codex repeats token_count events.
        tc(t0 + 5, 23_100, 20_000, 100, 43_300),
    ]
    .join("\n")
        + "\n"
}

fn seeded() -> Fixture {
    let f = Fixture::new("usage");
    f.clean_machine();
    let t0 = now_ms() - 3_600_000;
    f.write(".claude/projects/-work-demo/s1.jsonl", &claude_session(t0));
    f.write(
        ".codex/sessions/2026/11/24/rollout-c1.jsonl",
        &codex_session(t0),
    );
    let mut audit = String::new();
    audit.push_str(&json!({"kind": "session", "server": "toolport", "ts": t0}).to_string());
    audit.push('\n');
    for i in 0..10 {
        audit.push_str(
            &json!({"server": "github", "tool": "search", "ok": true, "sessionId": "g1", "ts": t0 + i}).to_string(),
        );
        audit.push('\n');
    }
    // Outside a 7 day window.
    audit.push_str(&json!({"server": "old", "tool": "x", "ok": true, "ts": 1_000}).to_string());
    audit.push('\n');
    std::fs::write(f.data.join("audit.jsonl"), audit).unwrap();
    f
}

#[test]
fn usage_profiles_claude_and_codex_sessions() {
    let f = seeded();
    let r = f.json(&["usage", "--json", "--days", "7"]);
    assert_eq!(r["totals"]["sessions"], 2, "{r:#}");
    let models = r["models"].as_array().unwrap();
    let opus = models
        .iter()
        .find(|m| m["model"] == "claude-opus-5-5")
        .unwrap();
    assert_eq!(opus["turns"], 5, "message m1 counted once");
    assert_eq!(
        opus["output"],
        80 + 50 * 4,
        "the final output of a split message is kept"
    );
    let gpt = models.iter().find(|m| m["model"] == "gpt-6.1-sol").unwrap();
    assert_eq!(gpt["turns"], 2, "a repeated token_count is not a turn");
    assert_eq!(gpt["cacheRead"], 20_000);
    assert!(gpt["listPriceUsd"].is_null());
    assert!(opus["listPriceUsd"].as_f64().unwrap() > 0.0);
    assert!(r["totals"]["listPriceUsd"].as_f64().unwrap() > 0.0);

    let share = |source: &str| {
        r["resident"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["source"] == source)
            .map(|s| s["share"].as_f64().unwrap())
            .unwrap_or(0.0)
    };
    assert!(share("bash") > 0.05, "{r:#}");
    assert!(share("floor") > 0.2);
    assert!(
        share("mcp:github") > 0.0,
        "gateway calls are filed under the real server"
    );
    assert!(share("image") > 0.0);
    let total: f64 = r["resident"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["share"].as_f64().unwrap())
        .sum();
    assert!((total - 1.0).abs() < 0.02, "shares add up: {total}");

    assert_eq!(r["waste"]["duplicateReads"], 1);
    assert_eq!(r["waste"]["images"], 1);
    assert_eq!(r["cache"]["breaks"], 1, "{:#}", r["cache"]);
    let github = r["mcp"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["server"] == "github")
        .unwrap();
    assert_eq!(
        github["calls"], 10,
        "the audit count wins over the single transcript call"
    );
    assert_eq!(github["bursts"], 1);
    assert!(r["mcp"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["server"] != "old"));
    // The fixture configures `fs` in Claude Code and never calls it.
    assert!(r["unusedServers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["name"] == "fs"));
    let ids: Vec<&str> = r["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    for id in ["bash-output", "cache-breaks", "code-mode", "unused-servers"] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    assert_eq!(r["stats"]["malformedLines"], 1);
}

#[test]
fn usage_caches_summaries_and_rereads_changed_files() {
    let f = seeded();
    let first = f.json(&["usage", "--json"]);
    assert_eq!(first["stats"]["parsed"], 2);
    let second = f.json(&["usage", "--json"]);
    assert_eq!(second["stats"]["parsed"], 0);
    assert_eq!(second["stats"]["cached"], 2);
    assert_eq!(first["totals"], second["totals"]);
    assert_eq!(first["resident"], second["resident"]);

    // Appending to one transcript re-reads only that one.
    let path = f.home.join(".codex/sessions/2026/11/24/rollout-c1.jsonl");
    let mut text = f.read(&path);
    text.push_str("{\"type\":\"event_msg\"}\n");
    std::fs::write(&path, text).unwrap();
    let third = f.json(&["usage", "--json"]);
    assert_eq!(third["stats"]["parsed"], 1);
    assert_eq!(third["stats"]["cached"], 1);

    let fresh = f.json(&["usage", "--json", "--no-cache"]);
    assert_eq!(fresh["stats"]["parsed"], 2);
}

#[test]
fn usage_filters_by_client_and_handles_an_empty_machine() {
    let f = seeded();
    let codex = f.json(&["usage", "--json", "--client", "codex"]);
    assert_eq!(codex["totals"]["sessions"], 1);
    assert!(codex["models"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["model"] != "claude-opus-5-5"));
    assert_eq!(f.run(&["usage", "--client", "nope"]).status.code(), Some(1));

    let human = stdout(&f.run(&["usage"]));
    for heading in [
        "What stays in context",
        "Cache health",
        "MCP servers",
        "Recommendations",
        "list-price equivalent",
    ] {
        assert!(human.contains(heading), "{heading} missing:\n{human}");
    }
    assert!(!human.contains('\x1b'));

    let empty = Fixture::new("usage-empty");
    let out = empty.run(&["usage"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("No agent sessions found"));
}

#[test]
fn a_window_counts_the_turns_inside_it_not_whole_files() {
    let f = Fixture::new("usage-window");
    f.clean_machine();
    let now = now_ms();
    let old = now - 40 * 86_400_000;
    // One session that started 40 days ago and was resumed an hour ago: the file was
    // touched today, but only today's turn is inside a 7 day window.
    let lines = [
        assistant(old, "a1", usage(10, 0, 50_000, 0, 1_000), json!([])),
        assistant(
            old + 60_000,
            "a2",
            usage(10, 50_000, 500, 0, 1_000),
            json!([]),
        ),
        assistant(now - 3_600_000, "a3", usage(10, 0, 51_000, 0, 7), json!([])),
    ];
    f.write(".claude/projects/-work-demo/long.jsonl", &lines.join("\n"));

    let week = f.json(&["usage", "--json", "--days", "7"]);
    assert_eq!(week["totals"]["sessions"], 1, "{week:#}");
    assert_eq!(week["totals"]["turns"], 1, "{week:#}");
    assert_eq!(week["totals"]["output"], 7, "{week:#}");

    // The cached summary answers a wider window with every turn.
    let all = f.json(&["usage", "--json", "--days", "60"]);
    assert_eq!(all["totals"]["turns"], 3, "{all:#}");
    assert_eq!(all["stats"]["cached"], 1, "{all:#}");
}
