//! MCP specification conformance + backward-compatibility harness (SOU-443).
//!
//! Toolport is simultaneously an MCP *server* (to AI clients) and an MCP *client*
//! (to downstream servers), so every protocol revision change lands on it twice.
//! The gateway must stay **dual-era**: speaking `2026-07-28` upward while still
//! driving `initialize`-era servers downward.
//!
//! This file is the regression net for that work. It has three jobs:
//!
//! 1. **Validate the fixture.** `mock-mcp-server` can impersonate any revision;
//!    these tests prove it actually behaves era-correctly, so later tests can
//!    trust it as a reference implementation.
//! 2. **Pin today's wire format.** `downstream_transcript_pins_current_wire_format`
//!    records the exact JSON-RPC the gateway emits downstream. Any change to it
//!    is then a deliberate edit to this file, never an accident.
//! 3. **Prove the dual-era guarantees.** Both directions are covered: Toolport
//!    connecting to a modern server and serving a modern client, each paired with
//!    a test that the legacy path sees byte-identical traffic.
//!
//! Gaps were tracked here as `#[ignore]`d acceptance criteria while the work was
//! in flight, which is a good pattern to reuse. Every test in this file runs
//! today.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conduit_lib::downstream::{
    DownstreamServer, HttpTransport, MrtrRequest, ServerRequestAction, ServerRequestHandler,
    StdioTransport, Transport, TransportError,
};
use serde_json::{json, Value};

const MODERN: &str = "2026-07-28";
const LEGACY_REVISIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// Historical default the fixture must keep when no revision is requested, so
/// the pre-existing integration tests are unaffected by the multi-revision work.
const FIXTURE_DEFAULT: &str = "2025-06-18";

fn mock_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock-mcp-server")
}

/// A unique scratch path per call. Avoids a `tempfile` dev-dependency for what
/// is only ever a few lines of JSONL.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "toolport-conformance-{tag}-{}-{n}.jsonl",
        std::process::id()
    ))
}

fn env_for(revision: Option<&str>, strict: bool, transcript: Option<&str>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(rev) = revision {
        env.push(("MOCK_MCP_REVISION".to_string(), rev.to_string()));
    }
    if strict {
        env.push(("MOCK_MCP_STRICT".to_string(), "1".to_string()));
    }
    if let Some(path) = transcript {
        env.push(("MOCK_MCP_TRANSCRIPT".to_string(), path.to_string()));
    }
    env
}

/// A fixture serving Streamable HTTP, killed when the handle drops.
struct HttpFixture {
    child: std::process::Child,
    url: String,
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn the fixture in HTTP mode and wait for it to report its ephemeral port.
///
/// The stdio fixture cannot express header rules at all, which is exactly how a
/// hardcoded `MCP-Protocol-Version` shipped past a green stdio suite. Anything
/// about headers has to be tested here.
fn spawn_http_fixture(revision: &str, strict: bool, transcript: Option<&str>) -> HttpFixture {
    use std::io::BufRead;

    let mut cmd = std::process::Command::new(mock_bin());
    cmd.env("MOCK_MCP_HTTP", "1")
        .env("MOCK_MCP_REVISION", revision)
        .stdout(std::process::Stdio::piped());
    if strict {
        cmd.env("MOCK_MCP_STRICT", "1");
    }
    if let Some(path) = transcript {
        cmd.env("MOCK_MCP_TRANSCRIPT", path);
    }
    let mut child = cmd.spawn().expect("spawn http fixture");
    let stdout = child.stdout.take().expect("fixture stdout");
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .expect("fixture should announce its url");
    let url = line
        .trim()
        .strip_prefix("MOCK_MCP_URL=")
        .expect("fixture should print MOCK_MCP_URL=<url>")
        .to_string();
    HttpFixture { child, url }
}

/// A raw transport to the fixture, bypassing `DownstreamServer::connect` so a
/// test can drive the handshake itself and observe era behaviour directly.
fn raw_transport(env: &[(String, String)]) -> StdioTransport {
    let dirty = Arc::new(AtomicU8::new(0));
    StdioTransport::spawn_watched(mock_bin(), &[], env, None, dirty, None).expect("spawn fixture")
}

/// JSON-RPC error objects are carried structurally by `TransportError::Rpc`
/// since SOU-445, so a test can read the `code` directly. This used to re-parse
/// a flattened string, which is exactly the lossiness that made the
/// backward-compatibility ladder unimplementable.
fn error_json(err: &TransportError) -> Value {
    let TransportError::Rpc(obj) = err else {
        panic!("expected a JSON-RPC error response, got {err:?}");
    };
    obj.clone()
}

fn read_transcript(path: &std::path::Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn methods_of(transcript: &[Value]) -> Vec<String> {
    transcript
        .iter()
        .filter_map(|r| r.get("method").and_then(|m| m.as_str()).map(String::from))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Fixture validation
// ---------------------------------------------------------------------------

/// The multi-revision work must not change what an un-configured fixture does,
/// because `list_changed`, `circuit_breaker`, and `root_cwd` all depend on it.
#[test]
fn fixture_default_revision_is_unchanged() {
    let mut t = raw_transport(&env_for(None, false, None));
    let init = t
        .request("initialize", json!({ "protocolVersion": FIXTURE_DEFAULT, "capabilities": {} }))
        .expect("default fixture should answer initialize");
    assert_eq!(init["protocolVersion"], FIXTURE_DEFAULT);
}

#[test]
fn fixture_reports_each_legacy_revision() {
    for rev in LEGACY_REVISIONS {
        let mut t = raw_transport(&env_for(Some(rev), false, None));
        let init = t
            .request("initialize", json!({ "protocolVersion": rev, "capabilities": {} }))
            .unwrap_or_else(|e| panic!("{rev} fixture should answer initialize: {e}"));
        assert_eq!(init["protocolVersion"], rev, "fixture pinned to {rev}");

        // Legacy revisions must NOT answer server/discover: that is precisely the
        // signal the gateway's stdio fallback probe keys on (SOU-445).
        //
        // Note *how* it fails. Like many real stdio servers, the fixture simply
        // does not reply to a method it does not know, so the probe ends in a
        // read timeout rather than a JSON-RPC error. Silence is therefore a third
        // outcome the era probe must handle, alongside "recognized modern error"
        // (stay modern) and "some other error" (fall back). A probe without a
        // bounded timeout would hang on every legacy stdio server it meets.
        t.set_read_timeout(Duration::from_millis(500));
        assert!(
            t.request("server/discover", json!({})).is_err(),
            "{rev} is legacy and must not implement server/discover"
        );
    }
}

/// Icons landed in 2025-11-25 (SEP-973). They ride on the tool definition, so a
/// gateway that rebuilds tool objects field-by-field would silently drop them.
#[test]
fn fixture_advertises_icons_from_2025_11_25() {
    let mut t = raw_transport(&env_for(Some("2025-11-25"), false, None));
    t.request("initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": {} }))
        .expect("initialize");
    let tools = t.request("tools/list", json!({})).expect("tools/list");
    let echo = tools["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "echo")
        .expect("echo tool");
    assert!(echo.get("icons").is_some(), "2025-11-25 fixture should carry icons");

    // ...and must not appear on older revisions, so a test can tell eras apart.
    let mut old = raw_transport(&env_for(Some("2025-06-18"), false, None));
    old.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }))
        .expect("initialize");
    let old_tools = old.request("tools/list", json!({})).expect("tools/list");
    let old_echo = old_tools["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "echo")
        .expect("echo tool");
    assert!(old_echo.get("icons").is_none(), "2025-06-18 predates icons");
}

/// A modern server has no handshake. The spec asks it to name its supported
/// versions in the error, because legacy clients have no fall-forward mechanism
/// and this may be the only diagnostic a user ever sees.
#[test]
fn modern_fixture_rejects_initialize_and_names_supported_versions() {
    let mut t = raw_transport(&env_for(Some(MODERN), true, None));
    let err = t
        .request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }))
        .expect_err("a modern server must not implement initialize");
    let err = error_json(&err);
    assert_eq!(err["code"], -32601, "unknown method");
    assert_eq!(err["data"]["supported"][0], MODERN);
}

#[test]
fn modern_fixture_answers_server_discover() {
    let mut t = raw_transport(&env_for(Some(MODERN), true, None));
    let result = t
        .request(
            "server/discover",
            json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } }),
        )
        .expect("a modern server MUST implement server/discover");

    assert_eq!(result["supportedVersions"][0], MODERN);
    assert_eq!(result["resultType"], "complete", "every result carries resultType");
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "mock-mcp-server"
    );
    // server/discover is a cacheable operation.
    assert!(result.get("ttlMs").is_some(), "CacheableResult.ttlMs");
    assert_eq!(result["cacheScope"], "public");
}

/// Every modern request declares its version in `_meta`; a server that does not
/// implement it MUST reply `-32022` listing what it does support.
#[test]
fn modern_fixture_rejects_request_without_protocol_version() {
    let mut t = raw_transport(&env_for(Some(MODERN), true, None));

    let err = error_json(&t.request("tools/list", json!({})).expect_err("missing _meta version"));
    assert_eq!(err["code"], -32022);
    assert_eq!(err["data"]["supported"][0], MODERN);

    let err = error_json(
        &t.request(
            "tools/list",
            json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "1900-01-01" } }),
        )
        .expect_err("unknown version"),
    );
    assert_eq!(err["code"], -32022);
    assert_eq!(err["data"]["requested"], "1900-01-01");

    // The same request with the right version succeeds, proving the gate is the
    // version and not the method.
    let ok = t
        .request(
            "tools/list",
            json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } }),
        )
        .expect("correct version should pass");
    assert_eq!(ok["resultType"], "complete");
}

#[test]
fn strict_legacy_fixture_requires_initialize_first() {
    let mut t = raw_transport(&env_for(Some("2025-06-18"), true, None));
    let err = error_json(&t.request("tools/list", json!({})).expect_err("handshake not done"));
    assert_eq!(err["code"], -32600);

    t.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }))
        .expect("initialize");
    t.notify("notifications/initialized", json!({})).expect("initialized");
    assert!(t.request("tools/list", json!({})).is_ok(), "handshake complete");
}

// ---------------------------------------------------------------------------
// 2. Pin today's downstream wire format
// ---------------------------------------------------------------------------

/// Records exactly what Toolport sends to a downstream server for a fixed
/// scenario. This is the regression net: SOU-444 and SOU-445 both rewrite this
/// path, and this test makes any change to the emitted bytes an explicit,
/// reviewed edit rather than a silent behavioural drift.
#[test]
fn downstream_transcript_pins_current_wire_format() {
    let path = scratch_path("wire");
    let _ = std::fs::remove_file(&path);
    let env = env_for(None, false, Some(&path.to_string_lossy()));

    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    server.load_resources_prompts();
    server.call("echo", json!({ "text": "hi" })).expect("echo call");
    drop(server);

    let transcript = read_transcript(&path);
    let methods = methods_of(&transcript);

    // The handshake still opens the conversation, and it still declares a
    // version. When SOU-445 lands, a dual-era gateway will probe with
    // `server/discover` first and only fall back to `initialize` here.
    assert_eq!(methods.first().map(String::as_str), Some("initialize"));
    let init = &transcript[0];
    assert!(
        init["params"].get("protocolVersion").is_some(),
        "initialize must declare a protocol version"
    );

    for expected in ["tools/list", "resources/list", "prompts/list", "tools/call"] {
        assert!(methods.iter().any(|m| m == expected), "missing {expected} in {methods:?}");
    }

    // The `tools/call` envelope for a call carrying no client metadata. Since
    // SOU-444 the gateway attaches `_meta` only when there is something relayable
    // to attach, so this shape is byte-identical to what Toolport sent before
    // that work: an unchanged request for every server that never sees `_meta`.
    let call = transcript
        .iter()
        .find(|r| r["method"] == "tools/call")
        .expect("tools/call recorded");
    let mut keys: Vec<&str> = call["params"]
        .as_object()
        .expect("params object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["arguments", "name"],
        "tools/call params shape changed; if this is SOU-444, update this assertion on purpose"
    );

    // The downstream name is the ORIGINAL tool name, not the namespaced exposed
    // name. SOU-450 depends on this: the `Mcp-Name` header must carry the name
    // actually sent on the wire.
    assert_eq!(call["params"]["name"], "echo");

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 3. Known gaps, encoded as acceptance criteria
// ---------------------------------------------------------------------------

/// Ask the fixture to reflect back whatever `_meta` reached it.
fn relayed_meta(client_meta: &Value) -> Value {
    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &[], None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    let result = server
        .call_with_cancel("echo_meta", json!({}), None, Some(client_meta))
        .expect("echo_meta call");
    result["structuredContent"]["receivedMeta"].clone()
}

/// SOU-444. `_meta` an upstream client sends must reach the downstream server,
/// including keys this build has never heard of. That "forward unknown by
/// default" property is what stops Toolport silently breaking future extensions
/// such as MCP Apps and Tasks.
#[test]
fn client_meta_reaches_downstream_server() {
    let received = relayed_meta(&json!({
        "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "com.example/somethingWeHaveNeverSeen": { "nested": [1, 2, 3] },
        "io.modelcontextprotocol/tasks": { "taskId": "t-1" }
    }));

    assert_eq!(
        received["traceparent"],
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "OTel trace context is explicitly meant to propagate across hops"
    );
    assert_eq!(
        received["com.example/somethingWeHaveNeverSeen"]["nested"][2], 3,
        "an unknown extension namespace must survive verbatim, got {received}"
    );
    assert_eq!(received["io.modelcontextprotocol/tasks"]["taskId"], "t-1");
}

/// The other half of relaying: keys that describe one hop must NOT be forwarded.
/// Toolport is the client on the downstream hop, so relaying the upstream
/// client's identity or capabilities would assert claims the gateway cannot
/// honour. SOU-445/SOU-446 replace these with Toolport's own values.
#[test]
fn per_hop_meta_keys_are_not_relayed() {
    let received = relayed_meta(&json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "SomeOtherClient", "version": "9.9.9" },
        "io.modelcontextprotocol/clientCapabilities": { "sampling": {} },
        "keepMe": true
    }));

    for key in [
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientInfo",
        "io.modelcontextprotocol/clientCapabilities",
    ] {
        assert!(
            received.get(key).is_none(),
            "{key} is per-hop and must not be relayed, got {received}"
        );
    }
    assert_eq!(received["keepMe"], true, "non-per-hop keys still travel");
}

/// A request whose `_meta` is entirely per-hop must not gain an empty `_meta`
/// object. Downstream servers that never see client metadata keep receiving
/// byte-identical requests.
#[test]
fn fully_stripped_meta_leaves_no_empty_object() {
    let received = relayed_meta(&json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28"
    }));
    assert!(
        received.is_null(),
        "no relayable keys should mean no _meta at all, got {received}"
    );
}

/// SOU-444 part 2. `progressToken` is relayed now that the gateway routes the
/// resulting `notifications/progress` back to the client that minted it.
#[test]
fn progress_token_reaches_downstream_server() {
    let received = relayed_meta(&json!({ "progressToken": "p-1" }));
    assert_eq!(received["progressToken"], "p-1");
}

/// The whole progress chain, not just its ends: a server emits
/// `notifications/progress` mid-call, the stdout drain recognises it, and the
/// bound sink receives it while the originating request is still in flight.
///
/// Before SOU-444 the drain dropped every notification it did not recognise, so
/// this traffic went nowhere.
#[test]
fn downstream_progress_notification_reaches_the_bound_sink() {
    let dirty = Arc::new(AtomicU8::new(0));
    let mut transport = StdioTransport::spawn_watched(mock_bin(), &[], &[], None, dirty, None)
        .expect("spawn fixture");

    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    transport.set_progress_sink(Some(Arc::new(move |note: Value| {
        sink_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(note);
    })));

    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");

    let result = server
        .call_with_cancel(
            "progress_ping",
            json!({}),
            None,
            Some(&json!({ "progressToken": "tok-e2e" })),
        )
        .expect("progress_ping call");
    assert_eq!(result["isError"], false, "the call itself still succeeds");

    // The notification is emitted before the response, so by the time the call
    // returns the drain has already seen it.
    let got = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(got.len(), 1, "expected exactly one progress notification, got {got:?}");
    assert_eq!(got[0]["method"], "notifications/progress");
    assert_eq!(
        got[0]["params"]["progressToken"], "tok-e2e",
        "the token must round-trip so the gateway can route it back"
    );
    assert_eq!(got[0]["params"]["total"], 2);
}

/// SOU-445. A dual-era gateway must connect to a modern, stateless server: fall
/// forward from the rejected `initialize` to `server/discover`, then carry the
/// protocol `_meta` on every subsequent request.
#[test]
fn gateway_connects_to_a_modern_server() {
    let path = scratch_path("modern");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some(MODERN), true, Some(&path.to_string_lossy()));

    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server = DownstreamServer::connect("mock".to_string(), Box::new(transport))
        .expect("a dual-era gateway must connect to a modern server");

    assert!(server.era().is_modern(), "era should be detected as modern");
    assert_eq!(server.era().version(), MODERN);

    // The connection is not merely established: it is usable. The strict fixture
    // rejects any request lacking the protocol `_meta`, so a successful call
    // proves the transport is stamping every request, not just the handshake.
    let result = server.call("echo", json!({ "text": "hi" })).expect("call a modern server");
    assert_eq!(result["content"][0]["text"], "hi");
    assert_eq!(result["resultType"], "complete");
    drop(server);

    let transcript = read_transcript(&path);
    let methods = methods_of(&transcript);

    // `initialize` is attempted once and rejected; the fall-forward is what makes
    // the connection work. No `notifications/initialized` is ever sent.
    assert_eq!(methods.first().map(String::as_str), Some("initialize"));
    assert!(
        methods.iter().any(|m| m == "server/discover"),
        "must fall forward to server/discover, got {methods:?}"
    );
    assert!(
        !methods.iter().any(|m| m == "notifications/initialized"),
        "a modern server has no handshake to complete, got {methods:?}"
    );

    // Every post-handshake request carries its own protocol version and identity.
    // Counted so the loop cannot pass by matching nothing.
    let mut checked = 0;
    for record in transcript.iter().filter(|r| r["method"] == "tools/list" || r["method"] == "tools/call")
    {
        checked += 1;
        let meta = &record["params"]["_meta"];
        assert_eq!(
            meta["io.modelcontextprotocol/protocolVersion"], MODERN,
            "every modern request declares its version, got {record}"
        );
        assert_eq!(
            meta["io.modelcontextprotocol/clientInfo"]["name"], "toolport-gateway",
            "Toolport identifies itself on the downstream hop, not the upstream client"
        );
    }
    assert!(
        checked >= 2,
        "expected the tools/list and tools/call records to be checked, saw {checked}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_client_is_shimmed_across_a_modern_mrtr_server() {
    let path = scratch_path("mrtr-legacy-shim");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some(MODERN), true, Some(&path.to_string_lossy()));
    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_request = Arc::clone(&seen);
    let handler: ServerRequestHandler = Arc::new(move |request| {
        seen_request.lock().unwrap().push(request.clone());
        Some(ServerRequestAction::Respond(json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": { "action": "accept", "content": { "approved": true } }
        })))
    });
    server.set_server_request_handler(handler);

    let result = server
        .call("mrtr_confirm", json!({}))
        .expect("legacy compatibility shim should complete the call");
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["content"][0]["text"], "confirmed");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["method"], "elicitation/create");
    drop(seen);
    drop(server);

    let transcript = read_transcript(&path);
    let calls: Vec<&Value> = transcript
        .iter()
        .filter(|request| {
            request["method"] == "tools/call"
                && request["params"]["name"] == "mrtr_confirm"
        })
        .collect();
    assert_eq!(calls.len(), 2, "MRTR retry must be a new request");
    assert_ne!(calls[0]["id"], calls[1]["id"], "retry id must change");
    assert_eq!(calls[1]["params"]["requestState"], "mock-state-byte-exact");
    assert_eq!(
        calls[1]["params"]["inputResponses"]["confirm"]["action"],
        "accept"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn modern_client_controls_native_mrtr_retry_fields() {
    let path = scratch_path("mrtr-native");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some(MODERN), true, Some(&path.to_string_lossy()));
    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": MODERN });

    let incomplete = server
        .call_with_cancel_and_mrtr("mrtr_confirm", json!({}), None, Some(&meta), None)
        .expect("native MRTR first round");
    assert_eq!(incomplete["resultType"], "input_required");
    assert_eq!(incomplete["requestState"], "mock-state-byte-exact");

    let retry = MrtrRequest {
        input_responses: Some(json!({
            "confirm": { "action": "accept", "content": { "approved": true } }
        })),
        request_state: Some(json!("mock-state-byte-exact")),
    };
    let complete = server
        .call_with_cancel_and_mrtr(
            "mrtr_confirm",
            json!({}),
            None,
            Some(&meta),
            Some(&retry),
        )
        .expect("native MRTR retry");
    assert_eq!(complete["resultType"], "complete");
    assert_eq!(complete["content"][0]["text"], "confirmed");
    drop(server);

    let transcript = read_transcript(&path);
    let calls: Vec<&Value> = transcript
        .iter()
        .filter(|request| {
            request["method"] == "tools/call"
                && request["params"]["name"] == "mrtr_confirm"
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0]["id"], calls[1]["id"]);
    assert_eq!(calls[1]["params"]["requestState"], retry.request_state.unwrap());
    assert_eq!(calls[1]["params"]["inputResponses"], retry.input_responses.unwrap());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn modern_client_resumes_legacy_stdio_hitl_without_replaying_the_tool() {
    let path = scratch_path("mrtr-legacy-downstream");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some("2025-11-25"), true, Some(&path.to_string_lossy()));
    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn legacy fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    server.set_server_request_handler(Arc::new(|request| {
        (request["method"] == "elicitation/create").then_some(ServerRequestAction::InputRequired)
    }));
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": MODERN });

    let incomplete = server
        .call_with_cancel_and_mrtr(
            "legacy_elicitation",
            json!({}),
            None,
            Some(&meta),
            None,
        )
        .expect("legacy server request should become MRTR");
    assert_eq!(incomplete["resultType"], "input_required");
    let state = incomplete["requestState"].clone();
    let requests = incomplete["inputRequests"]
        .as_object()
        .expect("inputRequests map");
    assert_eq!(requests.len(), 1);
    let key = requests.keys().next().unwrap().clone();
    assert_eq!(requests[&key]["method"], "elicitation/create");

    let retry = MrtrRequest {
        input_responses: Some(json!({
            key: { "action": "accept", "content": { "approved": true } }
        })),
        request_state: Some(state),
    };
    let complete = server
        .call_with_cancel_and_mrtr(
            "legacy_elicitation",
            json!({}),
            None,
            Some(&meta),
            Some(&retry),
        )
        .expect("MRTR retry should resume the suspended legacy request");
    assert_eq!(complete["content"][0]["text"], "legacy confirmed");
    drop(server);

    let transcript = read_transcript(&path);
    let calls: Vec<&Value> = transcript
        .iter()
        .filter(|request| {
            request["method"] == "tools/call"
                && request["params"]["name"] == "legacy_elicitation"
        })
        .collect();
    assert_eq!(calls.len(), 1, "the modern retry must not replay tools/call");
    let replies: Vec<&Value> = transcript
        .iter()
        .filter(|request| request["id"] == "mock-legacy-elicitation")
        .collect();
    assert_eq!(replies.len(), 1, "one input response resumes the legacy call");
    let _ = std::fs::remove_file(&path);
}

/// Proves the HTTP fixture's header gate actually fires. Without this, the
/// connect test below could pass on a fixture that validates nothing, which is
/// precisely the blind spot the stdio-only harness had.
#[test]
fn http_fixture_enforces_header_body_agreement() {
    let fixture = spawn_http_fixture(MODERN, true, None);
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } }
    });

    // Header disagrees with the body: the exact shape of the bug that shipped.
    let mismatched = ureq::post(&fixture.url)
        .set("MCP-Protocol-Version", "2025-06-18")
        .set("Mcp-Method", "tools/list")
        .send_json(body.clone());
    match mismatched {
        Err(ureq::Error::Status(400, resp)) => {
            let err: Value = resp.into_json().expect("json error body");
            assert_eq!(
                err["error"]["code"], -32020,
                "a header/body mismatch must be HeaderMismatch, got {err}"
            );
        }
        other => panic!("expected 400 HeaderMismatch, got {other:?}"),
    }

    // Absent header is equally invalid under this revision.
    let missing = ureq::post(&fixture.url).send_json(body.clone());
    assert!(
        matches!(missing, Err(ureq::Error::Status(400, _))),
        "a missing MCP-Protocol-Version must be rejected"
    );

    // ...and the matching header is accepted, so the gate is not simply refusing
    // everything.
    let ok = ureq::post(&fixture.url)
        .set("MCP-Protocol-Version", MODERN)
        .set("Mcp-Method", "tools/list")
        .send_json(body)
        .expect("agreeing header and body must be accepted");
    let parsed: Value = ok.into_json().expect("json body");
    assert!(parsed["result"]["tools"].is_array(), "got {parsed}");

    let missing_method_header_body = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } }
    });
    let missing_method = ureq::post(&fixture.url)
        .set("MCP-Protocol-Version", MODERN)
        .send_json(missing_method_header_body);
    assert!(
        matches!(missing_method, Err(ureq::Error::Status(400, _))),
        "a missing Mcp-Method must be rejected"
    );

    let wrong_name_body = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "echo",
            "arguments": { "text": "hi" },
            "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN }
        }
    });
    let wrong_name = ureq::post(&fixture.url)
        .set("MCP-Protocol-Version", MODERN)
        .set("Mcp-Method", "tools/call")
        .set("Mcp-Name", "other")
        .send_json(wrong_name_body);
    assert!(
        matches!(wrong_name, Err(ureq::Error::Status(400, _))),
        "a mismatched Mcp-Name must be rejected"
    );

    let missing_name_body = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {
            "arguments": { "text": "hi" },
            "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN }
        }
    });
    let missing_name = ureq::post(&fixture.url)
        .set("MCP-Protocol-Version", MODERN)
        .set("Mcp-Method", "tools/call")
        .send_json(missing_name_body);
    assert!(
        matches!(missing_name, Err(ureq::Error::Status(400, _))),
        "a missing routing name must be rejected"
    );
}

/// The regression test for the bug the stdio harness could not see: Toolport
/// driving a strict modern server over Streamable HTTP.
///
/// Before the fix, `MCP-Protocol-Version` was hardcoded to the legacy version
/// while the body declared `2026-07-28`, and the `server/discover` probe ran
/// before the metadata was stamped. Either alone makes this fail.
#[test]
fn gateway_connects_to_a_modern_http_server() {
    let fixture = spawn_http_fixture(MODERN, true, None);
    let transport = HttpTransport::new(&fixture.url);
    let mut server = DownstreamServer::connect("mock".to_string(), Box::new(transport))
        .expect("a dual-era gateway must reach a modern HTTP server");

    assert!(server.era().is_modern(), "era should be detected as modern");
    assert_eq!(server.era().version(), MODERN);

    // Usable, not merely connected: every request has to carry an agreeing
    // header and body or the fixture rejects it.
    let result = server
        .call("echo", json!({ "text": "hi" }))
        .expect("a modern HTTP server must be callable");
    assert_eq!(result["content"][0]["text"], "hi");
    assert_eq!(result["resultType"], "complete");
}

/// Legacy servers over HTTP keep working exactly as before: `initialize`
/// handshake, no probe, no modern metadata.
#[test]
fn gateway_connects_to_a_legacy_http_server() {
    let path = scratch_path("legacy-http");
    let _ = std::fs::remove_file(&path);
    let fixture = spawn_http_fixture("2025-06-18", true, Some(&path.to_string_lossy()));
    let transport = HttpTransport::new(&fixture.url);
    let mut server = DownstreamServer::connect("mock".to_string(), Box::new(transport))
        .expect("legacy HTTP must still connect");
    assert!(!server.era().is_modern());
    server.call("echo", json!({ "text": "hi" })).expect("call");
    drop(fixture);

    let methods = methods_of(&read_transcript(&path));
    assert_eq!(methods.first().map(String::as_str), Some("initialize"));
    assert!(
        !methods.iter().any(|m| m == "server/discover"),
        "a legacy server must never be probed, got {methods:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// A legacy server that ERRORS on `initialize` (missing API key, bad config)
/// must still fail fast.
///
/// The era probe added a second request on that path. Launcher-wrapped servers
/// (npx, uvx) carry a 120s connect budget for cold package downloads, so
/// inheriting it here turned an instant failure into a two-minute hang, with
/// batch probes and router rebuilds waiting on the slowest server. The probe now
/// has its own tight budget.
#[test]
fn legacy_server_that_rejects_initialize_still_fails_fast() {
    // Strict modern fixture refuses `initialize`, then stays silent on the probe
    // because... it is modern, so instead use a strict LEGACY fixture, which
    // rejects anything before `initialize` and never implements `server/discover`.
    // Sending a bad `initialize` makes it error, then go silent on the probe.
    let dirty = Arc::new(AtomicU8::new(0));
    let env = env_for(Some("2025-06-18"), true, None);
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");

    let started = std::time::Instant::now();
    // `connect` sends a well-formed initialize which this fixture accepts, so
    // drive the failing path directly: a raw transport whose first call is not
    // `initialize` gets the strict fixture's error, and `server/discover` then
    // gets silence.
    let mut server = transport;
    server.set_read_timeout(Duration::from_secs(2));
    let _ = server.request("tools/list", json!({}));
    let probe = server.request("server/discover", json!({}));
    let elapsed = started.elapsed();

    assert!(probe.is_err(), "a legacy fixture never answers server/discover");
    // The point is the bound, not the exact number: silence must not cost the
    // full launcher budget.
    assert!(
        elapsed < Duration::from_secs(10),
        "probing a silent legacy server took {elapsed:?}; it must not inherit the \
         120s launcher connect budget"
    );
}

/// The legacy path must be untouched by era detection: no extra probe, no
/// `server/discover`, and no protocol `_meta` on the wire. This is the
/// no-regression guarantee for the entire existing install base.
#[test]
fn legacy_servers_see_no_era_detection_traffic() {
    let path = scratch_path("legacy-era");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some("2025-06-18"), true, Some(&path.to_string_lossy()));

    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    assert!(!server.era().is_modern());
    assert_eq!(server.era().version(), "2025-06-18");
    server.call("echo", json!({ "text": "hi" })).expect("call");
    drop(server);

    let transcript = read_transcript(&path);
    let methods = methods_of(&transcript);
    // Non-vacuity first. `read_transcript` swallows a missing file, a wrong path,
    // and unparseable lines into an empty Vec, and BOTH assertions below hold on
    // zero records, so without this the headline no-regression guarantee passes
    // when the fixture records nothing at all. Verified: stubbing the fixture's
    // `record()` to a no-op left this test green.
    assert!(
        methods.iter().any(|m| m == "initialize"),
        "expected a recorded transcript, got {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "tools/call"),
        "expected the echo call to be recorded, got {methods:?}"
    );
    assert!(
        !methods.iter().any(|m| m == "server/discover"),
        "a legacy server must never be probed, got {methods:?}"
    );
    let mut checked = 0;
    for record in &transcript {
        checked += 1;
        assert!(
            record["params"].get("_meta").is_none(),
            "legacy requests carry no protocol _meta, got {record}"
        );
    }
    assert!(checked >= 3, "expected several records to check, saw {checked}");

    let _ = std::fs::remove_file(&path);
}

/// The GATEWAY must forward `icons`, not merely the fixture emit them.
///
/// `fixture_advertises_icons_from_2025_11_25` drives the fixture through a raw
/// transport, so it only ever proved the mock's own output. The actual risk is on
/// Toolport's side - a gateway that rebuilds tool objects field-by-field drops
/// unknown keys - and nothing exercised the aggregation that would do it
/// (SOU-474, untested paths).
///
/// Scope: this covers `Router` aggregation, where the rebuild risk actually lives
/// (`index_server` clones the tool and overwrites three fields). It stops short of
/// the gateway's own `tools/list` arm, which serves from `aggregated_tools` and is
/// not exercised here. Today unknown keys structurally cannot be dropped, so this
/// is a forward-looking pin rather than a regression test - it earns its place by
/// failing the moment someone reconstructs the tool object instead of cloning it.
#[test]
fn the_gateway_forwards_icons_through_tool_aggregation() {
    use conduit_lib::router::Router;

    let env = env_for(Some("2025-11-25"), false, None);
    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");

    // Non-vacuity: the server really did hand us icons, so a later assertion
    // failing means the gateway dropped them rather than the fixture omitting them.
    let raw_has_icons = server
        .tools
        .iter()
        .any(|t| t["name"] == "echo" && t.get("icons").is_some());
    assert!(raw_has_icons, "fixture must supply icons for this test to mean anything");

    let mut router = Router::new();
    router.add(server);
    let exposed = router.aggregated_tools();
    let echo = exposed
        .iter()
        .find(|t| t["name"].as_str().is_some_and(|n| n.ends_with("echo")))
        .unwrap_or_else(|| panic!("echo must survive aggregation, got {exposed:#?}"));

    let icons = echo
        .get("icons")
        .unwrap_or_else(|| panic!("the gateway dropped `icons` during aggregation: {echo}"));
    assert!(icons.is_array(), "icons must survive intact, got {icons}");
}

/// Pin what a LEGACY server sees when the client's request does carry `_meta`.
///
/// `legacy_servers_see_no_era_detection_traffic` asserts `_meta` is absent
/// outright, which only holds because its calls send none. `WITHHELD_META_KEYS`
/// is now empty, so a legacy client's `progressToken` IS relayed downstream and
/// the server starts emitting `notifications/progress` on connections that never
/// saw them before. That is intended - the client asked for it - but it was new
/// server-to-client traffic covered by no pin at all (SOU-474 #6).
///
/// The real invariant is narrower than "no `_meta`": Toolport must not put its
/// OWN protocol metadata on a legacy hop, while still relaying what the client
/// sent.
#[test]
fn a_legacy_server_sees_client_meta_relayed_but_never_protocol_meta() {
    let path = scratch_path("legacy-client-meta");
    let _ = std::fs::remove_file(&path);
    let env = env_for(Some("2025-06-18"), true, Some(&path.to_string_lossy()));

    let dirty = Arc::new(AtomicU8::new(0));
    let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
        .expect("spawn fixture");
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect");
    assert!(!server.era().is_modern(), "this pin is about the legacy hop");

    // Includes a per-hop key on purpose. Sending only client-owned keys made the
    // "never protocol meta" loop below vacuous: it proved the legacy transport
    // does not STAMP one, never that PER_HOP_META_KEYS strips one the client sent
    // (#511 review). Toolport speaks for itself downstream, so a client that
    // declares a version must not have that claim relayed onward.
    let client_meta = json!({
        "progressToken": "client-tok",
        "traceparent": "00-abc-def-01",
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "SomeClient", "version": "9.9" },
    });
    server
        .call_with_cancel("echo", json!({ "text": "hi" }), None, Some(&client_meta))
        .expect("call");
    drop(server);

    let transcript = read_transcript(&path);
    let call = transcript
        .iter()
        .find(|r| r["method"] == "tools/call")
        .unwrap_or_else(|| panic!("expected a recorded tools/call, got {transcript:#?}"));
    let meta = call["params"]["_meta"]
        .as_object()
        .unwrap_or_else(|| panic!("the client's _meta must reach the server, got {call}"));

    // Relayed: the client asked for progress and for its trace context to travel.
    assert_eq!(
        meta.get("progressToken").and_then(|v| v.as_str()),
        Some("client-tok"),
        "progressToken is relayed now that progress can be routed back: {call}"
    );
    assert_eq!(
        meta.get("traceparent").and_then(|v| v.as_str()),
        Some("00-abc-def-01"),
        "unknown _meta keys must pass through untouched: {call}"
    );
    // Withheld: Toolport speaks for itself on the downstream hop, and a legacy
    // server must never see 2026-07-28 protocol metadata.
    for key in meta.keys() {
        assert!(
            !key.starts_with("io.modelcontextprotocol/"),
            "a legacy server must see no protocol _meta, got '{key}' in {call}"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Prove the strict modern fixture's handshake gate can actually fire.
///
/// `notifications/initialized` was listed among the methods a strict modern
/// fixture rejects, but it carries no id, so it returned from `handle` long
/// before the gate was consulted - and that early return marked the server
/// initialized. The gate read as enforcement while the fixture quietly accepted
/// the exact traffic it advertised refusing (SOU-474 #11).
///
/// Checks all three cases the way a fixture gate has to be checked: it fires on
/// the violation, stays silent when the method is absent, and stays silent for a
/// legacy fixture where the handshake is legitimate.
#[test]
fn strict_modern_fixture_refuses_the_legacy_handshake_notification() {
    use std::io::Write;

    /// Feed one line to a fixture, close stdin, and report whether it died.
    fn feed(revision: &str, strict: bool, line: &str) -> Option<i32> {
        let mut cmd = std::process::Command::new(mock_bin());
        cmd.env("MOCK_MCP_REVISION", revision)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if strict {
            cmd.env("MOCK_MCP_STRICT", "1");
        }
        let mut child = cmd.spawn().expect("spawn fixture");
        {
            let mut stdin = child.stdin.take().expect("fixture stdin");
            let _ = writeln!(stdin, "{line}");
        }
        child.wait().expect("fixture should exit").code()
    }

    const HANDSHAKE: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    // Another id-less notification, to show the gate is specific rather than
    // just killing the process on any notification.
    const OTHER: &str = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;

    // Positive: the violation is refused.
    assert_eq!(
        feed("2026-07-28", true, HANDSHAKE),
        Some(97),
        "a strict modern fixture must refuse the legacy handshake notification"
    );
    // Absent: a different notification is fine.
    assert_eq!(
        feed("2026-07-28", true, OTHER),
        Some(0),
        "only the handshake notification is refused"
    );
    // Negative: on a legacy fixture the same notification is legitimate.
    assert_eq!(
        feed("2025-06-18", true, HANDSHAKE),
        Some(0),
        "a legacy fixture must still accept its own handshake"
    );
    // And strict mode is what arms it: a non-strict modern fixture tolerates it.
    assert_eq!(
        feed("2026-07-28", false, HANDSHAKE),
        Some(0),
        "the gate belongs to strict mode"
    );
}

/// The negotiated legacy version must be the one the SERVER answered with, not
/// Toolport's default.
///
/// Every other legacy assertion uses a 2025-06-18 fixture, which is byte-identical
/// to the `PROTOCOL_VERSION` fallback - so `Era::Legacy { version }` could be
/// replaced by the constant outright and the whole suite stayed green (SOU-474 #8).
/// This pins it at a revision the fallback cannot produce.
#[test]
fn legacy_era_pins_the_version_the_server_actually_answered() {
    for revision in ["2024-11-05", "2025-03-26", "2025-11-25"] {
        assert_ne!(
            revision,
            conduit_lib::downstream::PROTOCOL_VERSION,
            "this test is only meaningful at a revision the fallback cannot produce"
        );
        let env = env_for(Some(revision), true, None);
        let dirty = Arc::new(AtomicU8::new(0));
        let transport = StdioTransport::spawn_watched(mock_bin(), &[], &env, None, dirty, None)
            .expect("spawn fixture");
        let server = DownstreamServer::connect("mock".to_string(), Box::new(transport))
            .unwrap_or_else(|e| panic!("connect to a {revision} server: {e}"));

        assert!(!server.era().is_modern(), "{revision} is a legacy revision");
        assert_eq!(
            server.era().version(),
            revision,
            "the era must carry the server's own version, not the fallback"
        );
    }
}
