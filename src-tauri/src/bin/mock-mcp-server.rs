//! A minimal, multi-revision MCP server used as a test fixture for the gateway's
//! downstream proxying. Exposes `echo` (returns its `text` arg), `add` (returns
//! `a + b`), `echo_meta` (returns the request's `params._meta` verbatim), and
//! `grow`, plus a baseline resource and prompt. Calling `grow` adds a `greet`
//! tool, a `mock://grown` resource, and a `grown_prompt`, then emits the tools,
//! resources, and prompts `list_changed` notifications, simulating a server that
//! changes its own catalog mid-session so the gateway's live refresh can be
//! exercised for all three kinds. Self-contained: no dependency on conduit_lib.
//!
//! # Multi-revision behaviour (SOU-443)
//!
//! The fixture can impersonate any MCP revision so the gateway's backward
//! compatibility can be tested against every era it must support. Controlled by
//! environment, because the spawn path already threads env through:
//!
//! - `MOCK_MCP_REVISION` — one of `2024-11-05`, `2025-03-26`, `2025-06-18`
//!   (default), `2025-11-25`, `2026-07-28`. Unset or unrecognized values keep
//!   the historical behaviour, so existing tests are unaffected.
//! - `MOCK_MCP_STRICT=1` — enforce era rules instead of being permissive.
//!   Legacy revisions then reject any request that arrives before `initialize`;
//!   the modern revision rejects `initialize`/`ping` and demands per-request
//!   `_meta` protocol fields. Off by default so the permissive fixture that
//!   existing tests rely on is unchanged.
//! - `MOCK_MCP_TRANSCRIPT` — path to append every received request to, one JSON
//!   object per line. This is what lets a test assert exactly what bytes the
//!   gateway sent downstream, which is the regression net for the envelope
//!   transparency work (SOU-444).
//!
//! The default configuration (no env set) is byte-identical to the pre-SOU-443
//! fixture apart from the added `echo_meta` tool, so `list_changed`,
//! `circuit_breaker`, and `root_cwd` keep passing untouched.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const SERVER_NAME: &str = "mock-mcp-server";
const SERVER_VERSION: &str = "0.1.0";

// Standard `_meta` keys introduced by 2026-07-28. Spelled out rather than
// derived so a typo here fails loudly in tests rather than silently matching
// whatever the gateway happens to send.
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

// JSON-RPC error codes. `-32022` is `UnsupportedProtocolVersion` from the
// 2026-07-28 allocation policy (`-32020`..`-32099` reserved for the spec).
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const HEADER_MISMATCH: i64 = -32020;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_REQUEST: i64 = -32600;

/// Which MCP revision this fixture impersonates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Revision {
    V20241105,
    V20250326,
    V20250618,
    V20251125,
    V20260728,
}

impl Revision {
    fn as_str(self) -> &'static str {
        match self {
            Revision::V20241105 => "2024-11-05",
            Revision::V20250326 => "2025-03-26",
            Revision::V20250618 => "2025-06-18",
            Revision::V20251125 => "2025-11-25",
            Revision::V20260728 => "2026-07-28",
        }
    }

    /// Unrecognized values fall back to the historical default rather than
    /// failing, so a stale env var can never turn an unrelated test red.
    fn from_env() -> Self {
        match std::env::var("MOCK_MCP_REVISION").as_deref() {
            Ok("2024-11-05") => Revision::V20241105,
            Ok("2025-03-26") => Revision::V20250326,
            Ok("2025-11-25") => Revision::V20251125,
            Ok("2026-07-28") => Revision::V20260728,
            _ => Revision::V20250618,
        }
    }

    /// True for revisions that carry version/identity/capabilities as
    /// per-request `_meta` instead of an `initialize` handshake.
    fn is_modern(self) -> bool {
        self == Revision::V20260728
    }

    /// `icons` on tools/resources/prompts landed in 2025-11-25 (SEP-973).
    fn has_icons(self) -> bool {
        matches!(self, Revision::V20251125 | Revision::V20260728)
    }
}

/// Runtime knobs, read once at startup.
struct Config {
    revision: Revision,
    strict: bool,
    transcript: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            revision: Revision::from_env(),
            strict: std::env::var("MOCK_MCP_STRICT").as_deref() == Ok("1"),
            transcript: std::env::var("MOCK_MCP_TRANSCRIPT").ok().filter(|p| !p.is_empty()),
        }
    }
}

/// Mutable per-process state.
struct State {
    /// Flipped by a `grow` call so the next `tools/list` reflects the larger set.
    grown: bool,
    /// Whether a legacy `initialize` has been seen. Only enforced in strict mode.
    initialized: bool,
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(data) = data {
        err["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// Append one received request to the transcript file, if configured. Opened
/// per write (rather than held) so a `die` call cannot lose buffered lines.
fn record(cfg: &Config, req: &Value) {
    let Some(path) = cfg.transcript.as_deref() else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{req}");
        let _ = f.flush();
    }
}

/// Decorate a result per the negotiated revision. Modern results carry a
/// required `resultType` and a `serverInfo` `_meta` entry; list-shaped results
/// additionally carry the `CacheableResult` fields. Legacy revisions are left
/// exactly as they were.
fn decorate(cfg: &Config, method: &str, mut result: Value) -> Value {
    if !cfg.revision.is_modern() {
        return result;
    }
    result["resultType"] = json!("complete");
    result["_meta"] = json!({
        META_SERVER_INFO: { "name": SERVER_NAME, "version": SERVER_VERSION }
    });
    if matches!(
        method,
        "tools/list"
            | "resources/list"
            | "resources/templates/list"
            | "prompts/list"
            | "resources/read"
            | "server/discover"
    ) {
        result["ttlMs"] = json!(60_000);
        result["cacheScope"] = json!("public");
    }
    result
}

/// The advertised tool list. `greet` only appears once the server has "grown"
/// (after a `grow` call), modeling a runtime tool-set change.
fn tool_list(cfg: &Config, grown: bool) -> Value {
    let mut tools = vec![
        json!({ "name": "echo", "description": "Echo back the text argument.",
                "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } }),
        json!({ "name": "add", "description": "Add two numbers a and b.",
                "inputSchema": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } } } }),
        json!({ "name": "grow", "description": "Add a new tool and announce tools/list_changed.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "die", "description": "Exit the process to simulate a mid-session crash.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "pwd", "description": "Return the process working directory.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "echo_meta", "description": "Return the request's params._meta verbatim.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "progress_ping", "description": "Emit notifications/progress, then reply.",
                "inputSchema": { "type": "object", "properties": {} } }),
    ];
    if grown {
        tools.push(json!({ "name": "greet", "description": "Greet someone by name.",
                "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } } } }));
    }
    // Icons ride on the tool definition from 2025-11-25. The gateway must carry
    // them through untouched even though it does not consume them (SOU-452).
    if cfg.revision.has_icons() {
        if let Some(first) = tools.first_mut() {
            first["icons"] = json!([{ "src": "https://example.invalid/echo.png", "sizes": "48x48" }]);
        }
    }
    json!({ "tools": tools })
}

/// The advertised resource list. `grown` adds a second resource, modeling a
/// runtime `resources/list_changed`.
fn resource_list(grown: bool) -> Value {
    let mut resources = vec![json!({ "uri": "mock://base", "name": "base" })];
    if grown {
        resources.push(json!({ "uri": "mock://grown", "name": "grown" }));
    }
    json!({ "resources": resources })
}

/// The advertised prompt list. `grown` adds a second prompt, modeling a runtime
/// `prompts/list_changed`.
fn prompt_list(grown: bool) -> Value {
    let mut prompts = vec![json!({ "name": "hi", "description": "Say hi." })];
    if grown {
        prompts.push(json!({ "name": "grown_prompt", "description": "A newly grown prompt." }));
    }
    json!({ "prompts": prompts })
}

fn capabilities() -> Value {
    json!({
        "tools": { "listChanged": true },
        "resources": { "listChanged": true },
        "prompts": { "listChanged": true }
    })
}

/// Reject era-mismatched traffic, but only in strict mode. Returns the error
/// response to send instead of handling the request, if any.
fn era_gate(cfg: &Config, state: &State, method: &str, req: &Value, id: &Value) -> Option<Value> {
    if !cfg.strict {
        return None;
    }
    if cfg.revision.is_modern() {
        // A modern server has no handshake and no `ping`. `notifications/initialized`
        // belongs to the same list but is deliberately absent: it carries no id, so
        // it never reaches this gate and cannot be answered with an error at all.
        // `handle` enforces it directly (SOU-474 #11).
        if method == "initialize" || method == "ping" {
            // The spec asks a modern-only server to name its supported versions
            // in any error it returns to `initialize`, since legacy clients have
            // no fall-forward mechanism and this may be their only diagnostic.
            return Some(error(
                id.clone(),
                METHOD_NOT_FOUND,
                &format!("{method} is not part of {}", cfg.revision.as_str()),
                Some(json!({ "supported": [cfg.revision.as_str()] })),
            ));
        }
        let requested = req
            .get("params")
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get(META_PROTOCOL_VERSION))
            .and_then(|v| v.as_str());
        match requested {
            Some(v) if v == cfg.revision.as_str() => None,
            other => Some(error(
                id.clone(),
                UNSUPPORTED_PROTOCOL_VERSION,
                "Unsupported protocol version",
                Some(json!({
                    "supported": [cfg.revision.as_str()],
                    "requested": other.unwrap_or(""),
                })),
            )),
        }
    } else {
        // A legacy server expects the handshake before anything else.
        if method != "initialize" && !state.initialized {
            return Some(error(
                id.clone(),
                INVALID_REQUEST,
                "expected initialize before any other request",
                None,
            ));
        }
        None
    }
}

/// Handle one request, returning its response. `pre` collects notifications the
/// server wants to emit *before* the response, which is how real servers stream
/// progress: the client sees them while its request is still in flight.
fn handle(cfg: &Config, state: &mut State, req: &Value, pre: &mut Vec<Value>) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications carry no id and get no response, but still drive state.
    let id = match req.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => {
            // A modern server has no handshake, so `notifications/initialized`
            // is legacy traffic that must never arrive. It cannot be refused
            // with an error response - it has no id - so `era_gate` structurally
            // could not enforce it, and listing it there merely looked like
            // enforcement while this arm silently accepted the handshake and
            // even marked the server initialized (SOU-474 #11).
            //
            // Dying is the enforcement a fixture has: the test fails instead of
            // passing against a mock that quietly tolerated what it claims to
            // reject. `handle` is shared with `serve_http`, so this fires in HTTP
            // mode too - there the client sees a connection error rather than a
            // closed stdio stream, which is equally loud and equally correct.
            if cfg.strict && cfg.revision.is_modern() && method == "notifications/initialized" {
                eprintln!(
                    "mock-mcp-server: strict {} server received legacy '{method}'",
                    cfg.revision.as_str()
                );
                std::process::exit(97);
            }
            if method == "notifications/initialized" {
                state.initialized = true;
            }
            return None;
        }
    };

    if let Some(err) = era_gate(cfg, state, method, req, &id) {
        return Some(err);
    }

    let result = match method {
        "initialize" => {
            state.initialized = true;
            json!({
                "protocolVersion": cfg.revision.as_str(),
                "capabilities": capabilities(),
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
            })
        }
        // Modern servers MUST implement this. Legacy revisions deliberately do
        // not, so the gateway's stdio fallback probe has something to fail on.
        "server/discover" if cfg.revision.is_modern() => json!({
            "supportedVersions": [cfg.revision.as_str()],
            "capabilities": capabilities(),
            "instructions": "Mock server used as a gateway test fixture.",
        }),
        "tools/list" => tool_list(cfg, state.grown),
        "resources/list" => resource_list(state.grown),
        "prompts/list" => prompt_list(state.grown),
        "tools/call" => {
            let params = req.get("params");
            let name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("");
            let args = params.and_then(|p| p.get("arguments")).cloned().unwrap_or_else(|| json!({}));
            // `echo_meta` reflects the request's `_meta` back as structured
            // content so a test can assert end-to-end propagation through the
            // gateway without reading the transcript file.
            if name == "echo_meta" {
                let meta = params.and_then(|p| p.get("_meta")).cloned().unwrap_or(Value::Null);
                let text = serde_json::to_string(&meta).unwrap_or_else(|_| "null".to_string());
                return Some(success(
                    id,
                    decorate(
                        cfg,
                        method,
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "isError": false,
                            "structuredContent": { "receivedMeta": meta }
                        }),
                    ),
                ));
            }
            // Stream a progress notification for the token the caller supplied,
            // before the response, so the gateway's routing can be exercised
            // end to end (SOU-444).
            if name == "progress_ping" {
                if let Some(token) = params
                    .and_then(|p| p.get("_meta"))
                    .and_then(|m| m.get("progressToken"))
                {
                    pre.push(json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": { "progressToken": token, "progress": 1, "total": 2 }
                    }));
                }
            }
            let text = match name {
                "echo" => args.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                "add" => {
                    let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    format!("{}", a + b)
                }
                "grow" => {
                    state.grown = true;
                    "grew: greet is now available".to_string()
                }
                "die" => {
                    // Crash without responding, so the gateway sees the connection die
                    // (used to exercise the circuit breaker).
                    std::process::exit(0);
                }
                "greet" => {
                    let who = args.get("name").and_then(|t| t.as_str()).unwrap_or("there");
                    format!("hello {who}")
                }
                "pwd" => std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                other => format!("unknown tool {other}"),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": false })
        }
        "ping" => json!({}),
        _ => return None,
    };

    Some(success(id, decorate(cfg, method, result)))
}

/// Header/body agreement check for a modern Streamable HTTP request.
///
/// From 2026-07-28 `MCP-Protocol-Version` **MUST** equal the
/// `io.modelcontextprotocol/protocolVersion` in the body's `_meta`, and a server
/// that sees them disagree rejects with `400` and `HeaderMismatch` (-32020).
/// This is the rule a stdio fixture structurally cannot express, which is how a
/// hardcoded header shipped past a green stdio suite (SOU-443 follow-up).
///
/// Returns the JSON-RPC error to send instead, if the request is invalid.
fn header_gate(cfg: &Config, header_version: Option<&str>, req: &Value, id: &Value) -> Option<Value> {
    if !cfg.strict || !cfg.revision.is_modern() {
        return None;
    }
    let body_version = req
        .get("params")
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(META_PROTOCOL_VERSION))
        .and_then(|v| v.as_str());
    match (header_version, body_version) {
        (Some(h), Some(b)) if h == b => None,
        (None, _) => Some(error(
            id.clone(),
            HEADER_MISMATCH,
            "missing required MCP-Protocol-Version header",
            None,
        )),
        (Some(h), b) => Some(error(
            id.clone(),
            HEADER_MISMATCH,
            &format!(
                "MCP-Protocol-Version header '{h}' does not match body _meta '{}'",
                b.unwrap_or("<absent>")
            ),
            None,
        )),
    }
}

/// Serve the same era logic over Streamable HTTP instead of stdio.
///
/// Prints `MOCK_MCP_URL=<url>` on stdout once bound so a test can discover the
/// ephemeral port.
fn serve_http(cfg: &Config) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind fixture port");
    let port = server
        .server_addr()
        .to_ip()
        .expect("ip addr")
        .port();
    println!("MOCK_MCP_URL=http://127.0.0.1:{port}/mcp");
    let _ = std::io::stdout().flush();

    let mut state = State { grown: false, initialized: false };
    for mut request in server.incoming_requests() {
        let header_version = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("MCP-Protocol-Version"))
            .map(|h| h.value.as_str().to_string());
        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        let req: Value = match serde_json::from_str(body.trim()) {
            Ok(v) => v,
            Err(_) => {
                let _ = request.respond(
                    tiny_http::Response::from_string("bad json").with_status_code(400),
                );
                continue;
            }
        };
        record(cfg, &req);

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let (status, payload) = match header_gate(cfg, header_version.as_deref(), &req, &id) {
            // A header/body disagreement is a 400, per the transport spec.
            Some(err) => (400, Some(err)),
            None => {
                let mut pre = Vec::new();
                (200, handle(cfg, &mut state, &req, &mut pre))
            }
        };
        let response = match payload {
            Some(value) => tiny_http::Response::from_string(value.to_string())
                .with_status_code(status)
                .with_header(
                    tiny_http::Header::from_bytes(b"Content-Type", b"application/json").unwrap(),
                ),
            // A notification gets 202 with no body.
            None => tiny_http::Response::from_string(String::new()).with_status_code(202),
        };
        let _ = request.respond(response);
    }
}

fn main() {
    let cfg = Config::from_env();
    if std::env::var("MOCK_MCP_HTTP").as_deref() == Ok("1") {
        serve_http(&cfg);
        return;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut state = State { grown: false, initialized: false };
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        record(&cfg, &req);
        let was_grown = state.grown;
        let mut pre = Vec::new();
        let resp = handle(&cfg, &mut state, &req, &mut pre);
        // Emitted before the response, so the client reads them while its request
        // is still in flight - the realistic ordering for streamed progress.
        if !pre.is_empty() {
            for note in &pre {
                if writeln!(out, "{note}").is_err() {
                    return;
                }
            }
            let _ = out.flush();
        }
        if let Some(resp) = resp {
            if writeln!(out, "{resp}").is_err() {
                break;
            }
            let _ = out.flush();
        }
        // A `grow` call just changed all three lists: announce each (after the call
        // response) so a watching gateway re-fetches and surfaces the new entries.
        if state.grown && !was_grown {
            for method in [
                "notifications/tools/list_changed",
                "notifications/resources/list_changed",
                "notifications/prompts/list_changed",
            ] {
                let notif = json!({ "jsonrpc": "2.0", "method": method });
                if writeln!(out, "{notif}").is_err() {
                    return;
                }
            }
            let _ = out.flush();
        }
    }
}
