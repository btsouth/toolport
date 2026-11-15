// Included in the gateway's test module: Code Mode signatures on search hits and
// `toolport.describe`, measured over the search-eval fixture.

fn signature_fixture() -> Vec<Value> {
    let mut tools = crate::search_eval::catalog();
    // The fixture has no output schemas; one typed tool covers the return type.
    tools.push(json!({
        "name": "weather__get_forecast",
        "description": "Get the weather forecast for a city.",
        "inputSchema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
        "outputSchema": {"type": "object", "properties": {"tempC": {"type": "number"}, "summary": {"type": "string"}}, "required": ["tempC"]}
    }));
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    tools
}

fn signature_queries() -> Vec<String> {
    let intents: Vec<Value> = serde_json::from_str(include_str!(
        "../../tests/fixtures/search-eval/intents.json"
    ))
    .unwrap();
    let mut queries: Vec<String> = intents
        .iter()
        .map(|intent| intent["query"].as_str().unwrap().to_string())
        .collect();
    // Exact-name describes and a server listing take the other two output paths.
    queries.push("filesystem__read_text_file".into());
    queries.push("weather__get_forecast".into());
    queries.push("weather forecast".into());
    queries
}

fn signature_search(code_mode: bool, tools: &dyn ToolCatalog, query: &str) -> String {
    let host = dispatch_host(code_mode);
    let reg = Registry {
        code_mode,
        ..Default::default()
    };
    let req = json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "toolport_search_tools", "arguments": { "query": query } }
    });
    let resp = handle_request(
        &host,
        &req,
        &reg,
        &Router::new(),
        tools,
        true,
        None,
        &SearchGuard::default(),
        None,
        None,
    )
    .unwrap();
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

fn signature_tools_list(code_mode: bool, tools: &dyn ToolCatalog, lazy: bool) -> String {
    let host = dispatch_host(code_mode);
    let reg = Registry {
        code_mode,
        ..Default::default()
    };
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let resp = handle_request(
        &host,
        &req,
        &reg,
        &Router::new(),
        tools,
        lazy,
        None,
        &SearchGuard::default(),
        None,
        None,
    )
    .unwrap();
    serde_json::to_string(&resp["result"]).unwrap()
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Split a search reply into its lead text and parsed menu payload.
fn split_search(text: &str) -> (&str, Value) {
    let (lead, payload) = text.split_once("\n\n").expect("lead and payload");
    (lead, serde_json::from_str(payload).expect("payload parses"))
}

/// Blocking requirement: with Code Mode off, search output and tools/list are the
/// exact bytes `main` produced before signatures existed. The digests were captured
/// from origin/main (49614543f) with this same test body.
/// The tools/list digests were updated once for this branch's `anthropic/alwaysLoad`
/// on Toolport's meta-tools; the search digest is unchanged.
#[test]
fn code_mode_off_search_and_tools_list_are_byte_for_byte_unchanged() {
    let _data = DataDirTestEnv::new("code-mode-off-search-unchanged");
    let tools = signature_fixture();
    let searches: String = signature_queries()
        .iter()
        .map(|query| signature_search(false, &tools, query))
        .collect::<Vec<_>>()
        .join("\n--\n");
    let lazy = signature_tools_list(false, &tools, true);
    let full = signature_tools_list(false, &tools, false);
    assert!(
        !searches.contains("ToolResult"),
        "a signature leaked with Code Mode off"
    );
    assert!(!searches.contains("outputSchema"));
    assert_eq!(
        [sha256_hex(&searches), sha256_hex(&lazy), sha256_hex(&full)],
        [
            "8f4f9904eebf070a0810daea357b04056513f3ddf4e89fb442aff22b3b9761fa".to_string(),
            "c1014053103ee9f76fa161ff9a60654e984f4717c0bb94518c2cfab5dc488052".to_string(),
            "f68ebfb55064ce074f03ada89d30f0cbaf712f77df1fabc96b2a0c1f884e4db3".to_string(),
        ],
        "Code Mode off output changed"
    );
}

/// With Code Mode on, the only change to a search reply is the signature: a trailing
/// string on a menu row or a `signature` field on a full definition. Reports the
/// bytes added per response and the signature size against the compacted schema.
#[test]
fn code_mode_on_search_adds_only_signatures() {
    let _data = DataDirTestEnv::new("code-mode-on-search-signatures");
    let tools = signature_fixture();
    let queries = signature_queries();
    let (mut added, mut signed, mut hits, mut signature_bytes, mut schema_bytes) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    for query in &queries {
        let off = signature_search(false, &tools, query);
        let on = signature_search(true, &tools, query);
        added += on.len() - off.len();
        let (off_lead, off_payload) = split_search(&off);
        let (on_lead, mut on_payload) = split_search(&on);
        assert_eq!(off_lead, on_lead, "{query}");
        for entry in on_payload.as_array_mut().unwrap() {
            hits += 1;
            let signature = match entry {
                Value::Array(row) if row.len() > 3 && row.last().unwrap().is_string() => row
                    .pop()
                    .and_then(|value| value.as_str().map(str::to_string)),
                Value::Object(map) => map
                    .remove("signature")
                    .and_then(|value| value.as_str().map(str::to_string)),
                _ => None,
            };
            let name = match &*entry {
                Value::Array(row) => row[0].as_str().unwrap().to_string(),
                _ => entry["name"].as_str().unwrap().to_string(),
            };
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name.as_str())
                .unwrap();
            assert_eq!(signature, tool_signature::signature(tool), "{name}");
            if let Some(signature) = signature {
                assert!(signature.chars().count() <= tool_signature::MAX_SIGNATURE_CHARS);
                signed += 1;
                signature_bytes += signature.len();
                let mut schema = tool["inputSchema"].clone();
                integrity::neutralize_value_strings(&mut schema);
                schema_bytes += serde_json::to_string(&compact_search_schema(schema))
                    .unwrap()
                    .len();
            }
        }
        // Stripping the signatures restores the Code Mode off payload exactly.
        assert_eq!(
            serde_json::to_string(&on_payload).unwrap(),
            serde_json::to_string(&off_payload).unwrap(),
            "{query}"
        );
    }
    let forecast = signature_search(true, &tools, "weather__get_forecast");
    assert!(
        forecast.contains("ToolResult<{summary?: string; tempC: number}>"),
        "{forecast}"
    );
    assert!(!forecast.contains("outputSchema"), "{forecast}");
    let menu = signature_search(true, &tools, "read the contents of a local file from disk");
    assert!(
        menu.contains("servers.filesystem.read_text_file(args: {head?: number; path: string; tail?: number}): ToolResult"),
        "{menu}"
    );
    eprintln!(
        "code mode signatures: {} responses, +{} bytes total, {:.0} bytes per response; \
         {signed}/{hits} hits signed; signatures {signature_bytes} bytes vs compacted \
         inputSchema {schema_bytes} bytes for the same tools ({:.1}%)",
        queries.len(),
        added,
        added as f64 / queries.len() as f64,
        100.0 * signature_bytes as f64 / schema_bytes.max(1) as f64,
    );
    for name in [
        "git__git_create_branch",
        "memory__create_entities",
        "fetch__fetch",
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        eprintln!("example: {}", tool_signature::signature(tool).unwrap());
    }
    assert!(signed > 0 && added > 0);
}

/// With Code Mode on, tools/list differs only in the run_script description.
#[test]
fn code_mode_on_tools_list_keeps_output_schemas_out() {
    let _data = DataDirTestEnv::new("code-mode-on-tools-list");
    let tools = signature_fixture();
    for lazy in [true, false] {
        let listed = signature_tools_list(true, &tools, lazy);
        assert!(!listed.contains("outputSchema"), "lazy={lazy}");
        assert!(listed.contains("toolport.describe(name)"), "lazy={lazy}");
    }
}

#[test]
fn run_script_describe_reads_the_scoped_catalog() {
    let _data = DataDirTestEnv::new("run-script-describe");
    let tools = signature_fixture();
    let host = dispatch_host(true);
    let reg = Registry {
        code_mode: true,
        ..Default::default()
    };
    let script = r#"
        const byName = toolport.describe("weather__get_forecast");
        const byStub = toolport.describe(servers.filesystem.read_text_file);
        const seen = {
            byName: byName.signature,
            stub: byStub.signature,
            required: byStub.inputSchema.required.join(","),
            missing: toolport.describe("nope__nothing") === null ? "describe-null-ok" : "describe-null-bad",
        };
        // A dry run returns no value; its plan records call arguments instead.
        if (data.validate) toolport.call("filesystem__read_text_file", seen);
        return seen;
    "#;
    for validate in [false, true] {
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "toolport_run_script", "arguments": {
                "script": script, "validate": validate, "data": {"validate": validate}
            } }
        });
        let router = Arc::new(Router::new());
        let index = CatalogSearchIndex::build(&tools);
        // The plain wrapper passes no router Arc, which disables code mode.
        let resp = handle_request_with_cancel(
            &host,
            &req,
            &reg,
            &router,
            &tools,
            DiscoveryMode::Lazy,
            None,
            &SearchGuard::default(),
            None,
            None,
            None,
            None,
            Some(&index),
            Some(&router),
            None,
        )
        .unwrap();
        let result = &resp["result"];
        assert_eq!(result["isError"], false, "validate={validate}: {result}");
        let text = result.to_string();
        for expected in [
            "servers.weather.get_forecast(args: {city: string}): ToolResult<{summary?: string; tempC: number}>",
            "servers.filesystem.read_text_file(args: {head?: number; path: string; tail?: number}): ToolResult",
            "describe-null-ok",
            "path",
        ] {
            assert!(text.contains(expected), "validate={validate}: {expected} missing from {text}");
        }
    }
}

/// Measurement only, no behaviour change: what Code Mode off menus would cost if
/// hits 2..N carried a `toolport_call_tool` arguments type in place of the omitted
/// schema. Code Mode off output itself is pinned by the digest test above.
#[test]
fn measure_call_tool_arguments_on_code_mode_off_menus() {
    let _data = DataDirTestEnv::new("measure-call-tool-arguments");
    let tools = signature_fixture();
    let queries = signature_queries();
    let (mut today, mut with_arguments, mut menus, mut rows) = (0usize, 0usize, 0usize, 0usize);
    for query in &queries {
        let off = signature_search(false, &tools, query);
        let (lead, mut payload) = split_search(&off);
        let Some(entries) = payload.as_array_mut() else {
            continue;
        };
        if entries.iter().any(Value::is_object) {
            // Exact-name describes already carry the full definition.
            continue;
        }
        menus += 1;
        today += off.len();
        for row in entries.iter_mut().filter_map(Value::as_array_mut) {
            if row.len() > 3 {
                continue; // hit #1 already has its schema
            }
            let tool = tools.iter().find(|tool| tool["name"] == row[0]).unwrap();
            let mut probe = tool.clone();
            probe["name"] = json!("x");
            let sig = tool_signature::signature(&probe).unwrap();
            let args = sig
                .strip_prefix("toolport.call(\"x\", ")
                .and_then(|rest| rest.split_once("): ToolResult"))
                .map(|(args, _)| args.replacen("args", "arguments", 1))
                .unwrap();
            row.push(Value::String(args));
            rows += 1;
        }
        with_arguments += lead.len() + 2 + serde_json::to_string(&payload).unwrap().len();
    }
    eprintln!(
        "call_tool arguments on code mode off menus: {menus} menus, {rows} rows; today {} bytes \
         per response, with arguments {} bytes per response (+{} bytes, +{:.1}%)",
        today / menus.max(1),
        with_arguments / menus.max(1),
        (with_arguments - today) / menus.max(1),
        100.0 * (with_arguments - today) as f64 / today.max(1) as f64,
    );
    assert!(menus > 0 && with_arguments > today);
}

/// Run a script that describes `names` and reports the stubs it can see, executed
/// and as a dry run. Returns the two reports.
fn describe_report(
    reg: &Registry,
    router: &Arc<Router>,
    cached: &[Value],
    allowed: Option<&std::collections::HashSet<String>>,
    names: &[&str],
) -> [Value; 2] {
    let script = r#"
        const seen = { described: {}, stubs: {} };
        for (const name of data.names) {
            const d = toolport.describe(name);
            seen.described[name] = d === null ? null : d.signature;
        }
        for (const server of Object.keys(servers)) {
            seen.stubs[server] = Object.keys(servers[server]).sort();
        }
        seen.listed = toolport.listTools();
        if (data.validate) toolport.call("s__keep", seen);
        return seen;
    "#;
    [false, true].map(|validate| {
        let args = json!({"script": script, "validate": validate, "data": {"names": names, "validate": validate}});
        let result = run_script_dispatch(reg, Some(router), &cached.to_vec(), None, None, allowed, None, &args, None);
        if validate {
            // Out of scope, the probe call itself is unresolved; the plan still has it.
            result["structuredContent"]["toolportValidate"]["plan"][0]["args"].clone()
        } else {
            assert_eq!(result["isError"], false, "{result}");
            result["structuredContent"]["result"].clone()
        }
    })
}

/// S5 and L11: describe and the `servers.*` stubs see what search can return. An
/// app-only tool and an out-of-scope server are hidden by the same predicate search
/// uses. A stale cache entry the router now refuses (quarantined, disabled, outside
/// this client's tool allow-list, or destructive under the gate) is dropped the way
/// tools/list drops it. A bare override name is describable, as its signature says.
#[test]
fn describe_and_stubs_follow_search_visibility() {
    let _data = DataDirTestEnv::new("describe-search-visibility");
    let tool = |name: &str| json!({"name": name, "description": format!("{name} tool"), "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}});
    let mut app = tool("app");
    app["_meta"] = json!({"ui": {"resourceUri": "ui://s/app", "visibility": ["app"]}});
    let mut drop_tool = tool("drop");
    drop_tool["annotations"] = json!({"destructiveHint": true});
    let upstream = vec![
        tool("keep"),
        tool("wipe"),
        tool("off"),
        tool("hidden"),
        drop_tool.clone(),
        app.clone(),
    ];
    let policy = ToolPolicy {
        quarantined: ["s__wipe".to_string()].into_iter().collect(),
        disabled: [("s".to_string(), ["off".to_string()].into_iter().collect())]
            .into_iter()
            .collect(),
        allow: [(
            "s".to_string(),
            ["keep", "wipe", "off", "drop", "app"]
                .map(String::from)
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect(),
        deny_destructive: true,
        ..Default::default()
    };
    let mut router = Router::with_policy(policy);
    router.add(
        DownstreamServer::connect("s".into(), Box::new(MockRoute { tools: upstream })).unwrap(),
    );
    let router = Arc::new(router);
    // The stale cache still lists every tool under its exposed name.
    let mut cached: Vec<Value> = ["keep", "wipe", "off", "hidden"]
        .iter()
        .map(|name| tool(&format!("s__{name}")))
        .collect();
    let mut stale_drop = drop_tool;
    stale_drop["name"] = json!("s__drop");
    let mut stale_app = app;
    stale_app["name"] = json!("s__app");
    cached.extend([stale_drop, stale_app, tool("lonely")]);
    let reg = Registry {
        code_mode: true,
        deny_destructive: true,
        ..Default::default()
    };
    let names = [
        "s__keep",
        "s__wipe",
        "s__off",
        "s__hidden",
        "s__drop",
        "s__app",
        "lonely",
    ];
    for report in describe_report(&reg, &router, &cached, None, &names) {
        let described = &report["described"];
        assert_eq!(
            described["s__keep"], "servers.s.keep(args?: {q?: string}): ToolResult",
            "{report}"
        );
        assert_eq!(
            described["lonely"], "toolport.call(\"lonely\", args?: {q?: string}): ToolResult",
            "{report}"
        );
        for hidden in ["s__wipe", "s__off", "s__hidden", "s__drop", "s__app"] {
            assert_eq!(described[hidden], Value::Null, "{hidden}: {report}");
        }
        assert_eq!(report["stubs"], json!({"s": ["keep"]}), "{report}");
        assert_eq!(report["listed"], json!(["s__keep"]), "{report}");
    }
    // Search hides the app-only tool through the same predicate.
    let host = dispatch_host(true);
    let searched = handle_request(
        &host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "toolport_search_tools", "arguments": {"query": "app tool"}}}),
        &reg,
        &router,
        &cached,
        true,
        None,
        &SearchGuard::default(),
        None,
        None,
    )
    .unwrap();
    assert!(!searched.to_string().contains("s__app"), "{searched}");
    assert!(
        searched.to_string().contains("servers.s.keep"),
        "{searched}"
    );
    // Server scope: a client allowed only `other` sees nothing from `s`; one allowed
    // `s` sees the same as unscoped, minus the bare name no server owns.
    let other: std::collections::HashSet<String> = ["other".to_string()].into_iter().collect();
    for report in describe_report(&reg, &router, &cached, Some(&other), &names) {
        assert!(
            report["described"]
                .as_object()
                .unwrap()
                .values()
                .all(Value::is_null),
            "{report}"
        );
        assert_eq!(report["stubs"], json!({}), "{report}");
    }
    let own: std::collections::HashSet<String> = ["s".to_string()].into_iter().collect();
    for report in describe_report(&reg, &router, &cached, Some(&own), &names) {
        assert!(report["described"]["s__keep"].is_string(), "{report}");
        assert_eq!(report["described"]["lonely"], Value::Null, "{report}");
        assert_eq!(report["described"]["s__app"], Value::Null, "{report}");
    }
}

/// `describe` reports `truncated` and keeps the complete signature, recursive types
/// declared ahead of it.
#[test]
fn run_script_describe_returns_complete_recursive_signatures() {
    let _data = DataDirTestEnv::new("describe-recursive");
    let tools = vec![json!({
        "name": "tree__walk",
        "description": "Walk a tree.",
        "inputSchema": {"$ref": "#/$defs/Node", "$defs": {"Node": {"type": "object", "properties": {
            "value": {"type": "string"},
            "children": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
        }, "required": ["value"]}}}
    })];
    let router = Arc::new(Router::new());
    let reg = Registry {
        code_mode: true,
        ..Default::default()
    };
    let script = "const d = toolport.describe('tree__walk'); return { sig: d.signature, truncated: d.truncated, reason: d.truncatedReason === undefined };";
    let result = run_script_dispatch(
        &reg,
        Some(&router),
        &tools,
        None,
        None,
        None,
        None,
        &json!({"script": script}),
        None,
    );
    assert_eq!(
        result["structuredContent"]["result"],
        json!({
            "sig": "type $Node = {children?: $Node[]; value: string};\nservers.tree.walk(args: $Node): ToolResult",
            "truncated": false,
            "reason": true
        }),
        "{result}"
    );
    let hit = tool_signature::signature(&tools[0]).unwrap();
    assert!(hit.ends_with("/* $Node: see toolport.describe */"), "{hit}");
}

/// A stale cache (fetched before a policy change) must not leak refused tools into
/// search results. Search uses the same `tool_visible_to_model` predicate as
/// describe, stubs and execution, in both Code Mode states.
#[test]
fn search_hides_stale_refused_tools_in_both_code_mode_states() {
    let _data = DataDirTestEnv::new("search-stale-refused");
    let tool = |name: &str| json!({"name": name, "description": format!("{name} record tool"), "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}});
    let mut drop_tool = tool("drop");
    drop_tool["annotations"] = json!({"destructiveHint": true});
    let upstream = vec![
        tool("keep"),
        tool("wipe"),
        tool("off"),
        tool("hidden"),
        drop_tool.clone(),
    ];
    let policy = ToolPolicy {
        quarantined: ["s__wipe".to_string()].into_iter().collect(),
        disabled: [("s".to_string(), ["off".to_string()].into_iter().collect())]
            .into_iter()
            .collect(),
        allow: [(
            "s".to_string(),
            ["keep", "wipe", "off", "drop"]
                .map(String::from)
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect(),
        deny_destructive: true,
        ..Default::default()
    };
    let mut router = Router::with_policy(policy);
    router.add(
        DownstreamServer::connect("s".into(), Box::new(MockRoute { tools: upstream })).unwrap(),
    );
    let mut cached: Vec<Value> = ["keep", "wipe", "off", "hidden"]
        .iter()
        .map(|name| tool(&format!("s__{name}")))
        .collect();
    let mut stale_drop = drop_tool;
    stale_drop["name"] = json!("s__drop");
    cached.push(stale_drop);
    let refused = ["s__wipe", "s__off", "s__hidden", "s__drop"];
    let queries = [
        json!({"query": "record tool", "limit": 25}),
        json!({"query": "", "server": "s"}),
        json!({"query": "s__wipe"}),
        json!({"query": "s__off"}),
        json!({"query": "s__hidden"}),
        json!({"query": "s__drop"}),
    ];
    let own: std::collections::HashSet<String> = ["s".to_string()].into_iter().collect();
    let other: std::collections::HashSet<String> = ["other".to_string()].into_iter().collect();
    for code_mode in [false, true] {
        let reg = Registry {
            code_mode,
            deny_destructive: true,
            ..Default::default()
        };
        let host = dispatch_host(true);
        let search = |arguments: &Value, allowed: Option<&std::collections::HashSet<String>>| {
            handle_request(
                &host,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "toolport_search_tools", "arguments": arguments}}),
                &reg,
                &router,
                &cached,
                true,
                None,
                &SearchGuard::default(),
                allowed,
                None,
            )
            .unwrap()
            .to_string()
        };
        for arguments in &queries {
            for allowed in [None, Some(&own)] {
                let text = search(arguments, allowed);
                for name in refused {
                    let bare = name.trim_start_matches("s__");
                    assert!(
                        !text.contains(name) && !text.contains(&format!("servers.s.{bare}")),
                        "code_mode={code_mode} {arguments} leaked {name}: {text}"
                    );
                }
            }
            // A client allowed only another server sees nothing from `s`.
            let text = search(arguments, Some(&other));
            assert!(
                !text.contains("s__") && !text.contains("servers.s."),
                "code_mode={code_mode} {arguments} leaked across scope: {text}"
            );
        }
        let text = search(&queries[0], None);
        assert!(
            text.contains("s__keep") || text.contains("servers.s.keep"),
            "code_mode={code_mode}: {text}"
        );
    }
}

/// Search results trimmed to the describe budget hand out fetch cursors. Every cursor
/// a response names is distinct, and paging each one to the end through
/// `toolport_fetch_result` returns every hit exactly once, in both Code Mode states.
#[test]
fn search_cursors_are_distinct_and_page_every_hit_once() {
    let _data = DataDirTestEnv::new("search-cursor-paging");
    let mut tools = off_identity_fixture();
    let wide: serde_json::Map<String, Value> = (0..300)
        .map(|i| {
            (
                format!("reset_{i:03}"),
                json!({"type": "boolean", "description": format!("Reset option {i}.")}),
            )
        })
        .collect();
    tools.push(json!({
        "name": "wide__reset",
        "description": "Reset every option of the wide tool at once.",
        "inputSchema": {"type": "object", "properties": wide}
    }));
    let cursor_re = regex::Regex::new(r#""cursor":"(r[0-9]+)","offset":([0-9]+)"#).unwrap();
    let next_re =
        regex::Regex::new(r"call toolport_fetch_result with offset=([0-9]+) for").unwrap();
    let text_of = |resp: &Value| -> String {
        resp["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for code_mode in [false, true] {
        let reg = Registry {
            code_mode,
            ..Default::default()
        };
        let host = dispatch_host(true);
        let router = Router::new();
        let call = |name: &str, arguments: Value| {
            handle_request(
                &host,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
                &reg,
                &router,
                &tools,
                true,
                None,
                &SearchGuard::default(),
                None,
                None,
            )
            .unwrap()
        };
        let mut every_cursor = std::collections::HashSet::new();
        for (query, prefix, count) in [
            ("wide__configure", "option_", 300),
            ("wide__reset", "reset_", 300),
        ] {
            let first = text_of(&call("toolport_search_tools", json!({"query": query})));
            let cursors: Vec<(String, usize)> = cursor_re
                .captures_iter(&first)
                .map(|c| (c[1].to_string(), c[2].parse().unwrap()))
                .collect();
            assert_eq!(cursors.len(), 1, "code_mode={code_mode} {query}: {first}");
            let distinct: std::collections::HashSet<_> = cursors.iter().map(|c| &c.0).collect();
            assert_eq!(distinct.len(), cursors.len(), "{first}");
            let (cursor, mut offset) = cursors[0].clone();
            assert!(
                every_cursor.insert(cursor.clone()),
                "cursor {cursor} reused"
            );
            // Reassemble the stashed body: the head shown, then each page in order.
            let marker = first.rfind("\n\n[Toolport shaped this result").unwrap();
            let mut body = first[..marker].to_string();
            let mut pages = 0;
            loop {
                let page = text_of(&call(
                    "toolport_fetch_result",
                    json!({"cursor": cursor, "offset": offset, "len": 6000}),
                ));
                pages += 1;
                assert!(pages < 100, "paging never ended: {page}");
                let at = page.rfind("\n\n[Toolport: ").expect("page footer");
                body.push_str(&page[..at]);
                match next_re.captures(&page[at..]) {
                    Some(next) => offset = next[1].parse().unwrap(),
                    None => {
                        assert!(page[at..].contains("end of result"), "{page}");
                        break;
                    }
                }
            }
            assert!(pages > 1, "code_mode={code_mode} {query}: one page only");
            for i in 0..count {
                let key = format!("\"{prefix}{i:03}\":");
                assert_eq!(
                    body.matches(&key).count(),
                    1,
                    "code_mode={code_mode} {query}: {key} after {pages} page(s)"
                );
            }
        }
    }
}

/// R3-2: a paged tool definition from exact-name search is served only while
/// search would still return that tool. The traced sequence: search creates the
/// cursor, policy or scope then refuses the tool, a fresh search filters it, and
/// the old cursor is refused instead of handing back the schema. Both Code Mode
/// states, for quarantine, disabling, the tool allow-list and the client scope.
#[test]
fn discovery_cursors_follow_tool_visibility() {
    let _data = DataDirTestEnv::new("discovery-cursor-visibility");
    let options: serde_json::Map<String, Value> = (0..300)
        .map(|i| {
            (
                format!("option_{i:03}"),
                json!({"type": "string", "description": format!("Option number {i}.")}),
            )
        })
        .collect();
    let upstream = vec![
        json!({"name": "configure", "description": "Configure every option.", "inputSchema": {"type": "object", "properties": options}}),
        json!({"name": "other", "description": "Another tool.", "inputSchema": {"type": "object"}}),
    ];
    let cached: Vec<Value> = upstream
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            tool["name"] = json!(format!("wide__{}", tool["name"].as_str().unwrap()));
            tool
        })
        .collect();
    let router_with = |policy: ToolPolicy| {
        let mut router = Router::with_policy(policy);
        router.add(
            DownstreamServer::connect(
                "wide".into(),
                Box::new(MockRoute {
                    tools: upstream.clone(),
                }),
            )
            .unwrap(),
        );
        router
    };
    let open = router_with(ToolPolicy::default());
    let quarantined = router_with(ToolPolicy {
        quarantined: ["wide__configure".to_string()].into_iter().collect(),
        ..Default::default()
    });
    let disabled = router_with(ToolPolicy {
        disabled: [(
            "wide".to_string(),
            ["configure".to_string()].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let allow_other = router_with(ToolPolicy {
        allow: [(
            "wide".to_string(),
            ["other".to_string()].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let other_scope: std::collections::HashSet<String> =
        ["elsewhere".to_string()].into_iter().collect();
    let cursor_re = regex::Regex::new(r#""cursor":"(r[0-9]+)""#).unwrap();
    for code_mode in [false, true] {
        let reg = Registry {
            code_mode,
            ..Default::default()
        };
        let host = dispatch_host(true);
        let call = |router: &Router,
                    allowed: Option<&std::collections::HashSet<String>>,
                    name: &str,
                    arguments: Value| {
            let resp = handle_request(
                &host,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
                &reg,
                router,
                &cached,
                true,
                None,
                &SearchGuard::default(),
                allowed,
                None,
            )
            .unwrap();
            resp["result"].clone()
        };
        let describe = || {
            let found = call(
                &open,
                None,
                "toolport_search_tools",
                json!({"query": "wide__configure"}),
            );
            let text = found["content"][0]["text"].as_str().unwrap().to_string();
            cursor_re.captures(&text).unwrap()[1].to_string()
        };
        // Still visible: the cursor pages.
        let cursor = describe();
        let page = call(
            &open,
            None,
            "toolport_fetch_result",
            json!({"cursor": cursor, "offset": 100}),
        );
        assert_eq!(page["isError"], false, "code_mode={code_mode}: {page}");
        for (case, router, allowed) in [
            ("quarantined", &quarantined, None),
            ("disabled", &disabled, None),
            ("allow-list", &allow_other, None),
            ("scope", &open, Some(&other_scope)),
        ] {
            let cursor = describe();
            let fresh = call(
                router,
                allowed,
                "toolport_search_tools",
                json!({"query": "wide__configure"}),
            );
            assert!(
                !fresh.to_string().contains("option_150"),
                "code_mode={code_mode} {case}: fresh search shows the schema: {fresh}"
            );
            let refused = call(
                router,
                allowed,
                "toolport_fetch_result",
                json!({"cursor": cursor, "offset": 100}),
            );
            let text = refused["content"][0]["text"].as_str().unwrap();
            assert_eq!(
                refused["isError"], true,
                "code_mode={code_mode} {case}: {refused}"
            );
            assert!(
                text.contains("no longer available"),
                "code_mode={code_mode} {case}: {text}"
            );
            assert!(
                !text.contains("option_"),
                "code_mode={code_mode} {case}: {text}"
            );
            // The refused entry is gone, even for the policy it was made under.
            let again = call(
                &open,
                None,
                "toolport_fetch_result",
                json!({"cursor": cursor, "offset": 100}),
            );
            assert_eq!(
                again["isError"], true,
                "code_mode={code_mode} {case}: {again}"
            );
        }
        // A script's fetchResult applies the same rule.
        if code_mode {
            let cursor = describe();
            let script = format!(
                "var r = toolport.fetchResult({{cursor: '{cursor}', offset: 100}}); return r.content[0].text;"
            );
            let router = Arc::new(router_with(ToolPolicy {
                quarantined: ["wide__configure".to_string()].into_iter().collect(),
                ..Default::default()
            }));
            let result = run_script_dispatch(
                &reg,
                Some(&router),
                &cached,
                None,
                None,
                None,
                None,
                &json!({"script": script}),
                None,
            );
            let text = result["structuredContent"]["result"]
                .as_str()
                .unwrap_or_default();
            assert!(text.contains("no longer available"), "{result}");
        }
        // A policy or registry change drops every paged definition at once.
        let cursor = describe();
        assert!(shaping::forget_discovery(host.discovery_realm(), |_| true) >= 1);
        let gone = call(
            &open,
            None,
            "toolport_fetch_result",
            json!({"cursor": cursor, "offset": 100}),
        );
        assert!(
            gone["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown or expired"),
            "{gone}"
        );
    }
}

/// R3-2: a shaped tool-call result is the output of a call that policy allowed when
/// it ran, already partly delivered to that caller. Its later pages stay readable
/// by the same caller after the tool is disabled, and no policy change drops them.
#[test]
fn result_cursors_outlive_later_policy_for_their_caller() {
    let _data = DataDirTestEnv::new("result-cursor-policy");
    let body = "z".repeat(shaping::DEFAULT_BUDGET_BYTES * 2);
    let router_with = |policy: ToolPolicy| {
        let mut router = Router::with_policy(policy);
        router.add(
            DownstreamServer::connect("s".into(), Box::new(PagingRoute { body: body.clone() }))
                .unwrap(),
        );
        router
    };
    let open = router_with(ToolPolicy::default());
    let disabled = router_with(ToolPolicy {
        disabled: [("s".to_string(), ["big".to_string()].into_iter().collect())]
            .into_iter()
            .collect(),
        ..Default::default()
    });
    let reg = Registry::default();
    let host = dispatch_host(true);
    let cached = open.aggregated_tools();
    let call = |router: &Router, name: &str, arguments: Value| {
        handle_request(
            &host,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
            &reg,
            router,
            &cached,
            true,
            None,
            &SearchGuard::default(),
            None,
            None,
        )
        .unwrap()["result"]
            .clone()
    };
    let shaped = call(
        &open,
        "toolport_call_tool",
        json!({"name": "s__big", "arguments": {}}),
    );
    let text = shaped["content"][0]["text"].as_str().unwrap();
    let cursor = regex::Regex::new(r#""cursor":"(r[0-9]+)""#)
        .unwrap()
        .captures(text)
        .unwrap()[1]
        .to_string();
    shaping::forget_discovery(host.discovery_realm(), |_| true);
    let page = call(
        &disabled,
        "toolport_fetch_result",
        json!({"cursor": cursor, "offset": 10}),
    );
    assert_eq!(page["isError"], false, "{page}");
    assert!(
        page["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("zzz"),
        "{page}"
    );
}

/// `toolport_status` counts the tools search would return: a stale cache entry for
/// a quarantined, disabled, allow-listed-out or destructive-denied tool is not
/// counted.
#[test]
fn status_counts_agree_with_search() {
    let _data = DataDirTestEnv::new("status-counts-search");
    let tool = |name: &str| json!({"name": name, "description": format!("{name} tool"), "inputSchema": {"type": "object"}});
    let mut drop_tool = tool("drop");
    drop_tool["annotations"] = json!({"destructiveHint": true});
    let upstream = vec![
        tool("keep"),
        tool("wipe"),
        tool("off"),
        tool("hidden"),
        drop_tool.clone(),
    ];
    let mut router = Router::with_policy(ToolPolicy {
        quarantined: ["s__wipe".to_string()].into_iter().collect(),
        disabled: [("s".to_string(), ["off".to_string()].into_iter().collect())]
            .into_iter()
            .collect(),
        allow: [(
            "s".to_string(),
            ["keep", "wipe", "off", "drop"]
                .map(String::from)
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect(),
        deny_destructive: true,
        ..Default::default()
    });
    router.add(
        DownstreamServer::connect("s".into(), Box::new(MockRoute { tools: upstream })).unwrap(),
    );
    let mut cached: Vec<Value> = ["keep", "wipe", "off", "hidden"]
        .iter()
        .map(|name| tool(&format!("s__{name}")))
        .collect();
    let mut stale_drop = drop_tool;
    stale_drop["name"] = json!("s__drop");
    cached.push(stale_drop);
    let mut reg = Registry {
        deny_destructive: true,
        ..Default::default()
    };
    reg.servers.push(
        serde_json::from_str(r#"{"id":"s","name":"S","transport":"stdio","command":"s-cmd"}"#)
            .unwrap(),
    );
    let host = dispatch_host(true);
    let allowed: std::collections::HashSet<String> = ["s".to_string()].into_iter().collect();
    let status = enabled_summary_with(&host, &router, &reg, &cached, None, Some(&allowed));
    assert!(status.contains("- s: 1 tool(s)"), "{status}");
    // The same catalog through search lists only that one tool.
    let searched = handle_request(
        &host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "toolport_search_tools", "arguments": {"query": "", "server": "s"}}}),
        &reg,
        &router,
        &cached,
        true,
        None,
        &SearchGuard::default(),
        Some(&allowed),
        None,
    )
    .unwrap()
    .to_string();
    for name in ["s__wipe", "s__off", "s__hidden", "s__drop"] {
        assert!(!searched.contains(name), "{name}: {searched}");
    }
    assert!(searched.contains("s__keep"), "{searched}");
}

/// Calling `wait` publishes `next` to the live router slot, as a reconcile does
/// while a script waits on a call.
struct PublishOnCall {
    tools: Vec<Value>,
    slot: Arc<Mutex<Arc<Router>>>,
    next: Arc<Router>,
}

impl conduit_lib::downstream::Transport for PublishOnCall {
    fn request(
        &mut self,
        method: &str,
        _params: Value,
    ) -> Result<Value, conduit_lib::downstream::TransportError> {
        match method {
            "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
            "tools/list" => Ok(json!({ "tools": self.tools })),
            "tools/call" => {
                *self
                    .slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(&self.next);
                Ok(json!({"content": [{"type": "text", "text": "waited"}], "isError": false}))
            }
            other => Err(conduit_lib::downstream::TransportError::Fatal(format!(
                "unexpected {other}"
            ))),
        }
    }
    fn notify(
        &mut self,
        _method: &str,
        _params: Value,
    ) -> Result<(), conduit_lib::downstream::TransportError> {
        Ok(())
    }
}

/// R4-1: a script that waits on a call while `s__configure` is quarantined can no
/// longer fetch its paged definition or describe it, though both were visible when
/// the script started. Checked in both Code Mode states.
#[test]
fn script_callbacks_see_policy_published_mid_script() {
    let _data = DataDirTestEnv::new("script-callbacks-live-policy");
    let options: serde_json::Map<String, Value> = (0..300)
        .map(|i| {
            (
                format!("option_{i:03}"),
                json!({"type": "string", "description": format!("Option number {i}.")}),
            )
        })
        .collect();
    let upstream = vec![
        json!({"name": "configure", "description": "Configure every option.", "inputSchema": {"type": "object", "properties": options}}),
        json!({"name": "wait", "description": "Wait for something.", "inputSchema": {"type": "object"}}),
    ];
    let cached: Vec<Value> = upstream
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            tool["name"] = json!(format!("s__{}", tool["name"].as_str().unwrap()));
            tool
        })
        .collect();
    let cursor_re = regex::Regex::new(r#""cursor":"(r[0-9]+)""#).unwrap();
    for code_mode in [false, true] {
        let reg = Registry {
            code_mode,
            ..Default::default()
        };
        let mut quarantined = Router::with_policy(ToolPolicy {
            quarantined: ["s__configure".to_string()].into_iter().collect(),
            ..Default::default()
        });
        quarantined.add(
            DownstreamServer::connect(
                "s".into(),
                Box::new(MockRoute {
                    tools: upstream.clone(),
                }),
            )
            .unwrap(),
        );
        let quarantined = Arc::new(quarantined);
        let slot: Arc<Mutex<Arc<Router>>> = Arc::new(Mutex::new(Arc::new(Router::new())));
        let mut open = Router::with_policy(ToolPolicy::default());
        open.add(
            DownstreamServer::connect(
                "s".into(),
                Box::new(PublishOnCall {
                    tools: upstream.clone(),
                    slot: Arc::clone(&slot),
                    next: Arc::clone(&quarantined),
                }),
            )
            .unwrap(),
        );
        let open = Arc::new(open);
        *slot.lock().unwrap() = Arc::clone(&open);
        let host = dispatch_host(true);
        let found = handle_request(
            &host,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "toolport_search_tools", "arguments": {"query": "s__configure"}}}),
            &reg,
            &open,
            &cached,
            true,
            None,
            &SearchGuard::default(),
            None,
            None,
        )
        .unwrap();
        let text = found["result"]["content"][0]["text"].as_str().unwrap();
        let cursor = cursor_re.captures(text).unwrap()[1].to_string();
        let script = format!(
            "var before = toolport.describe('s__configure') !== null; \
             toolport.call('s__wait', {{}}); \
             var page = toolport.fetchResult({{cursor: '{cursor}', offset: 100}}).content[0].text; \
             return {{before: before, refused: page.indexOf('no longer available') >= 0, \
                     leaked: page.indexOf('option_') >= 0, \
                     described: toolport.describe('s__configure') !== null}};"
        );
        let result = run_script_dispatch(
            &reg,
            Some(&open),
            &cached,
            None,
            None,
            None,
            None,
            &json!({"script": script}),
            Some(&slot),
        );
        let value = &result["structuredContent"]["result"];
        assert_eq!(
            value,
            &json!({"before": true, "refused": true, "leaked": false, "described": false}),
            "code_mode={code_mode}: {result}"
        );
    }
}
