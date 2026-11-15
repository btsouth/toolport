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
            "8c8e04fc38b5f24e49b6c0c53169da9718129f2a42b65d074f988a1b701d8741".to_string(),
            "2230a54cc8268f9b40b16ecde95f134ecdd44b67bf397ac2b82456dd0d3d8c4f".to_string(),
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
