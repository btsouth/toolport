// Included in the gateway's test module so measurements use its real dispatch paths.
fn token_audit_public_tools() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../benchmark/fixtures/token-budget/public-tools.json"
    ))
    .unwrap()
}

fn token_audit_catalog() -> Vec<Value> {
    let shape: Value = serde_json::from_str(include_str!(
        "../../../benchmark/fixtures/token-budget/shape.json"
    ))
    .unwrap();
    let seeds = token_audit_public_tools();
    let pairs = shape["sizePairs"].as_array().unwrap();
    let counts = shape["serverCounts"].as_array().unwrap();
    let mut catalog = Vec::new();
    for (server, count) in counts.iter().enumerate() {
        for number in 0..count.as_u64().unwrap() as usize {
            let i = catalog.len();
            let seed = &seeds[i % seeds.len()];
            // A coprime permutation spreads the observed size tail across servers.
            let pair = &pairs[(i * 641 % 1707) * (pairs.len() - 1) / 1706];
            let desc_size = pair[0].as_u64().unwrap() as usize;
            let schema_size = pair[1].as_u64().unwrap() as usize;
            let source = seed["description"].as_str().unwrap();
            let description: String = source.chars().cycle().take(desc_size).collect();
            let mut schema = json!({"type":"object","properties":{}});
            // Realistic repeated property objects, not a single huge filler string.
            let properties = schema["properties"].as_object_mut().unwrap();
            for p in 0..schema_size.saturating_sub(70) / 160 {
                properties.insert(format!("option_{p}"), json!({
                    "type":"string", "description":format!("Optional resource setting {p}. {}", &source[..source.len().min(90)])
                }));
            }
            let size = schema.to_string().len();
            if schema_size > size + 17 {
                schema["description"] = json!(source
                    .chars()
                    .cycle()
                    .take(schema_size - size - 17)
                    .collect::<String>());
            }
            catalog.push(json!({
                "name":format!("synthetic{server:02}__read_record_{number:04}"),
                "description":description, "inputSchema":schema,
                "annotations":{"readOnlyHint":true,"destructiveHint":false}
            }));
        }
    }
    assert_eq!(catalog.len(), 1707);
    catalog
}

fn token_audit_measure(bpe: &tiktoken_rs::CoreBPE, text: &str) -> Value {
    json!({
        "bytes":text.len(), "o200k_base":bpe.encode_ordinary(text).len(),
        // This is a reproducible heuristic, not Claude's tokenizer or API count.
        "claude_approx_chars_div_4":text.chars().count().div_ceil(4)
    })
}

fn token_audit_dispatch(catalog: &dyn ToolCatalog, mode: DiscoveryMode, request: &Value) -> Value {
    let host = dispatch_host(false);
    host.set_discovery_mode(mode);
    handle_request(
        &host,
        request,
        &Registry::default(),
        &Router::new(),
        catalog,
        mode == DiscoveryMode::Lazy,
        None,
        &SearchGuard::default(),
        None,
        None,
    )
    .unwrap()["result"]
        .clone()
}

#[test]
#[ignore = "needs a Rust toolchain and takes several minutes; run npm run bench:tokens"]
fn token_budget_audit() {
    let _env = DataDirTestEnv::new("token-budget-audit");
    let bpe = tiktoken_rs::o200k_base().unwrap();
    let mut measurements = serde_json::Map::new();
    let mut payloads = serde_json::Map::new();
    let mut record = |name: &str, text: String| {
        measurements.insert(name.into(), token_audit_measure(&bpe, &text));
        payloads.insert(name.into(), json!(text));
    };
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
    let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    for (corpus, catalog) in [
        ("shape1707", token_audit_catalog()),
        ("public11", token_audit_public_tools()),
    ] {
        record(
            &format!("{corpus}.plain_passthrough"),
            json!({"tools":catalog}).to_string(),
        );
        for mode in [
            DiscoveryMode::Lazy,
            DiscoveryMode::Grouped,
            DiscoveryMode::Full,
        ] {
            let prefix = format!("{corpus}.{}", mode.as_str());
            let init = token_audit_dispatch(&catalog, mode, &initialize);
            let tools = token_audit_dispatch(&catalog, mode, &list);
            record(&format!("{prefix}.initialize_wire"), init.to_string());
            record(
                &format!("{prefix}.instructions"),
                init["instructions"].as_str().unwrap_or("").into(),
            );
            record(&format!("{prefix}.tools_list"), tools.to_string());
            let host = dispatch_host(true);
            record(&format!("{prefix}.tools_list_code_mode"), json!({"tools":tool_surface(&host, &Registry::default(), &Router::new(), &catalog, None, mode)}).to_string());
        }
        let names: Vec<&Value> = catalog.iter().map(|tool| &tool["name"]).collect();
        record(
            &format!("{corpus}.native_names_only"),
            json!(names).to_string(),
        );
        // An API individual-function deferral leaves names AND descriptions loaded.
        let headers: Vec<Value> = catalog
            .iter()
            .map(|t| json!({"name":t["name"],"description":t["description"]}))
            .collect();
        record(
            &format!("{corpus}.native_individual_headers"),
            json!(headers).to_string(),
        );
        let query = if corpus == "shape1707" {
            "read record"
        } else {
            "list channels"
        };
        let search_req = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"toolport_search_tools","arguments":{"query":query}}});
        record(
            &format!("{corpus}.search_request"),
            search_req["params"].to_string(),
        );
        let search = token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &search_req);
        record(&format!("{corpus}.search_response"), search.to_string());
        let text = search["content"][0]["text"].as_str().unwrap();
        let hits: Value = serde_json::from_str(text.split_once("\n\n").unwrap().1).unwrap();
        let top = hits[0]["name"].as_str().unwrap();
        // Exact-name search is Toolport's describe operation; there is no describe tool.
        let describe_req = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"toolport_search_tools","arguments":{"query":top}}});
        record(
            &format!("{corpus}.describe_request"),
            describe_req["params"].to_string(),
        );
        record(
            &format!("{corpus}.describe_response"),
            token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &describe_req).to_string(),
        );
        record(
            &format!("{corpus}.native_loaded_top"),
            catalog
                .iter()
                .find(|t| t["name"] == top)
                .unwrap()
                .to_string(),
        );
        record(
            &format!("{corpus}.lazy_call_request"),
            json!({"name":"toolport_call_tool","arguments":{"name":top,"arguments":{}}})
                .to_string(),
        );
        record(
            &format!("{corpus}.direct_call_request"),
            json!({"name":top,"arguments":{}}).to_string(),
        );
        for (case, args) in [
            ("missing_name", json!({})),
            ("unknown_tool", json!({"name":"missing__tool"})),
        ] {
            let req = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"toolport_call_tool","arguments":args}});
            record(
                &format!("{corpus}.error_{case}"),
                token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &req).to_string(),
            );
        }
        let req = json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"toolport_search_tools","arguments":{"query":"zzzxxyynonexistent"}}});
        record(
            &format!("{corpus}.low_confidence"),
            token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &req).to_string(),
        );
    }
    for count in [5, 1500] {
        let rows: Vec<Value> = (0..count).map(|i| json!({"id":i,"title":format!("Issue {i}: review deployment settings"),"state":"open","labels":["review","deployment"]})).collect();
        let data = json!({"issues":rows});
        for pretty in [false, true] {
            let text = if pretty {
                serde_json::to_string_pretty(&data).unwrap()
            } else {
                data.to_string()
            };
            for duplicate in [false, true] {
                let prefix = format!("result.{count}.pretty_{pretty}.duplicate_{duplicate}");
                let mut result = json!({"content":[{"type":"text","text":text}],"isError":false});
                if duplicate {
                    result["structuredContent"] = data.clone();
                }
                record(&format!("{prefix}.raw"), result.to_string());
                integrity::neutralize_untrusted_result(&mut result);
                integrity::label_untrusted_result("public-fixture", &mut result);
                record(&format!("{prefix}.provenance"), result.to_string());
                let shaped = shaping::shape_result(
                    &mut result,
                    shaping::DEFAULT_BUDGET_BYTES,
                    Some("audit"),
                );
                integrity::label_untrusted_result("public-fixture", &mut result);
                record(&format!("{prefix}.emitted"), result.to_string());
                if shaped {
                    let text = result["content"][0]["text"].as_str().unwrap();
                    let cursor = text
                        .split("\"cursor\":\"")
                        .nth(1)
                        .unwrap()
                        .split('"')
                        .next()
                        .unwrap();
                    let fetched = shaping::fetch_result(cursor, 0, usize::MAX, Some("audit"), None);
                    record(&format!("{prefix}.full_fetch"), fetched.to_string());
                    if duplicate {
                        record(
                            &format!("{prefix}.projection"),
                            shaping::fetch_result(cursor, 0, 0, Some("audit"), Some("issues.0"))
                                .to_string(),
                        );
                    }
                }
            }
        }
    }
    let output = std::env::var("TOOLPORT_TOKEN_AUDIT_OUTPUT").expect("benchmark output path");
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({"measurements":measurements,"payloads":payloads}))
            .unwrap(),
    )
    .unwrap();
    println!(
        "TOKEN_AUDIT {} measurements; tokenizer=o200k_base; Claude=chars/4 approximation",
        measurements.len()
    );
}

#[test]
fn token_budget_regression() {
    let _env = DataDirTestEnv::new("token-budget-regression");
    let bpe = tiktoken_rs::o200k_base().unwrap();
    let host = dispatch_host(false);
    let tools = floor_tool_defs(&host);
    let floor = json!(tools).to_string() + &discovery_instructions(DiscoveryMode::Lazy, None);
    let tokens = bpe.encode_ordinary(&floor).len();
    // About 10% headroom above the measured payloads, not optimization targets.
    let check = |label: &str, measured: usize, limit: usize| {
        assert!(
            measured <= limit,
            "{label} {measured} tokens exceeds {limit}; raise the limit deliberately and record before/after token counts"
        );
    };
    check("lazy floor", tokens, 550);
    let floor_tokens = tokens;
    let help = help_tool_def("synthetic00", 618).to_string();
    let help_tokens = bpe.encode_ordinary(&help).len();
    check("grouped help", help_tokens, 105);
    let catalog = token_audit_public_tools();
    let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"toolport_search_tools","arguments":{"query":"list channels"}}});
    let response = token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &req);
    let tokens = bpe.encode_ordinary(&response.to_string()).len();
    check("public search", tokens, 545);
    let exact = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"toolport_search_tools","arguments":{"query":"slack__slack_list_channels"}}});
    let exact_response = token_audit_dispatch(&catalog, DiscoveryMode::Lazy, &exact);
    let exact_tokens = bpe.encode_ordinary(&exact_response.to_string()).len();
    check("exact-name lookup", exact_tokens, 185);
    println!("TOKEN_BUDGET floor={floor_tokens} search={tokens} exact={exact_tokens} grouped_help={help_tokens}");
}
