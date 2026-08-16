//! Tool router.
//!
//! Aggregates the tools of every connected downstream server into one list the
//! gateway exposes upward, namespacing each tool by its server id so names can't
//! collide. Routing a call maps the exposed name back to its owning server and
//! that server's original tool name.
//!
//! Exposed names are sanitized to `[A-Za-z0-9_]`. MCP allows hyphens in tool
//! names, but clients like Cursor enforce the OpenAI function-name charset and
//! silently drop any tool whose name (server id included) contains a hyphen - so
//! `revenuecat-rigcast__list-offerings` would never appear. We rewrite hyphens
//! (and anything else out of charset) to `_` on the way out, and keep a reverse
//! map so `tools/call` still forwards the server's real, hyphenated tool name.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde_json::{json, Value};

use crate::downstream::{
    backoff_delay, CacheHint, CancelContext, DownstreamServer, MrtrRequest, TransportError,
    HTTP_MAX_RETRIES, HTTP_RETRY_CAP,
};
use crate::registry::ToolOverride;

const TASK_HANDLE_PREFIX: &str = "toolport-task:v1:";
const TASK_HANDLE_NONCE_LEN: usize = 24;
const MCP_APPS_EXTENSION: &str = "io.modelcontextprotocol/ui";
const MCP_APP_HTML_MIME: &str = "text/html;profile=mcp-app";
/// Retry-After waits observe upstream cancellation within this bound.
const RETRY_CANCEL_POLL: Duration = Duration::from_millis(25);

fn wait_for_retry_or_cancel(
    wait: Duration,
    cancel: Option<&CancelContext>,
) -> Result<(), TransportError> {
    let deadline = Instant::now() + wait;
    loop {
        if cancel.is_some_and(CancelContext::is_cancelled) {
            return Err(TransportError::Cancelled(
                "request cancelled during downstream retry wait".to_string(),
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::park_timeout(remaining.min(RETRY_CANCEL_POLL));
    }
}

fn supports_mcp_app_html(extensions: &serde_json::Map<String, Value>) -> bool {
    extensions
        .get(MCP_APPS_EXTENSION)
        .and_then(|settings| settings.get("mimeTypes"))
        .and_then(Value::as_array)
        .is_some_and(|mime_types| mime_types.iter().any(|mime| mime == MCP_APP_HTML_MIME))
}

/// MCP Apps resources are primarily discovered through tool metadata and MAY be
/// omitted from `resources/list`. Keep both the current nested field and the
/// pre-GA flat spelling so a transparent gateway can still route the host's
/// subsequent `resources/read` to the tool's owning server.
fn mcp_app_resource_uri(tool: &Value) -> Option<&str> {
    tool.pointer("/_meta/ui/resourceUri")
        .or_else(|| tool.pointer("/_meta/ui~1resourceUri"))
        .and_then(Value::as_str)
        .filter(|uri| uri.starts_with("ui://"))
}

/// Seal the owner and native task id into one opaque, unguessable handle. The
/// installation-local key survives restarts; authenticated encryption prevents
/// a client from changing either component to reach another task (SOU-453).
fn expose_task_id(server_id: &str, task_id: &str) -> Result<String, String> {
    let key = crate::secrets::task_handle_key()?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let plain = serde_json::to_vec(&(server_id, task_id)).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; TASK_HANDLE_NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plain.as_ref())
        .map_err(|_| "could not seal Toolport task id".to_string())?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(format!(
        "{TASK_HANDLE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    ))
}

fn decode_task_id(exposed: &str) -> Result<(String, String), String> {
    let encoded = exposed
        .strip_prefix(TASK_HANDLE_PREFIX)
        .ok_or_else(|| "task id was not issued by Toolport".to_string())?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "malformed Toolport task id".to_string())?;
    if blob.len() <= TASK_HANDLE_NONCE_LEN {
        return Err("malformed Toolport task id".to_string());
    }
    let (nonce, ciphertext) = blob.split_at(TASK_HANDLE_NONCE_LEN);
    let key = crate::secrets::task_handle_key()?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| "task id was not issued by Toolport".to_string())?;
    let (server, task): (String, String) = serde_json::from_slice(&plain)
        .map_err(|_| "malformed Toolport task id".to_string())?;
    if server.is_empty() || task.is_empty() {
        return Err("malformed Toolport task id".to_string());
    }
    Ok((server, task))
}

fn expose_task_result(mut result: Value, server_id: &str) -> Result<Value, String> {
    let task_id = result
        .get("taskId")
        .and_then(Value::as_str)
        .ok_or_else(|| "downstream task result is missing taskId".to_string())?;
    result["taskId"] = json!(expose_task_id(server_id, task_id)?);
    Ok(result)
}

fn client_supports_tasks(meta: Option<&Value>) -> bool {
    meta.and_then(|meta| meta.get("io.modelcontextprotocol/clientCapabilities"))
        .and_then(|capabilities| capabilities.get("extensions"))
        .and_then(|extensions| extensions.get("io.modelcontextprotocol/tasks"))
        .is_some()
}

/// The delay before a retry attempt. Prefers a server-advertised `Retry-After`,
/// else our exponential backoff, but never longer than `HTTP_RETRY_CAP` so a
/// downstream advertising `Retry-After: 3600` can't pin the calling agent's
/// thread. Retries are bounded, so if the server is still limiting past the cap
/// the loop exhausts and surfaces the error to the caller.
fn retry_wait(retry_after: Option<std::time::Duration>, attempt: u32) -> std::time::Duration {
    retry_after
        .unwrap_or_else(|| backoff_delay(attempt))
        .min(HTTP_RETRY_CAP)
}

/// Rewrite a name segment to the function-name charset clients accept
/// (`[A-Za-z0-9_]`); every other character becomes `_`.
pub fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Bound URI/template matching so a hostile client URI cannot blow the stack
/// or dominate the request thread with pathological backtracking.
const MAX_URI_MATCH_LEN: usize = 8_192;
const MAX_TEMPLATE_MATCH_LEN: usize = 1_024;

/// True when `uri` is an expansion of an RFC 6570 Level-1 URI template
/// (`{var}` placeholders). Used to route `resources/read` for expanded
/// template URIs that were never listed as concrete resources.
pub fn uri_matches_template(uri: &str, template: &str) -> bool {
    if uri.len() > MAX_URI_MATCH_LEN || template.len() > MAX_TEMPLATE_MATCH_LEN {
        return false;
    }
    if !template.contains('{') {
        return uri == template;
    }
    let mut pattern = String::from("^");
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let (literal, after_open) = rest.split_at(open);
        for ch in literal.chars() {
            if ".+*?^$()[]{}|\\".contains(ch) {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        let Some(close) = after_open.find('}') else {
            return false;
        };
        // Level-1 `{var}`: one path segment. `{+var}` / `{#var}` (Level 2) match
        // the remainder, including slashes.
        let expr = &after_open[1..close];
        if expr.starts_with('+') || expr.starts_with('#') {
            pattern.push_str(".+");
        } else {
            pattern.push_str("[^/]+");
        }
        rest = &after_open[close + 1..];
    }
    for ch in rest.chars() {
        if ".+*?^$()[]{}|\\".contains(ch) {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('$');
    regex_is_match(&pattern, uri)
}

/// Tiny anchored match helper so the router does not take a full regex crate
/// dependency for Level-1 template matching. Only the character classes we emit
/// above are recognized: literals, `[^/]+`, and `.+`.
fn regex_is_match(pattern: &str, text: &str) -> bool {
    // pattern is ^...$ from uri_matches_template.
    let inner = pattern
        .strip_prefix('^')
        .and_then(|p| p.strip_suffix('$'))
        .unwrap_or(pattern);
    match_simple_pattern(inner, text)
}

/// Byte offsets just past each char of `s`, longest prefix first — the lengths a greedy
/// variable segment should try, in greedy order.
///
/// Backtracking used to count raw bytes (`(1..=end).rev()`), which slices mid-codepoint
/// and panics on any multi-byte URI: matching `file://café` against `file://{name}é`
/// took the router down. Only whole chars are valid stopping points.
fn char_end_offsets(s: &str) -> impl Iterator<Item = usize> + '_ {
    s.char_indices().map(|(i, c)| i + c.len_utf8()).rev()
}

fn match_simple_pattern(mut pattern: &str, mut text: &str) -> bool {
    // Iterative literal consumption keeps recursion depth proportional to the
    // number of placeholders, not the URI length. Variable branches still
    // backtrack, but inputs are length-capped in uri_matches_template.
    loop {
        if pattern.is_empty() {
            return text.is_empty();
        }
        if let Some(rest) = pattern.strip_prefix("[^/]+") {
            // Match one or more non-slash chars (greedy, then backtrack).
            if text.is_empty() || text.starts_with('/') {
                return false;
            }
            let end = text.find('/').unwrap_or(text.len());
            for take in char_end_offsets(&text[..end]) {
                if match_simple_pattern(rest, &text[take..]) {
                    return true;
                }
            }
            return false;
        }
        if let Some(rest) = pattern.strip_prefix(".+") {
            if text.is_empty() {
                return false;
            }
            for take in char_end_offsets(text) {
                if match_simple_pattern(rest, &text[take..]) {
                    return true;
                }
            }
            return false;
        }
        // Consume a run of literal characters without recursing.
        let (pat_ch, pat_rest) = if let Some(rest) = pattern.strip_prefix('\\') {
            let mut chars = rest.chars();
            match chars.next() {
                Some(c) => (c, chars.as_str()),
                None => return false,
            }
        } else {
            let mut chars = pattern.chars();
            match chars.next() {
                Some(c) => (c, chars.as_str()),
                None => return text.is_empty(),
            }
        };
        let mut text_chars = text.chars();
        match text_chars.next() {
            Some(c) if c == pat_ch => {
                pattern = pat_rest;
                text = text_chars.as_str();
            }
            _ => return false,
        }
    }
}

/// Inline local `$ref` pointers into a self-contained JSON Schema, so a downstream
/// consumer that can't resolve refs gets a complete schema. Handles `#/$defs/X`,
/// `#/definitions/X`, AND any in-document JSON Pointer (`#/properties/a/b`, which
/// real servers like revenuecat use to share subschemas). mcpo (the MCP-to-OpenAPI
/// proxy OpenWebUI uses) aborts with "Custom field not found" on an unresolved
/// `$ref`, so one such server would otherwise break the whole full-discovery bridge.
/// Refs resolve against a snapshot of the original schema; a recursive or otherwise
/// unresolvable ref collapses to a permissive `{}`, so the output is always ref-free.
pub fn inline_refs(schema: &mut Value) {
    if !has_ref(schema) {
        return;
    }
    let root = schema.clone();
    let mut active = HashSet::new();
    inline_node(schema, &root, &mut active);
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$defs");
        obj.remove("definitions");
    }
}

/// True if `node` contains a `$ref` anywhere, so we can skip the clone otherwise.
fn has_ref(node: &Value) -> bool {
    match node {
        Value::Object(map) => map.contains_key("$ref") || map.values().any(has_ref),
        Value::Array(arr) => arr.iter().any(has_ref),
        _ => false,
    }
}

/// Replace a `{"$ref": "#/..."}` node with a copy of what that JSON Pointer resolves
/// to in `root` (itself inlined). `active` holds the ref strings currently expanding;
/// a ref into one (a cycle), an external ref (no `#` prefix), or an unresolvable
/// pointer collapses to a permissive `{}` so NO `$ref` ever leaks to a consumer that
/// can't resolve it. Cycles thus terminate with a wildcard rather than recursing.
fn inline_node(node: &mut Value, root: &Value, active: &mut HashSet<String>) {
    let ref_str = node.get("$ref").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(r) = ref_str {
        let mut resolved = None;
        if let Some(ptr) = r.strip_prefix('#') {
            if !active.contains(&r) {
                if let Some(target) = root.pointer(ptr).cloned() {
                    let mut sub = target;
                    active.insert(r.clone());
                    inline_node(&mut sub, root, active);
                    active.remove(&r);
                    resolved = Some(sub);
                }
            }
        }
        *node = resolved.unwrap_or_else(|| json!({}));
        return;
    }
    match node {
        Value::Object(map) => {
            for v in map.values_mut() {
                inline_node(v, root, active);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                inline_node(v, root, active);
            }
        }
        _ => {}
    }
}

/// True if a tool advertises `destructiveHint: true` (MCP tool annotations), or
/// has an obvious write/delete verb when no explicit hint is present. Accepts the
/// spec's nested `annotations.destructiveHint` and a top-level fallback some
/// servers emit. An explicit `false` hint wins over the name fallback.
pub fn is_destructive(tool: &Value) -> bool {
    if let Some(hint) = tool
        .get("annotations")
        .and_then(|a| a.get("destructiveHint"))
        .and_then(|v| v.as_bool())
        .or_else(|| tool.get("destructiveHint").and_then(|v| v.as_bool()))
    {
        return hint;
    }

    tool.get("name")
        .and_then(Value::as_str)
        .map(name_looks_destructive)
        .unwrap_or(false)
}

/// True when `name` contains an obvious write/delete verb. Used by
/// [`is_destructive`] as a fallback when no hint is present, and by integrity
/// drift tiering even when the server set `destructiveHint: false` (SBS-875:
/// the hint is attacker-controlled and must not disarm quarantine).
pub fn name_looks_destructive(name: &str) -> bool {
    let mut tokens = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .flat_map(split_camel_lower);
    tokens.any(|t| {
        matches!(
            t.as_str(),
            "create"
                | "delete"
                | "destroy"
                | "drop"
                | "execute"
                | "insert"
                | "move"
                | "patch"
                | "post"
                | "publish"
                | "remove"
                | "rename"
                | "replace"
                | "run"
                | "send"
                | "truncate"
                | "update"
                | "upload"
                | "write"
        )
        // `edit`/`modify` are deliberately omitted: they overlap with the benign
        // description-churn class that integrity drift tiering keeps quiet (see
        // `drift_severity_tiers_loud_vs_benign`), and widening them there would
        // trade the alert-fatigue win for louder, lower-signal drift alerts.
    })
}

fn split_camel_lower(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    for window in chars.windows(2) {
        let (idx, ch) = window[0];
        let (_, next) = window[1];
        if idx > start && ch.is_ascii_lowercase() && next.is_ascii_uppercase() {
            out.push(word[start..idx + ch.len_utf8()].to_ascii_lowercase());
            start = idx + ch.len_utf8();
        }
    }
    if start < word.len() {
        out.push(word[start..].to_ascii_lowercase());
    }
    out
}

/// Which downstream tools the gateway is allowed to expose. Default-allow: an
/// empty policy passes everything. This is the enforcement point behind the
/// per-tool toggle and the global destructive-tool deny switch.
#[derive(Default, Clone)]
pub struct ToolPolicy {
    /// server id -> original tool names the user switched off.
    pub disabled: HashMap<String, HashSet<String>>,
    /// server id -> the ONLY original tool names the active profile exposes (tool-granular
    /// scoping / "FeatureSet"). A server present here allow-lists: every other tool on it is
    /// hidden and blocked. A server ABSENT exposes all of its tools. Empty = no tool-granular
    /// scoping, so this is fully backward compatible.
    pub allow: HashMap<String, HashSet<String>>,
    /// Hide and block any tool annotated `destructiveHint: true`.
    pub deny_destructive: bool,
    /// Exposed (namespaced) tool names quarantined after a high-risk drift; hidden
    /// until the user re-approves them. Empty unless quarantine-on-drift is enabled.
    pub quarantined: BTreeSet<String>,
}

impl ToolPolicy {
    /// Reason this tool is blocked, or `None` if it may be exposed. `exposed` is the
    /// namespaced client-facing name (what quarantine is keyed by).
    fn blocked_reason(
        &self,
        exposed: &str,
        server_id: &str,
        orig: &str,
        tool: &Value,
    ) -> Option<&'static str> {
        if self
            .disabled
            .get(server_id)
            .is_some_and(|set| set.contains(orig))
        {
            return Some("disabled");
        }
        // Tool-granular profile scope: if this server is narrowed to an allow-list, a tool
        // not on it is outside the active profile's scope (hidden + blocked, same as disabled).
        if self
            .allow
            .get(server_id)
            .is_some_and(|set| !set.contains(orig))
        {
            return Some("outside the active profile's tool scope");
        }
        if self.deny_destructive && is_destructive(tool) {
            return Some("blocked by the destructive-tool policy");
        }
        if self.quarantined.contains(exposed) {
            return Some("quarantined after a high-risk change; re-approve to restore");
        }
        None
    }
}

/// One connected downstream server behind its own lock. A call to it only blocks
/// other calls to the SAME server (a single stdio pipe is one-in-flight by
/// design), never calls to other servers. Held as an `Arc` so an in-flight call
/// can keep the slot (and its live child process) alive across the downstream I/O
/// without holding the router lock, and survive a concurrent router replacement.
struct ServerSlot {
    id: String,
    inner: Mutex<DownstreamServer>,
    /// Fast-fail state for a server that keeps failing (dead/hung), so we don't pay
    /// its full read timeout on every call once it's clearly down.
    breaker: Mutex<Breaker>,
    /// Rebuild this server's connection from scratch (re-spawn a crashed stdio child
    /// / re-dial a dropped remote). Invoked only on the breaker's half-open probe,
    /// i.e. after the server has failed for a full cooldown, so a live server is never
    /// needlessly re-spawned on a transient blip. `None` = not reconnectable (e.g. a
    /// test fixture), in which case a dead server just stays fast-failed as before.
    reconnect: Option<Reconnect>,
}

/// Factory that rebuilds a downstream connection on demand. Supplied by the gateway
/// (which owns the registry + secret injection) so `router` stays free of spawn logic;
/// returns `None` if the server still can't be reached.
pub type Reconnect = Box<dyn Fn() -> Option<DownstreamServer> + Send + Sync>;

/// After this many consecutive health failures, a server's circuit opens.
const BREAKER_FAILURE_THRESHOLD: u32 = 3;
/// How long a tripped circuit stays open before one probe call is let through.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(20);

/// Per-server circuit breaker. Once a server racks up consecutive health failures
/// (timeouts / dead connections), the circuit opens and calls fast-fail for a
/// cooldown instead of each one waiting out the read timeout and piling up worker
/// threads. `now` is passed in so the transitions are unit-testable without sleeping.
#[derive(Default)]
struct Breaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl Breaker {
    /// Remaining open time if the circuit is tripped at `now`. A circuit whose
    /// cooldown has elapsed transitions to half-open here (clears `open_until` and
    /// returns `None`) so the next call probes the server.
    fn open_remaining(&mut self, now: Instant) -> Option<Duration> {
        match self.open_until {
            Some(t) if now < t => Some(t - now),
            Some(_) => {
                self.open_until = None;
                None
            }
            None => None,
        }
    }

    /// A successful call closes the circuit and clears the failure streak.
    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    /// A health failure; opens the circuit once the streak hits the threshold.
    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= BREAKER_FAILURE_THRESHOLD {
            self.open_until = Some(now + BREAKER_COOLDOWN);
        }
    }
}

/// Cloneable so the dispatcher can hold the live router as a `Mutex<Arc<Router>>`,
/// clone the `Arc` for a request, and release the lock BEFORE the (possibly
/// long-blocking) downstream call or human-approval hold. Cloning shares the
/// `Arc<ServerSlot>` connections, so it never re-spawns a server.
#[derive(Default, Clone)]
pub struct Router {
    servers: Vec<Arc<ServerSlot>>,
    /// Server id -> index into `servers`, so a call resolves its server without a
    /// linear scan and without locking any server to read its id.
    by_id: HashMap<String, usize>,
    /// Exposed (client-facing) tools, names already sanitized, in add order.
    tools: Vec<Value>,
    /// Exposed tool name -> (server id, original downstream tool name).
    routes: HashMap<String, (String, String)>,
    /// Exposed names already handed out, for collision disambiguation.
    seen: HashSet<String>,
    /// What may be exposed; applied as each server is added.
    policy: ToolPolicy,
    /// Per-tool exposure overrides (rename / re-describe), keyed by server id then ORIGINAL
    /// tool name (NOT the exposed name, so a rename or a `_2` collision suffix can't
    /// misalign the key). Applied while indexing; the route still points at the real
    /// downstream tool, so a rename never changes where a call goes.
    overrides: HashMap<String, HashMap<String, ToolOverride>>,
    /// Exposed name -> why it's hidden, for a clear message if a hidden tool is
    /// still called by name (e.g. via conduit_call_tool).
    blocked: HashMap<String, String>,
    /// Aggregated resources, passed through as-is (uris are server-scoped).
    resources: Vec<Value>,
    /// Resource uri -> owning server id (for resources/read).
    /// First writer in server add order wins; later collisions are refused
    /// rather than last-writer-wins (SOU-325).
    resource_routes: HashMap<String, String>,
    /// Aggregated resource templates (`uriTemplate` strings as advertised).
    resource_templates: Vec<Value>,
    /// Resource template uriTemplate -> owning server id. First writer wins.
    template_routes: HashMap<String, String>,
    /// Aggregated prompts, names namespaced like tools.
    prompts: Vec<Value>,
    /// Exposed prompt name -> (server id, original prompt name).
    prompt_routes: HashMap<String, (String, String)>,
}

impl Router {
    pub fn new() -> Self {
        Router::default()
    }

    /// A router that enforces `policy` as servers are added.
    pub fn with_policy(policy: ToolPolicy) -> Self {
        Router {
            policy,
            ..Router::default()
        }
    }

    /// Set the per-tool exposure overrides. Must be called BEFORE `add`/`refresh`, since
    /// they're applied while indexing each server's tools.
    pub fn set_overrides(&mut self, overrides: HashMap<String, HashMap<String, ToolOverride>>) {
        self.overrides = overrides;
    }

    /// The real `(server id, original tool name)` an exposed name routes to, or `None` if
    /// unknown. Callers that need a call's provenance or server-scoping MUST use this rather
    /// than string-splitting the exposed name on `__` — that split silently mis-derives the
    /// server for a renamed tool (overrides) or any server id containing `__`.
    pub fn route_of(&self, exposed: &str) -> Option<(&str, &str)> {
        self.routes.get(exposed).map(|(s, t)| (s.as_str(), t.as_str()))
    }

    /// Index one server's advertised tools/resources/templates/prompts into the
    /// exposed aggregation (names, routes, policy). Shared by `add` (a new
    /// server) and `rebuild_aggregation` (after a refresh). Within a server,
    /// `_2` collision suffixes are allocated by raw name rather than list
    /// position, so neither the call order nor a downstream reordering its own
    /// catalog can move them (see [`allocate_exposed_names`](Self::allocate_exposed_names)).
    fn index_server(
        &mut self,
        server_id: &str,
        tools: &[Value],
        resources: &[Value],
        resource_templates: &[Value],
        prompts: &[Value],
        route_mcp_apps: bool,
    ) {
        // Allocate the exposed name regardless of policy so toggling one tool
        // never renames its siblings (their `_2` suffixes stay put), and in an
        // order that doesn't depend on how the server happened to list them.
        let tool_names = self.allocate_exposed_names(server_id, tools);
        for (idx, tool) in tools.iter().enumerate() {
            let Some(orig) = tool.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let base = tool_names[idx]
                .clone()
                .expect("a tool with a name always gets an allocated exposed name");
            // Apply the user's exposure override (keyed by the ORIGINAL name) BEFORE
            // evaluating policy, so the quarantine check (keyed by the client-facing
            // name) sees the SAME name the client will call. Evaluating it on the
            // pre-rename base name meant a renamed tool could never be quarantined, and
            // the app would show it quarantined while the gateway kept routing it (#423).
            // Cloned to owned so we don't hold a borrow of `self.overrides` across the
            // `self.seen` mutation below. A rename that is empty or would collide with an
            // existing exposed name is ignored (keep the base) so routing stays
            // unambiguous. Both the base name (reserved by allocate_exposed_names) and the
            // rename's own slot stay reserved in `seen`, even when the tool ends up
            // blocked, so neither can be reused by a sibling's `_2` suffix.
            let ov = self.overrides.get(server_id).and_then(|m| m.get(orig));
            let ov_name = ov.and_then(|o| o.name.clone());
            let ov_desc = ov.and_then(|o| o.description.clone());
            let exposed = match ov_name {
                Some(new) => {
                    let cand = sanitize_segment(&new);
                    if !cand.is_empty() && self.seen.insert(cand.clone()) {
                        cand
                    } else {
                        base
                    }
                }
                None => base,
            };
            // Policy: disabled / scope / destructive gate on the ORIGINAL downstream
            // name (server_id + orig); quarantine gates on the final exposed name.
            if let Some(reason) = self.policy.blocked_reason(&exposed, server_id, orig, tool) {
                self.blocked.insert(exposed, reason.to_string());
                continue;
            }
            let mut t = tool.clone();
            if let Some(desc) = ov_desc {
                t["description"] = json!(desc);
            }
            t["name"] = json!(exposed);
            if let Some(schema) = t.get_mut("inputSchema") {
                inline_refs(schema);
            }
            self.tools.push(t);
            self.routes
                .insert(exposed, (server_id.to_string(), orig.to_string()));

            // UI-only resources are allowed to stay out of resources/list. The
            // tool linkage is therefore an authoritative route hint, subject to
            // the same first-writer collision rule as ordinary resources below.
            if route_mcp_apps {
                if let Some(uri) = mcp_app_resource_uri(tool) {
                    match self.resource_routes.get(uri) {
                        Some(owner) if owner != server_id => {
                            eprintln!(
                                "toolport: MCP App resource URI collision on '{uri}': keeping owner '{owner}', refusing claim from '{server_id}'"
                            );
                        }
                        Some(_) => {}
                        None => {
                            self.resource_routes
                                .insert(uri.to_string(), server_id.to_string());
                        }
                    }
                }
            }
        }

        // Resources: pass uris through unchanged and remember which server owns
        // each, so resources/read can reach it. First writer in server add order
        // owns a colliding bare URI (SOU-325); later claims are refused so a
        // hostile or overlapping registry entry cannot steal reads.
        for resource in resources {
            if let Some(uri) = resource.get("uri").and_then(|u| u.as_str()) {
                match self.resource_routes.get(uri) {
                    Some(owner) if owner != server_id => {
                        eprintln!(
                            "toolport: resource URI collision on '{uri}': keeping owner '{owner}', refusing claim from '{server_id}'"
                        );
                    }
                    Some(_) => {
                        // The route may already have come from this server's MCP
                        // App tool metadata. Keep the explicit resource visible in
                        // resources/list, while still deduplicating repeated rows.
                        if !self.resources.iter().any(|listed| {
                            listed.get("uri").and_then(Value::as_str) == Some(uri)
                        }) {
                            self.resources.push(resource.clone());
                        }
                    }
                    None => {
                        self.resources.push(resource.clone());
                        self.resource_routes
                            .insert(uri.to_string(), server_id.to_string());
                    }
                }
            }
        }

        // Resource templates: same first-writer ownership on uriTemplate so
        // completion and expanded-URI reads stay deterministic under collisions.
        for template in resource_templates {
            if let Some(uri_template) = template.get("uriTemplate").and_then(|u| u.as_str()) {
                match self.template_routes.get(uri_template) {
                    Some(owner) if owner != server_id => {
                        eprintln!(
                            "toolport: resource template collision on '{uri_template}': keeping owner '{owner}', refusing claim from '{server_id}'"
                        );
                    }
                    Some(_) => {}
                    None => {
                        self.resource_templates.push(template.clone());
                        self.template_routes
                            .insert(uri_template.to_string(), server_id.to_string());
                    }
                }
            }
        }

        // Prompts: namespace names like tools so two servers can't collide, and
        // allocate them in the same order-independent way.
        let prompt_names = self.allocate_exposed_names(server_id, prompts);
        for (idx, prompt) in prompts.iter().enumerate() {
            let Some(orig) = prompt.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let exposed = prompt_names[idx]
                .clone()
                .expect("a prompt with a name always gets an allocated exposed name");
            let mut p = prompt.clone();
            p["name"] = json!(exposed);
            self.prompts.push(p);
            self.prompt_routes
                .insert(exposed, (server_id.to_string(), orig.to_string()));
        }
    }

    pub fn add(&mut self, server: DownstreamServer) {
        self.add_with_reconnect(server, None);
    }

    /// Add a server whose connection can be rebuilt on demand (see [`Reconnect`]). The
    /// router re-spawns it automatically if it dies mid-session; `add` is the
    /// non-reconnectable variant kept for tests and callers with no factory.
    pub fn add_with_reconnect(&mut self, server: DownstreamServer, reconnect: Option<Reconnect>) {
        let id = server.id.clone();
        let route_mcp_apps = supports_mcp_app_html(server.extensions());
        self.index_server(
            &id,
            &server.tools,
            &server.resources,
            &server.resource_templates,
            &server.prompts,
            route_mcp_apps,
        );
        let idx = self.servers.len();
        self.servers.push(Arc::new(ServerSlot {
            id: id.clone(),
            inner: Mutex::new(server),
            breaker: Mutex::new(Breaker::default()),
            reconnect,
        }));
        self.by_id.insert(id, idx);
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// Allocate exposed names for one server's `items` (tools or prompts),
    /// returned positionally so the caller keeps the server's own catalog order.
    ///
    /// Names are handed out in order of the item's RAW name rather than the order
    /// the server listed them in. Two names that sanitize to the same string
    /// (`get-user` and `get_user`) collide, and the loser takes a `_2` suffix;
    /// allocating in list order meant a downstream that reordered its
    /// `tools/list` across a refresh swapped that suffix between two real tools.
    /// The client's cached name then pointed at the *other* tool, so calls kept
    /// working and silently went somewhere new. Sorting on the raw name makes the
    /// assignment a property of the tools themselves, so list order can't move it.
    ///
    /// Cross-server collisions can't arise here: server ids are slugified to
    /// `[a-z0-9-]` and `sanitize_segment` only maps `-` to `_`, which is injective
    /// over that alphabet. So the contested namespace is always within one server.
    fn allocate_exposed_names(&mut self, server_id: &str, items: &[Value]) -> Vec<Option<String>> {
        fn raw_name(item: &Value) -> Option<&str> {
            item.get("name").and_then(|n| n.as_str())
        }
        let mut order: Vec<usize> = (0..items.len()).collect();
        // Ties (a server listing the same raw name twice) fall back to list
        // position, which keeps the sort total and the result reproducible.
        order.sort_by(|&a, &b| raw_name(&items[a]).cmp(&raw_name(&items[b])).then(a.cmp(&b)));
        let mut out = vec![None; items.len()];
        for i in order {
            if let Some(orig) = raw_name(&items[i]) {
                out[i] = Some(self.exposed_name(server_id, orig));
            }
        }
        out
    }

    /// Allocate a unique exposed name for `server_id`'s `tool`, sanitizing both
    /// halves and suffixing `_2`, `_3`, ... if two distinct tools would collide.
    fn exposed_name(&mut self, server_id: &str, tool: &str) -> String {
        let base = format!(
            "{}__{}",
            sanitize_segment(server_id),
            sanitize_segment(tool)
        );
        let mut name = base.clone();
        let mut i = 2;
        while !self.seen.insert(name.clone()) {
            name = format!("{base}_{i}");
            i += 1;
        }
        name
    }

    /// Every downstream tool, with its exposed (sanitized) name.
    pub fn aggregated_tools(&self) -> Vec<Value> {
        let mut tools = self.tools.clone();
        // MCP 2026-07-28 recommends deterministic tool ordering so both response
        // caches and LLM prompt caches survive incidental downstream reorderings.
        // Exposed names are unique, making them a stable total key across refreshes
        // and gateway restarts without changing routing ownership.
        tools.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        });
        tools
    }

    fn aggregate_cache_hints(
        &self,
        select: impl Fn(&DownstreamServer) -> Option<CacheHint>,
    ) -> Option<CacheHint> {
        let mut aggregate: Option<CacheHint> = None;
        for slot in &self.servers {
            let server = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(hint) = select(&server) else {
                continue;
            };
            aggregate = Some(match aggregate {
                Some(current) => current.merge(hint),
                None => hint,
            });
        }
        aggregate
    }

    pub fn tools_cache_hint(&self) -> Option<CacheHint> {
        self.aggregate_cache_hints(|server| Some(server.tool_cache_hint()))
    }

    pub fn resources_cache_hint(&self) -> Option<CacheHint> {
        self.aggregate_cache_hints(DownstreamServer::resource_cache_hint)
    }

    pub fn resource_templates_cache_hint(&self) -> Option<CacheHint> {
        self.aggregate_cache_hints(DownstreamServer::resource_template_cache_hint)
    }

    pub fn prompts_cache_hint(&self) -> Option<CacheHint> {
        self.aggregate_cache_hints(DownstreamServer::prompt_cache_hint)
    }

    /// Aggregate opaque extension settings from the selected modern downstream
    /// servers. Identical declarations are preserved byte-for-byte. If two
    /// servers use the same identifier with different settings, omit that
    /// identifier: there is no single capability value Toolport can truthfully
    /// advertise for the aggregate (SOU-453).
    pub fn aggregated_extensions(
        &self,
        include_server: impl Fn(&str) -> bool,
    ) -> serde_json::Map<String, Value> {
        let mut values: BTreeMap<String, Option<Value>> = BTreeMap::new();
        for slot in &self.servers {
            if !include_server(&slot.id) {
                continue;
            }
            let server = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (identifier, settings) in server.extensions() {
                match values.entry(identifier.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(Some(settings.clone()));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if entry.get().as_ref() != Some(settings) {
                            entry.insert(None);
                        }
                    }
                }
            }
        }
        values
            .into_iter()
            .filter_map(|(identifier, settings)| settings.map(|settings| (identifier, settings)))
            .collect()
    }

    /// One connected server's settings for an extension. Callers that depend on
    /// a particular setting (rather than mere identifier presence) must inspect
    /// this server-local value instead of the aggregate.
    pub fn server_extension_settings(&self, server_id: &str, identifier: &str) -> Option<Value> {
        let &index = self.by_id.get(server_id)?;
        self.servers[index]
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extensions()
            .get(identifier)
            .cloned()
    }

    /// Positive downstream TTLs schedule a refresh at their expiry. Zero/missing
    /// hints do not create a one-second polling loop; notifications still invalidate
    /// them immediately through the existing dirty-bit path.
    pub fn expired_cache_kinds(&self) -> u8 {
        let mut kinds = 0;
        for slot in &self.servers {
            let server = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if server.tool_cache_hint().needs_refresh() {
                kinds |= crate::downstream::change::TOOLS;
            }
            if server.resource_cache_hint().is_some_and(|hint| hint.needs_refresh())
                || server
                    .resource_template_cache_hint()
                    .is_some_and(|hint| hint.needs_refresh())
            {
                kinds |= crate::downstream::change::RESOURCES;
            }
            if server.prompt_cache_hint().is_some_and(|hint| hint.needs_refresh()) {
                kinds |= crate::downstream::change::PROMPTS;
            }
        }
        kinds
    }

    /// Re-query every live server's tool list (a downstream announced a
    /// `tools/list_changed`) and rebuild the exposed aggregation in place. Unlike
    /// a full rebuild this keeps the existing connections, so a runtime or
    /// session-scoped tool change isn't lost to a freshly spawned process that
    /// never saw it.
    pub fn refresh_tools(&mut self) {
        // `&mut self` is exclusive, so locking each slot here can't contend.
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_tools();
        }
        self.rebuild_aggregation();
    }

    pub fn refresh_stale_tools(&mut self) {
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_tools_if_stale();
        }
        self.rebuild_aggregation();
    }

    /// Re-query every live server's resource list (a downstream announced a
    /// `resources/list_changed`) and rebuild the exposed aggregation in place.
    /// Also refreshes resource templates: MCP has no separate templates
    /// list-change notification, so this is the protocol-aligned trigger.
    /// Mirrors [`refresh_tools`].
    pub fn refresh_resources(&mut self) {
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_resources();
        }
        self.rebuild_aggregation();
    }

    pub fn refresh_stale_resources(&mut self) {
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_resources_if_stale();
        }
        self.rebuild_aggregation();
    }

    /// Re-query every live server's prompt list (a downstream announced a
    /// `prompts/list_changed`) and rebuild the exposed aggregation in place.
    /// Mirrors [`refresh_tools`].
    pub fn refresh_prompts(&mut self) {
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_prompts();
        }
        self.rebuild_aggregation();
    }

    pub fn refresh_stale_prompts(&mut self) {
        for slot in &self.servers {
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_prompts_if_stale();
        }
        self.rebuild_aggregation();
    }

    /// Forward one JSON-RPC notification to every connected downstream server.
    pub fn notify_all_downstreams(&self, method: &str, params: Value) {
        for slot in &self.servers {
            if let Ok(mut ds) = slot.inner.lock() {
                let _ = ds.notify_downstream(method, params.clone());
            }
        }
    }

    /// Replace the quarantine set and re-derive the exposed aggregation so newly
    /// quarantined tools are hidden (or re-approved ones restored) without re-querying
    /// downstream. Cheap: it only re-applies the policy to the cached tool lists.
    pub fn requarantine(&mut self, quarantined: BTreeSet<String>) {
        self.policy.quarantined = quarantined;
        self.rebuild_aggregation();
    }

    /// The quarantine set this router is currently enforcing. Lets a caller diff the
    /// live set against the persisted one and skip `requarantine` (and the client
    /// `list_changed` that follows it) when nothing actually changed.
    pub fn quarantined(&self) -> &BTreeSet<String> {
        &self.policy.quarantined
    }

    /// Why an exposed tool is hidden from the catalog / refused by [`Self::route_call`],
    /// if it is. Same `blocked` map `route_call` consults — used by post-HITL revalidation
    /// (SOU-321) so an approval held across a live `requarantine` can fail closed without
    /// attempting the downstream call.
    pub fn block_reason(&self, exposed_name: &str) -> Option<&str> {
        self.blocked.get(exposed_name).map(String::as_str)
    }

    /// Re-adopt the routes (and exposed tool entries) for tools that came back
    /// from a previous catalog during a guarded rebuild. The rebuild guard
    /// keeps the previous catalog for a server whose fresh connect implausibly
    /// shrank, but the rebuilt router was indexed from that degraded connect,
    /// so `route_of` would miss every restored tool while the cache still
    /// advertises it. `previous` is the pre-rebuild router, whose `routes` map
    /// is the authoritative `(server id, original downstream name)` source --
    /// never re-derive the original name by splitting the exposed name on `__`
    /// (overrides and `_2` collision suffixes make that split wrong, see
    /// [`Self::route_of`]). Only exposed names this router does not already
    /// route are adopted, so a healthy server is never touched. Policy is
    /// re-evaluated per restored tool before adoption: the rebuilt router only
    /// indexed the degraded connect, so tools quarantined or disabled since the
    /// previous build are absent from its `blocked` map and must not slip back
    /// in through the guarded catalog (which still carries them from the cache).
    pub fn adopt_restored_routes(&mut self, previous: &Router, catalog: &[Value]) {
        for tool in catalog {
            let Some(exposed) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            if self.routes.contains_key(exposed) || self.blocked.contains_key(exposed) {
                continue;
            }
            // The previous router indexed the same exposed name; reuse its
            // (server, original) pair verbatim instead of re-deriving it.
            let Some((server_id, original)) = previous.route_of(exposed) else {
                continue;
            };
            // Re-evaluate policy before adopting. The rebuilt router only indexed
            // the degraded connect, so a tool quarantined / disabled / scoped out
            // since the previous build has no entry in `self.blocked` yet and the
            // guarded catalog (from the disk cache) still carries it. Adopting it
            // now would silently bypass the quarantine and scope guards while the
            // cache keeps advertising it (review on #717).
            if let Some(reason) = self
                .policy
                .blocked_reason(exposed, server_id, original, tool)
            {
                self.blocked.insert(exposed.to_string(), reason.to_string());
                continue;
            }
            self.routes
                .insert(exposed.to_string(), (server_id.to_string(), original.to_string()));
            // Re-adopt the exposed tool entry so aggregated_tools() and the
            // quarantine/fingerprint paths see the restored tool, matching what
            // the cache advertises.
            if !self.tools.iter().any(|t| {
                t.get("name").and_then(Value::as_str) == Some(exposed)
            }) {
                self.tools.push(tool.clone());
            }
            self.seen.insert(exposed.to_string());
        }
    }

    /// Re-derive the exposed tool/resource/template/prompt aggregation from the
    /// current servers' (possibly refreshed) lists, in the original add order so
    /// exposed names and their `_2` collision suffixes stay stable. The server
    /// set itself is unchanged, so `servers` and `by_id` are kept.
    fn rebuild_aggregation(&mut self) {
        self.tools.clear();
        self.routes.clear();
        self.seen.clear();
        self.blocked.clear();
        self.resources.clear();
        self.resource_routes.clear();
        self.resource_templates.clear();
        self.template_routes.clear();
        self.prompts.clear();
        self.prompt_routes.clear();
        // Clone the Arcs (cheap) and snapshot each slot's lists under its lock, so
        // we hold neither a slot lock nor a borrow of `self.servers` across the
        // `&mut self` re-index.
        let slots: Vec<Arc<ServerSlot>> = self.servers.clone();
        for slot in &slots {
            let (tools, resources, resource_templates, prompts, route_mcp_apps) = {
                let s = slot
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    s.tools.clone(),
                    s.resources.clone(),
                    s.resource_templates.clone(),
                    s.prompts.clone(),
                    supports_mcp_app_html(s.extensions()),
                )
            };
            self.index_server(
                &slot.id,
                &tools,
                &resources,
                &resource_templates,
                &prompts,
                route_mcp_apps,
            );
        }
    }

    /// The slot owning `server_id`, as a cloned `Arc` so the caller can lock and
    /// use it after dropping any borrow of the router (this is what lets the
    /// downstream call run without holding the router lock).
    fn slot_for(&self, server_id: &str) -> Result<Arc<ServerSlot>, String> {
        self.by_id
            .get(server_id)
            .and_then(|&i| self.servers.get(i))
            .cloned()
            .ok_or_else(|| format!("no connected server '{server_id}'"))
    }

    /// Retry wrapper that releases the per-server Mutex during the backoff sleep
    /// so concurrent calls to the same server aren't blocked while one call waits
    /// for a rate-limit or connection-retry delay.
    fn call_with_retry<T, F>(
        &self,
        slot: &Arc<ServerSlot>,
        cancel: Option<&CancelContext>,
        dispatch_cancelled_continuation: bool,
        mut f: F,
    ) -> Result<T, String>
    where
        F: FnMut(&mut DownstreamServer) -> Result<T, TransportError>,
    {
        if !dispatch_cancelled_continuation
            && cancel.is_some_and(CancelContext::is_cancelled)
        {
            return Err("request cancelled before downstream attempt".to_string());
        }
        // Circuit breaker: a server that just failed repeatedly is fast-failed here,
        // BEFORE taking its `inner` lock, so a dead/hung server neither pays its full
        // read timeout again nor queues callers behind an in-flight timing-out call.
        // A call that gets past this after the cooldown is the half-open PROBE: if it
        // still fails, the server has been down for a full cooldown and we try to
        // re-spawn it (below) rather than fast-failing forever.
        let is_probe = {
            let mut breaker = slot
                .breaker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(remaining) = breaker.open_remaining(Instant::now()) {
                return Err(format!(
                    "server '{}' is temporarily unavailable (too many recent failures; retrying in {}s)",
                    slot.id,
                    remaining.as_secs() + 1
                ));
            }
            // Cooldown elapsed but the failure streak is still at/over threshold: this
            // call is the half-open probe of a tripped breaker.
            breaker.consecutive_failures >= BREAKER_FAILURE_THRESHOLD
        };
        let mut attempt = 0u32;
        loop {
            if !dispatch_cancelled_continuation
                && cancel.is_some_and(CancelContext::is_cancelled)
            {
                return Err("request cancelled before downstream attempt".to_string());
            }
            let result = {
                let mut server = slot.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                f(&mut server)
            };
            match result {
                Ok(v) => {
                    slot.breaker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_success();
                    return Ok(v);
                }
                Err(TransportError::Retry { retry_after, message }) if attempt < HTTP_MAX_RETRIES => {
                    let wait = retry_wait(retry_after, attempt);
                    eprintln!("conduit: retrying downstream call after {wait:?}: {message}");
                    wait_for_retry_or_cancel(wait, cancel).map_err(|error| error.to_string())?;
                    attempt += 1;
                }
                Err(e) => {
                    // Only a health failure (timeout / dead connection / exhausted
                    // retries) counts toward the breaker; a normal error response does
                    // not disable the server.
                    if e.is_health_failure() {
                        // The server has now failed for a full cooldown and the probe
                        // confirms it's still down. Re-spawn the connection once and
                        // retry: this recovers a crashed stdio child or a dropped remote
                        // that the plain breaker would otherwise fast-fail forever (its
                        // self-heal only fires when EVERY server is dead). Gated on the
                        // probe so a live server is never re-spawned on a transient blip.
                        if is_probe {
                            if cancel.is_some_and(CancelContext::is_cancelled) {
                                return Err("request cancelled before downstream reconnect".to_string());
                            }
                            if let Some(v) = self.reconnect_and_retry(slot, cancel, &mut f) {
                                return v;
                            }
                        }
                        slot.breaker
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .record_failure(Instant::now());
                    }
                    return Err(e.to_string());
                }
            }
        }
    }

    /// Re-spawn a slot's downstream connection and retry the call once on the fresh
    /// transport. Returns `Some(result)` when a reconnect was attempted (so the caller
    /// stops), or `None` when the slot has no reconnect factory (fall through to the
    /// normal breaker-failure path). The spawn runs without holding the `inner` lock so
    /// a slow re-spawn doesn't wedge other callers to the same server.
    fn reconnect_and_retry<T, F>(
        &self,
        slot: &Arc<ServerSlot>,
        cancel: Option<&CancelContext>,
        f: &mut F,
    ) -> Option<Result<T, String>>
    where
        F: FnMut(&mut DownstreamServer) -> Result<T, TransportError>,
    {
        let factory = slot.reconnect.as_ref()?;
        eprintln!("conduit: server '{}' is down; re-spawning it", slot.id);
        let Some(fresh) = factory() else {
            eprintln!("conduit: re-spawn of '{}' failed; leaving it fast-failed", slot.id);
            return None; // still unreachable: fall through to record_failure
        };
        if cancel.is_some_and(CancelContext::is_cancelled) {
            return Some(Err(
                "request cancelled before retrying the reconnected downstream".to_string(),
            ));
        }
        let retry = {
            let mut server = slot.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *server = fresh; // swap the live child/connection for the fresh one
            f(&mut server)
        };
        let mut breaker = slot
            .breaker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(match retry {
            Ok(v) => {
                eprintln!("conduit: server '{}' recovered after re-spawn", slot.id);
                breaker.record_success();
                Ok(v)
            }
            Err(e) => {
                if e.is_health_failure() {
                    breaker.record_failure(Instant::now());
                }
                Err(e.to_string())
            }
        })
    }

    /// Forward an exposed tool call to its owning downstream server, using that
    /// server's original tool name. Takes `&self`: it locks only the target
    /// server, so concurrent calls to different servers run in parallel while
    /// calls to the same server (one stdio pipe) serialize.
    pub fn route_call(&self, exposed_name: &str, arguments: Value) -> Result<Value, String> {
        self.route_call_with_cancel(exposed_name, arguments, None, None)
    }

    /// `meta` carries the upstream client's `params._meta` through to the
    /// downstream server (SOU-444). The router does not interpret it; the
    /// downstream layer decides which keys are relayable.
    pub fn route_call_with_cancel(
        &self,
        exposed_name: &str,
        arguments: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
    ) -> Result<Value, String> {
        self.route_call_with_cancel_and_mrtr(exposed_name, arguments, cancel, meta, None)
    }

    pub fn route_call_with_cancel_and_mrtr(
        &self,
        exposed_name: &str,
        arguments: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
        mrtr: Option<&MrtrRequest>,
    ) -> Result<Value, String> {
        if let Some(reason) = self.blocked.get(exposed_name) {
            return Err(format!("tool '{exposed_name}' is {reason}"));
        }
        let (server_id, tool) = self.routes.get(exposed_name).ok_or_else(|| {
            // Several client harnesses expose gateway tools to their model as
            // `mcp__<gateway-alias>__<tool>`; models then reuse that spelling inside
            // toolport_run_script and land here (observed with Codex, 2026-08-13).
            // Point at the name that will actually route instead of a dead end.
            let client_prefixed = exposed_name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split_once("__"))
                .map(|(_, tool)| tool)
                .filter(|candidate| self.routes.contains_key(*candidate));
            match client_prefixed {
                Some(real) => format!(
                    "no route for tool '{exposed_name}'; that looks like a client-side alias - \
                     inside Toolport the tool is named '{real}', call that instead"
                ),
                None => format!("no route for tool '{exposed_name}'"),
            }
        })?;
        let slot = self.slot_for(server_id)?;
        let (result, downstream_supports_tasks) = self.call_with_retry(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            |server| {
            let supports_tasks = server
                .extensions()
                .contains_key("io.modelcontextprotocol/tasks");
            server
                .call_with_cancel_and_mrtr(
                    tool,
                    arguments.clone(),
                    cancel.clone(),
                    meta,
                    mrtr,
                )
                .map(|result| (result, supports_tasks))
            },
        )?;
        if result.get("resultType").and_then(Value::as_str) == Some("task") {
            if !client_supports_tasks(meta) {
                return Err(
                    "downstream returned a task without the required client capability"
                        .to_string(),
                );
            }
            if !downstream_supports_tasks {
                return Err(
                    "downstream returned a task without advertising the Tasks extension"
                        .to_string(),
                );
            }
            expose_task_result(result, server_id)
        } else {
            Ok(result)
        }
    }

    /// Owning server encoded into a Toolport task handle. Used for the HTTP
    /// client's allowed-server check before any downstream request is sent.
    pub fn task_server(&self, task_id: &str) -> Option<String> {
        decode_task_id(task_id).ok().map(|(server, _)| server)
    }

    /// Route one Tasks extension operation to the server that minted the handle,
    /// translating the opaque client-facing id in both directions.
    pub fn route_task(
        &self,
        method: &str,
        params: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
    ) -> Result<Value, String> {
        if !matches!(method, "tasks/get" | "tasks/update" | "tasks/cancel") {
            return Err(format!("unsupported task method '{method}'"));
        }
        if !client_supports_tasks(meta) {
            return Err(format!(
                "{method} requires the io.modelcontextprotocol/tasks client capability"
            ));
        }
        let exposed = params
            .get("taskId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{method} requires params.taskId"))?
            .to_string();
        let (server_id, native_task_id) = decode_task_id(&exposed)?;
        let slot = self.slot_for(&server_id)?;
        let mut forwarded = params;
        forwarded["taskId"] = json!(native_task_id);
        let result = self.call_with_retry(&slot, cancel.as_ref(), false, |server| {
            server.task_request(method, forwarded.clone(), cancel.clone(), meta)
        })?;
        let mut result = result;
        if method == "tasks/get" {
            if result.get("taskId").and_then(Value::as_str).is_none() {
                return Err("downstream task result is missing taskId".to_string());
            }
            result["taskId"] = json!(exposed);
        } else if result.get("taskId").is_some() {
            // update/cancel are empty acknowledgements in the Tasks spec. If a
            // non-conforming downstream includes its native id, never leak it
            // across the gateway boundary.
            result["taskId"] = json!(exposed);
        }
        Ok(result)
    }

    /// Every downstream resource, uris unchanged.
    pub fn aggregated_resources(&self) -> Vec<Value> {
        self.resources.clone()
    }

    /// Every downstream resource template, `uriTemplate` values unchanged.
    pub fn aggregated_resource_templates(&self) -> Vec<Value> {
        self.resource_templates.clone()
    }

    /// Every downstream prompt, with its exposed (namespaced) name.
    pub fn aggregated_prompts(&self) -> Vec<Value> {
        self.prompts.clone()
    }

    /// The server that advertised resource `uri`, if any. Used to scope a registered
    /// HTTP client's resource access to its allowed server set (see the gateway).
    /// Falls back to template ownership when `uri` is an expansion of a known
    /// resource template and was never listed as a concrete resource.
    pub fn resource_server(&self, uri: &str) -> Option<&str> {
        if let Some(owner) = self.resource_routes.get(uri) {
            return Some(owner.as_str());
        }
        self.template_owner_for_uri(uri)
    }

    /// The server that owns resource template `uri_template`, if any.
    pub fn resource_template_server(&self, uri_template: &str) -> Option<&str> {
        self.template_routes
            .get(uri_template)
            .map(String::as_str)
    }

    /// The server that owns the exposed prompt `name`, if any. Used to scope a
    /// registered HTTP client's prompt access to its allowed server set.
    pub fn prompt_server(&self, exposed_name: &str) -> Option<&str> {
        self.prompt_routes.get(exposed_name).map(|(s, _)| s.as_str())
    }

    /// The original (downstream) prompt name for an exposed prompt, if any.
    pub fn prompt_downstream_name(&self, exposed_name: &str) -> Option<&str> {
        self.prompt_routes
            .get(exposed_name)
            .map(|(_, name)| name.as_str())
    }

    /// First-writer template whose `uriTemplate` expands to `uri`, if any.
    fn template_owner_for_uri(&self, uri: &str) -> Option<&str> {
        for template in &self.resource_templates {
            let Some(uri_template) = template.get("uriTemplate").and_then(|u| u.as_str()) else {
                continue;
            };
            if uri_matches_template(uri, uri_template) {
                return self.template_routes.get(uri_template).map(String::as_str);
            }
        }
        None
    }

    /// Resolve which server owns a `completion/complete` reference, and the
    /// params to forward (prompt names un-namespaced). Returns
    /// `(server_id, downstream_params)`.
    pub fn resolve_completion(
        &self,
        params: &Value,
    ) -> Result<(String, Value), String> {
        let ref_obj = params
            .get("ref")
            .ok_or_else(|| "completion/complete requires params.ref".to_string())?;
        let ref_type = ref_obj
            .get("type")
            .and_then(|t| t.as_str())
            .ok_or_else(|| "completion/complete ref.type is required".to_string())?;
        let mut forwarded = params.clone();
        match ref_type {
            "ref/prompt" => {
                let exposed = ref_obj
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| "completion/complete ref/prompt requires name".to_string())?;
                let (server_id, original) = self
                    .prompt_routes
                    .get(exposed)
                    .cloned()
                    .ok_or_else(|| format!("no route for prompt '{exposed}'"))?;
                if let Some(name_slot) = forwarded
                    .get_mut("ref")
                    .and_then(|r| r.as_object_mut())
                    .and_then(|r| r.get_mut("name"))
                {
                    *name_slot = json!(original);
                }
                Ok((server_id, forwarded))
            }
            "ref/resource" => {
                let uri = ref_obj
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .ok_or_else(|| "completion/complete ref/resource requires uri".to_string())?;
                // Prefer exact template ownership; fall back to matching an
                // expanded URI against known templates (and then concrete resources).
                let server_id = self
                    .template_routes
                    .get(uri)
                    .cloned()
                    .or_else(|| self.resource_server(uri).map(str::to_string))
                    .ok_or_else(|| {
                        format!("no server owns resource template or uri '{uri}'")
                    })?;
                Ok((server_id, forwarded))
            }
            other => Err(format!("unsupported completion ref type '{other}'")),
        }
    }

    /// Read a resource by uri from whichever server advertised it (or owns a
    /// matching resource template). `&self`: locks only the owning server
    /// (see `route_call`).
    pub fn read_resource(&self, uri: &str) -> Result<Value, String> {
        self.read_resource_with_cancel(uri, None, None)
    }

    pub fn read_resource_with_cancel(
        &self,
        uri: &str,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
    ) -> Result<Value, String> {
        self.read_resource_with_cancel_and_mrtr(uri, cancel, meta, None)
    }

    pub fn read_resource_with_cancel_and_mrtr(
        &self,
        uri: &str,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
        mrtr: Option<&MrtrRequest>,
    ) -> Result<Value, String> {
        let server_id = self
            .resource_server(uri)
            .ok_or_else(|| format!("no server owns resource '{uri}'"))?
            .to_string();
        let slot = self.slot_for(&server_id)?;
        self.call_with_retry(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            |server| {
            server.read_resource_with_cancel_and_mrtr(uri, cancel.clone(), meta, mrtr)
            },
        )
    }

    /// Subscribe to resource-updated notifications on the owning downstream
    /// (concrete first-writer, then template expansion — same as
    /// [`read_resource`]). SOU-394.
    pub fn subscribe_resource(&self, uri: &str) -> Result<Value, String> {
        let server_id = self
            .resource_server(uri)
            .ok_or_else(|| format!("no server owns resource '{uri}'"))?
            .to_string();
        let slot = self.slot_for(&server_id)?;
        self.call_with_retry(&slot, None, false, |server| server.subscribe_resource(uri))
    }

    /// Unsubscribe from resource-updated notifications on the owning downstream
    /// (resolved from current aggregation). Prefer
    /// [`unsubscribe_resource_on_server`] when the original owner was recorded
    /// at subscribe time so rebuild ownership drift cannot redirect the unsub.
    pub fn unsubscribe_resource(&self, uri: &str) -> Result<Value, String> {
        let server_id = self
            .resource_server(uri)
            .ok_or_else(|| format!("no server owns resource '{uri}'"))?
            .to_string();
        self.unsubscribe_resource_on_server(&server_id, uri)
    }

    /// Unsubscribe on a specific downstream server id (the owner recorded when
    /// the first upstream client subscribed). Used for session cleanup and
    /// last-holder unsub so a later ownership change cannot leave a live sub
    /// on the original server or hit the wrong one.
    pub fn unsubscribe_resource_on_server(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<Value, String> {
        let slot = self.slot_for(server_id)?;
        self.call_with_retry(&slot, None, false, |server| server.unsubscribe_resource(uri))
    }

    /// Get a prompt by its exposed name, forwarding the server's real name.
    /// `&self`: locks only the owning server (see `route_call`).
    pub fn get_prompt(&self, exposed_name: &str, arguments: Value) -> Result<Value, String> {
        self.get_prompt_with_cancel(exposed_name, arguments, None, None)
    }

    pub fn get_prompt_with_cancel(
        &self,
        exposed_name: &str,
        arguments: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
    ) -> Result<Value, String> {
        self.get_prompt_with_cancel_and_mrtr(exposed_name, arguments, cancel, meta, None)
    }

    pub fn get_prompt_with_cancel_and_mrtr(
        &self,
        exposed_name: &str,
        arguments: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
        mrtr: Option<&MrtrRequest>,
    ) -> Result<Value, String> {
        let (server_id, name) = self
            .prompt_routes
            .get(exposed_name)
            .cloned()
            .ok_or_else(|| format!("no route for prompt '{exposed_name}'"))?;
        let slot = self.slot_for(&server_id)?;
        self.call_with_retry(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            |server| {
            server.get_prompt_with_cancel_and_mrtr(
                &name,
                arguments.clone(),
                cancel.clone(),
                meta,
                mrtr,
            )
            },
        )
    }

    /// Forward `completion/complete` to the owning downstream server, remapping
    /// namespaced prompt names back to the server's original names.
    pub fn complete(&self, params: Value) -> Result<Value, String> {
        self.complete_with_cancel(params, None)
    }

    pub fn complete_with_cancel(
        &self,
        params: Value,
        cancel: Option<CancelContext>,
    ) -> Result<Value, String> {
        let (server_id, forwarded) = self.resolve_completion(&params)?;
        let slot = self.slot_for(&server_id)?;
        self.call_with_retry(&slot, cancel.as_ref(), false, |server| {
            server.complete_with_cancel(forwarded.clone(), cancel.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn unrouted_client_prefixed_alias_error_names_the_real_tool() {
        let mut router = Router::new();
        router.routes.insert(
            "deepwiki__read".to_string(),
            ("s".to_string(), "read".to_string()),
        );
        let err = router
            .route_call_with_cancel(
                "mcp__toolport__deepwiki__read",
                serde_json::json!({}),
                None,
                None,
            )
            .unwrap_err();
        assert!(err.contains("'deepwiki__read'"), "{err}");
        assert!(err.contains("client-side alias"), "{err}");

        // No routed candidate behind the prefix: the plain message, no bad advice.
        let plain = router
            .route_call_with_cancel("mcp__unknown__tool", serde_json::json!({}), None, None)
            .unwrap_err();
        assert_eq!(plain, "no route for tool 'mcp__unknown__tool'");
    }

    #[test]
    fn breaker_opens_after_threshold_then_half_opens_after_cooldown() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        // Below the threshold the circuit stays closed.
        for _ in 0..BREAKER_FAILURE_THRESHOLD - 1 {
            b.record_failure(t0);
            assert!(b.open_remaining(t0).is_none(), "closed below threshold");
        }
        // The threshold-th consecutive failure opens it.
        b.record_failure(t0);
        let rem = b.open_remaining(t0).expect("circuit should be open");
        assert!(rem > Duration::ZERO && rem <= BREAKER_COOLDOWN);
        // Still open partway through the cooldown.
        assert!(b.open_remaining(t0 + BREAKER_COOLDOWN / 2).is_some());
        // Once the cooldown elapses it half-opens: a probe is let through (None) and
        // the tripped state is cleared.
        assert!(b.open_remaining(t0 + BREAKER_COOLDOWN).is_none());
        assert!(b.open_remaining(t0 + BREAKER_COOLDOWN).is_none());
    }

    #[test]
    fn breaker_success_resets_the_streak() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        b.record_failure(t0);
        b.record_failure(t0);
        b.record_success(); // a good call clears the streak
        // Two failures alone no longer open it (needs THRESHOLD consecutive).
        b.record_failure(t0);
        b.record_failure(t0);
        assert!(b.open_remaining(t0).is_none(), "success reset the streak");
        // The threshold-th consecutive failure opens it.
        b.record_failure(t0);
        assert!(b.open_remaining(t0).is_some());
    }

    #[test]
    fn retry_wait_clamps_large_retry_after() {
        // A downstream advertising a huge Retry-After is clamped to our cap so it
        // can't pin the calling thread.
        assert_eq!(retry_wait(Some(Duration::from_secs(3600)), 0), HTTP_RETRY_CAP);
        // A reasonable Retry-After under the cap is honored as-is.
        assert_eq!(
            retry_wait(Some(Duration::from_secs(2)), 0),
            Duration::from_secs(2)
        );
        // With no Retry-After, it falls back to the exponential backoff schedule.
        assert_eq!(retry_wait(None, 0), backoff_delay(0));
        assert_eq!(retry_wait(None, 1), backoff_delay(1));
    }

    #[test]
    fn inline_refs_resolves_defs() {
        let mut schema = json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/Foo" } },
            "$defs": { "Foo": { "type": "string", "enum": ["x", "y"] } }
        });
        inline_refs(&mut schema);
        assert!(schema.get("$defs").is_none(), "defs should be dropped");
        assert_eq!(schema["properties"]["a"]["type"], "string");
        assert_eq!(schema["properties"]["a"]["enum"][0], "x");
        assert!(!serde_json::to_string(&schema).unwrap().contains("$ref"));
    }

    #[test]
    fn inline_refs_handles_definitions_keyword() {
        let mut schema = json!({
            "properties": { "b": { "$ref": "#/definitions/Bar" } },
            "definitions": { "Bar": { "type": "number" } }
        });
        inline_refs(&mut schema);
        assert_eq!(schema["properties"]["b"]["type"], "number");
        assert!(schema.get("definitions").is_none());
    }

    #[test]
    fn inline_refs_breaks_cycles() {
        let mut schema = json!({
            "$ref": "#/$defs/Node",
            "$defs": { "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } } }
        });
        inline_refs(&mut schema); // must terminate, not recurse forever
        assert_eq!(schema["type"], "object");
        // the cyclic inner ref collapses to {}, so nothing references out
        assert!(!serde_json::to_string(&schema).unwrap().contains("$ref"));
    }

    #[test]
    fn inline_refs_noop_without_defs() {
        let mut schema = json!({ "type": "object", "properties": { "x": { "type": "string" } } });
        let before = schema.clone();
        inline_refs(&mut schema);
        assert_eq!(schema, before);
    }

    #[test]
    fn inline_refs_resolves_json_pointer_into_properties() {
        // revenuecat-style: a property $refs another property by JSON Pointer.
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "alias": { "$ref": "#/properties/name" }
            }
        });
        inline_refs(&mut schema);
        assert_eq!(schema["properties"]["alias"]["type"], "string");
        assert_eq!(schema["properties"]["alias"]["minLength"], 1);
        assert!(!serde_json::to_string(&schema).unwrap().contains("$ref"));
    }
    use crate::downstream::{CancelRegistry, DownstreamServer, Transport};

    /// A fake downstream server: advertises `echo` + `add`, echoes calls back.
    struct MockTransport {
        label: String,
    }

    impl Transport for MockTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "resources": {}, "prompts": {}, "completions": {} }
                })),
                "tools/list" => Ok(json!({
                    "tools": [
                        { "name": "echo", "description": "echo back" },
                        { "name": "add", "description": "add numbers" }
                    ]
                })),
                "tools/call" => {
                    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    Ok(json!({
                        "content": [{ "type": "text", "text": format!("{}:{}", self.label, name) }],
                        "isError": false
                    }))
                }
                "resources/list" => Ok(json!({
                    "resources": [
                        { "uri": format!("{}://readme", self.label), "name": "readme" }
                    ]
                })),
                "resources/templates/list" => Ok(json!({
                    "resourceTemplates": [
                        {
                            "uriTemplate": format!("{}://item/{{id}}", self.label),
                            "name": "item",
                            "description": "An item by id"
                        }
                    ]
                })),
                "resources/read" => {
                    let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    Ok(json!({ "contents": [{ "uri": uri, "text": format!("{}-body", self.label) }] }))
                }
                "resources/subscribe" | "resources/unsubscribe" => {
                    let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    Ok(json!({ "uri": uri, "via": self.label }))
                }
                "prompts/list" => Ok(json!({
                    "prompts": [{ "name": "greet", "description": "greeting" }]
                })),
                "prompts/get" => {
                    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    Ok(json!({ "messages": [{ "role": "user", "content": format!("{}:{}", self.label, name) }] }))
                }
                "completion/complete" => {
                    let ref_type = params
                        .pointer("/ref/type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let arg = params
                        .pointer("/argument/value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let label = match ref_type {
                        "ref/prompt" => {
                            let name = params
                                .pointer("/ref/name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            format!("{}:prompt:{name}:{arg}", self.label)
                        }
                        "ref/resource" => {
                            let uri = params
                                .pointer("/ref/uri")
                                .and_then(|u| u.as_str())
                                .unwrap_or("");
                            format!("{}:resource:{uri}:{arg}", self.label)
                        }
                        other => format!("{}:unknown:{other}", self.label),
                    };
                    Ok(json!({
                        "completion": {
                            "values": [label],
                            "total": 1,
                            "hasMore": false
                        }
                    }))
                }
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn mock_server(id: &str) -> DownstreamServer {
        let mut ds = DownstreamServer::connect(
            id.to_string(),
            Box::new(MockTransport {
                label: id.to_string(),
            }),
        )
        .unwrap();
        // Mirror the gateway: load resources/prompts after connect.
        ds.load_resources_prompts();
        ds
    }

    struct ExtensionTransport {
        extensions: Value,
    }

    impl Transport for ExtensionTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Err(TransportError::Rpc(json!({
                    "code": -32601,
                    "message": "method not found"
                }))),
                "server/discover" => Ok(json!({
                    "supportedVersions": [crate::downstream::MODERN_PROTOCOL_VERSION],
                    "capabilities": { "extensions": self.extensions.clone() }
                })),
                "tools/list" => Ok(json!({ "tools": [] })),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }

        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn extension_server(id: &str, extensions: Value) -> DownstreamServer {
        DownstreamServer::connect(
            id.to_string(),
            Box::new(ExtensionTransport { extensions }),
        )
        .unwrap()
    }

    struct AppTransport {
        resource_uri: &'static str,
        legacy_meta: bool,
        protocol_meta: Option<Value>,
    }

    impl Transport for AppTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Err(TransportError::Rpc(json!({
                    "code": -32601,
                    "message": "method not found"
                }))),
                "server/discover" => Ok(json!({
                    "supportedVersions": [crate::downstream::MODERN_PROTOCOL_VERSION],
                    "capabilities": {
                        "resources": {},
                        "extensions": {
                            "io.modelcontextprotocol/ui": {
                                "mimeTypes": ["text/html;profile=mcp-app"]
                            }
                        }
                    }
                })),
                "tools/list" => {
                    let ui_negotiated = self
                        .protocol_meta
                        .as_ref()
                        .and_then(|meta| {
                            meta.pointer("/io.modelcontextprotocol~1clientCapabilities/extensions/io.modelcontextprotocol~1ui/mimeTypes")
                        })
                        .and_then(Value::as_array)
                        .is_some_and(|mime_types| {
                            mime_types
                                .iter()
                                .any(|mime| mime == "text/html;profile=mcp-app")
                        });
                    if !ui_negotiated {
                        return Ok(json!({ "tools": [] }));
                    }
                    let meta = if self.legacy_meta {
                        json!({ "ui/resourceUri": self.resource_uri })
                    } else {
                        json!({ "ui": { "resourceUri": self.resource_uri } })
                    };
                    Ok(json!({
                        "tools": [{
                            "name": "dashboard",
                            "inputSchema": { "type": "object" },
                            "_meta": meta
                        }]
                    }))
                }
                "resources/list" => Ok(json!({ "resources": [] })),
                "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
                "resources/read" => {
                    assert_eq!(params["uri"], self.resource_uri);
                    assert!(
                        self.protocol_meta
                            .as_ref()
                            .and_then(|meta| meta.pointer(
                                "/io.modelcontextprotocol~1clientCapabilities/extensions/io.modelcontextprotocol~1ui"
                            ))
                            .is_none(),
                        "catalog-only Apps capability leaked into resources/read"
                    );
                    Ok(json!({
                        "contents": [{
                            "uri": self.resource_uri,
                            "mimeType": "text/html;profile=mcp-app",
                            "text": "<!doctype html><title>Toolport App</title>"
                        }]
                    }))
                }
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }

        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }

        fn set_protocol_meta(&mut self, meta: Option<Value>) {
            self.protocol_meta = meta;
        }
    }

    fn app_server(id: &str, uri: &'static str, legacy_meta: bool) -> DownstreamServer {
        DownstreamServer::connect(
            id.to_string(),
            Box::new(AppTransport {
                resource_uri: uri,
                legacy_meta,
                protocol_meta: None,
            }),
        )
        .unwrap()
    }

    #[test]
    fn extension_aggregation_is_scoped_and_omits_conflicting_settings() {
        let mut router = Router::new();
        router.add(extension_server(
            "alpha",
            json!({
                "io.modelcontextprotocol/tasks": {},
                "com.example/passive": {},
                "com.example/mode": { "version": 1 }
            }),
        ));
        router.add(extension_server(
            "beta",
            json!({
                "io.modelcontextprotocol/tasks": {},
                "com.example/passive": {},
                "com.example/mode": { "version": 2 },
                "com.example/beta": { "enabled": true }
            }),
        ));

        let all = router.aggregated_extensions(|_| true);
        assert_eq!(all["com.example/passive"], json!({}));
        assert_eq!(all["io.modelcontextprotocol/tasks"], json!({}));
        assert_eq!(all["com.example/beta"]["enabled"], true);
        assert!(all.get("com.example/mode").is_none());

        let alpha = router.aggregated_extensions(|server_id| server_id == "alpha");
        assert_eq!(alpha["com.example/mode"]["version"], 1);
        assert!(alpha.get("com.example/beta").is_none());
    }

    #[test]
    fn mcp_app_tool_metadata_routes_unlisted_ui_resources() {
        for (legacy_meta, uri) in [
            (false, "ui://modern/dashboard"),
            (true, "ui://legacy/dashboard"),
        ] {
            let mut router = Router::new();
            router.add(app_server("apps", uri, legacy_meta));
            router.refresh_tools();

            assert_eq!(
                router.aggregated_resources(),
                Vec::<Value>::new(),
                "the fixture intentionally omits its UI resource from resources/list"
            );
            assert_eq!(router.resource_server(uri), Some("apps"));
            let result = router
                .read_resource(uri)
                .expect("UI resource routes through tool metadata");
            assert_eq!(result["contents"][0]["uri"], uri);
        }
    }

    #[test]
    fn listed_mcp_app_resource_stays_visible_after_tool_route_hint() {
        let uri = "ui://listed/dashboard";
        let mut server = app_server("apps", uri, false);
        server.resources.push(json!({
            "uri": uri,
            "name": "Dashboard",
            "mimeType": "text/html;profile=mcp-app"
        }));
        let mut router = Router::new();
        router.add(server);

        assert_eq!(router.aggregated_resources().len(), 1);
        assert_eq!(router.aggregated_resources()[0]["uri"], uri);
        assert_eq!(router.resource_server(uri), Some("apps"));
    }

    #[test]
    fn ui_route_hints_require_the_reserved_html_mime_capability() {
        let uri = "ui://unsupported/dashboard";
        let mut server = extension_server(
            "unsupported",
            json!({
                "io.modelcontextprotocol/ui": {
                    "mimeTypes": ["image/svg+xml"]
                }
            }),
        );
        server.tools.push(json!({
            "name": "dashboard",
            "inputSchema": { "type": "object" },
            "_meta": { "ui": { "resourceUri": uri } }
        }));
        let mut router = Router::new();
        router.add(server);

        assert_eq!(router.resource_server(uri), None);
    }

    struct TaskTransport {
        seen: Arc<Mutex<Vec<(String, Value)>>>,
        advertise_tasks: bool,
    }

    impl Transport for TaskTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Err(TransportError::Rpc(json!({
                    "code": -32601,
                    "message": "method not found"
                }))),
                "server/discover" => Ok(json!({
                    "supportedVersions": [crate::downstream::MODERN_PROTOCOL_VERSION],
                    "capabilities": {
                        "extensions": if self.advertise_tasks {
                            json!({ "io.modelcontextprotocol/tasks": {} })
                        } else {
                            json!({})
                        }
                    }
                })),
                "tools/list" => Ok(json!({ "tools": [{ "name": "job" }] })),
                "tools/call" => {
                    self.seen.lock().unwrap().push((method.to_string(), params));
                    Ok(json!({
                        "resultType": "task",
                        "taskId": "same-native-id",
                        "status": "working",
                        "createdAt": "2026-08-01T00:00:00Z",
                        "lastUpdatedAt": "2026-08-01T00:00:00Z",
                        "ttlMs": null,
                        "pollIntervalMs": 100
                    }))
                }
                "tasks/get" => {
                    self.seen.lock().unwrap().push((method.to_string(), params.clone()));
                    Ok(json!({
                        "resultType": "complete",
                        "taskId": params["taskId"],
                        "status": "completed",
                        "createdAt": "2026-08-01T00:00:00Z",
                        "lastUpdatedAt": "2026-08-01T00:00:01Z",
                        "ttlMs": null,
                        "result": { "content": [{ "type": "text", "text": "done" }] }
                    }))
                }
                "tasks/update" | "tasks/cancel" => {
                    self.seen.lock().unwrap().push((method.to_string(), params.clone()));
                    Ok(json!({ "resultType": "complete", "taskId": params["taskId"] }))
                }
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }

        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn task_server(id: &str, seen: Arc<Mutex<Vec<(String, Value)>>>) -> DownstreamServer {
        DownstreamServer::connect(
            id.to_string(),
            Box::new(TaskTransport {
                seen,
                advertise_tasks: true,
            }),
        )
        .unwrap()
    }

    #[test]
    fn task_handles_bind_owner_and_route_poll_update_and_cancel() {
        let alpha_seen = Arc::new(Mutex::new(Vec::new()));
        let beta_seen = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router::new();
        router.add(task_server("alpha", Arc::clone(&alpha_seen)));
        router.add(task_server("beta", Arc::clone(&beta_seen)));
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": crate::downstream::MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": { "io.modelcontextprotocol/tasks": {} }
            }
        });

        let alpha = router
            .route_call_with_cancel("alpha__job", json!({}), None, Some(&meta))
            .unwrap();
        let beta = router
            .route_call_with_cancel("beta__job", json!({}), None, Some(&meta))
            .unwrap();
        let alpha_id = alpha["taskId"].as_str().unwrap();
        let beta_id = beta["taskId"].as_str().unwrap();
        assert_ne!(alpha_id, beta_id, "same native id on two servers must not collide");
        assert_eq!(router.task_server(alpha_id).as_deref(), Some("alpha"));
        assert_eq!(router.task_server(beta_id).as_deref(), Some("beta"));
        assert_eq!(
            Router::new().task_server(alpha_id).as_deref(),
            Some("alpha"),
            "task ownership must survive a router rebuild"
        );
        let mut tampered = alpha_id.to_string();
        let changed = TASK_HANDLE_PREFIX.len() + 5;
        let replacement = if &tampered[changed..=changed] == "A" { "B" } else { "A" };
        tampered.replace_range(changed..=changed, replacement);
        assert!(
            router.task_server(&tampered).is_none(),
            "an edited task handle must fail authentication"
        );

        let polled = router
            .route_task(
                "tasks/get",
                json!({ "taskId": alpha_id }),
                None,
                Some(&meta),
            )
            .unwrap();
        assert_eq!(polled["taskId"], alpha_id);
        assert_eq!(polled["status"], "completed");
        let updated = router
            .route_task(
                "tasks/update",
                json!({
                    "taskId": alpha_id,
                    "inputResponses": { "answer": { "content": "yes" } }
                }),
                None,
                Some(&meta),
            )
            .unwrap();
        assert_eq!(updated["taskId"], alpha_id);
        let cancelled = router
            .route_task(
                "tasks/cancel",
                json!({ "taskId": alpha_id }),
                None,
                Some(&meta),
            )
            .unwrap();
        assert_eq!(cancelled["taskId"], alpha_id);

        let seen = alpha_seen.lock().unwrap();
        for (method, params) in seen.iter().filter(|(method, _)| method.starts_with("tasks/")) {
            assert_eq!(params["taskId"], "same-native-id", "{method} must use native id");
            assert_eq!(
                params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
                    ["io.modelcontextprotocol/tasks"],
                json!({})
            );
        }
        assert!(router.route_task("tasks/get", json!({ "taskId": "forged" }), None, Some(&meta)).is_err());
        assert!(router
            .route_task("tasks/get", json!({ "taskId": alpha_id }), None, None)
            .unwrap_err()
            .contains("client capability"));
    }

    #[test]
    fn task_results_require_both_sides_to_advertise_the_extension() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router::new();
        router.add(task_server("tasks", Arc::clone(&seen)));

        let missing_client_capability = router
            .route_call_with_cancel("tasks__job", json!({}), None, None)
            .unwrap_err();
        assert!(missing_client_capability.contains("required client capability"));

        let mut unadvertised = Router::new();
        unadvertised.add(
            DownstreamServer::connect(
                "plain".to_string(),
                Box::new(TaskTransport {
                    seen,
                    advertise_tasks: false,
                }),
            )
            .unwrap(),
        );
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": crate::downstream::MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": { "io.modelcontextprotocol/tasks": {} }
            }
        });
        let missing_server_capability = unadvertised
            .route_call_with_cancel("plain__job", json!({}), None, Some(&meta))
            .unwrap_err();
        assert!(missing_server_capability.contains("without advertising"));
    }

    struct HintTransport {
        tool: String,
        ttl_ms: u64,
        scope: &'static str,
    }

    impl Transport for HintTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18", "capabilities": {} })),
                "tools/list" => Ok(json!({
                    "tools": [{ "name": self.tool }],
                    "ttlMs": self.ttl_ms,
                    "cacheScope": self.scope
                })),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }

        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn hinted_server(id: &str, ttl_ms: u64, scope: &'static str) -> DownstreamServer {
        DownstreamServer::connect(
            id.to_string(),
            Box::new(HintTransport {
                tool: "tool".to_string(),
                ttl_ms,
                scope,
            }),
        )
        .unwrap()
    }

    /// Handshakes fine (so it can be constructed) but every `tools/call` reports the
    /// connection is dead - i.e. a crashed/hung stdio child mid-session.
    struct DeadOnCallTransport;
    impl Transport for DeadOnCallTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18", "capabilities": {} })),
                "tools/list" => Ok(json!({ "tools": [{ "name": "echo" }] })),
                "tools/call" => Err(TransportError::Unavailable("broken pipe".into())),
                _ => Ok(json!({})),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn dead_slot(reconnect: Option<Reconnect>) -> Arc<ServerSlot> {
        Arc::new(ServerSlot {
            id: "s".into(),
            inner: Mutex::new(
                DownstreamServer::connect("s".into(), Box::new(DeadOnCallTransport)).unwrap(),
            ),
            breaker: Mutex::new(Breaker::default()),
            reconnect,
        })
    }

    #[test]
    fn reconnect_and_retry_recovers_a_dead_server() {
        let router = Router::new();
        // Factory hands back a healthy connection, mirroring a re-spawn that succeeds.
        let slot = dead_slot(Some(Box::new(|| Some(mock_server("s")))));
        let out = router.reconnect_and_retry(&slot, None, &mut |ds| ds.call("echo", json!({})));
        // The probe re-spawned the server and the retried call went through.
        let value = out.expect("reconnect attempted").expect("call recovered");
        assert!(serde_json::to_string(&value).unwrap().contains("s:echo"));
        // The live connection was swapped in, so subsequent calls hit the healthy one.
        assert!(slot.inner.lock().unwrap().call("echo", json!({})).is_ok());
        // A successful recovery closes the breaker.
        assert!(slot.breaker.lock().unwrap().open_remaining(Instant::now()).is_none());
    }

    #[test]
    fn reconnect_and_retry_gives_up_when_respawn_fails() {
        let router = Router::new();
        // Factory still can't reach the server (returns None): no recovery, and the
        // caller must fall through to record the failure.
        let slot = dead_slot(Some(Box::new(|| None)));
        let out: Option<Result<Value, String>> =
            router.reconnect_and_retry(&slot, None, &mut |ds| ds.call("echo", json!({})));
        assert!(out.is_none(), "a failed re-spawn falls through to the breaker");
    }

    #[test]
    fn cancellation_during_reconnect_prevents_the_retried_call() {
        let router = Router::new();
        let cancellations = CancelRegistry::new();
        assert!(cancellations.begin_client_request("reconnect-cancel".to_string()));
        let cancel = cancellations.context("reconnect-cancel".to_string());
        let cancel_from_factory = cancellations.clone();
        let slot = dead_slot(Some(Box::new(move || {
            assert!(cancel_from_factory.cancel("reconnect-cancel", Some("user pressed stop")));
            Some(mock_server("s"))
        })));
        let retried_calls = Arc::new(AtomicU32::new(0));
        let calls = Arc::clone(&retried_calls);

        let result = router
            .reconnect_and_retry(&slot, Some(&cancel), &mut move |ds| {
                calls.fetch_add(1, Ordering::SeqCst);
                ds.call("echo", json!({}))
            })
            .expect("reconnect was attempted")
            .unwrap_err();

        assert!(result.contains("cancelled"), "unexpected error: {result}");
        assert_eq!(
            retried_calls.load(Ordering::SeqCst),
            0,
            "a reconnect that races cancellation must not emit the retry"
        );
        cancellations.finish_client_request("reconnect-cancel");
    }

    #[test]
    fn cancellation_from_reconnected_attempt_does_not_penalize_breaker() {
        let router = Router::new();
        let cancellations = CancelRegistry::new();
        assert!(cancellations.begin_client_request("retry-cancel".to_string()));
        let cancel = cancellations.context("retry-cancel".to_string());
        let cancel_from_retry = cancellations.clone();
        let slot = dead_slot(Some(Box::new(|| Some(mock_server("s")))));

        let result: Result<Value, String> = router
            .reconnect_and_retry(&slot, Some(&cancel), &mut move |_server| {
                assert!(cancel_from_retry.cancel("retry-cancel", Some("user pressed stop")));
                Err(TransportError::Cancelled(
                    "request cancelled during reconnected call".to_string(),
                ))
            })
            .expect("reconnect was attempted");

        assert!(result.unwrap_err().contains("cancelled"));
        let mut breaker = slot
            .breaker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(breaker.consecutive_failures, 0);
        assert!(breaker.open_remaining(Instant::now()).is_none());
        drop(breaker);
        cancellations.finish_client_request("retry-cancel");
    }

    #[test]
    fn reconnect_and_retry_noops_without_a_factory() {
        let router = Router::new();
        // A slot with no reconnect factory (e.g. a test fixture) behaves as before:
        // reconnect is skipped and the breaker path handles the failure.
        let slot = dead_slot(None);
        let out: Option<Result<Value, String>> =
            router.reconnect_and_retry(&slot, None, &mut |ds| ds.call("echo", json!({})));
        assert!(out.is_none());
    }

    #[test]
    fn sanitizes_hyphens_in_both_halves() {
        // Server ids and tool names with hyphens are rewritten to `_` so clients
        // like Cursor don't drop them.
        assert_eq!(sanitize_segment("file-system"), "file_system");
        assert_eq!(sanitize_segment("list-offerings"), "list_offerings");
        assert_eq!(sanitize_segment("already_ok"), "already_ok");
    }

    #[test]
    fn resource_and_prompt_server_resolve_owner() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));
        // Resources keep their server-scoped uris; the map resolves the owner.
        assert_eq!(router.resource_server("github://readme"), Some("github"));
        assert_eq!(router.resource_server("postgres://readme"), Some("postgres"));
        assert_eq!(router.resource_server("unknown://x"), None);
        // Prompts resolve by their exposed (namespaced) name.
        let prompts = router.aggregated_prompts();
        let gh_prompt = prompts
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
            .find(|n| router.prompt_server(n) == Some("github"))
            .expect("a github prompt is exposed")
            .to_string();
        assert_eq!(router.prompt_server(&gh_prompt), Some("github"));
        assert_eq!(router.prompt_server("no__such_prompt"), None);
    }

    #[test]
    fn aggregates_and_namespaces_tools() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        let tools = router.aggregated_tools();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                "github__add",
                "github__echo",
                "postgres__add",
                "postgres__echo"
            ]
        );
    }

    #[test]
    fn tool_order_is_stable_across_server_add_order() {
        let mut first = Router::new();
        first.add(mock_server("zeta"));
        first.add(mock_server("alpha"));
        let mut second = Router::new();
        second.add(mock_server("alpha"));
        second.add(mock_server("zeta"));

        assert_eq!(first.aggregated_tools(), second.aggregated_tools());
    }

    #[test]
    fn aggregated_cache_hint_uses_minimum_ttl_and_private_wins() {
        let mut public = Router::new();
        public.add(hinted_server("slow", 60_000, "public"));
        public.add(hinted_server("fast", 30_000, "public"));
        let hint = public.tools_cache_hint().unwrap();
        assert!(hint.is_public());
        let ttl = hint.remaining_ttl_ms();
        assert!(ttl > 0 && ttl <= 30_000, "minimum contributor TTL should win: {ttl}");

        let mut mixed = Router::new();
        mixed.add(hinted_server("public", 60_000, "public"));
        mixed.add(hinted_server("private", 60_000, "private"));
        assert!(!mixed.tools_cache_hint().unwrap().is_public());
    }

    #[test]
    fn positive_cache_ttl_marks_its_catalog_for_refresh() {
        let mut router = Router::new();
        router.add(hinted_server("expiring", 5, "public"));
        std::thread::sleep(std::time::Duration::from_millis(15));

        assert_ne!(
            router.expired_cache_kinds() & crate::downstream::change::TOOLS,
            0
        );
    }

    #[test]
    fn routes_call_to_the_right_server() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        let result = router.route_call("postgres__add", json!({ "a": 1 })).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "postgres:add");
    }

    #[test]
    fn tool_overrides_rename_and_redescribe() {
        let mut router = Router::new();
        // Keyed by (server id, ORIGINAL tool name), not the exposed name.
        let mut srv = HashMap::new();
        srv.insert(
            "echo".to_string(),
            ToolOverride { name: Some("say".into()), description: Some("say it back".into()) },
        );
        srv.insert(
            "add".to_string(),
            ToolOverride { name: None, description: Some("cleaned".into()) },
        );
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        let tools = router.aggregated_tools();
        let by_name: HashMap<&str, &Value> =
            tools.iter().map(|t| (t["name"].as_str().unwrap(), t)).collect();

        // echo is renamed to "say" (its original exposed name is gone) and re-described.
        assert!(by_name.contains_key("say"));
        assert!(!by_name.contains_key("srv__echo"));
        assert_eq!(by_name["say"]["description"], "say it back");
        // add keeps its name, description replaced (the poisoned-desc neutralize case).
        assert_eq!(by_name["srv__add"]["description"], "cleaned");

        // The renamed tool STILL routes to the original downstream tool (echo).
        let out = router.route_call("say", json!({})).unwrap();
        assert_eq!(out["content"][0]["text"], "srv:echo");
        let out = router.route_call("srv__add", json!({})).unwrap();
        assert_eq!(out["content"][0]["text"], "srv:add");
    }

    #[test]
    fn quarantine_follows_a_renamed_tool_by_its_exposed_name() {
        // #423: quarantine is keyed by the client-facing (exposed) name. A tool renamed
        // via an override must be quarantined under its RENAMED name, and blocking must
        // key on that same name. The old code evaluated the policy on the pre-rename base
        // name, so a renamed tool could never be quarantined: the app showed it blocked
        // while the gateway kept exposing and routing it.
        let mut srv = HashMap::new();
        srv.insert(
            "echo".to_string(),
            ToolOverride { name: Some("say".into()), description: None },
        );
        let policy = ToolPolicy {
            quarantined: BTreeSet::from(["say".to_string()]),
            ..Default::default()
        };
        let mut router = Router::with_policy(policy);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        // The renamed tool is hidden from the catalog...
        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert!(!names.contains(&"say".to_string()), "quarantined rename must be hidden");
        // ...and blocked on a direct call, with the quarantine reason.
        let err = router.route_call("say", json!({})).unwrap_err();
        assert!(err.contains("quarantine"), "unexpected: {err}");
    }

    #[test]
    fn a_stale_pre_rename_quarantine_entry_does_not_block_the_renamed_tool() {
        // The mirror of the above: quarantining the OLD exposed name (srv__echo) must NOT
        // block the tool now exposed as "say", so the fix doesn't just swap which name is
        // wrong. A stale entry from before a rename is inert, not a silent block.
        let mut srv = HashMap::new();
        srv.insert(
            "echo".to_string(),
            ToolOverride { name: Some("say".into()), description: None },
        );
        let policy = ToolPolicy {
            quarantined: BTreeSet::from(["srv__echo".to_string()]),
            ..Default::default()
        };
        let mut router = Router::with_policy(policy);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        assert_eq!(router.route_of("say"), Some(("srv", "echo")));
        assert!(router.route_call("say", json!({})).is_ok(), "stale entry must not block");
    }

    #[test]
    fn renamed_tool_quarantines_and_releases_end_to_end() {
        // #423 end-to-end through REAL integrity persistence and a REAL router, beyond the
        // unit tests that hand-set the quarantine set: a tool renamed to an exposed name
        // with no `__` drifts, integrity quarantines that renamed name to disk, the router
        // reads it back and blocks the call, then a re-approve restores it. This is the
        // whole chain the app relies on.
        use crate::integrity;
        // Isolate persistence: hold the shared lock EVERY conduit_dir-resolving test takes
        // (#409's invariant, since this touches integrity's on-disk store indirectly), and
        // redirect the data dir to a scratch path so nothing hits the real one (#400).
        let _lock = crate::registry::data_dir_test_lock();
        let scratch =
            std::env::temp_dir().join(format!("toolport-rename-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let _data_dir = crate::registry::DataDirOverride::set(&scratch);
        let profile = Some("rename-e2e");

        // Rename srv/echo -> "search" (an exposed name with NO `server__` prefix).
        let mut overrides = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            ToolOverride { name: Some("search".into()), description: None },
        );
        let mut router = Router::new();
        router.set_overrides(HashMap::from([("srv".to_string(), overrides)]));
        router.add(mock_server("srv"));

        // Sanity: exposed under the renamed name, routes to the real tool, callable.
        assert_eq!(router.route_of("search"), Some(("srv", "echo")));
        assert!(router.route_call("search", json!({})).is_ok());

        // The server ships a poisoned redefinition. integrity quarantines the RENAMED
        // exposed name - only reachable because #423 stopped skipping non-`__` tools.
        let current = router.aggregated_tools();
        let events = vec![json!({
            "server": "srv", "tool": "search", "change": "poison", "severity": "high"
        })];
        assert!(
            integrity::apply_quarantine(profile, &current, &events).unwrap(),
            "a poison drift on a renamed tool must quarantine it"
        );

        // The persisted set the watcher reads carries the renamed name...
        let persisted = integrity::quarantined(profile).expect("quarantine store readable");
        assert!(persisted.contains("search"), "quarantine is keyed by the exposed name");

        // ...and feeding it to the router hides and blocks the renamed tool.
        router.requarantine(persisted);
        let visible: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert!(!visible.contains(&"search".to_string()), "quarantined rename must be hidden");
        let err = router.route_call("search", json!({})).unwrap_err();
        assert!(err.contains("quarantine"), "a call to a quarantined rename must block: {err}");

        // Re-approve: release clears the persisted set and the router restores the tool.
        assert!(
            integrity::release(profile, "search").unwrap(),
            "release must clear the entry"
        );
        let after = integrity::quarantined(profile).expect("quarantine store readable");
        assert!(!after.contains("search"), "a released tool leaves the persisted set");
        router.requarantine(after);
        assert!(
            router.route_call("search", json!({})).is_ok(),
            "a re-approved renamed tool must work again"
        );

        // Clear the override before removing the scratch dir it points at; the lock is
        // released at end of scope.
        drop(_data_dir);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn rename_to_an_already_taken_name_is_ignored() {
        // add is indexed after echo, so renaming add -> "srv__echo" (already taken) must
        // fall back to add's original name, keeping routing unambiguous.
        let mut router = Router::new();
        let srv = HashMap::from([(
            "add".to_string(),
            ToolOverride { name: Some("srv__echo".into()), description: None },
        )]);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        let tools = router.aggregated_tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"srv__echo"), "the real echo keeps the name");
        assert!(names.contains(&"srv__add"), "add fell back to its own name");
        assert_eq!(router.route_call("srv__echo", json!({})).unwrap()["content"][0]["text"], "srv:echo");
        assert_eq!(router.route_call("srv__add", json!({})).unwrap()["content"][0]["text"], "srv:add");
    }

    #[test]
    fn route_of_resolves_renamed_tool_to_real_server_and_original_tool() {
        // The gate derives provenance/scoping from route_of, NOT by splitting the exposed
        // name. A renamed tool must still resolve to its real (server, original tool) so the
        // untrusted-source HITL check and per-client scoping aren't silently bypassed.
        let mut router = Router::new();
        let srv = HashMap::from([(
            "echo".to_string(),
            ToolOverride { name: Some("say".into()), description: None },
        )]);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        assert_eq!(router.route_of("say"), Some(("srv", "echo")), "renamed tool resolves to origin");
        assert_eq!(router.route_of("srv__add"), Some(("srv", "add")), "normal tool resolves");
        assert_eq!(router.route_of("nope"), None, "unknown name resolves to nothing");
    }

    /// A transport that advertises `n` tools (`t0`..`t(n-1)`) and echoes calls
    /// back as `server:tool`, like MockTransport but with a configurable catalog.
    struct CatalogTransport {
        label: String,
        n: usize,
    }

    impl Transport for CatalogTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "resources": {}, "prompts": {}, "completions": {} }
                })),
                "tools/list" => Ok(json!({
                    "tools": (0..self.n)
                        .map(|i| json!({ "name": format!("t{i}"), "description": "" }))
                        .collect::<Vec<_>>()
                })),
                "tools/call" => {
                    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    Ok(json!({
                        "content": [{ "type": "text", "text": format!("{}:{}", self.label, name) }],
                        "isError": false
                    }))
                }
                "resources/list" => Ok(json!({ "resources": [] })),
                "prompts/list" => Ok(json!({ "prompts": [] })),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }

        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn catalog_server(id: &str, n: usize) -> DownstreamServer {
        let mut ds = DownstreamServer::connect(
            id.to_string(),
            Box::new(CatalogTransport {
                label: id.to_string(),
                n,
            }),
        )
        .unwrap();
        ds.load_resources_prompts();
        ds
    }

    fn router_with_catalogs(specs: &[(&str, usize)]) -> Router {
        let mut router = Router::new();
        for (id, n) in specs {
            router.add(catalog_server(id, *n));
        }
        router
    }

    #[test]
    fn adopt_restored_routes_reroutes_a_guarded_rebuild() {
        // A rebuild guard keeps the previous catalog for a server whose fresh
        // connect implausibly shrank (40 -> 3). The rebuilt router was indexed
        // from the degraded connect, so route_of misses the 37 restored tools
        // while the cache still advertises them (issue #700).
        let previous = router_with_catalogs(&[("atlassian", 40), ("github", 5)]);
        let mut rebuilt = router_with_catalogs(&[("atlassian", 3), ("github", 5)]);

        // Simulate the gateway guard: the guarded catalog keeps atlassian's
        // previous 40 tools and github's fresh 5.
        let guarded = previous.aggregated_tools();
        rebuilt.adopt_restored_routes(&previous, &guarded);

        // Every advertised tool now routes, to the ORIGINAL downstream name.
        assert_eq!(rebuilt.route_of("atlassian__t39"), Some(("atlassian", "t39")));
        assert_eq!(rebuilt.route_of("atlassian__t37"), Some(("atlassian", "t37")));
        assert_eq!(
            rebuilt.route_of("github__t4"),
            Some(("github", "t4")),
            "healthy server's own routes are untouched"
        );
        // A call to one of the 37 restored tools reaches the downstream server.
        let result = rebuilt
            .route_call("atlassian__t39", json!({}))
            .unwrap();
        assert_eq!(
            result["content"][0]["text"].as_str().unwrap(),
            "atlassian:t39",
            "the call reaches the downstream under its original name"
        );
        // aggregated_tools() now matches what the cache advertises.
        let names: std::collections::HashSet<String> = rebuilt
            .aggregated_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
            .collect();
        assert!(names.contains("atlassian__t39"));
        assert_eq!(names.len(), 45, "40 restored + 5 healthy");
    }

    #[test]
    fn adopt_restored_routes_never_adopts_a_newly_quarantined_tool() {
        // A tool quarantined since the previous build is absent from the degraded
        // connect, so the rebuilt router's `blocked` map has no entry for it --
        // but the guarded catalog (kept from the previous/cached catalog) still
        // carries it. Adoption must re-check policy instead of blindly restoring
        // the route, or quarantine would be silently bypassed (review on #717).
        let previous = router_with_catalogs(&[("atlassian", 40)]);
        let mut rebuilt = router_with_catalogs(&[("atlassian", 3)]);
        // Quarantine a tool the degraded connect never returned, mirroring the
        // live flow: requarantine re-indexes from the current (degraded) connect.
        rebuilt.requarantine(BTreeSet::from(["atlassian__t30".to_string()]));
        assert!(rebuilt.block_reason("atlassian__t30").is_none());

        let guarded = previous.aggregated_tools();
        rebuilt.adopt_restored_routes(&previous, &guarded);

        // The quarantined tool must not be routed or advertised again.
        assert!(rebuilt.block_reason("atlassian__t30").is_some());
        assert!(rebuilt.route_of("atlassian__t30").is_none());
        let names: std::collections::HashSet<String> = rebuilt
            .aggregated_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
            .collect();
        assert!(!names.contains("atlassian__t30"));
        // The other 39 restored tools are still adopted (3 degraded + 36 more).
        assert!(names.contains("atlassian__t39"));
        assert_eq!(names.len(), 39, "3 degraded + 36 restored, t30 quarantined");
    }

    #[test]
    fn adopt_restored_routes_never_touches_an_already_routed_tool() {
        // A tool the rebuilt router already routes (one of the 3 the degraded
        // connect returned) must keep its live mapping, not be overwritten.
        // The previous router carries an override renaming one of those same
        // tools (t1 -> renamed-t1), so its catalog disagrees with the rebuilt
        // router's live routes; adoption must leave every live name alone and
        // adopt the renamed slot under its renamed exposure.
        let mut previous = Router::new();
        previous.set_overrides(HashMap::from([(
            "atlassian".to_string(),
            HashMap::from([(
                "t1".to_string(),
                ToolOverride {
                    name: Some("renamed-t1".into()),
                    description: None,
                },
            )]),
        )]));
        previous.add(catalog_server("atlassian", 40));
        let mut rebuilt = router_with_catalogs(&[("atlassian", 3)]);
        let guarded = previous.aggregated_tools();
        rebuilt.adopt_restored_routes(&previous, &guarded);

        // Live routes for tools the degraded connect still advertises are kept,
        // even though the previous catalog disagrees about one of them.
        assert_eq!(rebuilt.route_of("atlassian__t0"), Some(("atlassian", "t0")));
        assert_eq!(rebuilt.route_of("atlassian__t1"), Some(("atlassian", "t1")));
        assert_eq!(rebuilt.route_of("atlassian__t2"), Some(("atlassian", "t2")));
        // The renamed slot from the previous catalog is adopted under its
        // renamed (sanitized) exposure — never re-derived by splitting on `__`
        // — pointing at the same downstream tool.
        assert_eq!(rebuilt.route_of("renamed_t1"), Some(("atlassian", "t1")));
    }

    #[test]
    fn routes_call_with_a_sanitized_name() {
        // A hyphenated server id is exposed with `_`, but the call still reaches
        // the server under its real id.
        let mut router = Router::new();
        router.add(mock_server("file-system"));

        let tools = router.aggregated_tools();
        let name = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .find(|name| name.ends_with("__echo"))
            .unwrap();
        assert_eq!(name, "file_system__echo");

        let result = router.route_call(name, json!({})).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "file-system:echo");
    }

    #[test]
    fn unknown_namespace_errors() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        assert!(router.route_call("nope__x", json!({})).is_err());
        assert!(router.route_call("notnamespaced", json!({})).is_err());
    }

    /// A server whose single tool is annotated destructive.
    struct DestructiveMock;
    impl Transport for DestructiveMock {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                "tools/list" => Ok(json!({
                    "tools": [
                        { "name": "drop_table",
                          "description": "drops a table",
                          "annotations": { "destructiveHint": true } },
                        { "name": "list_tables", "description": "lists tables" }
                    ]
                })),
                "tools/call" => Ok(json!({
                    "content": [{ "type": "text", "text": "ok" }], "isError": false
                })),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[test]
    fn is_destructive_reads_annotations() {
        assert!(is_destructive(&json!({ "annotations": { "destructiveHint": true } })));
        assert!(is_destructive(&json!({ "destructiveHint": true }))); // top-level fallback
        assert!(!is_destructive(&json!({ "annotations": { "destructiveHint": false } })));
        assert!(!is_destructive(&json!({ "name": "x" })));
    }

    #[test]
    fn is_destructive_falls_back_to_obvious_write_verbs() {
        assert!(is_destructive(&json!({ "name": "delete_file" })));
        assert!(is_destructive(&json!({ "name": "sendEmail" })));
        assert!(is_destructive(&json!({ "name": "run_query" })));
        assert!(is_destructive(&json!({ "name": "rename_branch" })));
        assert!(is_destructive(&json!({ "name": "uploadObject" })));
        assert!(is_destructive(&json!({ "name": "patch_record" })));
        assert!(!is_destructive(&json!({ "name": "list_files" })));
        assert!(!is_destructive(&json!({
            "name": "delete_file",
            "annotations": { "destructiveHint": false }
        })));
    }

    #[test]
    fn name_looks_destructive_is_hint_independent() {
        // Drift tiering (SBS-875) uses this even when the hint is an explicit false.
        assert!(name_looks_destructive("delete_file"));
        assert!(name_looks_destructive("srv__run_admin_script"));
        assert!(name_looks_destructive("sendEmail"));
        assert!(!name_looks_destructive("list_files"));
        assert!(
            !name_looks_destructive("rc__edit_paywall_ai"),
            "edit/modify stay omitted so benign description churn stays quiet"
        );
    }

    #[test]
    fn disabled_tool_is_hidden_and_blocked() {
        let mut policy = ToolPolicy::default();
        policy
            .disabled
            .insert("github".to_string(), ["echo".to_string()].into_iter().collect());
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));

        // echo is hidden; add survives.
        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names, vec!["github__add"]);

        // Calling the hidden tool by name gives a clear policy error.
        let err = router.route_call("github__echo", json!({})).unwrap_err();
        assert!(err.contains("disabled"), "unexpected: {err}");
        // The allowed tool still routes.
        assert!(router.route_call("github__add", json!({})).is_ok());
    }

    #[test]
    fn requarantine_restores_a_re_approved_tool_without_a_rebuild() {
        // Regression for SOU-292: re-approving a quarantined tool left it blocked in the
        // running gateway. The refresh path could ADD to the quarantine set but never
        // REMOVE from it, and because `route_call` reads the materialized `blocked` map,
        // a client that already held its catalog stayed broken even though the app showed
        // nothing quarantined. Shrinking the set must restore the tool in place, with no
        // rebuild and no downstream re-query.
        let mut policy = ToolPolicy::default();
        policy.quarantined = ["github__echo".to_string()].into_iter().collect();
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));

        // Quarantined: hidden from the catalog AND blocked on a direct call.
        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names, vec!["github__add"]);
        let err = router.route_call("github__echo", json!({})).unwrap_err();
        assert!(err.contains("quarantined"), "unexpected: {err}");

        // Re-approval: the persisted set no longer holds the tool.
        router.requarantine(BTreeSet::new());

        // It must be routable again immediately, not "on the next rebuild".
        assert!(
            router.route_call("github__echo", json!({})).is_ok(),
            "a re-approved tool must route again without a rebuild"
        );
        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert!(
            names.contains(&"github__echo".to_string()),
            "and be re-exposed"
        );
    }

    #[test]
    fn quarantined_accessor_reflects_the_live_set() {
        // The watcher diffs this against the persisted set to decide whether to re-filter,
        // so it has to track `requarantine` exactly. If it went stale the reconciler would
        // either spin (re-filtering every tick) or never fire at all.
        let mut router = Router::new();
        router.add(mock_server("github"));
        assert!(router.quarantined().is_empty());

        let set: BTreeSet<String> = ["github__echo".to_string()].into_iter().collect();
        router.requarantine(set.clone());
        assert_eq!(router.quarantined(), &set);

        router.requarantine(BTreeSet::new());
        assert!(router.quarantined().is_empty());
    }

    #[test]
    fn block_reason_matches_route_call_blocked_map() {
        let mut policy = ToolPolicy::default();
        policy.quarantined = ["github__echo".to_string()].into_iter().collect();
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));

        assert!(router.block_reason("github__add").is_none());
        assert_eq!(router.block_reason("github__echo").map(|r| r.contains("quarantined")), Some(true));
        let err = router.route_call("github__echo", json!({})).unwrap_err();
        assert!(err.contains("quarantined"), "unexpected: {err}");
    }

    #[test]
    fn aggregates_and_routes_resources() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        // Resources pass through with their original uris.
        let uris: Vec<String> = router
            .aggregated_resources()
            .iter()
            .filter_map(|r| r.get("uri").and_then(|u| u.as_str()).map(String::from))
            .collect();
        assert_eq!(uris, vec!["github://readme", "postgres://readme"]);

        // resources/read reaches the owning server.
        let result = router.read_resource("postgres://readme").unwrap();
        assert_eq!(result["contents"][0]["text"], "postgres-body");
        assert!(router.read_resource("nope://x").is_err());
    }

    #[test]
    fn aggregates_and_routes_resource_templates() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        let templates: Vec<String> = router
            .aggregated_resource_templates()
            .iter()
            .filter_map(|t| {
                t.get("uriTemplate")
                    .and_then(|u| u.as_str())
                    .map(String::from)
            })
            .collect();
        assert_eq!(
            templates,
            vec!["github://item/{id}", "postgres://item/{id}"]
        );
        assert_eq!(
            router.resource_template_server("github://item/{id}"),
            Some("github")
        );
        // Expanded template URI routes to the owning server.
        assert_eq!(
            router.resource_server("postgres://item/42"),
            Some("postgres")
        );
        let result = router.read_resource("postgres://item/42").unwrap();
        assert_eq!(result["contents"][0]["text"], "postgres-body");
        // Subscribe/unsubscribe use the same ownership path as read (SOU-394).
        let sub = router.subscribe_resource("postgres://item/42").unwrap();
        assert_eq!(sub["via"], "postgres");
        let unsub = router.unsubscribe_resource("postgres://item/42").unwrap();
        assert_eq!(unsub["via"], "postgres");
        // Owner-pinned unsub (recorded at subscribe time) hits that server even
        // without re-resolving the URI.
        let unsub_on = router
            .unsubscribe_resource_on_server("postgres", "postgres://item/42")
            .unwrap();
        assert_eq!(unsub_on["via"], "postgres");
        // Unknown server id fails closed.
        assert!(router
            .unsubscribe_resource_on_server("no-such-server", "postgres://item/42")
            .is_err());
    }

    #[test]
    fn resource_uri_collision_keeps_first_writer() {
        // Two servers advertise the same bare URI: first add order owns it
        // (SOU-325). Later claims must not steal reads.
        struct CollisionTransport {
            label: String,
        }
        impl Transport for CollisionTransport {
            fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
                match method {
                    "initialize" => Ok(json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "resources": {} }
                    })),
                    "tools/list" => Ok(json!({ "tools": [] })),
                    "resources/list" => Ok(json!({
                        "resources": [{ "uri": "shared://readme", "name": "readme" }]
                    })),
                    "resources/templates/list" => Ok(json!({
                        "resourceTemplates": [{
                            "uriTemplate": "shared://item/{id}",
                            "name": "item"
                        }]
                    })),
                    "resources/read" => {
                        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                        Ok(json!({
                            "contents": [{ "uri": uri, "text": format!("{}-body", self.label) }]
                        }))
                    }
                    "resources/subscribe" | "resources/unsubscribe" => {
                        Ok(json!({ "via": self.label }))
                    }
                    other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
                }
            }
            fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
                Ok(())
            }
        }
        fn collision_server(id: &str) -> DownstreamServer {
            let mut ds = DownstreamServer::connect(
                id.to_string(),
                Box::new(CollisionTransport {
                    label: id.to_string(),
                }),
            )
            .unwrap();
            ds.load_resources_prompts();
            ds
        }

        let mut router = Router::new();
        router.add(collision_server("alpha"));
        router.add(collision_server("beta"));

        assert_eq!(router.resource_server("shared://readme"), Some("alpha"));
        assert_eq!(
            router.resource_template_server("shared://item/{id}"),
            Some("alpha")
        );
        // Only the first writer's copy is listed.
        assert_eq!(router.aggregated_resources().len(), 1);
        assert_eq!(router.aggregated_resource_templates().len(), 1);
        let result = router.read_resource("shared://readme").unwrap();
        assert_eq!(result["contents"][0]["text"], "alpha-body");
        let expanded = router.read_resource("shared://item/7").unwrap();
        assert_eq!(expanded["contents"][0]["text"], "alpha-body");
    }

    #[test]
    fn completion_forwards_prompt_and_resource_template_refs() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        // Prompt completion: remaps namespaced name back to downstream "greet".
        let prompt = router
            .complete(json!({
                "ref": { "type": "ref/prompt", "name": "github__greet" },
                "argument": { "name": "topic", "value": "py" }
            }))
            .unwrap();
        assert_eq!(
            prompt["completion"]["values"][0],
            "github:prompt:greet:py"
        );

        // Resource-template completion: routes by uriTemplate ownership.
        let resource = router
            .complete(json!({
                "ref": { "type": "ref/resource", "uri": "postgres://item/{id}" },
                "argument": { "name": "id", "value": "4" }
            }))
            .unwrap();
        assert_eq!(
            resource["completion"]["values"][0],
            "postgres:resource:postgres://item/{id}:4"
        );
    }

    #[test]
    fn uri_template_matching_handles_level1_placeholders() {
        assert!(uri_matches_template(
            "fixture://item/06",
            "fixture://item/{id}"
        ));
        assert!(!uri_matches_template(
            "fixture://item/06/extra",
            "fixture://item/{id}"
        ));
        assert!(uri_matches_template(
            "file:///a/b/c.txt",
            "file:///{+path}"
        ));
        assert!(!uri_matches_template("other://x", "fixture://item/{id}"));
    }

    #[test]
    fn route_call_passes_cancel_context_to_transport() {
        struct CancelAware {
            saw_cancel: Arc<AtomicBool>,
        }

        impl Transport for CancelAware {
            fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
                match method {
                    "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                    "tools/list" => Ok(json!({ "tools": [{ "name": "echo", "description": "" }] })),
                    other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
                }
            }

            fn request_with_cancel(
                &mut self,
                method: &str,
                params: Value,
                cancel: Option<CancelContext>,
            ) -> Result<Value, TransportError> {
                match method {
                    "tools/call" => {
                        self.saw_cancel.store(cancel.is_some(), Ordering::SeqCst);
                        Ok(json!({
                            "content": [{ "type": "text", "text": "ok" }],
                            "isError": false
                        }))
                    }
                    _ => self.request(method, params),
                }
            }

            fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
                Ok(())
            }
        }

        let saw_cancel = Arc::new(AtomicBool::new(false));
        let ds = DownstreamServer::connect(
            "s".into(),
            Box::new(CancelAware {
                saw_cancel: Arc::clone(&saw_cancel),
            }),
        )
        .unwrap();
        let mut router = Router::new();
        router.add(ds);
        let registry = CancelRegistry::new();
        assert!(registry.begin_client_request("99".to_string()));

        let result = router
            .route_call_with_cancel(
                "s__echo",
                json!({}),
                Some(registry.context("99".to_string())),
                None,
            )
            .unwrap();

        assert_eq!(result["content"][0]["text"], "ok");
        assert!(saw_cancel.load(Ordering::SeqCst));
        registry.finish_client_request("99");
    }

    #[test]
    fn aggregates_and_routes_prompts() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        // Prompt names are namespaced like tools.
        let names: Vec<String> = router
            .aggregated_prompts()
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names, vec!["github__greet", "postgres__greet"]);

        // prompts/get forwards the server's real prompt name.
        let result = router.get_prompt("github__greet", json!({})).unwrap();
        assert_eq!(result["messages"][0]["content"], "github:greet");
        assert!(router.get_prompt("nope__greet", json!({})).is_err());
    }

    #[test]
    fn deny_destructive_hides_flagged_tools() {
        let policy = ToolPolicy {
            deny_destructive: true,
            ..Default::default()
        };
        let mut router = Router::with_policy(policy);
        router.add(DownstreamServer::connect("db".to_string(), Box::new(DestructiveMock)).unwrap());

        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        // drop_table is blocked; list_tables remains.
        assert_eq!(names, vec!["db__list_tables"]);
        let err = router.route_call("db__drop_table", json!({})).unwrap_err();
        assert!(err.contains("destructive"), "unexpected: {err}");
    }

    #[test]
    fn tool_scope_allow_list_hides_and_blocks_non_listed_tools() {
        // A profile's per-server allow-list ("FeatureSet"): the server exposes ONLY the
        // listed tool; the rest are both hidden from the catalog and blocked on a direct call.
        let mut allow = HashMap::new();
        allow.insert("db".to_string(), HashSet::from(["echo".to_string()]));
        let policy = ToolPolicy {
            allow,
            ..Default::default()
        };
        let mut router = Router::with_policy(policy);
        router.add(mock_server("db"));

        let names: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names, vec!["db__echo"], "only the allow-listed tool is exposed");

        // Hidden, and also blocked on a direct call (not merely invisible).
        let err = router.route_call("db__add", json!({})).unwrap_err();
        assert!(err.contains("tool scope"), "unexpected: {err}");
        assert!(router.route_call("db__echo", json!({})).is_ok());
    }

    #[test]
    fn refresh_keeps_collision_suffixes_stable() {
        // Two tools that sanitize to the same exposed name collide; the second
        // gets a `_2` suffix. After a refresh (re-query + reindex) the order and
        // suffixes must not shuffle, or a client's tool names would change
        // mid-session and break in-flight calls.
        struct DupMock;
        impl Transport for DupMock {
            fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
                match method {
                    "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                    "tools/list" => Ok(json!({ "tools": [
                        { "name": "a-b", "description": "one" },
                        { "name": "a_b", "description": "two" }
                    ] })),
                    other => Err(TransportError::Fatal(format!("unexpected {other}"))),
                }
            }
            fn notify(&mut self, _m: &str, _p: Value) -> Result<(), TransportError> {
                Ok(())
            }
        }
        let names = |r: &Router| -> Vec<String> {
            r.aggregated_tools()
                .iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        };
        let mut router = Router::new();
        router.add(DownstreamServer::connect("s".to_string(), Box::new(DupMock)).unwrap());
        let before = names(&router);
        assert_eq!(before, vec!["s__a_b", "s__a_b_2"]);
        router.refresh_tools();
        assert_eq!(names(&router), before, "refresh shuffled the collision suffixes");
    }

    #[test]
    fn reordered_tool_list_keeps_each_tool_its_own_exposed_name() {
        // The dangerous variant of the test above: the server doesn't just get
        // re-queried, it comes back listing the SAME two colliding tools in the
        // opposite order. Allocating suffixes by list position swapped `_2`
        // between them, so the client's cached `s__a_b` silently started routing
        // to `a_b` instead of `a-b` — calls kept succeeding and went to the wrong
        // tool. The exposed name must be a property of the tool, not its position.
        struct ReorderMock {
            calls: AtomicU32,
        }
        impl Transport for ReorderMock {
            fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
                match method {
                    "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                    "tools/list" => {
                        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
                        // Same two tools, flipped on the second listing.
                        Ok(if first {
                            json!({ "tools": [
                                { "name": "a-b", "description": "one" },
                                { "name": "a_b", "description": "two" }
                            ] })
                        } else {
                            json!({ "tools": [
                                { "name": "a_b", "description": "two" },
                                { "name": "a-b", "description": "one" }
                            ] })
                        })
                    }
                    other => Err(TransportError::Fatal(format!("unexpected {other}"))),
                }
            }
            fn notify(&mut self, _m: &str, _p: Value) -> Result<(), TransportError> {
                Ok(())
            }
        }

        let mut router = Router::new();
        router.add(
            DownstreamServer::connect(
                "s".to_string(),
                Box::new(ReorderMock {
                    calls: AtomicU32::new(0),
                }),
            )
            .unwrap(),
        );
        // Assert the ROUTE, not just the name set: both names exist either way,
        // so only the mapping reveals the swap.
        assert_eq!(router.route_of("s__a_b"), Some(("s", "a-b")));
        assert_eq!(router.route_of("s__a_b_2"), Some(("s", "a_b")));

        router.refresh_tools();

        assert_eq!(
            router.route_of("s__a_b"),
            Some(("s", "a-b")),
            "a reordered tools/list re-pointed a cached exposed name at a different tool"
        );
        assert_eq!(router.route_of("s__a_b_2"), Some(("s", "a_b")));
    }

    /// Shared retry-capable mock used to exercise the Router helper.
    struct RetryMock {
        tool_failures: Arc<AtomicU32>,
        resource_failures: Arc<AtomicU32>,
        prompt_failures: Arc<AtomicU32>,
        tool_call_entries: Arc<AtomicU32>,
    }

    impl Transport for RetryMock {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "resources": {}, "prompts": {} }
                })),
                "tools/list" => Ok(json!({
                    "tools": [
                        { "name": "flaky", "description": "flaky tool" },
                        { "name": "stable", "description": "always succeeds" }
                    ]
                })),
                "resources/list" => Ok(json!({
                    "resources": [{ "uri": "retry://res", "name": "res" }]
                })),
                "prompts/list" => Ok(json!({
                    "prompts": [{ "name": "greet", "description": "greeting" }]
                })),
                "tools/call" => {
                    self.tool_call_entries.fetch_add(1, Ordering::SeqCst);
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name == "stable" {
                        return Ok(json!({
                            "content": [{ "type": "text", "text": "stable-ok" }],
                            "isError": false
                        }));
                    }
                    let prev = self.tool_failures.load(Ordering::SeqCst);
                    if prev > 0 {
                        self.tool_failures.store(prev - 1, Ordering::SeqCst);
                        Err(TransportError::Retry {
                            retry_after: Some(Duration::from_millis(50)),
                            message: "simulated 429".to_string(),
                        })
                    } else {
                        Ok(json!({
                            "content": [{ "type": "text", "text": "ok-after-retry" }],
                            "isError": false
                        }))
                    }
                }
                "resources/read" => {
                    let prev = self.resource_failures.load(Ordering::SeqCst);
                    if prev > 0 {
                        self.resource_failures.store(prev - 1, Ordering::SeqCst);
                        Err(TransportError::Retry {
                            retry_after: Some(Duration::from_millis(1)),
                            message: "retry resource".to_string(),
                        })
                    } else {
                        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                        Ok(json!({ "contents": [{ "uri": uri, "text": "resource-ok" }] }))
                    }
                }
                "prompts/get" => {
                    let prev = self.prompt_failures.load(Ordering::SeqCst);
                    if prev > 0 {
                        self.prompt_failures.store(prev - 1, Ordering::SeqCst);
                        Err(TransportError::Retry {
                            retry_after: Some(Duration::from_millis(1)),
                            message: "retry prompt".to_string(),
                        })
                    } else {
                        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        Ok(json!({ "messages": [{ "role": "user", "content": format!("gp:{name}") }] }))
                    }
                }
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    struct FatalMock;
    impl Transport for FatalMock {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                "tools/list" => Ok(json!({ "tools": [{ "name": "boom", "description": "always fails" }] })),
                "tools/call" => Err(TransportError::Fatal("HTTP 500: server error".to_string())),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn retry_server(id: &str, tool_failures: u32, resource_failures: u32, prompt_failures: u32) -> DownstreamServer {
        let mut ds = DownstreamServer::connect(
            id.to_string(),
            Box::new(RetryMock {
                tool_failures: Arc::new(AtomicU32::new(tool_failures)),
                resource_failures: Arc::new(AtomicU32::new(resource_failures)),
                prompt_failures: Arc::new(AtomicU32::new(prompt_failures)),
                tool_call_entries: Arc::new(AtomicU32::new(0)),
            }),
        )
        .unwrap();
        ds.load_resources_prompts();
        ds
    }

    fn retry_server_inspectable(
        id: &str,
        tool_failures: u32,
    ) -> (DownstreamServer, Arc<AtomicU32>) {
        let entries = Arc::new(AtomicU32::new(0));
        let mut ds = DownstreamServer::connect(
            id.to_string(),
            Box::new(RetryMock {
                tool_failures: Arc::new(AtomicU32::new(tool_failures)),
                resource_failures: Arc::new(AtomicU32::new(0)),
                prompt_failures: Arc::new(AtomicU32::new(0)),
                tool_call_entries: Arc::clone(&entries),
            }),
        )
        .unwrap();
        ds.load_resources_prompts();
        (ds, entries)
    }

    #[test]
    fn retry_succeeds_after_transient_failure() {
        let mut router = Router::new();
        router.add(retry_server("flaky", 1, 0, 0));
        let result = router.route_call("flaky__flaky", json!({})).unwrap();
        assert_eq!(result["content"][0]["text"], "ok-after-retry");
    }

    #[test]
    fn fatal_error_does_not_retry() {
        let mut router = Router::new();
        router.add(DownstreamServer::connect("fatal".to_string(), Box::new(FatalMock)).unwrap());
        let err = router.route_call("fatal__boom", json!({})).unwrap_err();
        assert!(err.contains("500"), "unexpected error: {err}");
    }

    #[test]
    fn get_prompt_also_retries() {
        let mut router = Router::new();
        router.add(retry_server("gp", 0, 0, 1));
        let result = router.get_prompt("gp__greet", json!({})).unwrap();
        assert_eq!(result["messages"][0]["content"], "gp:greet");
    }

    #[test]
    fn read_resource_also_retries() {
        let mut router = Router::new();
        router.add(retry_server("rr", 0, 1, 0));
        let result = router.read_resource("retry://res").unwrap();
        assert_eq!(result["contents"][0]["text"], "resource-ok");
    }

    #[test]
    fn retry_does_not_block_unrelated_server() {
        let slow = retry_server("slow", 1, 0, 0);
        let fast = mock_server("fast");
        let mut router = Router::new();
        router.add(slow);
        router.add(fast);

        let router = Arc::new(router);
        let router_a = Arc::clone(&router);
        let handle = std::thread::spawn(move || router_a.route_call("slow__flaky", json!({})));

        std::thread::sleep(Duration::from_millis(10));
        let fast_result = router.route_call("fast__echo", json!({}));
        assert!(fast_result.is_ok(), "fast server should not block behind slow retry");

        let slow_result = handle.join().unwrap();
        assert!(slow_result.is_ok(), "slow server should eventually succeed");
    }

    /// THE critical test: proves the per-server Mutex is RELEASED during the
    /// backoff sleep. Without the fix, call A would hold the lock while sleeping,
    /// and call B to the SAME server would block until A's retry completed.
    #[test]
    fn same_server_lock_released_during_backoff_sleep() {
        let (server, entries) = retry_server_inspectable("srv", 1);
        let mut router = Router::new();
        router.add(server);
        let router = Arc::new(router);

        let router1 = Arc::clone(&router);
        let handle = std::thread::spawn(move || router1.route_call("srv__flaky", json!({})));

        // Wait long enough for thread 1 to acquire the lock, get the 429, and
        // enter the backoff sleep — but NOT long enough for the 50ms retry.
        std::thread::sleep(Duration::from_millis(15));

        // Call the stable tool on the SAME server. If the fix is correct, the
        // lock was released during the backoff sleep, so this succeeds immediately.
        let result_b = router.route_call("srv__stable", json!({}));
        assert!(result_b.is_ok(), "same-server call should succeed during backoff sleep");
        let result = result_b.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "stable-ok");

        let result_a = handle.join().unwrap();
        assert!(result_a.is_ok(), "flaky call should succeed after retry");

        // At least 3 lock acquisitions: flaky 429, stable ok, flaky retry ok.
        assert!(
            entries.load(Ordering::SeqCst) >= 3,
            "expected >=3 tool/call lock acquisitions"
        );
    }

    #[test]
    fn cancellation_during_backoff_prevents_the_retry_attempt() {
        let (server, entries) = retry_server_inspectable("cancel-retry", 1);
        let mut router = Router::new();
        router.add(server);
        let router = Arc::new(router);
        let cancellations = CancelRegistry::new();
        assert!(cancellations.begin_client_request("cancel-retry-1".to_string()));
        let cancel = cancellations.context("cancel-retry-1".to_string());

        let worker_router = Arc::clone(&router);
        let handle = std::thread::spawn(move || {
            worker_router.route_call_with_cancel(
                "cancel_retry__flaky",
                json!({}),
                Some(cancel),
                None,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while entries.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(entries.load(Ordering::SeqCst), 1, "first attempt must run once");
        assert!(cancellations.cancel("cancel-retry-1", Some("user pressed stop")));

        let error = handle.join().unwrap().unwrap_err();
        assert!(error.contains("cancelled"), "unexpected error: {error}");
        assert_eq!(
            entries.load(Ordering::SeqCst),
            1,
            "no downstream attempt may be emitted after cancellation"
        );
        let breaker = router.servers[0].breaker.lock().unwrap();
        assert_eq!(
            breaker.consecutive_failures, 0,
            "upstream cancellation must not count as a downstream health failure"
        );
        assert!(breaker.open_until.is_none(), "cancellation must leave the breaker closed");
        cancellations.finish_client_request("cancel-retry-1");
    }
}
