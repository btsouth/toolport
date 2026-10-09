// Real public MCP definitions and frozen natural-language request families.
// Kept outside the gateway so ranking work does not collide with payload changes.
use super::*;

const SCALE_CATALOG: &str = include_str!("../../tests/fixtures/search-eval-scale/catalog.json");
const DEV: &str = include_str!("../../tests/fixtures/search-eval-scale/dev.json");
const DEV_V2: &str = include_str!("../../tests/fixtures/search-eval-scale/dev-v2.json");
const DEV_V2_SPLIT: &str = include_str!("../../tests/fixtures/search-eval-scale/dev-v2-split.json");
const OLD_HELD_OUT: &str = include_str!("../../tests/fixtures/search-eval-scale/held_out.json");

fn scale_catalog() -> Vec<Value> {
    let mut tools: Vec<Value> = serde_json::from_str(SCALE_CATALOG).unwrap();
    for tool in &mut tools {
        if let Some(schema) = tool.get_mut("inputSchema") {
            conduit_lib::router::normalize_tool_schema(schema);
            conduit_lib::router::inline_refs(schema);
        }
    }
    tools
}

fn evaluate(
    split: &str,
    intents: &[Value],
    tools: &dyn ToolCatalog,
    index: &CatalogSearchIndex,
    limit: usize,
) -> Value {
    let mut rows = Vec::new();
    let (mut confident, mut confident_correct, mut uncertain_correct) = (0, 0, 0);
    let mut recall = [0usize; 7];
    let (mut top1, mut top3, mut positives, mut reciprocal_rank) = (0, 0, 0, 0.0);
    let (mut rejected, mut true_rejected, mut negatives, mut honest_negative) = (0, 0, 0, 0);
    let (mut ambiguous, mut honest_ambiguous) = (0, 0);
    let mut latencies = Vec::new();
    for intent in intents {
        let query = intent["query"].as_str().unwrap();
        let server = intent["server"].as_str();
        let expected: Vec<&str> = intent["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        for name in &expected {
            assert!(
                tools.iter().any(|tool| tool["name"].as_str() == Some(name)),
                "unknown label {name}"
            );
        }
        let started = Instant::now();
        let outcome = search_catalog_indexed(tools, query, server, limit, None, Some(index));
        let micros = started.elapsed().as_secs_f64() * 1e6;
        latencies.push(micros);
        let names: Vec<&str> = outcome
            .matches
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        let rank = names
            .iter()
            .position(|name| expected.contains(name))
            .map(|rank| rank + 1);
        let kind = intent["kind"].as_str().unwrap();
        if kind == "ranked" {
            positives += 1;
            for (i, k) in [1, 3, 5, 8, 10, 12, 25].iter().enumerate() {
                if rank.is_some_and(|rank| rank <= *k) {
                    recall[i] += 1;
                }
            }
            if rank == Some(1) {
                top1 += 1;
            }
            if rank.is_some_and(|rank| rank <= 3) {
                top3 += 1;
            }
            reciprocal_rank += rank.map_or(0.0, |rank| 1.0 / rank as f64);
        }
        if !outcome.low_confidence {
            confident += 1;
            if kind == "ranked" && rank == Some(1) {
                confident_correct += 1;
            }
        } else if kind == "ranked" && rank == Some(1) {
            uncertain_correct += 1;
        }
        if outcome.total == 0 {
            rejected += 1;
            if kind == "no_match" {
                true_rejected += 1;
            }
        }
        if kind == "no_match" {
            negatives += 1;
            if outcome.low_confidence {
                honest_negative += 1;
            }
        }
        let candidates = names.iter().filter(|name| expected.contains(name)).count();
        if kind == "ambiguous" {
            ambiguous += 1;
            if outcome.low_confidence && candidates >= 2 {
                honest_ambiguous += 1;
            }
        }
        rows.push(
            json!({"id":intent["id"],"kind":kind,"query":query,"server":server,
            "expected":expected,"category":intent["category"],"rank":rank,"names":names,
            "low_confidence":outcome.low_confidence,"direct_matches":outcome.total,
            "candidate_coverage":candidates,"latency_us":micros,"returned":names.len()}),
        );
    }
    latencies.sort_by(f64::total_cmp);
    let percentile = |p: f64| latencies[((latencies.len() - 1) as f64 * p).ceil() as usize];
    let recall_at: serde_json::Map<String, Value> = [1, 3, 5, 8, 10, 12, 25]
        .iter()
        .zip(recall)
        .map(|(k, n)| {
            (
                k.to_string(),
                json!({"hits":n,"rate":n as f64 / positives as f64}),
            )
        })
        .collect();
    json!({"confident_count":confident,"confident_correct":confident_correct,
        "confident_precision":if confident == 0 {Value::Null} else {json!(confident_correct as f64 / confident as f64)},
        "confident_coverage":confident as f64 / intents.len() as f64,
        "uncertain_correct_top1":uncertain_correct,
        "correct_top1_uncertainty_rate":if top1 == 0 {Value::Null} else {json!(uncertain_correct as f64 / top1 as f64)},
        "split":split,"intents":intents.len(),"positives":positives,"top1":top1,
        "top3":top3,"recall_at": recall_at,"top1_rate":top1 as f64 / positives as f64,
        "top3_rate":top3 as f64 / positives as f64,"mrr_at_25":reciprocal_rank / positives as f64,
        "no_match_precision": if rejected == 0 { Value::Null } else {json!(true_rejected as f64 / rejected as f64)},
        "no_match_recall":true_rejected as f64 / negatives as f64,
        "no_match_honesty":honest_negative as f64 / negatives as f64,
        "no_match_count":negatives,"ambiguity_honesty":honest_ambiguous as f64 / ambiguous as f64,
        "ambiguity_count":ambiguous,"latency_p50_us":percentile(0.50),"latency_p95_us":percentile(0.95),
        "rows":rows})
}

#[test]
#[ignore = "explicit benchmark; SEARCH_SCALE_OUTPUT is an external evidence file"]
fn search_scale_measure() {
    let env = DataDirTestEnv::new("search-scale-measure");
    let tools = scale_catalog();
    assert_eq!(tools.len(), 1707);
    let started = Instant::now();
    let index = CatalogSearchIndex::build(&tools);
    let index_build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let limit: usize = std::env::var("SEARCH_SCALE_LIMIT")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(0);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let split = std::env::var("SEARCH_SCALE_SPLIT").unwrap_or_else(|_| "dev".into());
    assert!(["dev", "held_out", "both", "external"].contains(&split.as_str()));
    let external = std::env::var("SEARCH_SCALE_INTENTS")
        .ok()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|| "[]".into());
    let mut reports = Vec::new();
    let host = dispatch_host(false);
    let reg = Registry::default();
    let router = router();
    for (name, text) in [
        ("dev", DEV),
        ("held_out", OLD_HELD_OUT),
        ("external", external.as_str()),
    ] {
        if (split == "both" && name == "external") || (split != "both" && split != name) {
            continue;
        }
        let intents: Vec<Value> = serde_json::from_str(text).unwrap();
        let mut report = evaluate(name, &intents, &tools, &index, 25);
        // Capture the actual first-search response, including all model guidance.
        // A fresh guard per intent prevents repeated searches from truncating results.
        for (row, intent) in report["rows"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(&intents)
        {
            let mut arguments = json!({"query":intent["query"]});
            if limit != 0 {
                arguments["limit"] = json!(limit);
            }
            if let Some(server) = intent["server"].as_str() {
                arguments["server"] = json!(server);
            }
            let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"toolport_search_tools","arguments":arguments}});
            let started = Instant::now();
            let response = handle_request_with_cancel(
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
                None,
                None,
            )
            .unwrap();
            row["dispatch_latency_us"] = json!(started.elapsed().as_secs_f64() * 1e6);
            row["response_text"] = response["result"]["content"][0]["text"].clone();
            assert!(row["response_text"].is_string(), "{response}");
            let entries = response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .split_once("\n\n")
                .unwrap()
                .1;
            let mut menu: Value = serde_json::from_str(entries).unwrap();
            for entry in menu.as_array_mut().unwrap() {
                if let Some(row) = entry.as_array_mut() { row.truncate(3); }
                else { entry.as_object_mut().unwrap().remove("inputSchema"); }
            }
            row["menu_tokens"] = json!(tokenizer
                .encode_ordinary(&serde_json::to_string(&menu).unwrap())
                .len());
            row["response_tokens"] = json!(tokenizer
                .encode_ordinary(row["response_text"].as_str().unwrap())
                .len());
        }
        let mut dispatch: Vec<f64> = report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["dispatch_latency_us"].as_f64().unwrap())
            .collect();
        dispatch.sort_by(f64::total_cmp);
        report["dispatch_latency_p95_us"] =
            json!(dispatch[((dispatch.len() - 1) as f64 * 0.95).ceil() as usize]);
        let mut tokens: Vec<u64> = report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["response_tokens"].as_u64().unwrap())
            .collect();
        tokens.sort();
        let pct = |p: f64| tokens[((tokens.len() - 1) as f64 * p).ceil() as usize];
        report["tokens"] = json!({"tokenizer":"o200k_base","p50":pct(0.5),"p95":pct(0.95),"max":tokens.last().unwrap()});
        let mut summary = report.clone();
        summary.as_object_mut().unwrap().remove("rows");
        eprintln!("{summary}");
        reports.push(report);
    }
    assert!(conduit_lib::telemetry::flush_for_test(Duration::from_secs(
        10
    )));
    let path = std::env::var("SEARCH_SCALE_OUTPUT").expect("SEARCH_SCALE_OUTPUT required");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&json!({"tools":tools.len(),
        "index_auxiliary_bytes":index.estimated_auxiliary_bytes(),"index_build_ms":index_build_ms,"limit":limit,"reports":reports}))
        .unwrap(),
    )
    .unwrap();
    drop(env);
}

#[test]
fn search_respects_negative_constraints_and_unicode() {
    let tools = vec![
        json!({"name":"disk__read_text","description":"Read text","inputSchema":{}}),
        json!({"name":"disk__update_text","description":"Modify text","inputSchema":{}}),
    ];
    let outcome = search_catalog_with(&tools, "read text, don't modify text", None, 3, None);
    assert_eq!(outcome.matches[0]["name"], "disk__read_text");
    assert_eq!(
        positive_query("İ read text without modifying it"),
        "İ read text "
    );
}

#[test]
fn search_uses_bounded_parameter_evidence() {
    let tools = vec![
        json!({"name":"one__inspect_left","description":"Inspect a resource",
            "inputSchema":{"properties":{"timestamp":{"type":"string"}}}}),
        json!({"name":"one__inspect_right","description":"Inspect a resource",
            "inputSchema":{"properties":{"checksum":{"type":"string"}}}}),
    ];
    let outcome = search_catalog_with(&tools, "inspect checksum", Some("one"), 3, None);
    assert_eq!(outcome.matches[0]["name"], "one__inspect_right");
    let huge =
        json!({"description":(0..2000).map(|i|format!("word{i}")).collect::<Vec<_>>().join(" ")});
    assert!(schema_search_tokens(Some(&huge)).len() <= 256);
}

#[test]
fn search_collection_names_preserve_plural_intent() {
    let tools = vec![
        json!({"name":"agenda__getEvent","description":"Retrieve an event","inputSchema":{}}),
        json!({"name":"agenda__getEvents","description":"Retrieve events","inputSchema":{}}),
    ];
    let outcome = search_catalog_with(&tools, "get events", Some("agenda"), 3, None);
    assert_eq!(outcome.matches[0]["name"], "agenda__getEvents");
}

#[test]
fn search_does_not_claim_a_winner_for_equal_providers() {
    let tools = vec![
        json!({"name":"one__create_ticket","description":"Create a ticket","inputSchema":{}}),
        json!({"name":"two__create_ticket","description":"Create a ticket","inputSchema":{}}),
    ];
    let ambiguous = search_catalog_with(&tools, "create ticket", None, 3, None);
    assert!(ambiguous.low_confidence);
    assert_eq!(ambiguous.matches.len(), 2);
    let scoped = search_catalog_with(&tools, "create ticket", Some("two"), 3, None);
    assert!(!scoped.low_confidence);
    assert_eq!(scoped.matches[0]["name"], "two__create_ticket");
}

#[test]
fn search_provider_filters_keep_camelcase_identity() {
    let tools = vec![
        json!({"name":"codehub__create_comment","description":"Create comment","inputSchema":{}}),
        json!({"name":"other__add_comment","description":"Add comment","inputSchema":{}}),
    ];
    let outcome = search_catalog_with(&tools, "add a comment on CodeHub", Some("codehub"), 3, None);
    assert_eq!(outcome.matches[0]["name"], "codehub__create_comment");
}

// Fixed before held-out scoring. These are retrieval gates, not execution claims.
fn assert_quality(report: &Value) {
    for (metric, minimum) in [
        ("top1_rate", 0.85),
        ("top3_rate", 0.90),
        ("mrr_at_25", 0.88),
        ("no_match_honesty", 0.95),
        ("ambiguity_honesty", 0.75),
    ] {
        assert!(
            report[metric].as_f64().unwrap() >= minimum,
            "{} {metric} below {minimum}: {}",
            report["split"],
            report[metric]
        );
    }
    if let Some(precision) = report["no_match_precision"].as_f64() {
        assert!(
            precision >= 0.90,
            "{} no-match precision {precision}",
            report["split"]
        );
    }
}

#[test]
fn search_scale_legacy_development_measurement() {
    let tools = scale_catalog();
    let mut intents: Vec<Value> = serde_json::from_str(DEV).unwrap();
    intents.extend(serde_json::from_str::<Vec<Value>>(OLD_HELD_OUT).unwrap());
    let index = CatalogSearchIndex::build(&tools);
    let mut report = evaluate("legacy development", &intents, &tools, &index, 0);
    report.as_object_mut().unwrap().remove("rows");
    eprintln!("{report}");
}

// The round-1 held-out labels have been seen and are now development data.
// A separate author supplies a new blind set only to the lead's scoring run.
#[test]
fn search_scale_blind_quality_gate() {
    let Some(path) = std::env::var_os("TOOLPORT_SEARCH_BLIND_INTENTS") else {
        eprintln!("blind search gate skipped: TOOLPORT_SEARCH_BLIND_INTENTS is absent");
        return;
    };
    let tools = scale_catalog();
    let intents: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!intents.is_empty(), "blind intents must not be empty");
    let index = CatalogSearchIndex::build(&tools);
    assert_quality(&evaluate("blind", &intents, &tools, &index, 0));
}

#[test]
fn search_scale_labels_are_frozen_and_families_are_disjoint() {
    use sha2::{Digest, Sha256};
    for (name, content) in [
        ("catalog.json", SCALE_CATALOG),
        ("dev-v2.json", DEV_V2),
        ("dev-v2-split.json", DEV_V2_SPLIT),
        ("dev.json", DEV),
        ("held_out.json", OLD_HELD_OUT),
        (
            "sources.json",
            include_str!("../../tests/fixtures/search-eval-scale/sources.json"),
        ),
    ] {
        let expected = format!("{:x}  {name}", Sha256::digest(content.as_bytes()));
        assert!(
            include_str!("../../tests/fixtures/search-eval-scale/frozen.sha256")
                .lines()
                .any(|line| line == expected),
            "{name} changed after label freeze"
        );
    }
    let dev: Vec<Value> = serde_json::from_str(DEV).unwrap();
    let test: Vec<Value> = serde_json::from_str(OLD_HELD_OUT).unwrap();
    let families: HashSet<&str> = dev
        .iter()
        .map(|intent| intent["family"].as_str().unwrap())
        .collect();
    assert!(test
        .iter()
        .all(|intent| !families.contains(intent["family"].as_str().unwrap())));
    assert_eq!(dev.len() + test.len(), 408);
}

// Standalone release measurement, separate from tokenization and dispatch allocations.
#[test]
#[ignore = "explicit cold resource benchmark on Linux"]
#[cfg(target_os = "linux")]
fn search_scale_resources() {
    let rss = || -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap()
    };
    let before_catalog = rss();
    let tools = scale_catalog();
    let after_catalog = rss();
    let start = Instant::now();
    let index = CatalogSearchIndex::build(&tools);
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after_build = rss();
    let mut intents: Vec<Value> = serde_json::from_str(DEV).unwrap();
    intents.extend(serde_json::from_str::<Vec<Value>>(OLD_HELD_OUT).unwrap());
    let report = evaluate("dev", &intents, &tools, &index, 0);
    let after_search = rss();
    let summary = json!({"before_catalog_kib":before_catalog,"after_catalog_kib":after_catalog,
        "after_build_kib":after_build,"after_search_kib":after_search,"index_build_ms":build_ms,
        "index_auxiliary_bytes":index.estimated_auxiliary_bytes(),"latency_p50_us":report["latency_p50_us"],"latency_p95_us":report["latency_p95_us"]});
    eprintln!("{summary}");
    std::fs::write(
        std::env::var_os("SEARCH_RESOURCE_OUTPUT").unwrap(),
        summary.to_string(),
    )
    .unwrap();
}

#[test]
fn search_local_fusion_recovers_semantic_only_candidates_and_rejects_weak_signal() {
    let unrelated = json!({"name":"service__one"});
    let semantic = json!({"name":"service__two"});
    let lex = vec![(0.0, &unrelated), (0.0, &semantic)];
    assert!(local_search_fusion(&lex, &[0.1, 0.2]).is_empty());
    assert_eq!(
        local_search_fusion(&lex, &[0.1, 0.8])[0].1["name"],
        "service__two"
    );
    let tied = vec![(1.0, &unrelated), (1.0, &semantic)];
    assert_eq!(
        local_search_fusion(&tied, &[0.8, 0.8])[0].1["name"],
        "service__one"
    );
}

#[test]
fn search_compact_candidates_expose_required_params_without_partial_schemas() {
    let tool = json!({"name":"calendar__create_event","description":"Create an event", "inputSchema":{
        "type":"object","required":["title","start"],"properties":{
        "title":{"type":"string"},"start":{"type":"string"},"optional":{"type":"number"}}}});
    let results = project_search_results(&[&tool, &tool], true);
    assert_eq!(results[0]["inputSchema"], tool["inputSchema"]);
    assert!(results[1].get("inputSchema").is_none());
    assert_eq!(results[1]["requiredParams"], json!(["title", "start"]));
}

#[test]
fn search_giant_schema_describe_is_lossless_and_owner_scoped() {
    let _env = DataDirTestEnv::new("search-schema-describe");
    let schema = json!({"type":"object","properties":{"payload":{"type":"string","description":"Z".repeat(180_000)}}});
    let tools = vec![
        json!({"name":"archive__put_payload","description":"Store payload","inputSchema":schema}),
    ];
    let fuzzy = search_catalog_with(&tools, "store payload", Some("archive"), 5, None);
    assert_eq!(fuzzy.matches[0]["inputSchema"], schema);
    assert!(fuzzy.matches[0].get("schemaOmitted").is_none());
    let index = CatalogSearchIndex::build(&tools);
    let host = dispatch_host(false);
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"toolport_search_tools","arguments":{"query":"archive__put_payload"}}});
    let response = handle_request_with_cancel(
        &host,
        &request,
        &Registry::default(),
        &router(),
        &tools,
        DiscoveryMode::Lazy,
        None,
        &SearchGuard::default(),
        None,
        None,
        Some("alice"),
        None,
        Some(&index),
        None,
        None,
    )
    .unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Toolport shaped this result"), "{text}");
    assert!(!text.contains("complete schema"));
    let cursor_start = text.find("\"cursor\":\"").unwrap() + "\"cursor\":\"".len();
    let cursor = text[cursor_start..].split('"').next().unwrap();
    let other = shaping::fetch_result(cursor, 0, usize::MAX, Some("bob"), None);
    assert_eq!(other["isError"], true);
    let full = shaping::fetch_result(cursor, 0, usize::MAX, Some("alice"), None);
    assert_eq!(full["isError"], false, "{full}");
    let text = full["content"][0]["text"].as_str().unwrap();
    let array = text
        .split_once("\n\n")
        .unwrap()
        .1
        .split("\n\n[Toolport: end of result")
        .next()
        .unwrap();
    let recovered: Vec<Value> = serde_json::from_str(array).unwrap();
    assert_eq!(recovered[0]["inputSchema"], schema);
}

#[test]
fn search_exact_case_collisions_keep_routing_identity() {
    let tools = vec![
        json!({"name":"service__Fetch","description":"Read resource","inputSchema":{"title":"upper"}}),
        json!({"name":"service__fetch","description":"Read resource","inputSchema":{"title":"lower"}}),
    ];
    assert_eq!(
        search_catalog_with(&tools, "service__Fetch", None, 5, None).matches,
        vec![tools[0].clone()]
    );
    assert_eq!(
        search_catalog_with(&tools, "service__fetch", None, 5, None).matches,
        vec![tools[1].clone()]
    );
    let ambiguous = search_catalog_with(&tools, "service__FETCH", None, 5, None);
    assert_eq!(ambiguous.matches.len(), 2);
    assert!(ambiguous.low_confidence);
}

#[test]
#[ignore = "explicit offline rank calibration, without dispatch allocations"]
fn search_scale_rank_measure() {
    let tools = scale_catalog();
    let intents: Vec<Value> = serde_json::from_slice(
        &std::fs::read(std::env::var_os("SEARCH_SCALE_INTENTS").unwrap()).unwrap(),
    )
    .unwrap();
    let index = CatalogSearchIndex::build(&tools);
    let report = evaluate("external", &intents, &tools, &index, 25);
    std::fs::write(
        std::env::var_os("SEARCH_SCALE_RANK_OUTPUT").unwrap(),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let mut summary = report;
    summary.as_object_mut().unwrap().remove("rows");
    eprintln!("{summary}");
}

#[test]
fn search_provider_mentions_are_preferences_and_never_spelling_targets() {
    let tools = vec![
        json!({"name":"fetch__fetch","description":"Fetch a URL","inputSchema":{}}),
        json!({"name":"code__list_pull_requests","description":"Fetch open pull requests","inputSchema":{}}),
        json!({"name":"notion__read_record","description":"Read record","inputSchema":{}}),
    ];
    let index = CatalogSearchIndex::build(&tools);
    assert!(!index.spelling_frequency.contains_key("notion"));
    let result = search_catalog_indexed(
        &tools,
        "fetch my open pull requests",
        None,
        25,
        None,
        Some(&index),
    );
    assert_eq!(result.matches[0]["name"], tools[1]["name"]);
    assert!(result
        .matches
        .iter()
        .any(|tool| tool["name"] == tools[0]["name"]));
    assert!(query_names_server("read from my_store", "my_store"));
    assert!(!query_names_server("my_store_key", "my_store"));
    assert!(!query_names_server("motion", "notion"));
}

#[test]
fn search_negation_preserves_later_positive_clauses() {
    let tools = vec![
        json!({"name":"code__delete_branch","description":"Delete a branch","inputSchema":{}}),
        json!({"name":"code__list_branches","description":"List branches","inputSchema":{}}),
    ];
    for query in [
        "do not delete anything, just list the branches",
        "do not delete anything but list branches",
        "list branches and do not delete anything",
    ] {
        let result = search_catalog_with(&tools, query, None, 25, None);
        assert_eq!(result.matches[0]["name"], tools[1]["name"], "{query}");
    }
}

#[test]
fn search_works_while_background_vectors_are_pending() {
    let tools =
        vec![json!({"name":"records__read_entry","description":"Read an entry","inputSchema":{}})];
    let mut index = CatalogSearchIndex::build(&tools);
    let ready = std::mem::take(&mut index.semantic_vectors);
    let cold = search_catalog_indexed(&tools, "read entry", Some("records"), 8, None, Some(&index));
    assert_eq!(cold.matches[0]["name"], tools[0]["name"]);
    index.semantic_vectors = ready;
    let warm = search_catalog_indexed(&tools, "read entry", Some("records"), 8, None, Some(&index));
    assert_eq!(warm.matches[0]["name"], tools[0]["name"]);
}

#[test]
fn search_spelling_correction_is_unique_and_preserves_identifiers() {
    let words: HashMap<String, usize> = [("reader".into(), 2), ("render".into(), 2)]
        .into_iter()
        .collect();
    assert_eq!(
        corrected_query_token("reaedr", &words).as_deref(),
        Some("reader")
    );
    assert_eq!(corrected_query_token("reder", &words), None);
    assert_eq!(corrected_query_token("reader_123", &words), None);
}

fn expected_tool_family(name: &str) -> String {
    let (server, operation) = name.split_once("__").unwrap();
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .flat_map(split_camel)
            .collect()
    };
    let mut parts = words(operation);
    let prefix = words(server);
    if parts.starts_with(&prefix) {
        parts.drain(..prefix.len());
    }
    let resource = parts
        .get(1)
        .or_else(|| parts.first())
        .map_or(operation.to_string(), |word| stem_token(word));
    format!("{server}__{resource}")
}

// Development self-check groups resource families and their alternatives together.
fn dev_v2_self_check() -> Vec<Value> {
    use sha2::{Digest, Sha256};
    let manifest: Value = serde_json::from_str(DEV_V2_SPLIT).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(DEV_V2)),
        manifest["fixture_sha256"]
    );
    let intents: Vec<Value> = serde_json::from_str(DEV_V2).unwrap();
    let ids = |field: &str| -> HashSet<&str> {
        manifest[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect()
    };
    let selected = ids("self_check_ids");
    let tuning = ids("tuning_ids");
    assert!(selected.is_disjoint(&tuning));
    assert_eq!(selected.len() + tuning.len(), intents.len());
    let tools = |ids: &HashSet<&str>| -> HashSet<&str> {
        intents
            .iter()
            .filter(|intent| ids.contains(intent["id"].as_str().unwrap()))
            .flat_map(|intent| {
                intent["expected"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap())
            })
            .collect()
    };
    assert!(
        tools(&selected).is_disjoint(&tools(&tuning)),
        "expected tools leak between subsets"
    );
    let mut families = [HashSet::new(), HashSet::new()];
    for intent in &intents {
        let side = usize::from(selected.contains(intent["id"].as_str().unwrap()));
        for name in intent["expected"].as_array().unwrap() {
            families[side].insert(expected_tool_family(name.as_str().unwrap()));
        }
    }
    assert!(
        families[0].is_disjoint(&families[1]),
        "expected resource families leak between subsets"
    );
    intents
        .iter()
        .filter(|intent| selected.contains(intent["id"].as_str().unwrap()))
        .cloned()
        .collect()
}

#[test]
fn search_dev_v2_quality_gate() {
    let tools = scale_catalog();
    let index = CatalogSearchIndex::build(&tools);
    let report = evaluate("dev-v2 self-check", &dev_v2_self_check(), &tools, &index, 0);
    // Recall floors are set below the measured baseline with room for general
    // retrieval tradeoffs. Confidence is informational, never an accuracy claim.
    assert!(
        report["top3_rate"].as_f64().unwrap() >= 0.55,
        "recall@3 {}",
        report["top3_rate"]
    );
    assert!(
        report["recall_at"]["10"]["rate"].as_f64().unwrap() >= 0.75,
        "{}",
        report["recall_at"]["10"]
    );
    assert!(
        report["no_match_honesty"].as_f64().unwrap() >= 0.95,
        "{}",
        report["no_match_honesty"]
    );
    if let Some(precision) = report["no_match_precision"].as_f64() {
        assert!(precision >= 0.90);
    }
}

#[test]
fn search_scoped_requests_reuse_full_index_and_never_encode() {
    let _env = DataDirTestEnv::new("search-scoped-index");
    let tools = vec![
        json!({"name":"one__read_record","description":"Read a record","inputSchema":{}}),
        json!({"name":"two__read_record","description":"Read a record","inputSchema":{}}),
        json!({"name":"one__app_record","description":"Read a record", "_meta":{"ui":{"visibility":["app"],"resourceUri":"ui://sample"}},"inputSchema":{}}),
    ];
    let index = CatalogSearchIndex::build(&tools);
    let host = dispatch_host(false);
    let router = router();
    let before = SEARCH_INDEX_BUILDS.with(|count| count.get());
    let mut registry = Registry::default();
    registry.servers.push(
        serde_json::from_value(json!({"id":"one","name":"One","transport":"stdio","enabled":true}))
            .unwrap(),
    );
    let allowed = ["one".to_string()].into_iter().collect();
    for _ in 0..3 {
        let response = handle_request_with_cancel(
            &host,
            &search_req("read record"),
            &registry,
            &router,
            &tools,
            DiscoveryMode::Lazy,
            None,
            &SearchGuard::default(),
            Some(&allowed),
            None,
            None,
            None,
            Some(&index),
            None,
            None,
        )
        .unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("one__read_record"));
        assert!(!text.contains("two__read_record"));
        assert!(!text.contains("one__app_record"));
    }
    assert_eq!(SEARCH_INDEX_BUILDS.with(|count| count.get()), before);
}

#[test]
#[ignore = "release latency comparison at both catalog sizes"]
fn search_scale_latency() {
    let fixture = scale_catalog();
    let intents: Vec<Value> = serde_json::from_str(DEV_V2).unwrap();
    let mut measurements = Vec::new();
    for size in [1707, 10000] {
        let tools: Vec<Value> = (0..size)
            .map(|position| {
                let mut tool = fixture[position % fixture.len()].clone();
                if position >= fixture.len() {
                    tool["name"] = json!(format!("{}_{position}", tool["name"].as_str().unwrap()));
                }
                tool
            })
            .collect();
        let index = CatalogSearchIndex::build(&tools);
        let mut times = Vec::new();
        for _ in 0..3 {
            for intent in &intents {
                let start = Instant::now();
                std::hint::black_box(search_catalog_indexed(
                    &tools,
                    intent["query"].as_str().unwrap(),
                    intent["server"].as_str(),
                    25,
                    None,
                    Some(&index),
                ));
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        measurements.push(json!({"tools":size,"samples":times.len(),"p50_ms":times[(times.len()-1)/2],"p95_ms":times[((times.len()-1) as f64*0.95).ceil() as usize]}));
    }
    eprintln!("{}", json!(measurements));
    std::fs::write(
        std::env::var_os("SEARCH_LATENCY_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(&measurements).unwrap(),
    )
    .unwrap();
}

#[test]
fn search_configured_display_names_are_soft_preferences() {
    let tools = vec![
        json!({"name":"private_store__read_record","description":"Read a record","inputSchema":{}}),
        json!({"name":"other__read_record","description":"Read a record","inputSchema":{}}),
    ];
    let index = CatalogSearchIndex::build(&tools);
    let identities = vec![("private_store".into(), "My Store".into())];
    let result = search_catalog_filtered(
        &tools,
        "read record from My Store",
        None,
        25,
        None,
        Some(&index),
        |_| true,
        &identities,
    );
    assert_eq!(result.matches[0]["name"], tools[0]["name"]);
    assert_eq!(result.matches.len(), 2);
    assert!(!query_names_server("my-store-key", "my-store"));
}

#[test]
fn search_negation_preserves_camelcase_and_unicode() {
    assert_eq!(
        positive_query("getItem AND do not deleteAnything"),
        "getItem "
    );
    assert_eq!(
        positive_query("do not deleteAnything, readTextFile"),
        "  readTextFile"
    );
    assert_eq!(positive_query("İ getItem without deleting"), "İ getItem ");
}

#[test]
fn search_top_schema_factoring_preserves_every_constraint_and_instance_data() {
    let parameter = json!({"type":"object", "description":"Detailed parameter documentation. ".repeat(100),
        "properties":{"kind":{"enum":["left","right"]}, "payload":{"type":"string","minLength":2}},
        "required":["kind","payload"],"additionalProperties":false});
    let schema = json!({"type":"object","properties":{"first":parameter,"second":parameter},
        "required":["first","second"],"default":{"properties":{"first":parameter}}});
    let mut compact = compact_search_schema(schema.clone());
    assert!(
        serde_json::to_vec(&compact).unwrap().len() < serde_json::to_vec(&schema).unwrap().len()
    );
    assert_eq!(compact["default"], schema["default"]);
    conduit_lib::router::inline_refs(&mut compact);
    assert_eq!(compact, schema);
}

#[test]
fn search_schema_factoring_is_lossless_on_the_public_catalog() {
    for tool in scale_catalog() {
        let schema = &tool["inputSchema"];
        let mut compact = compact_search_schema(schema.clone());
        if compact.get("$defs").is_some() && schema.get("$defs").is_none() {
            conduit_lib::router::inline_refs(&mut compact);
        }
        assert_eq!(&compact, schema, "schema changed for {}", tool["name"]);
    }
}
