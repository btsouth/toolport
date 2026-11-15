// Included in the gateway's test module. Self-contained on purpose: the same body
// runs on origin/main to capture the digests pinned below.

fn off_identity_fixture() -> Vec<Value> {
    let mut tools = crate::search_eval::catalog();
    let wide: serde_json::Map<String, Value> = (0..300)
        .map(|i| {
            (
                format!("option_{i:03}"),
                json!({"type": "string", "description": format!("Option number {i} for the wide tool.")}),
            )
        })
        .collect();
    tools.extend([
        // Over the 8 KiB describe budget, so an exact-name search gets trimmed.
        json!({
            "name": "wide__configure",
            "description": "Configure every option of the wide tool at once.",
            "inputSchema": {"type": "object", "properties": wide, "required": ["option_000"]}
        }),
        json!({
            "name": "apps__dashboard",
            "description": "Render the dashboard view.",
            "inputSchema": {"type": "object"},
            "_meta": {"ui": {"resourceUri": "ui://apps/dashboard", "visibility": ["app"]}}
        }),
        json!({
            "name": "renamed_lookup",
            "description": "Look up a record by id after an override rename.",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }),
        json!({
            "name": "weather__get_forecast",
            "description": "Get the weather forecast for a city.",
            "inputSchema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
            "outputSchema": {"type": "object", "properties": {"tempC": {"type": "number"}}, "required": ["tempC"]}
        }),
    ]);
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    tools
}

fn off_identity_calls() -> Vec<(String, Value)> {
    let intents: Vec<Value> = serde_json::from_str(include_str!(
        "../../tests/fixtures/search-eval/intents.json"
    ))
    .unwrap();
    let search = |arguments: Value| ("toolport_search_tools".to_string(), arguments);
    let mut calls: Vec<(String, Value)> = intents
        .iter()
        .map(|intent| search(json!({"query": intent["query"]})))
        .collect();
    calls.extend([
        search(json!({"query": "filesystem__read_text_file"})),
        search(json!({"query": "wide__configure"})),
        search(json!({"query": "renamed_lookup"})),
        search(json!({"query": "apps__dashboard"})),
        search(json!({"query": "weather forecast"})),
        search(json!({"query": "file", "limit": 25})),
        search(json!({"query": "", "server": "filesystem"})),
        search(json!({"query": "option", "server": "wide"})),
        ("help_filesystem".to_string(), json!({"query": "read"})),
        ("help_wide".to_string(), json!({})),
    ]);
    calls
}

fn off_identity_transcript(mode: DiscoveryMode) -> String {
    let host = dispatch_host(false);
    host.set_discovery_mode(mode);
    let reg = Registry::default();
    let tools = off_identity_fixture();
    // The test wrapper takes Lazy as a bool and reads Grouped from the host.
    let lazy = mode == DiscoveryMode::Lazy;
    let request = |req: Value| {
        let resp = handle_request(
            &host,
            &req,
            &reg,
            &Router::new(),
            &tools,
            lazy,
            None,
            &SearchGuard::default(),
            None,
            None,
        );
        serde_json::to_string(&resp).unwrap()
    };
    let mut out = vec![request(
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )];
    for (index, (name, arguments)) in off_identity_calls().into_iter().enumerate() {
        out.push(request(json!({
            "jsonrpc": "2.0", "id": index + 2, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        })));
    }
    // Cursor ids come from a process-wide counter, so they depend on which other
    // tests ran first. Everything else is compared byte for byte.
    let cursor = regex::Regex::new(r#"(\\"cursor\\":\\")r[0-9]+"#).unwrap();
    cursor
        .replace_all(&out.join("\n--\n"), "${1}rN")
        .into_owned()
}

fn off_identity_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Blocking requirement: with Code Mode off, tools/list and every search path (menus,
/// exact-name definitions trimmed by the describe budget, raised limits, server
/// listings and grouped `help_<server>`) are the bytes origin/main (95d5ca6db)
/// produced for this same test body, in every listing mode.
///
/// Updated once for this branch's own changes, both deliberate: Toolport's meta-tools
/// carry `anthropic/alwaysLoad` for Claude Code, and a shaped result's head fills the
/// budget by calculation (7044 characters here instead of 6970). Nothing else differs.
#[test]
fn code_mode_off_is_unchanged_in_every_listing_mode() {
    let _data = DataDirTestEnv::new("code-mode-off-identity");
    let digests: Vec<String> = [
        DiscoveryMode::Lazy,
        DiscoveryMode::Full,
        DiscoveryMode::Grouped,
    ]
    .into_iter()
    .map(|mode| {
        let transcript = off_identity_transcript(mode);
        assert!(!transcript.contains("ToolResult"), "{mode:?}");
        assert!(!transcript.contains("\"signature\""), "{mode:?}");
        // The wide exact-name definition is trimmed to the describe budget.
        assert!(transcript.contains(r#"\"cursor\":\"rN"#), "{mode:?}");
        off_identity_sha256(&transcript)
    })
    .collect();
    eprintln!("code mode off identity digests: {digests:?}");
    assert_eq!(
        digests,
        [
            "f82a18586c8e33a6cdd617d1f7c1ede1e6fa3906a72ecfeffa257cc1949fcf15".to_string(),
            "c42a8575699db61a6a8c5b307bc83a0228a6b2b781310f3322ea03e71248bcdc".to_string(),
            "cd64fb222adfcb9a4f212f5976cc695eb98c78a804cb572db57be6924dc3954b".to_string(),
        ],
        "Code Mode off output changed"
    );
}
