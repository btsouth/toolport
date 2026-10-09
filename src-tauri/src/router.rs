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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde_json::{json, Value};

use crate::call_failure::{CallFailure, CallFailureKind};
use crate::downstream::{
    backoff_delay, is_implausible_shrink, CacheHint, CancelContext, DownstreamServer, MrtrRequest,
    ServerDispatch, TransportError, HTTP_MAX_RETRIES, HTTP_RETRY_CAP,
};
use crate::registry::ToolOverride;
use crate::tool_definitions::{
    content_digest, SerializedTools, SharedTools, ToolCatalog, ToolDefinition, ToolPolicyMetadata,
};
use std::sync::Weak;

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
    let (server, task): (String, String) =
        serde_json::from_slice(&plain).map_err(|_| "malformed Toolport task id".to_string())?;
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
///
/// A charset rewrite, not an identity: `gh-api` and `gh_api` meet here. The
/// gateway still keys client scope, PII origin, block exemptions and result
/// budgets on this form, so `registry::unique_id` keeps new ids injective under
/// it (SBS-880). Compare raw ids wherever a raw id is available.
pub fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
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

/// Normalize client input schemas when reading a legacy catalog snapshot.
pub fn normalize_tool_schema(schema: &mut Value) {
    crate::schema_compat::normalize(schema);
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
    let ref_str = node
        .get("$ref")
        .and_then(|v| v.as_str())
        .map(str::to_string);
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
    /// Server ids this router may dispatch to. `None` sets no limit (tests and the
    /// startup placeholder). A server that drops out of the registry's enabled set
    /// is refused as soon as the policy is republished, even though its connection
    /// stays up until the next rebuild.
    pub servers: Option<HashSet<String>>,
    /// server id -> original tool names the user switched off.
    pub disabled: HashMap<String, HashSet<String>>,
    /// server id -> the ONLY original tool names this client exposes (tool-granular
    /// scoping / "FeatureSet"). A server present here allow-lists: every other tool on it is
    /// hidden and blocked. A server ABSENT exposes all of its tools. Empty = no tool-granular
    /// scoping, so this is fully backward compatible.
    pub allow: HashMap<String, HashSet<String>>,
    /// Hide and block any tool annotated `destructiveHint: true`.
    pub deny_destructive: bool,
    /// Exposed (namespaced) tool names quarantined after a high-risk drift; hidden
    /// until the user re-approves them. Empty unless quarantine-on-drift is enabled.
    pub quarantined: BTreeSet<String>,
    /// Hide every tool. Used when a cold-start quarantine-store read fails
    /// (SBS-871): there is no prior live set to keep, so the catalog stays
    /// blocked until a SUCCESSFUL store read installs a real set through
    /// [`Router::requarantine_from_store`]. Error paths must use
    /// [`Router::requarantine`], which leaves this set.
    pub fail_closed_catalog: bool,
}

impl ToolPolicy {
    /// Whether this policy lets a request reach `server_id` at all.
    pub fn allows_server(&self, server_id: &str) -> bool {
        self.servers
            .as_ref()
            .is_none_or(|servers| servers.contains(server_id))
    }

    /// Reason this tool is blocked, or `None` if it may be exposed. `exposed` is the
    /// namespaced client-facing name (what quarantine is keyed by).
    fn blocked_reason(
        &self,
        exposed: &str,
        server_id: &str,
        orig: &str,
        tool: ToolPolicyMetadata,
    ) -> Option<&'static str> {
        // Tool-granular profile scope: if this server is narrowed to an allow-list, a tool
        // not on it is outside this client's scope (hidden + blocked, same as disabled).
        if self
            .allow
            .get(server_id)
            .is_some_and(|set| !set.contains(orig))
        {
            return Some("outside this client's tool scope");
        }
        self.blocked_reason_unscoped(exposed, server_id, orig, tool)
    }

    /// [`Self::blocked_reason`] without the tool-granular profile scope. Daemon
    /// adapter views replace the base router's scope with their own, so a recheck
    /// against the live base router must not apply the base's scope to them.
    fn blocked_reason_unscoped(
        &self,
        exposed: &str,
        server_id: &str,
        orig: &str,
        tool: ToolPolicyMetadata,
    ) -> Option<&'static str> {
        if !self.allows_server(server_id) {
            return Some("on a server that is turned off");
        }
        if self
            .disabled
            .get(server_id)
            .is_some_and(|set| set.contains(orig))
        {
            return Some("disabled");
        }
        if self.deny_destructive && tool.destructive {
            return Some("blocked by the destructive-tool policy");
        }
        if self.fail_closed_catalog {
            return Some("quarantine store unreadable; catalog blocked until the store reads");
        }
        if self.quarantined.contains(exposed) {
            return Some("quarantined after a high-risk change; re-approve to restore");
        }
        None
    }
}

/// The part of [`ToolPolicy`] that comes from the registry alone. Quarantine has its
/// own store and reconcile path, so republishing this leaves quarantine untouched.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct RegistryPolicy {
    pub servers: Option<HashSet<String>>,
    pub disabled: HashMap<String, HashSet<String>>,
    pub allow: HashMap<String, HashSet<String>>,
    pub deny_destructive: bool,
}

impl RegistryPolicy {
    /// The full policy, with the quarantine state supplied separately.
    pub fn with_quarantine(
        self,
        quarantined: BTreeSet<String>,
        fail_closed_catalog: bool,
    ) -> ToolPolicy {
        ToolPolicy {
            servers: self.servers,
            disabled: self.disabled,
            allow: self.allow,
            deny_destructive: self.deny_destructive,
            quarantined,
            fail_closed_catalog,
        }
    }
}

/// What a downstream dispatch is about to touch, for [`Router::authorize`].
#[derive(Clone, Copy, Debug)]
pub enum DispatchTarget<'a> {
    /// A tool, by its exposed (client-facing) name.
    Tool(&'a str),
    /// A resource, subscription, prompt, completion or task request to a server.
    Server(&'a str),
}

/// One connected downstream server behind its own lock. Calls on a transport
/// that multiplexes requests (stdio) take the lock only to fetch a
/// [`crate::downstream::CallHandle`], so one slow call never blocks other calls to
/// the same server; other transports still serialize on the lock. Held as an
/// `Arc` so an in-flight call can keep the slot (and its live child process)
/// alive across the downstream I/O without holding the router lock, and survive
/// a concurrent router replacement.
struct ServerSlot {
    id: String,
    inner: Mutex<DownstreamServer>,
    tool_revision: AtomicU64,
    definitions: Mutex<HashMap<[u8; 32], Weak<ToolDefinition>>>,
    /// Fast-fail state for a server that keeps failing (dead/hung), so we don't pay
    /// its full read timeout on every call once it's clearly down.
    breaker: Mutex<Breaker>,
    /// Rebuild this server's connection from scratch (re-spawn a crashed stdio child
    /// / re-dial a dropped remote). Invoked only on the breaker's half-open probe,
    /// i.e. after the server has failed for a full cooldown, so a live server is never
    /// needlessly re-spawned on a transient blip. `None` = not reconnectable (e.g. a
    /// test fixture), in which case a dead server just stays fast-failed as before.
    reconnect: Option<Reconnect>,
    /// Bounds concurrent calls to this server.
    in_flight: InFlightLimit,
    /// Bumped each time `reconnect` replaces the connection, so concurrent calls
    /// that failed on the same dead connection re-spawn it only once.
    generation: AtomicU64,
    /// Calls that succeeded. A failed probe re-spawns a live multiplexed
    /// connection only if no other call succeeded while it was in flight.
    successes: AtomicU64,
    /// Calls running on a call handle. Counted while the slot lock is held to
    /// fetch the handle, so a re-spawn that checks it under the lock sees every
    /// call that could still be using the connection it replaces.
    handle_calls: AtomicUsize,
    reconnect_gate: Mutex<()>,
    supervisor: Option<Mutex<Supervisor>>,
    /// Set when a rebuild replaced or removed this supervisor. It finishes
    /// active calls but never starts again, and its waiters fail fast.
    retired: AtomicBool,
}

impl ServerSlot {
    fn new(id: String, server: DownstreamServer, reconnect: Option<Reconnect>) -> Self {
        ServerSlot {
            id,
            inner: Mutex::new(server),
            tool_revision: AtomicU64::new(0),
            definitions: Mutex::default(),
            breaker: Mutex::new(Breaker::default()),
            reconnect,
            in_flight: InFlightLimit::default(),
            generation: AtomicU64::new(0),
            successes: AtomicU64::new(0),
            handle_calls: AtomicUsize::new(0),
            reconnect_gate: Mutex::new(()),
            supervisor: None,
            retired: AtomicBool::new(false),
        }
    }

    /// Whether nothing shows a live multiplexed connection is still working:
    /// no call succeeded since `successes` was read, no other call is running
    /// on it, and none is suspended waiting for the client's input. Only then
    /// may a failed probe replace it, since that ends every call in flight.
    /// Read under the slot lock (`server` is its guard) for an exact count.
    fn quiescent_since(&self, server: &DownstreamServer, successes: u64) -> bool {
        self.successes.load(Ordering::Acquire) == successes
            && self.handle_calls.load(Ordering::Acquire) == 0
            && server.suspended_calls() == 0
    }
}

/// A downstream stays warm for five minutes after the last completed use.
/// Discovery from an existing catalog does not count as use.
pub const SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Counts supervisor startups that produced a connection, so the gateway can
/// publish one without waiting for its next watcher tick.
static SUPERVISOR_RESULTS: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

pub fn started_supervisors() -> u64 {
    *SUPERVISOR_RESULTS
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wait until a startup after `seen` produced a connection, or until
/// `deadline`. Returns the latest count.
pub fn wait_for_started_supervisor(seen: u64, deadline: Instant) -> u64 {
    let (count, signal) = &SUPERVISOR_RESULTS;
    let mut current = count
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *current == seen {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        current = signal
            .wait_timeout(current, deadline - now)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    *current
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorState {
    Stopped,
    Starting,
    Ready,
    Degraded,
    Backoff,
    NeedsAuth,
    Stopping,
}

struct Supervisor {
    state: SupervisorState,
    connect: Connect,
    backoff: ReconnectBackoff,
    failures: u32,
    ever_ready: bool,
    last_error: String,
    next_attempt: Instant,
    last_use: Instant,
    last_attempt: Instant,
    subscription_use: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ready: Option<DownstreamServer>,
    publishing: bool,
}

impl ServerSlot {
    fn supervised(
        id: String,
        tools: Vec<Value>,
        connect: Connect,
        backoff: ReconnectBackoff,
    ) -> Self {
        let mut slot = Self::new(id.clone(), DownstreamServer::stopped(id, tools), None);
        slot.supervisor = Some(Mutex::new(Supervisor {
            state: SupervisorState::Stopped,
            connect,
            backoff,
            failures: 0,
            ever_ready: false,
            last_error: String::new(),
            next_attempt: Instant::now(),
            last_use: Instant::now(),
            last_attempt: Instant::now(),
            subscription_use: None,
            ready: None,
            publishing: false,
        }));
        slot
    }

    fn retire(&self) {
        let _lifecycle = self
            .supervisor
            .as_ref()
            .map(|s| s.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        self.retired.store(true, Ordering::Release);
    }

    /// A published connection loaded the full catalog, and stopping keeps it.
    fn catalog_complete(&self) -> bool {
        self.supervisor.as_ref().is_none_or(|s| {
            s.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ever_ready
        })
    }

    fn status(&self) -> Option<PendingStatus> {
        let state = self
            .supervisor
            .as_ref()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(PendingStatus {
            id: self.id.clone(),
            needs_auth: state.state == SupervisorState::NeedsAuth,
            connecting: state.state == SupervisorState::Starting,
            failures: state.failures,
            last_error: state.last_error.clone(),
            retry_in: Some(state.next_attempt.saturating_duration_since(Instant::now())),
        })
    }

    fn start(self: &Arc<Self>, demand: bool) -> bool {
        self.start_at(demand, Instant::now())
    }

    fn start_at(self: &Arc<Self>, demand: bool, now: Instant) -> bool {
        let Some(supervisor) = &self.supervisor else {
            return false;
        };
        if self.retired.load(Ordering::Acquire) {
            return false;
        }
        let connect = {
            let mut state = supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.publishing
                || !(demand && state.state == SupervisorState::Stopped
                    || state.state == SupervisorState::Backoff
                        && (now >= state.next_attempt
                            || demand
                                && now.saturating_duration_since(state.last_attempt)
                                    >= Duration::from_secs(15)))
            {
                return false;
            }
            state.state = SupervisorState::Starting;
            state.last_attempt = now;
            Arc::clone(&state.connect)
        };
        // A Weak reference lets removal retire an in-progress start: its result
        // cannot keep an obsolete supervisor or child alive on its own.
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name(format!("supervisor-{}", self.id))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connect()))
                    .unwrap_or_else(|_| {
                        Err(ConnectFailure {
                            message: "connection startup panicked".to_string(),
                            needs_auth: false,
                        })
                    });
                if let Some(slot) = weak.upgrade() {
                    let mut state = slot
                        .supervisor
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    // Retirement is set under this lock, so a replaced start
                    // can never hand its connection to an obsolete slot.
                    if slot.retired.load(Ordering::Acquire) {
                        state.state = SupervisorState::Stopped;
                        return;
                    }
                    match result {
                        Ok(server) => {
                            state.ready = Some(server);
                            drop(state);
                            let (count, signal) = &SUPERVISOR_RESULTS;
                            let mut count = count
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            *count = count.wrapping_add(1);
                            signal.notify_all();
                        }
                        Err(failure) => {
                            state.failures = state.failures.saturating_add(1);
                            state.last_error = failure.message;
                            state.next_attempt = Instant::now()
                                + state
                                    .backoff
                                    .delay(state.failures)
                                    .mul_f64(reconnect_jitter())
                                    .min(state.backoff.cap);
                            state.state = if failure.needs_auth {
                                SupervisorState::NeedsAuth
                            } else {
                                SupervisorState::Backoff
                            };
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            let mut state = supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.state = SupervisorState::Backoff;
            state.last_error = error.to_string();
            state.next_attempt = Instant::now() + state.backoff.base;
            return false;
        }
        true
    }

    /// Every dispatch waits for a demand start, including a post-approval call.
    fn wait_for_start(
        self: &Arc<Self>,
        cancel: Option<&CancelContext>,
        continuation: bool,
    ) -> Result<(), String> {
        let Some(supervisor) = &self.supervisor else {
            return Ok(());
        };
        if !continuation && cancel.is_some_and(CancelContext::is_cancelled) {
            return Err("request cancelled while the server was starting".to_string());
        }
        self.start(true);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if !continuation && cancel.is_some_and(CancelContext::is_cancelled) {
                return Err("request cancelled while the server was starting".to_string());
            }
            let mut state = supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.state == SupervisorState::Ready {
                state.last_use = Instant::now();
                return Ok(());
            }
            if self.retired.load(Ordering::Acquire) {
                return Err(format!(
                    "server '{}' was reconfigured while starting; send the request again",
                    self.id
                ));
            }
            let starting = state.state == SupervisorState::Starting;
            drop(state);
            if !starting || Instant::now() >= deadline {
                return Err(self.unavailable());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn unavailable(&self) -> String {
        let status = self.status().unwrap();
        if status.needs_auth {
            format!("server '{}' {}", self.id, status.describe())
        } else {
            let state = self
                .supervisor
                .as_ref()
                .unwrap()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let reason = if !state.ever_ready && state.state == SupervisorState::Backoff {
                "has not connected yet"
            } else {
                "is restarting"
            };
            format!(
                "server '{}' {reason}, retry in {}s ({})",
                self.id,
                // A demand retries after 15 seconds even on a longer schedule.
                state
                    .next_attempt
                    .min(state.last_attempt + Duration::from_secs(15))
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    + 1,
                client_safe_error(&status.last_error)
            )
        }
    }

    /// Only a dead connection or a failed half-open probe can degrade a live
    /// supervisor. A lone slow call must never end other multiplexed calls.
    fn degrade(&self, generation: u64, successes: u64, error: &str, probe: bool) -> bool {
        let Some(supervisor) = &self.supervisor else {
            return false;
        };
        let mut state = supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.state != SupervisorState::Ready
            || self.generation.load(Ordering::Acquire) != generation
        {
            return state.state != SupervisorState::Ready;
        }
        let Ok(mut server) = self.inner.try_lock() else {
            return false;
        };
        let closed = server.connection_closed() == Some(true);
        if !closed && !(probe && self.quiescent_since(&server, successes)) {
            return false;
        }
        state.state = SupervisorState::Degraded;
        state.last_error = error.to_string();
        state.failures = state.failures.saturating_add(1);
        state.next_attempt = Instant::now()
            + state
                .backoff
                .delay(state.failures)
                .mul_f64(reconnect_jitter())
                .min(state.backoff.cap);
        server.stop();
        state.state = SupervisorState::Backoff;
        true
    }

    fn maintain(self: &Arc<Self>, now: Instant) {
        let Some(supervisor) = &self.supervisor else {
            return;
        };
        {
            let mut state = supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.state == SupervisorState::Ready {
                // try_lock keeps a serial remote call from blocking the watcher.
                if let Ok(mut server) = self.inner.try_lock() {
                    if server.connection_closed() == Some(true) {
                        state.state = SupervisorState::Degraded;
                        state.last_error = "the server closed the connection".to_string();
                        state.failures = state.failures.saturating_add(1);
                        state.next_attempt = now
                            + state
                                .backoff
                                .delay(state.failures)
                                .mul_f64(reconnect_jitter())
                                .min(state.backoff.cap);
                        server.stop();
                        state.state = SupervisorState::Backoff;
                    } else if now.saturating_duration_since(state.last_use) >= SERVER_IDLE_TIMEOUT
                        && self.handle_calls.load(Ordering::Acquire) == 0
                        && server.suspended_calls() == 0
                        && !state.subscription_use.as_ref().is_some_and(|used| used())
                    {
                        state.state = SupervisorState::Stopping;
                        server.stop();
                        state.state = SupervisorState::Stopped;
                        state.failures = 0;
                    }
                }
            }
        }
        self.start(false);
    }
}

/// Counts a dispatch until its completion time is recorded. Both happen under
/// the lifecycle lock, so idle shutdown cannot slip between those two updates.
struct HandleCall<'a>(&'a ServerSlot);

impl Drop for HandleCall<'_> {
    fn drop(&mut self) {
        if let Some(supervisor) = &self.0.supervisor {
            let mut state = supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.last_use = Instant::now();
            self.0.handle_calls.fetch_sub(1, Ordering::AcqRel);
        } else {
            self.0.handle_calls.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Concurrent calls allowed per server. Further calls wait for a free slot up to
/// their own deadline, then fail as busy, so a flood of calls cannot grow
/// without bound.
const MAX_IN_FLIGHT_PER_SERVER: usize = 64;

#[derive(Default)]
struct InFlightLimit {
    count: Mutex<usize>,
    freed: Condvar,
}

struct InFlightPermit<'a>(&'a InFlightLimit);

impl InFlightLimit {
    fn acquire(
        &self,
        server: &str,
        wait: Duration,
        cancel: Option<&CancelContext>,
    ) -> Result<InFlightPermit<'_>, TransportError> {
        let deadline = Instant::now() + wait;
        let mut count = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *count >= MAX_IN_FLIGHT_PER_SERVER {
            if cancel.is_some_and(CancelContext::is_cancelled) {
                return Err(TransportError::Cancelled(
                    "request cancelled while waiting for a free slot on the server".to_string(),
                ));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(TransportError::Busy(format!(
                    "server '{server}' is busy: {MAX_IN_FLIGHT_PER_SERVER} calls are already \
                     in flight. Try again shortly."
                )));
            }
            // Wake periodically so a cancelled caller stops waiting promptly.
            let slice = (deadline - now).min(Duration::from_millis(250));
            count = self
                .freed
                .wait_timeout(count, slice)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        *count += 1;
        Ok(InFlightPermit(self))
    }
}

impl Drop for InFlightPermit<'_> {
    fn drop(&mut self) {
        let mut count = self
            .0
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count -= 1;
        self.0.freed.notify_one();
    }
}

/// How a dispatch reaches its server.
#[derive(Clone, Copy)]
enum SlotAccess {
    /// Through a call handle when the transport multiplexes requests, without
    /// holding the slot lock across the round trip.
    Shared,
    /// Under the slot lock: the operation changes connection state.
    Locked,
}

/// The locked server behind a [`SlotAccess::Locked`] dispatch.
fn locked_server(server: &mut dyn ServerDispatch) -> Result<&mut DownstreamServer, TransportError> {
    server.locked_server().ok_or_else(|| {
        TransportError::Fatal("internal error: state change without the server lock".to_string())
    })
}
/// An opaque reference to one live downstream launch. The gateway's launch
/// pool can hold this without retaining an obsolete router or its other slots.
#[derive(Clone)]
pub struct SharedServerSlot(Arc<ServerSlot>);

impl SharedServerSlot {
    pub fn maintain(&self) {
        self.0.maintain(Instant::now());
    }

    /// Retire a removed launch without interrupting its active calls.
    pub fn retire(&self) {
        self.0.retire();
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Factory that rebuilds a downstream connection on demand. Supplied by the gateway
/// (which owns the registry + secret injection) so `router` stays free of spawn logic;
/// returns `None` if the server still can't be reached.
pub type Reconnect = Box<dyn Fn() -> Option<DownstreamServer> + Send + Sync>;

/// Whether an ambiguous health failure may replay the current operation after
/// rebuilding its connection. Tool annotations are not evidence of safe replay.
#[derive(Clone, Copy)]
enum ReplayPolicy {
    ReadOnly,
    NoAmbiguousReplay,
}

impl ReplayPolicy {
    fn for_task(method: &str) -> Self {
        if method == "tasks/get" {
            Self::ReadOnly
        } else {
            Self::NoAmbiguousReplay
        }
    }

    fn uncertain_failure<'a>(self, error: &'a TransportError) -> Option<&'a TransportError> {
        match (self, error) {
            (Self::NoAmbiguousReplay, TransportError::Unavailable(_)) => Some(error),
            (_, TransportError::Classified(kind, _)) if kind.uncertain() => Some(error),
            _ => None,
        }
    }
}

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
    /// When the last counted failure was recorded.
    last_failure: Option<Instant>,
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
        self.last_failure = None;
    }

    /// A health failure; opens the circuit once the streak hits the threshold.
    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure = Some(now);
        if self.consecutive_failures >= BREAKER_FAILURE_THRESHOLD {
            self.open_until = Some(now + BREAKER_COOLDOWN);
        }
    }

    /// A health failure of a concurrent call that began at `started`. Calls that
    /// were in flight together when the connection failed are one failure, so
    /// several concurrent timeouts do not trip the breaker at once.
    fn record_concurrent_failure(&mut self, started: Option<Instant>, now: Instant) {
        if started.is_some_and(|started| self.last_failure.is_some_and(|last| started < last)) {
            return;
        }
        self.record_failure(now);
    }
}

/// Why connecting a server failed, as the background retry needs it.
#[derive(Debug, Clone)]
pub struct ConnectFailure {
    pub message: String,
    /// The server refused our credentials (or has none). Retrying cannot help until
    /// they change, and a change rewrites the registry, which rebuilds the router.
    pub needs_auth: bool,
}

/// Connect (or re-connect) one server from scratch. Supplied by the gateway, like
/// [`Reconnect`], but keeps the failure so it can be retried and reported.
pub type Connect = Arc<dyn Fn() -> Result<DownstreamServer, ConnectFailure> + Send + Sync>;

/// Capped exponential backoff for retrying a server that never connected.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectBackoff {
    pub base: Duration,
    pub cap: Duration,
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        ReconnectBackoff {
            base: Duration::from_secs(2),
            cap: Duration::from_secs(300),
        }
    }
}

impl ReconnectBackoff {
    /// Delay after `failures` consecutive failures (1-based), before jitter.
    fn delay(&self, failures: u32) -> Duration {
        let mult = 1u32 << failures.saturating_sub(1).min(16);
        self.base.saturating_mul(mult).min(self.cap)
    }
}

/// A demand-driven retry (a call or search naming the server) may come sooner than
/// the schedule, but never more often than once per backoff step, and at most this
/// long apart.
const KICK_MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Spread retries by +/-20% so many gateways that failed together (the usual case:
/// no network at login) do not all retry in lockstep.
fn reconnect_jitter() -> f64 {
    let mut byte = [0u8; 1];
    let unit = match getrandom::getrandom(&mut byte) {
        Ok(()) => f64::from(byte[0]) / 255.0,
        Err(_) => 0.5,
    };
    0.8 + 0.4 * unit
}

/// Retry bookkeeping for one pending server. `now` and the jitter are passed in so
/// the transitions are unit-testable without sleeping.
struct PendingState {
    failures: u32,
    last_error: String,
    needs_auth: bool,
    last_attempt: Instant,
    next_attempt: Instant,
    in_flight: bool,
    /// A retry that connected, waiting for the gateway to adopt it into the catalog.
    ready: Option<DownstreamServer>,
}

impl PendingState {
    fn new(failure: ConnectFailure, backoff: &ReconnectBackoff, now: Instant, jitter: f64) -> Self {
        let mut state = PendingState {
            failures: 0,
            last_error: String::new(),
            needs_auth: false,
            last_attempt: now,
            next_attempt: now,
            in_flight: false,
            ready: None,
        };
        state.record_failure(failure, backoff, now, jitter);
        state
    }

    fn idle(&self) -> bool {
        !self.needs_auth && !self.in_flight && self.ready.is_none()
    }

    fn due(&self, now: Instant) -> bool {
        self.idle() && now >= self.next_attempt
    }

    fn kickable(&self, backoff: &ReconnectBackoff, now: Instant) -> bool {
        let floor = backoff.delay(self.failures).min(KICK_MAX_INTERVAL);
        self.idle() && now.duration_since(self.last_attempt) >= floor
    }

    fn begin(&mut self, now: Instant) {
        self.in_flight = true;
        self.last_attempt = now;
    }

    fn record_failure(
        &mut self,
        failure: ConnectFailure,
        backoff: &ReconnectBackoff,
        now: Instant,
        jitter: f64,
    ) {
        self.in_flight = false;
        self.failures = self.failures.saturating_add(1);
        self.last_error = failure.message;
        self.needs_auth = failure.needs_auth;
        // Cap after jitter, so the slowest retry is the cap itself.
        let delay = backoff
            .delay(self.failures)
            .mul_f64(jitter)
            .min(backoff.cap);
        self.next_attempt = now + delay;
    }
}

/// A server that should be in the catalog but has never connected (it failed at
/// build time). Kept so it can be retried in the background and reported, instead
/// of disappearing until the next full rebuild (REL-03).
struct PendingServer {
    id: String,
    connect: Connect,
    backoff: ReconnectBackoff,
    state: Mutex<PendingState>,
    /// Set when the router that owned this entry was replaced or the server was
    /// adopted. Stops further attempts and discards an in-flight result.
    cancelled: std::sync::atomic::AtomicBool,
}

impl PendingServer {
    fn lock(&self) -> std::sync::MutexGuard<'_, PendingState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn status(&self, now: Instant) -> PendingStatus {
        let state = self.lock();
        PendingStatus {
            id: self.id.clone(),
            needs_auth: state.needs_auth,
            connecting: state.in_flight || state.ready.is_some(),
            failures: state.failures,
            last_error: state.last_error.clone(),
            retry_in: (!state.needs_auth)
                .then(|| state.next_attempt.saturating_duration_since(now)),
        }
    }

    /// Start one connect attempt on its own thread if `ready_to_start` allows it.
    /// The attempt never holds a router lock, so a slow server blocks nothing else.
    fn try_start(self: &Arc<Self>, ready_to_start: impl FnOnce(&PendingState) -> bool) -> bool {
        if self.is_cancelled() {
            return false;
        }
        {
            let mut state = self.lock();
            if !ready_to_start(&state) {
                return false;
            }
            state.begin(Instant::now());
        }
        let pending = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("reconnect-{}", self.id))
            .spawn(move || {
                let result = (pending.connect)();
                pending.finish(result);
            });
        if spawned.is_err() {
            self.lock().in_flight = false;
            return false;
        }
        true
    }

    fn finish(&self, result: Result<DownstreamServer, ConnectFailure>) {
        let mut state = self.lock();
        if self.is_cancelled() {
            state.in_flight = false;
            drop(state);
            // Dropping the connection closes it (and kills a stdio child).
            drop(result);
            return;
        }
        match result {
            Ok(server) => {
                state.in_flight = false;
                state.ready = Some(server);
            }
            Err(failure) => {
                let backoff = self.backoff;
                state.record_failure(failure, &backoff, Instant::now(), reconnect_jitter());
            }
        }
    }
}

/// What a pending server is doing, for status text and error messages.
#[derive(Debug, Clone)]
pub struct PendingStatus {
    pub id: String,
    pub needs_auth: bool,
    /// An attempt is running, or one just connected and is joining the catalog.
    pub connecting: bool,
    pub failures: u32,
    pub last_error: String,
    pub retry_in: Option<Duration>,
}

impl PendingStatus {
    /// One line describing the server's state, without its id.
    pub fn describe(&self) -> String {
        let error = client_safe_error(&self.last_error);
        if self.needs_auth {
            format!("needs sign-in in Toolport (last error: {error})")
        } else if self.connecting {
            format!("connecting (last error: {error})")
        } else {
            let secs = self.retry_in.map_or(0, |d| d.as_secs() + 1);
            format!(
                "retrying in {secs}s after {} failed attempt(s) (last error: {error})",
                self.failures
            )
        }
    }
}

/// What an MCP client may see about a connect failure. The raw error can carry a
/// child's stderr or a server's response body, either of which may echo a secret
/// the gateway injected, so only a fixed category (with an exit or HTTP status)
/// reaches the model. The full text is already in the gateway log.
fn client_safe_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let code_after = |marker: &str| -> Option<String> {
        let at = lower.find(marker)? + marker.len();
        let code: String = lower[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!code.is_empty()).then_some(code)
    };
    let summary = if let Some(code) = code_after("http ") {
        format!("the server answered HTTP {code}")
    } else if lower.contains("exited") {
        match code_after("status ") {
            Some(code) => format!("the server process exited (status {code})"),
            None => "the server process exited".to_string(),
        }
    } else if lower.contains("failed to spawn") {
        "the server command could not be started".to_string()
    } else if lower.contains("broken pipe") || lower.contains("eof") || lower.contains("closed") {
        "the server closed the connection".to_string()
    } else if lower.contains("vault") || lower.contains("keychain") || lower.contains("keyring") {
        "its stored credentials could not be read".to_string()
    } else if lower.contains("name resolution")
        || lower.contains("dns")
        || lower.contains("resolve")
    {
        "the server's address could not be resolved".to_string()
    } else if lower.contains("connection refused") {
        "the connection was refused".to_string()
    } else if lower.contains("certificate") || lower.contains("tls") {
        "the TLS connection failed".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "the server did not answer in time".to_string()
    } else {
        "the connection failed".to_string()
    };
    format!("{summary}; details are in the Toolport log")
}

/// Opaque handle on one pending server, so the gateway can cancel retries a
/// replaced router still owns without keeping that router alive.
#[derive(Clone)]
pub struct PendingHandle(Arc<PendingServer>);

impl PendingHandle {
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Stop retrying. An attempt already running finishes and is discarded.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        // Drop a result that connected but was never adopted, outside the lock.
        let ready = self.0.lock().ready.take();
        drop(ready);
    }
}

/// Cloneable so the dispatcher can hold the live router as a `Mutex<Arc<Router>>`,
/// clone the `Arc` for a request, and release the lock BEFORE the (possibly
/// long-blocking) downstream call or human-approval hold. Cloning shares the
/// `Arc<ServerSlot>` connections, so it never re-spawns a server.
#[derive(Default, Clone)]
pub struct Router {
    servers: Vec<Arc<ServerSlot>>,
    launch_specs: HashMap<String, Value>,
    /// Catalog coverage belongs to this indexed view, not the shared mutable slots.
    catalog_servers: HashSet<String>,
    /// Server id -> index into `servers`, so a call resolves its server without a
    /// linear scan and without locking any server to read its id.
    by_id: HashMap<String, usize>,
    /// Exposed (client-facing) tools, names already sanitized, in add order.
    /// All views share each immutable normalized definition.
    tools: SharedTools,
    /// Exposed tool name -> (server id, original downstream tool name).
    routes: HashMap<String, (String, String)>,
    /// Argument aliases compiled alongside the published tool definitions.
    schema_arguments: HashMap<String, Arc<crate::schema_compat::ArgumentMap>>,
    /// Routes kept across a guarded catalog collapse. Profile views recheck
    /// these under their own allowlist after indexing the shared live slots.
    restored_candidates: Vec<RestoredTool>,
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
    /// still called by name (e.g. via toolport_call_tool).
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
    /// False only for the never-built placeholder the gateway installs before its
    /// first build. `with_policy` (the constructor every real build uses) sets it,
    /// so a live router whose connects all failed is still a real prior decision
    /// and not a cold start (SBS-871).
    built: bool,
    /// Servers that failed to connect at build time, retried in the background
    /// and adopted into the catalog once they connect (REL-03).
    pending: Vec<Arc<PendingServer>>,
    /// Server ids in the order the build added or deferred them, so a server that
    /// joins late takes the same place (and collision suffixes) a cold build would.
    server_order: Vec<String>,
}

#[derive(Clone)]
struct RestoredTool {
    definition: Arc<ToolDefinition>,
    exposed: String,
    server: String,
    original: String,
    source_revision: u64,
    schema_arguments: Option<Arc<crate::schema_compat::ArgumentMap>>,
}

impl Router {
    pub fn new() -> Self {
        Router::default()
    }

    /// A router that enforces `policy` as servers are added.
    pub fn with_policy(policy: ToolPolicy) -> Self {
        Router {
            policy,
            built: true,
            ..Router::default()
        }
    }

    /// Whether this router came from a real build rather than the startup
    /// placeholder. A built router carries a quarantine decision even when it
    /// connected zero servers (SBS-871).
    pub fn is_built(&self) -> bool {
        self.built
    }

    /// Set the per-tool exposure overrides. Must be called BEFORE `add`/`refresh`, since
    /// they're applied while indexing each server's tools.
    pub fn set_overrides(&mut self, overrides: HashMap<String, HashMap<String, ToolOverride>>) {
        self.overrides = overrides;
    }

    /// Preview one server's aliases using the same collision and override rules as dispatch.
    pub fn server_tool_aliases(
        server_id: &str,
        tools: &[Value],
        overrides: HashMap<String, HashMap<String, ToolOverride>>,
    ) -> HashMap<String, String> {
        let mut router = Self::new();
        router.set_overrides(overrides);
        router.index_server(
            server_id,
            &tools.to_vec().into(),
            &[],
            &[],
            &[],
            false,
            &Mutex::default(),
        );
        router
            .routes
            .into_iter()
            .map(|(alias, (_, upstream))| (upstream, alias))
            .collect()
    }

    /// Resolve a server-detail selection without guessing a sanitized alias.
    pub fn exposed_tool_name(&self, server_id: &str, tool: &str) -> Option<&str> {
        self.routes.iter().find_map(|(alias, (server, upstream))| {
            (server == server_id && upstream == tool).then_some(alias.as_str())
        })
    }

    /// The real `(server id, original tool name)` an exposed name routes to, or `None` if
    /// unknown. Callers that need a call's provenance or server-scoping MUST use this rather
    /// than string-splitting the exposed name on `__` — that split silently mis-derives the
    /// server for a renamed tool (overrides) or any server id containing `__`.
    pub fn route_of(&self, exposed: &str) -> Option<(&str, &str)> {
        self.routes
            .get(exposed)
            .map(|(s, t)| (s.as_str(), t.as_str()))
    }

    /// Why a call to `exposed_name` cannot be routed.
    pub fn no_route_message(&self, exposed_name: &str) -> String {
        self.no_route_message_within(exposed_name, |_| true)
    }

    /// [`Router::no_route_message`] for a scoped caller: the alias hint only
    /// names a tool whose server `visible` accepts, so the message cannot reveal
    /// that a tool outside the caller's scope exists.
    pub fn no_route_message_within(
        &self,
        exposed_name: &str,
        visible: impl Fn(&str) -> bool,
    ) -> String {
        // Several client harnesses expose gateway tools to their model as
        // `mcp__<gateway-alias>__<tool>`; models then reuse that spelling inside
        // toolport_run_script and land here (observed with Codex, 2026-08-13).
        // Point at the name that will actually route instead of a dead end.
        let client_prefixed = exposed_name
            .strip_prefix("mcp__")
            .and_then(|rest| rest.split_once("__"))
            .map(|(_, tool)| tool)
            .filter(|candidate| {
                self.routes
                    .get(*candidate)
                    .is_some_and(|(server, _)| visible(server))
            });
        if let Some(real) = client_prefixed {
            return format!(
                "no route for tool '{exposed_name}'; that looks like a client-side alias - \
                 inside Toolport the tool is named '{real}', call that instead"
            );
        }
        match self.kick_pending(exposed_name, &visible) {
            Some(status) if status.needs_auth => format!(
                "no route for tool '{exposed_name}': server '{}' {}. Sign in, then try again.",
                status.id,
                status.describe()
            ),
            Some(status) => format!(
                "no route for tool '{exposed_name}': server '{}' has not connected yet and is \
                 {}. Try again shortly.",
                status.id,
                status.describe()
            ),
            None => format!("no route for tool '{exposed_name}'"),
        }
    }

    /// Re-index the same live downstream slots under one adapter profile's
    /// original-tool allowlists. The shared HTTP router can keep its fail-closed
    /// intersection while each daemon adapter sees only its own tool scope.
    pub fn with_tool_allow(&self, allow: HashMap<String, HashSet<String>>) -> Self {
        let mut view = self.clone();
        view.policy.allow = allow;
        view.rebuild_preserving_restored();
        view
    }

    /// The registry-derived half of the policy this router enforces.
    pub fn registry_policy(&self) -> RegistryPolicy {
        RegistryPolicy {
            servers: self.policy.servers.clone(),
            disabled: self.policy.disabled.clone(),
            allow: self.policy.allow.clone(),
            deny_destructive: self.policy.deny_destructive,
        }
    }

    /// Enforce a new registry-derived policy on the connections this router
    /// already holds, without reconnecting anything. The gateway calls this as soon
    /// as it reads a changed registry, so a switch like deny-destructive takes
    /// effect before a rebuild that can take seconds. Returns false when nothing
    /// changed. Quarantine state is left as it is.
    pub fn apply_registry_policy(&mut self, policy: RegistryPolicy) -> bool {
        if self.registry_policy() == policy {
            return false;
        }
        let ToolPolicy {
            quarantined,
            fail_closed_catalog,
            ..
        } = std::mem::take(&mut self.policy);
        self.policy = policy.with_quarantine(quarantined, fail_closed_catalog);
        self.rebuild_preserving_restored();
        true
    }

    /// The one policy decision every downstream dispatch goes through. It reads
    /// only the policy installed on this router, which the gateway republishes on
    /// every registry change. A name this router does not know is not a policy
    /// denial; routing reports it.
    pub fn authorize(&self, target: DispatchTarget<'_>) -> Result<(), String> {
        match target {
            DispatchTarget::Tool(exposed) => {
                if let Some(reason) = self.blocked.get(exposed) {
                    return Err(format!("tool '{exposed}' is {reason}"));
                }
                match self.routes.get(exposed) {
                    Some((server_id, _)) => self.authorize(DispatchTarget::Server(server_id)),
                    None => Ok(()),
                }
            }
            DispatchTarget::Server(server_id) => {
                if self.policy.allows_server(server_id) {
                    Ok(())
                } else {
                    Err(format!("server '{server_id}' is turned off"))
                }
            }
        }
    }

    /// Recheck a dispatch this router is about to make against the policy of a
    /// newer `live` router. A request keeps the router it started with, so without
    /// this a call admitted just before a policy change would still go out under
    /// the old policy. Tool-granular scope is skipped: daemon adapter views carry
    /// their own scope, which the live base router does not know.
    pub fn recheck_live_policy(
        &self,
        live: &Router,
        target: DispatchTarget<'_>,
    ) -> Result<(), String> {
        match target {
            DispatchTarget::Tool(exposed) => {
                let Some((server_id, orig)) = self.routes.get(exposed) else {
                    return Ok(());
                };
                // Only the destructive switch reads the definition; skip the scan
                // on the common path.
                let definition = if live.policy.deny_destructive {
                    self.tools
                        .iter()
                        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(exposed))
                        .unwrap_or(&Value::Null)
                } else {
                    &Value::Null
                };
                match live.policy.blocked_reason_unscoped(
                    exposed,
                    server_id,
                    orig,
                    ToolPolicyMetadata::from(definition),
                ) {
                    Some(reason) => Err(format!("tool '{exposed}' is {reason}")),
                    None => Ok(()),
                }
            }
            DispatchTarget::Server(_) => live.authorize(target),
        }
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
        tools: &SerializedTools,
        resources: &[Value],
        resource_templates: &[Value],
        prompts: &[Value],
        route_mcp_apps: bool,
        cache: &Mutex<HashMap<[u8; 32], Weak<ToolDefinition>>>,
    ) {
        // Allocate the exposed name regardless of policy so toggling one tool
        // never renames its siblings (their `_2` suffixes stay put), and in an
        // order that doesn't depend on how the server happened to list them.
        if !tools.is_empty() {
            self.catalog_servers.insert(server_id.to_string());
        }
        let tool_names = self.allocate_names(server_id, tools.names());
        for (idx, orig) in tools.names().into_iter().enumerate() {
            let Some(orig) = orig else {
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
            if let Some(reason) =
                self.policy
                    .blocked_reason(&exposed, server_id, orig, tools.policy_metadata(idx))
            {
                self.blocked.insert(exposed, reason.to_string());
                continue;
            }
            let key = content_digest(&(tools.digest(idx), &exposed, &ov_desc));
            let mut definitions = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let definition = definitions
                .get(&key)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let mut t = tools.materialize(idx);
                    if let Some(desc) = ov_desc {
                        t["description"] = json!(desc);
                    }
                    t["name"] = json!(exposed);
                    let mut compiled = None;
                    if let Some(schema) = t.get_mut("inputSchema") {
                        let arguments = crate::schema_compat::normalize(schema);
                        inline_refs(schema);
                        if !arguments.is_empty() {
                            compiled = Some(Arc::new(arguments));
                        }
                    }
                    let definition = Arc::new(ToolDefinition::with_arguments(t, compiled));
                    definitions.insert(key, Arc::downgrade(&definition));
                    definition
                });
            drop(definitions);
            if let Some(arguments) = &definition.arguments {
                self.schema_arguments
                    .insert(exposed.clone(), Arc::clone(arguments));
            }
            self.tools.0.push(definition);
            self.routes
                .insert(exposed, (server_id.to_string(), orig.to_string()));

            // UI-only resources are allowed to stay out of resources/list. The
            // tool linkage is therefore an authoritative route hint, subject to
            // the same first-writer collision rule as ordinary resources below.
            if route_mcp_apps {
                if let Some(uri) = tools.app_uri(idx) {
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
                        if !self
                            .resources
                            .iter()
                            .any(|listed| listed.get("uri").and_then(Value::as_str) == Some(uri))
                        {
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
        let tools = server.tools.clone();
        let resources = server.resources.clone();
        let templates = server.resource_templates.clone();
        let prompts = server.prompts.clone();
        let slot = Arc::new(ServerSlot::new(id.clone(), server, reconnect));
        self.index_server(
            &id,
            &tools,
            &resources,
            &templates,
            &prompts,
            route_mcp_apps,
            &slot.definitions,
        );
        self.catalog_servers.insert(id.clone());
        let idx = self.servers.len();
        if !self.server_order.contains(&id) {
            self.server_order.push(id.clone());
        }
        self.servers.push(slot);
        self.by_id.insert(id, idx);
    }

    pub fn add_supervised(
        &mut self,
        id: String,
        tools: Vec<Value>,
        connect: Connect,
        backoff: ReconnectBackoff,
        spec: Value,
    ) {
        let index = self.servers.len();
        self.by_id.insert(id.clone(), index);
        self.server_order.push(id.clone());
        self.launch_specs.insert(id.clone(), spec);
        self.servers.push(Arc::new(ServerSlot::supervised(
            id, tools, connect, backoff,
        )));
        self.rebuild_preserving_restored();
    }

    pub fn launch_spec(&self, id: &str) -> Option<&Value> {
        self.launch_specs.get(id)
    }

    pub fn reuse_supervisor(&mut self, previous: &Router, id: &str, spec: &Value) -> bool {
        if previous.launch_spec(id) != Some(spec) {
            return false;
        }
        let Some(slot) = previous.server_slot(id) else {
            return false;
        };
        self.by_id.insert(id.to_string(), self.servers.len());
        self.server_order.push(id.to_string());
        self.servers.push(slot.0);
        self.launch_specs.insert(id.to_string(), spec.clone());
        self.rebuild_preserving_restored();
        true
    }

    pub fn raw_catalogs(&self) -> Option<HashMap<String, SerializedTools>> {
        self.servers
            .iter()
            .map(|slot| Some((slot.id.clone(), slot.inner.try_lock().ok()?.tools.clone())))
            .collect()
    }

    /// Subscription ownership stays with the gateway table across replacements.
    pub fn set_subscription_use(&self, id: &str, used: Arc<dyn Fn() -> bool + Send + Sync>) {
        if let Some(slot) = self.by_id.get(id).map(|&index| &self.servers[index]) {
            if let Some(supervisor) = &slot.supervisor {
                supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .subscription_use = Some(used);
            }
        }
    }

    /// Retire every supervisor `next` replaced or removed, so a waiter on an
    /// obsolete startup fails fast instead of waiting out its deadline.
    pub fn retire_replaced_supervisors(&self, next: &Router) {
        for slot in &self.servers {
            if next
                .server_slot(&slot.id)
                .is_none_or(|current| !Arc::ptr_eq(slot, &current.0))
            {
                slot.retire();
            }
        }
    }

    /// Discovery only starts servers without a catalog, in the caller's scope.
    pub fn any_starting(&self, visible: impl Fn(&str) -> bool) -> bool {
        self.servers.iter().any(|slot| {
            visible(&slot.id)
                && slot.supervisor.as_ref().is_some_and(|s| {
                    s.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .state
                        == SupervisorState::Starting
                })
        })
    }

    /// Demand starts a stopped server or a recovery whose retry is due.
    pub fn prepare_lazy_use(&self, id: &str) -> bool {
        if !self.policy.allows_server(id) {
            return false;
        }
        let Some(&index) = self.by_id.get(id) else {
            return false;
        };
        let slot = &self.servers[index];
        slot.start(true);
        self.lazy_starting(id)
    }

    pub fn wait_for_server(
        &self,
        id: &str,
        cancel: Option<&CancelContext>,
        continuation: bool,
    ) -> Result<(), String> {
        self.authorized_slot(id)?
            .wait_for_start(cancel, continuation)
    }

    pub fn lazy_starting(&self, id: &str) -> bool {
        self.by_id.get(id).is_some_and(|&index| {
            self.servers[index].supervisor.as_ref().is_some_and(|s| {
                let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.state == SupervisorState::Starting
            })
        })
    }

    /// Starts only servers whose full catalog was never loaded. An idle stop
    /// keeps prompts and resources, so later lists do not restart the server.
    pub fn demand_servers(&self, visible: impl Fn(&str) -> bool) {
        for slot in &self.servers {
            if visible(&slot.id) && !slot.catalog_complete() {
                slot.start(true);
            }
        }
    }

    /// A visible launch has no valid catalog in this indexed view.
    pub fn any_missing_catalog(&self, visible: impl Fn(&str) -> bool) -> bool {
        self.servers
            .iter()
            .any(|slot| visible(&slot.id) && !self.catalog_servers.contains(&slot.id))
    }

    /// Whether a visible server is starting to load its first full catalog.
    pub fn any_discovering(&self, visible: impl Fn(&str) -> bool) -> bool {
        self.servers.iter().any(|slot| {
            visible(&slot.id)
                && slot.supervisor.as_ref().is_some_and(|s| {
                    let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.state == SupervisorState::Starting && !state.ever_ready
                })
        })
    }

    /// Whether a visible server connected for its first catalog and now only
    /// waits for the gateway to publish it.
    pub fn any_publishing_first_catalog(&self, visible: impl Fn(&str) -> bool) -> bool {
        self.servers.iter().any(|slot| {
            visible(&slot.id)
                && slot.supervisor.as_ref().is_some_and(|s| {
                    let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    !state.ever_ready && (state.ready.is_some() || state.publishing)
                })
        })
    }

    pub fn discover_uncached(&self, visible: impl Fn(&str) -> bool) {
        for slot in &self.servers {
            if visible(&slot.id)
                && !slot.catalog_complete()
                && slot
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .tools
                    .is_empty()
            {
                slot.start(true);
            }
        }
    }

    /// Called only after the fresh catalog passed the gateway integrity gate.
    pub fn activate_supervisors(&self) {
        self.activate_supervisors_for(|_| true);
    }

    /// [`Router::activate_supervisors`] limited to the servers this view owns.
    pub fn activate_supervisors_for(&self, owned: impl Fn(&str) -> bool) {
        for slot in &self.servers {
            if !owned(&slot.id) {
                continue;
            }
            if let Some(supervisor) = &slot.supervisor {
                let mut state = supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.publishing {
                    state.publishing = false;
                    state.state = SupervisorState::Ready;
                    state.ever_ready = true;
                    state.failures = 0;
                    state.last_error.clear();
                }
            }
        }
    }

    pub fn maintain_supervisors(&self) {
        self.maintain_supervisors_at(Instant::now());
    }

    /// [`Router::maintain_supervisors`] as of `now`, for idle shutdown checks.
    pub fn maintain_supervisors_at(&self, now: Instant) {
        for slot in &self.servers {
            slot.maintain(now);
        }
    }

    /// Record a server whose first connect failed. It is retried in the background
    /// with capped exponential backoff (auth failures wait for new credentials, which
    /// arrive as a registry rebuild), and [`Router::adopt_ready_reconnects`] moves it
    /// into the catalog once a retry connects.
    pub fn add_pending(
        &mut self,
        id: String,
        failure: ConnectFailure,
        connect: Connect,
        backoff: ReconnectBackoff,
    ) {
        if self.by_id.contains_key(&id) || self.pending.iter().any(|p| p.id == id) {
            return;
        }
        if !self.server_order.contains(&id) {
            self.server_order.push(id.clone());
        }
        let state = PendingState::new(failure, &backoff, Instant::now(), reconnect_jitter());
        self.pending.push(Arc::new(PendingServer {
            id,
            connect,
            backoff,
            state: Mutex::new(state),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        }));
    }

    /// Whether any server is still waiting to connect in the background.
    pub fn has_pending(&self) -> bool {
        self.pending.iter().any(|p| !p.is_cancelled())
            || self.servers.iter().any(|slot| {
                slot.supervisor.as_ref().is_some_and(|s| {
                    s.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .state
                        != SupervisorState::Ready
                })
            })
    }

    /// Every server still waiting to connect, with its retry state.
    pub fn pending_statuses(&self) -> Vec<PendingStatus> {
        let now = Instant::now();
        self.pending
            .iter()
            .map(|p| p.status(now))
            .chain(self.servers.iter().filter_map(|slot| {
                let status = slot.status()?;
                let state = slot
                    .supervisor
                    .as_ref()?
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state;
                (state != SupervisorState::Ready && state != SupervisorState::Stopped)
                    .then_some(status)
            }))
            .collect()
    }

    pub fn pending_handles(&self) -> Vec<PendingHandle> {
        self.pending
            .iter()
            .map(|p| PendingHandle(Arc::clone(p)))
            .collect()
    }

    /// Start a background attempt for every pending server whose backoff elapsed.
    /// Returns how many started.
    pub fn start_due_reconnects(&self) -> usize {
        let now = Instant::now();
        self.pending
            .iter()
            .filter(|p| p.try_start(|state| state.due(now)))
            .count()
    }

    /// Whether a retry connected and is waiting for [`Router::adopt_ready_reconnects`].
    pub fn has_ready_reconnects(&self) -> bool {
        self.has_ready_reconnects_for(|_| true)
    }

    /// [`Router::has_ready_reconnects`] limited to the servers this view owns.
    pub fn has_ready_reconnects_for(&self, owned: impl Fn(&str) -> bool) -> bool {
        self.pending
            .iter()
            .any(|p| owned(&p.id) && !p.is_cancelled() && p.lock().ready.is_some())
            || self.servers.iter().any(|slot| {
                owned(&slot.id)
                    && slot.supervisor.as_ref().is_some_and(|s| {
                        let state = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.ready.is_some() || state.publishing
                    })
            })
    }

    /// Move every pending server whose retry connected into a live slot, in the
    /// position a cold build would have given it. Returns the adopted ids.
    pub fn adopt_ready_reconnects(&mut self) -> Vec<String> {
        self.adopt_ready_reconnects_for(|_| true)
    }

    /// Adopt only the servers this view owns. A rooted view shares the base
    /// router's slots, and those must pass the base integrity gate instead.
    pub fn adopt_ready_reconnects_for(&mut self, owned: impl Fn(&str) -> bool) -> Vec<String> {
        let mut adopted = Vec::new();
        for slot in &self.servers {
            if !owned(&slot.id) {
                continue;
            }
            if let Some(supervisor) = &slot.supervisor {
                let mut state = supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(server) = state.ready.take() {
                    *slot
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = server;
                    state.publishing = true;
                    state.last_use = Instant::now();
                    slot.generation.fetch_add(1, Ordering::AcqRel);
                    slot.tool_revision.fetch_add(1, Ordering::AcqRel);
                    slot.breaker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_success();
                    adopted.push(slot.id.clone());
                } else if state.publishing {
                    // A newer rooted view may share an adopted, unpublished slot.
                    adopted.push(slot.id.clone());
                }
            }
        }
        let mut still_pending = Vec::new();
        for pending in std::mem::take(&mut self.pending) {
            let ready = if pending.is_cancelled() || !owned(&pending.id) {
                None
            } else {
                pending.lock().ready.take()
            };
            let Some(server) = ready else {
                still_pending.push(pending);
                continue;
            };
            // Older router snapshots share this entry; stop them from retrying it.
            pending.cancelled.store(true, Ordering::SeqCst);
            let connect = Arc::clone(&pending.connect);
            let reconnect: Reconnect = Box::new(move || connect().ok());
            self.restored_candidates
                .retain(|candidate| candidate.server != pending.id);
            self.servers.push(Arc::new(ServerSlot::new(
                pending.id.clone(),
                server,
                Some(reconnect),
            )));
            adopted.push(pending.id.clone());
        }
        self.pending = still_pending;
        if adopted.is_empty() {
            return adopted;
        }
        let position = |id: &str| {
            self.server_order
                .iter()
                .position(|known| known == id)
                .unwrap_or(usize::MAX)
        };
        let mut servers = std::mem::take(&mut self.servers);
        servers.sort_by_key(|slot| position(&slot.id));
        self.by_id = servers
            .iter()
            .enumerate()
            .map(|(index, slot)| (slot.id.clone(), index))
            .collect();
        self.servers = servers;
        self.rebuild_preserving_restored();
        adopted
    }

    /// The pending server a call or search names, if `visible` admits it: a server
    /// id or prefix, or an exposed tool name under that prefix. Starts an attempt
    /// now when the backoff allows one, so a user waiting on the server does not
    /// wait out a long schedule.
    pub fn kick_pending(
        &self,
        name: &str,
        visible: impl Fn(&str) -> bool,
    ) -> Option<PendingStatus> {
        let wanted = name.trim().to_lowercase();
        if wanted.is_empty() {
            return None;
        }
        if let Some(slot) = self.servers.iter().find(|slot| {
            let prefix = sanitize_segment(&slot.id).to_lowercase();
            visible(&slot.id)
                && (slot.id.to_lowercase() == wanted
                    || prefix == wanted
                    || wanted.starts_with(&format!("{prefix}__")))
        }) {
            slot.start(true);
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                return slot.status();
            }
        }
        let pending = self.pending.iter().find(|p| {
            let prefix = sanitize_segment(&p.id).to_lowercase();
            !p.is_cancelled()
                && visible(&p.id)
                && (p.id.to_lowercase() == wanted
                    || prefix == wanted
                    || wanted.starts_with(&format!("{prefix}__")))
        })?;
        let now = Instant::now();
        let backoff = pending.backoff;
        pending.try_start(|state| state.kickable(&backoff, now));
        Some(pending.status(Instant::now()))
    }

    /// Build a view with one root-specific launch added or replaced. All other
    /// slots stay shared with the source router. Re-index from the selected
    /// slots so catalogs cannot leak across roots.
    pub fn with_server_launch(
        &self,
        server: DownstreamServer,
        reconnect: Option<Reconnect>,
    ) -> Self {
        let mut view = self.clone();
        if let Some(&index) = view.by_id.get(&server.id) {
            view.restored_candidates
                .retain(|candidate| candidate.server != server.id);
            view.servers[index] = Arc::new(ServerSlot::new(server.id.clone(), server, reconnect));
            view.rebuild_preserving_restored();
        } else {
            view.add_with_reconnect(server, reconnect);
        }
        view
    }

    pub fn with_supervised_launch(
        &self,
        server: DownstreamServer,
        connect: Connect,
        backoff: ReconnectBackoff,
    ) -> Self {
        let mut view = self.clone();
        let id = server.id.clone();
        let slot = ServerSlot::supervised(id.clone(), Vec::new(), connect, backoff);
        *slot
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = server;
        {
            let mut state = slot
                .supervisor
                .as_ref()
                .unwrap()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.publishing = true;
            state.state = SupervisorState::Starting;
        }
        if let Some(index) = view.by_id.get(&id) {
            view.servers[*index] = Arc::new(slot);
        } else {
            view.by_id.insert(id, view.servers.len());
            view.servers.push(Arc::new(slot));
        }
        view.rebuild_preserving_restored();
        view
    }

    /// Compose a launch already owned by another view into this one. The source
    /// and result share the exact slot, so selecting the same LaunchKey never
    /// starts a second child or splits its reconnect state.
    pub fn with_server_slot_from(&self, source: &Router, server_id: &str) -> Option<Self> {
        Some(self.with_shared_server_slot(&source.server_slot(server_id)?))
    }

    pub fn server_slot(&self, server_id: &str) -> Option<SharedServerSlot> {
        let index = *self.by_id.get(server_id)?;
        Some(SharedServerSlot(Arc::clone(self.servers.get(index)?)))
    }

    fn tool_revision(&self, server_id: &str) -> Option<u64> {
        self.by_id
            .get(server_id)
            .and_then(|index| self.servers.get(*index))
            .map(|slot| slot.tool_revision.load(Ordering::Acquire))
    }

    pub fn with_shared_server_slot(&self, slot: &SharedServerSlot) -> Self {
        let server_id = &slot.0.id;
        let mut view = self.clone();
        if let Some(&index) = view.by_id.get(server_id) {
            if !Arc::ptr_eq(&view.servers[index], &slot.0) {
                view.restored_candidates
                    .retain(|candidate| candidate.server != server_id.as_str());
            }
            view.servers[index] = Arc::clone(&slot.0);
        } else {
            let index = view.servers.len();
            view.servers.push(Arc::clone(&slot.0));
            view.by_id.insert(server_id.to_string(), index);
        }
        view.rebuild_preserving_restored();
        view
    }

    /// Re-index a view after one of its shared downstream slots refreshed its
    /// catalog. The view keeps its own policy and routes while sharing launches.
    pub fn reindexed(&self) -> Self {
        let mut view = self.clone();
        view.rebuild_preserving_restored();
        view
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
    /// Cross-server collisions *can* arise: personal `team-slack` and team
    /// `team_slack` both sanitize to `team_slack` (SBS-866). The `_2` suffix still
    /// keeps exposed names unique; authorization must use the raw registry id,
    /// not this prefix.
    fn allocate_exposed_names(&mut self, server_id: &str, items: &[Value]) -> Vec<Option<String>> {
        self.allocate_names(
            server_id,
            items
                .iter()
                .map(|item| item.get("name").and_then(Value::as_str))
                .collect(),
        )
    }

    fn allocate_names(&mut self, server_id: &str, names: Vec<Option<&str>>) -> Vec<Option<String>> {
        let mut order: Vec<usize> = (0..names.len()).collect();
        order.sort_by(|&a, &b| names[a].cmp(&names[b]).then(a.cmp(&b)));
        let mut out = vec![None; names.len()];
        for i in order {
            if let Some(orig) = names[i] {
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
    /// True when the live policy blocks this exposed tool.
    ///
    /// The persisted catalog cache is a snapshot taken under whatever policy was
    /// in force when it was written, and `tools/list` prefers it for an instant
    /// answer. Without this the cache keeps advertising a tool that
    /// `route_call` will refuse, which is the display half of the hazard
    /// `adopt_restored_routes` already guards the routing half of.
    pub fn is_blocked(&self, exposed: &str) -> bool {
        self.blocked.contains_key(exposed)
    }

    pub fn shared_tools(&self) -> SharedTools {
        let mut tools = self.tools.clone();
        tools.sort();
        tools
    }

    pub fn aggregated_tools(&self) -> Vec<Value> {
        self.shared_tools().to_vec()
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

    /// Capture app visibility once: reconnects can replace shared slot
    /// capabilities before the next indexed router is published.
    pub fn mcp_app_html_visibility(
        &self,
        include_server: impl Fn(&str) -> bool,
    ) -> (bool, Vec<String>) {
        let mut declaration = None;
        let mut conflict = false;
        let mut servers = Vec::new();
        for slot in &self.servers {
            if !include_server(&slot.id) {
                continue;
            }
            let server = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(settings) = server.extensions().get(MCP_APPS_EXTENSION) {
                if declaration.as_ref().is_some_and(|old| old != settings) {
                    conflict = true;
                } else if declaration.is_none() {
                    declaration = Some(settings.clone());
                }
            }
            if supports_mcp_app_html(server.extensions()) {
                servers.push(slot.id.clone());
            }
        }
        servers.sort();
        let relays = !conflict
            && declaration.as_ref().is_some_and(|settings| {
                settings
                    .get("mimeTypes")
                    .and_then(Value::as_array)
                    .is_some_and(|types| types.iter().any(|mime| mime == MCP_APP_HTML_MIME))
            });
        (relays, servers)
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
            if server
                .resource_cache_hint()
                .is_some_and(|hint| hint.needs_refresh())
                || server
                    .resource_template_cache_hint()
                    .is_some_and(|hint| hint.needs_refresh())
            {
                kinds |= crate::downstream::change::RESOURCES;
            }
            if server
                .prompt_cache_hint()
                .is_some_and(|hint| hint.needs_refresh())
            {
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
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            if slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_tools()
            {
                slot.tool_revision.fetch_add(1, Ordering::AcqRel);
            }
        }
        self.rebuild_aggregation();
    }

    pub fn refresh_stale_tools(&mut self) {
        for slot in &self.servers {
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            if slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_tools_if_stale()
            {
                slot.tool_revision.fetch_add(1, Ordering::AcqRel);
            }
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
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_resources();
        }
        self.rebuild_preserving_restored();
    }

    pub fn refresh_stale_resources(&mut self) {
        for slot in &self.servers {
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_resources_if_stale();
        }
        self.rebuild_preserving_restored();
    }

    /// Re-query every live server's prompt list (a downstream announced a
    /// `prompts/list_changed`) and rebuild the exposed aggregation in place.
    /// Mirrors [`refresh_tools`].
    pub fn refresh_prompts(&mut self) {
        for slot in &self.servers {
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_prompts();
        }
        self.rebuild_preserving_restored();
    }

    pub fn refresh_stale_prompts(&mut self) {
        for slot in &self.servers {
            if slot.supervisor.as_ref().is_some_and(|s| {
                s.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state
                    != SupervisorState::Ready
            }) {
                continue;
            }
            slot.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .refresh_prompts_if_stale();
        }
        self.rebuild_preserving_restored();
    }

    /// Forward one JSON-RPC notification only to downstream servers visible to
    /// this upstream session. `None` is the standalone stdio caller's full set.
    pub fn notify_downstreams_in_scope(
        &self,
        method: &str,
        params: Value,
        allowed: Option<&HashSet<String>>,
    ) {
        for slot in &self.servers {
            if allowed.is_some_and(|scope| !scope.contains(&slot.id)) {
                continue;
            }
            if let Ok(mut ds) = slot.inner.lock() {
                let _ = ds.notify_downstream(method, params.clone());
            }
        }
    }

    /// Replace the quarantine set and re-derive the exposed aggregation so newly
    /// quarantined tools are hidden (or re-approved ones restored) without re-querying
    /// downstream. Cheap: it only re-applies the policy to the cached tool lists.
    ///
    /// Deliberately does NOT lift a fail-closed catalog (SBS-871): every caller that
    /// reaches here on a store error would otherwise re-expose the whole catalog while
    /// the store is still unreadable. Use [`Self::requarantine_from_store`] when the
    /// set came from a successful read.
    pub fn requarantine(&mut self, quarantined: BTreeSet<String>) {
        self.policy.quarantined = quarantined;
        self.rebuild_preserving_restored();
    }

    /// Install a quarantine set that came from a SUCCESSFUL store read, lifting the
    /// fail-closed hide (SBS-871). This is the only way the catalog comes back: the
    /// store answered, so the set is known and enforcement can resume normally.
    pub fn requarantine_from_store(&mut self, quarantined: BTreeSet<String>) {
        self.policy.fail_closed_catalog = false;
        self.requarantine(quarantined);
    }

    /// Hide a derived catalog when its integrity store cannot be trusted.
    pub fn fail_closed_catalog(&mut self) {
        self.policy.fail_closed_catalog = true;
        self.rebuild_preserving_restored();
    }

    /// True when this router is hiding the whole catalog because the quarantine
    /// store could not be read and there was no prior live set to keep (SBS-871).
    pub fn catalog_fail_closed(&self) -> bool {
        self.policy.fail_closed_catalog
    }

    /// The quarantine set this router is currently enforcing. Lets a caller diff the
    /// live set against the persisted one and skip `requarantine` (and the client
    /// `list_changed` that follows it) when nothing actually changed.
    ///
    /// NOT the whole enforcement picture: while [`Self::catalog_fail_closed`] is true
    /// every tool is hidden even though this set can be empty, so a caller diffing
    /// live against persisted MUST check `catalog_fail_closed()` too or it will read
    /// "blocked everything" as "nothing blocked" (SBS-871).
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
    /// route from a live slot are adopted. Previously restored routes keep
    /// their candidates across repeated adoption. Policy is
    /// re-evaluated per restored tool before adoption: the rebuilt router only
    /// indexed the degraded connect, so tools quarantined or disabled since the
    /// previous build are absent from its `blocked` map and must not slip back
    /// in through the guarded catalog (which still carries them from the cache).
    pub fn adopt_restored_routes(&mut self, previous: &Router, catalog: &dyn ToolCatalog) {
        // Rebuilds have already reapplied these routes. Keep their provenance,
        // including tools visible only in a profile, across repeated publication.
        let mut candidates = std::mem::take(&mut self.restored_candidates);
        let mut seen: HashSet<String> =
            candidates.iter().map(|tool| tool.exposed.clone()).collect();
        for tool in catalog.shared().0 {
            let Some(exposed) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            // Healthy definitions remain available from the live slots. Retaining
            // them as restoration candidates duplicates every schema on reconnect.
            if self.routes.contains_key(exposed) && !seen.contains(exposed) {
                continue;
            }
            // The previous router indexed the same exposed name; reuse its
            // (server, original) pair verbatim instead of re-deriving it.
            let Some((server_id, original)) = previous.route_of(exposed) else {
                continue;
            };
            if seen.insert(exposed.to_string()) {
                candidates.push(RestoredTool {
                    definition: tool.clone(),
                    exposed: exposed.to_string(),
                    server: server_id.to_string(),
                    original: original.to_string(),
                    source_revision: self.tool_revision(server_id).unwrap_or(0),
                    schema_arguments: previous.schema_arguments.get(exposed).cloned(),
                });
            }
        }

        // The host's guarded catalog can omit tools visible only to one profile:
        // its base allowlist is the intersection. A severe raw slot shrink still
        // needs those previous definitions for that profile's view. The next
        // rebuild sees the degraded slot as its previous raw catalog and accepts
        // a genuine persistent shrink, matching the host's confirm-then-accept
        // guard rather than pinning stale definitions forever.
        let mut collapsed = HashSet::new();
        for slot in &self.servers {
            let Some(old_index) = previous.by_id.get(&slot.id) else {
                continue;
            };
            let old_count = previous.servers[*old_index]
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tools
                .len();
            let new_count = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tools
                .len();
            if new_count > 0 && is_implausible_shrink(old_count, new_count) {
                collapsed.insert(slot.id.clone());
            }
        }
        if !collapsed.is_empty() {
            let unrestricted = previous.with_tool_allow(HashMap::new());
            for tool in unrestricted.shared_tools().0 {
                let Some(exposed) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some((server_id, original)) = unrestricted.route_of(exposed) else {
                    continue;
                };
                if collapsed.contains(server_id)
                    && (!self.routes.contains_key(exposed) || seen.contains(exposed))
                    && seen.insert(exposed.to_string())
                {
                    candidates.push(RestoredTool {
                        definition: tool.clone(),
                        exposed: exposed.to_string(),
                        server: server_id.to_string(),
                        original: original.to_string(),
                        source_revision: self.tool_revision(server_id).unwrap_or(0),
                        schema_arguments: unrestricted.schema_arguments.get(exposed).cloned(),
                    });
                }
            }
        }
        self.restored_candidates = candidates;
        self.apply_restored_candidates();
    }

    fn apply_restored_candidates(&mut self) {
        for candidate in &self.restored_candidates {
            if self.routes.contains_key(&candidate.exposed)
                || self.blocked.contains_key(&candidate.exposed)
                || !self.by_id.contains_key(&candidate.server)
            {
                continue;
            }
            // Re-evaluate every candidate under this router's current policy.
            // A profile can allow a tool the host intersection hid, while a new
            // quarantine must still block it (review on #717).
            if let Some(reason) = self.policy.blocked_reason(
                &candidate.exposed,
                &candidate.server,
                &candidate.original,
                ToolPolicyMetadata::from(&**candidate.definition),
            ) {
                self.blocked
                    .insert(candidate.exposed.clone(), reason.to_string());
                continue;
            }
            self.routes.insert(
                candidate.exposed.clone(),
                (candidate.server.clone(), candidate.original.clone()),
            );
            if let Some(arguments) = &candidate.schema_arguments {
                self.schema_arguments
                    .insert(candidate.exposed.clone(), Arc::clone(arguments));
            }
            self.tools.0.push(candidate.definition.clone());
            self.seen.insert(candidate.exposed.clone());
        }
    }

    fn rebuild_preserving_restored(&mut self) {
        let restored: Vec<_> = self
            .restored_candidates
            .iter()
            .filter(|candidate| {
                self.tool_revision(&candidate.server) == Some(candidate.source_revision)
            })
            .cloned()
            .collect();
        self.rebuild_aggregation_with_reserved(&restored);
        self.restored_candidates = restored;
        self.apply_restored_candidates();
    }

    /// Re-derive the exposed tool/resource/template/prompt aggregation from the
    /// current servers' (possibly refreshed) lists, in the original add order so
    /// exposed names and their `_2` collision suffixes stay stable. The server
    /// set itself is unchanged, so `servers` and `by_id` are kept.
    fn rebuild_aggregation(&mut self) {
        self.rebuild_aggregation_with_reserved(&[]);
    }

    fn rebuild_aggregation_with_reserved(&mut self, restored: &[RestoredTool]) {
        self.restored_candidates.clear();
        self.tools = SharedTools::default();
        self.catalog_servers.clear();
        self.routes.clear();
        self.schema_arguments.clear();
        self.seen.clear();
        // A restored route keeps its exposed name until a fresh tool catalog
        // confirms its removal. Reserve that name before indexing new slots,
        // otherwise a later colliding tool can silently inherit the old route.
        for candidate in restored {
            let still_advertised = self
                .by_id
                .get(&candidate.server)
                .and_then(|index| self.servers.get(*index))
                .is_some_and(|slot| {
                    slot.inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .tools
                        .contains_name(&candidate.original)
                });
            if !still_advertised {
                self.seen.insert(candidate.exposed.clone());
            }
        }
        self.blocked.clear();
        self.resources.clear();
        self.resource_routes.clear();
        self.resource_templates.clear();
        self.template_routes.clear();
        self.prompts.clear();
        self.prompt_routes.clear();
        // Snapshot catalogs under each slot lock, then normalize/index without
        // blocking dispatch or raw_catalogs' nonblocking persistence snapshot.
        let slots: Vec<Arc<ServerSlot>> = self.servers.clone();
        for slot in &slots {
            if slot.catalog_complete() {
                self.catalog_servers.insert(slot.id.clone());
            }
            let (tools, resources, templates, prompts, route_mcp_apps) = {
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
                &templates,
                &prompts,
                route_mcp_apps,
                &slot.definitions,
            );
            slot.definitions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|_, value| value.strong_count() > 0);
        }
    }

    /// [`Self::slot_for`] behind [`Self::authorize`], for every dispatch except
    /// cleanup (unsubscribe, task cancel), which must still reach a server that was
    /// turned off.
    fn authorized_slot(&self, server_id: &str) -> Result<Arc<ServerSlot>, String> {
        self.authorize(DispatchTarget::Server(server_id))?;
        self.slot_for(server_id)
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

    /// Retry wrapper around one dispatch. On a multiplexing transport the
    /// closure runs on a call handle without the per-server lock, so concurrent
    /// calls to the same server proceed in parallel; otherwise it runs under the
    /// lock. Either way the lock is never held during a backoff sleep.
    fn call_with_retry<T, F>(
        &self,
        slot: &Arc<ServerSlot>,
        cancel: Option<&CancelContext>,
        dispatch_cancelled_continuation: bool,
        replay_policy: ReplayPolicy,
        access: SlotAccess,
        f: F,
    ) -> Result<T, String>
    where
        F: FnMut(&mut dyn ServerDispatch) -> Result<T, TransportError>,
    {
        self.call_with_retry_typed(
            slot,
            cancel,
            dispatch_cancelled_continuation,
            replay_policy,
            access,
            f,
        )
        .map_err(|failure| failure.to_string())
    }

    fn call_with_retry_typed<T, F>(
        &self,
        slot: &Arc<ServerSlot>,
        cancel: Option<&CancelContext>,
        dispatch_cancelled_continuation: bool,
        replay_policy: ReplayPolicy,
        access: SlotAccess,
        mut f: F,
    ) -> Result<T, CallFailure>
    where
        F: FnMut(&mut dyn ServerDispatch) -> Result<T, TransportError>,
    {
        if !dispatch_cancelled_continuation && cancel.is_some_and(CancelContext::is_cancelled) {
            return Err(CallFailure::new(
                CallFailureKind::Cancelled,
                "request cancelled before downstream attempt",
            ));
        }
        slot.wait_for_start(cancel, dispatch_cancelled_continuation)
            .map_err(|detail| {
                CallFailure::new(
                    if slot
                        .status()
                        .is_some_and(|status| status.needs_auth)
                    {
                        CallFailureKind::Auth {
                            target: crate::call_failure::AuthTarget::Endpoint,
                        }
                    } else {
                        CallFailureKind::Unavailable { after_send: false }
                    },
                    detail,
                )
            })?;
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
                ).into());
            }
            // Cooldown elapsed but the failure streak is still at/over threshold: this
            // call is the half-open probe of a tripped breaker.
            breaker.consecutive_failures >= BREAKER_FAILURE_THRESHOLD
        };
        let (reset, generation, successes) = {
            let server = slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                server.connection_reset_reason(),
                slot.generation.load(Ordering::Acquire),
                slot.successes.load(Ordering::Acquire),
            )
        };
        if let Some(reason) = reset {
            // This caller has not dispatched. Retired calls receive FrameRejected and
            // never reach recovery; only this new operation uses the fresh stream.
            if let Some(result) =
                self.reconnect_and_retry(slot, cancel, None, generation, successes, access, &mut f)
            {
                return result;
            }
            // A failed reconnect is still a health failure. Otherwise a
            // factory that cannot rebuild this rejected stream would storm too.
            slot.breaker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_failure(Instant::now());
            return Err(CallFailure::new(
                CallFailureKind::Unavailable { after_send: false },
                format!("{reason}; downstream reconnect required"),
            ));
        }
        let mut attempt = 0u32;
        loop {
            if !dispatch_cancelled_continuation && cancel.is_some_and(CancelContext::is_cancelled) {
                return Err(CallFailure::new(
                    CallFailureKind::Cancelled,
                    "request cancelled before downstream attempt",
                ));
            }
            let generation = slot.generation.load(Ordering::Acquire);
            let successes = slot.successes.load(Ordering::Acquire);
            let (result, started) = Self::attempt(slot, access, cancel, &mut f);
            match result {
                Ok(v) => {
                    slot.successes.fetch_add(1, Ordering::AcqRel);
                    if let Some(supervisor) = &slot.supervisor {
                        supervisor
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failures = 0;
                    }
                    slot.breaker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_success();
                    return Ok(v);
                }
                Err(
                    TransportError::Retry {
                        retry_after,
                        message,
                    }
                    | TransportError::RateLimited {
                        retry_after,
                        message,
                    },
                ) if attempt < HTTP_MAX_RETRIES => {
                    let wait = retry_wait(retry_after, attempt);
                    eprintln!("toolport: retrying downstream call after {wait:?}: {message}");
                    wait_for_retry_or_cancel(wait, cancel).map_err(|error| error.call_failure())?;
                    attempt += 1;
                }
                Err(e) => {
                    if matches!(e, TransportError::FrameRejected(_)) {
                        // Retire every owned call without replay, including read-only
                        // calls. Concurrent failures of one stream count once.
                        slot.breaker
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .record_concurrent_failure(started, Instant::now());
                        return Err(e.call_failure());
                    }
                    // Only a health failure (timeout / dead connection / exhausted
                    // retries) counts toward the breaker; a normal error response does
                    // not disable the server.
                    if e.is_health_failure() {
                        if slot.degrade(generation, successes, &e.to_string(), is_probe) {
                            slot.breaker
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .record_concurrent_failure(started, Instant::now());
                            let mut message = slot.unavailable();
                            if replay_policy.uncertain_failure(&e).is_some() {
                                message.push_str(". The previous operation may have completed; check before retrying it.");
                            }
                            return Err(CallFailure::new(e.call_failure().kind, message));
                        }
                        // The server has now failed for a full cooldown and the probe
                        // confirms it's still down. Re-spawn the connection once and
                        // replay only when safe: an uncertain mutation instead keeps
                        // its original error and recovers transport for future calls.
                        // This recovers a crashed stdio child or a dropped remote
                        // that the plain breaker would otherwise fast-fail forever (its
                        // self-heal only fires when EVERY server is dead). Gated on the
                        // probe so a live server is never re-spawned on a transient blip,
                        // and a live multiplexed connection only when no other call
                        // succeeded meanwhile or is still running on it: re-spawning
                        // ends every call in flight. The last of them to fail does it.
                        if is_probe && Self::respawn_after_failure(slot, successes) {
                            if cancel.is_some_and(CancelContext::is_cancelled) {
                                return Err(CallFailure::new(
                                    CallFailureKind::Cancelled,
                                    "request cancelled before downstream reconnect",
                                ));
                            }
                            if let Some(v) = self.reconnect_and_retry(
                                slot,
                                cancel,
                                replay_policy.uncertain_failure(&e),
                                generation,
                                successes,
                                access,
                                &mut f,
                            ) {
                                return v;
                            }
                        }
                        slot.breaker
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .record_concurrent_failure(started, Instant::now());
                    }
                    return Err(e.call_failure());
                }
            }
        }
    }

    /// Whether a failed probe should re-spawn the connection. A connection that
    /// serves one call at a time has nothing else to interrupt; a multiplexed
    /// one is re-spawned when it closed, or when it is quiescent (it is wedged,
    /// not just slow on one call while others still run).
    fn respawn_after_failure(slot: &ServerSlot, successes: u64) -> bool {
        let server = slot
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match server.connection_closed() {
            None | Some(true) => true,
            Some(false) => slot.quiescent_since(&server, successes),
        }
    }

    /// Run one dispatch attempt. The slot lock is held only to fetch a call
    /// handle; without one (a transport that cannot multiplex, or a state
    /// change) the closure runs under the lock as before. Also returns when a
    /// call on a handle began, for [`Breaker::record_concurrent_failure`].
    fn attempt<T, F>(
        slot: &ServerSlot,
        access: SlotAccess,
        cancel: Option<&CancelContext>,
        f: &mut F,
    ) -> (Result<T, TransportError>, Option<Instant>)
    where
        F: FnMut(&mut dyn ServerDispatch) -> Result<T, TransportError>,
    {
        let mut lifecycle = slot
            .supervisor
            .as_ref()
            .map(|s| s.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        if lifecycle
            .as_ref()
            .is_some_and(|s| s.state != SupervisorState::Ready)
        {
            drop(lifecycle);
            return (Err(TransportError::Busy(slot.unavailable())), None);
        }
        if let Some(state) = lifecycle.as_mut() {
            state.last_use = Instant::now();
        }
        // Count queued serial calls before releasing the lifecycle lock. The
        // watcher can now inspect other servers while this call waits for inner.
        slot.handle_calls.fetch_add(1, Ordering::AcqRel);
        let _counted = HandleCall(slot);
        drop(lifecycle);
        let handle = match access {
            SlotAccess::Shared => slot
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .call_handle(),
            SlotAccess::Locked => None,
        };
        match handle {
            Some(mut handle) => {
                let _permit = match slot
                    .in_flight
                    .acquire(&slot.id, handle.call_timeout(), cancel)
                {
                    Ok(permit) => permit,
                    Err(error) => return (Err(error), None),
                };
                let started = Instant::now();
                (f(&mut handle), Some(started))
            }
            None => {
                let mut server = slot
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let result = f(&mut *server);
                drop(server);
                (result, None)
            }
        }
    }

    /// Re-spawn a slot's downstream connection and retry the call once on the fresh
    /// transport only when replay is safe. `uncertain_failure` instead preserves
    /// the original error and installs the fresh connection for future requests.
    /// Returns `Some(result)` when a reconnect was attempted (so the caller
    /// stops), or `None` when the slot has no reconnect factory (fall through to the
    /// normal breaker-failure path). The spawn runs without holding the `inner` lock so
    /// a slow re-spawn doesn't wedge other callers to the same server. Concurrent
    /// probes that failed on the same connection (`generation`) re-spawn it once;
    /// the others retry on the connection that one installed. A live multiplexed
    /// connection that came back to life during the spawn (a call succeeded or
    /// started on it since `successes` was read) is kept and the fresh one dropped.
    #[allow(clippy::too_many_arguments)]
    fn reconnect_and_retry<T, F>(
        &self,
        slot: &Arc<ServerSlot>,
        cancel: Option<&CancelContext>,
        uncertain_failure: Option<&TransportError>,
        generation: u64,
        successes: u64,
        access: SlotAccess,
        f: &mut F,
    ) -> Option<Result<T, CallFailure>>
    where
        F: FnMut(&mut dyn ServerDispatch) -> Result<T, TransportError>,
    {
        let factory = slot.reconnect.as_ref()?;
        {
            let _reconnecting = slot
                .reconnect_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A caller may have waited at the gate while another fresh
            // connection failed and opened the breaker. Do not spawn past it.
            if let Some(remaining) = slot
                .breaker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .open_remaining(Instant::now())
            {
                return Some(Err(format!(
                    "server '{}' is temporarily unavailable (too many recent failures; retrying in {}s)",
                    slot.id,
                    remaining.as_secs() + 1
                ).into()));
            }
            if slot.generation.load(Ordering::Acquire) == generation {
                eprintln!("toolport: server '{}' is down; re-spawning it", slot.id);
                let Some(fresh) = factory() else {
                    eprintln!(
                        "toolport: re-spawn of '{}' failed; leaving it fast-failed",
                        slot.id
                    );
                    return None; // still unreachable: fall through to record_failure
                };
                if cancel.is_some_and(CancelContext::is_cancelled) {
                    return Some(Err(CallFailure::new(
                        CallFailureKind::Cancelled,
                        "request cancelled before retrying the reconnected downstream",
                    )));
                }
                // Swap the live child/connection for the fresh one.
                let mut server = slot
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if server.connection_closed() == Some(false)
                    && !slot.quiescent_since(&server, successes)
                {
                    drop(server);
                    drop(fresh);
                    eprintln!(
                        "toolport: server '{}' answered during its re-spawn; keeping it",
                        slot.id
                    );
                    return None;
                }
                *server = fresh;
                drop(server);
                slot.generation.fetch_add(1, Ordering::AcqRel);
                slot.tool_revision.fetch_add(1, Ordering::AcqRel);
            }
        }
        if let Some(error) = uncertain_failure {
            // Recovery invalidates cached tool identity but cannot prove whether
            // the previous mutation completed. Use fresh transport next time.
            slot.breaker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_success();
            return Some(Err(error.call_failure()));
        }
        if cancel.is_some_and(CancelContext::is_cancelled) {
            return Some(Err(CallFailure::new(
                CallFailureKind::Cancelled,
                "request cancelled before retrying the reconnected downstream",
            )));
        }
        let (retry, started) = Self::attempt(slot, access, cancel, f);
        if retry.is_ok() {
            slot.successes.fetch_add(1, Ordering::AcqRel);
        }
        let mut breaker = slot
            .breaker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(match retry {
            Ok(v) => {
                eprintln!("toolport: server '{}' recovered after re-spawn", slot.id);
                breaker.record_success();
                Ok(v)
            }
            Err(e) => {
                if e.is_health_failure() {
                    breaker.record_concurrent_failure(started, Instant::now());
                }
                Err(e.call_failure())
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
        self.route_call_typed(exposed_name, arguments, cancel, meta, mrtr)
            .map_err(|failure| failure.to_string())
    }

    pub fn route_call_typed(
        &self,
        exposed_name: &str,
        arguments: Value,
        cancel: Option<CancelContext>,
        meta: Option<&Value>,
        mrtr: Option<&MrtrRequest>,
    ) -> Result<Value, CallFailure> {
        self.authorize(DispatchTarget::Tool(exposed_name))?;
        let (server_id, tool) = self.routes.get(exposed_name).ok_or_else(|| {
            CallFailure::new(
                CallFailureKind::NotFound,
                self.no_route_message(exposed_name),
            )
        })?;
        let mut arguments = arguments;
        if let Some(plan) = self.schema_arguments.get(exposed_name) {
            plan.restore(&mut arguments).map_err(|detail| {
                CallFailure::new(
                    CallFailureKind::InvalidInput {
                        missing: vec![],
                        invalid: vec![],
                    },
                    detail,
                )
            })?;
        }
        let slot = self.authorized_slot(server_id)?;
        let (result, downstream_supports_tasks) = self.call_with_retry_typed(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            ReplayPolicy::NoAmbiguousReplay,
            SlotAccess::Shared,
            |server| {
                let supports_tasks = server
                    .extensions()
                    .contains_key("io.modelcontextprotocol/tasks");
                server
                    .call_with_cancel_and_mrtr(tool, arguments.clone(), cancel.clone(), meta, mrtr)
                    .map(|result| (result, supports_tasks))
            },
        )?;
        if result.get("resultType").and_then(Value::as_str) == Some("task") {
            if !client_supports_tasks(meta) {
                return Err(
                    "downstream returned a task without the required client capability"
                        .to_string()
                        .into(),
                );
            }
            if !downstream_supports_tasks {
                return Err(
                    "downstream returned a task without advertising the Tasks extension"
                        .to_string()
                        .into(),
                );
            }
            expose_task_result(result, server_id).map_err(Into::into)
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
        // Cancelling stops work the server already took on, so like unsubscribe
        // cleanup it still reaches a server that was turned off since.
        let slot = if method == "tasks/cancel" {
            self.slot_for(&server_id)?
        } else {
            self.authorized_slot(&server_id)?
        };
        let mut forwarded = params;
        forwarded["taskId"] = json!(native_task_id);
        let result = self.call_with_retry(
            &slot,
            cancel.as_ref(),
            false,
            ReplayPolicy::for_task(method),
            SlotAccess::Shared,
            |server| server.task_request(method, forwarded.clone(), cancel.clone(), meta),
        )?;
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
        self.template_routes.get(uri_template).map(String::as_str)
    }

    /// The server that owns the exposed prompt `name`, if any. Used to scope a
    /// registered HTTP client's prompt access to its allowed server set.
    pub fn prompt_server(&self, exposed_name: &str) -> Option<&str> {
        self.prompt_routes
            .get(exposed_name)
            .map(|(s, _)| s.as_str())
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
    pub fn resolve_completion(&self, params: &Value) -> Result<(String, Value), String> {
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
                    .ok_or_else(|| format!("no server owns resource template or uri '{uri}'"))?;
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
        let slot = self.authorized_slot(&server_id)?;
        self.call_with_retry(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            ReplayPolicy::ReadOnly,
            SlotAccess::Shared,
            |server| server.read_resource_with_cancel_and_mrtr(uri, cancel.clone(), meta, mrtr),
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
        let slot = self.authorized_slot(&server_id)?;
        self.call_with_retry(
            &slot,
            None,
            false,
            ReplayPolicy::NoAmbiguousReplay,
            SlotAccess::Locked,
            |server| locked_server(server)?.subscribe_resource(uri),
        )
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
        if let Some(supervisor) = &slot.supervisor {
            // Best-effort cleanup never starts a connection. A stopped server
            // has no subscription left, and `attempt` rechecks readiness.
            if supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .state
                != SupervisorState::Ready
            {
                return Ok(json!({}));
            }
            return Self::attempt(&slot, SlotAccess::Locked, None, &mut |server| {
                locked_server(server)?.unsubscribe_resource(uri)
            })
            .0
            .map_err(|error| error.to_string());
        }
        self.call_with_retry(
            &slot,
            None,
            false,
            ReplayPolicy::NoAmbiguousReplay,
            SlotAccess::Locked,
            |server| locked_server(server)?.unsubscribe_resource(uri),
        )
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
        let slot = self.authorized_slot(&server_id)?;
        self.call_with_retry(
            &slot,
            cancel.as_ref(),
            mrtr.is_some_and(|request| !request.is_empty()),
            ReplayPolicy::ReadOnly,
            SlotAccess::Shared,
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
        let slot = self.authorized_slot(&server_id)?;
        self.call_with_retry(
            &slot,
            cancel.as_ref(),
            false,
            ReplayPolicy::ReadOnly,
            SlotAccess::Shared,
            |server| server.complete_with_cancel(forwarded.clone(), cancel.clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn http_timeout_with_successful_sibling(body_stage: bool) {
        use crate::downstream::HttpTransport;
        use std::io::{Read, Write};
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = tempfile::tempdir().unwrap();
        let _data = crate::registry::DataDirOverride::set(scratch.path());
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", server.server_addr());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let writes = Arc::new(AtomicU32::new(0));
        let counted = writes.clone();
        let wire = std::thread::spawn(move || {
            let mut stalled = None;
            while !stopped.load(Ordering::Acquire) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(50)).unwrap()
                else {
                    continue;
                };
                let mut text = String::new();
                request.as_reader().read_to_string(&mut text).unwrap();
                let body: Value = serde_json::from_str(&text).unwrap();
                if body["params"]["name"] == "write" {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let mut output = request.into_writer();
                    if body_stage {
                        output.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{\"jsonrpc\":").unwrap();
                        output.flush().unwrap();
                    }
                    // Channel barriers, not sleeps, keep the write in flight while
                    // the sibling succeeds. Dropping this writer ends the fixture.
                    started_tx.send(()).unwrap();
                    stalled = Some(output);
                    continue;
                }
                if body.get("id").is_none() {
                    request.respond(tiny_http::Response::empty(202)).unwrap();
                    continue;
                }
                let result = match body["method"].as_str().unwrap_or_default() {
                    "initialize" => {
                        json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"timeout-fixture","version":"1"}})
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"write","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":false}},{"name":"read","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]})
                    }
                    "tools/call" => json!({"content":[{"type":"text","text":"sibling succeeded"}]}),
                    _ => json!({}),
                };
                let response = if body["method"] == "server/discover" {
                    json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32601,"message":"legacy fixture"}})
                } else {
                    json!({"jsonrpc":"2.0","id":body["id"],"result":result})
                };
                request
                    .respond(
                        tiny_http::Response::from_string(response.to_string()).with_header(
                            tiny_http::Header::from_bytes("Content-Type", "application/json")
                                .unwrap(),
                        ),
                    )
                    .unwrap();
            }
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(stalled);
        });
        let transport =
            HttpTransport::guarded_with_timeout(&url, None, None, false, Duration::from_secs(2));
        let downstream = DownstreamServer::connect("fixture".into(), Box::new(transport)).unwrap();
        let spawns = Arc::new(AtomicU32::new(0));
        let reconnects = spawns.clone();
        let mut router = Router::new();
        router.add_with_reconnect(
            downstream,
            Some(Box::new(move || {
                reconnects.fetch_add(1, Ordering::SeqCst);
                None
            })),
        );
        let router = Arc::new(router);
        let caller = router.clone();
        let slow = std::thread::spawn(move || {
            caller.route_call_typed("fixture__write", json!({}), None, None, None)
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let sibling = router
            .route_call_typed("fixture__read", json!({}), None, None, None)
            .unwrap();
        assert_eq!(sibling["content"][0]["text"], "sibling succeeded");
        let failure = slow.join().unwrap().unwrap_err();
        assert_eq!(
            failure.kind,
            CallFailureKind::Timeout { after_send: true },
            "{failure}"
        );
        assert_eq!(
            router.servers[0]
                .breaker
                .lock()
                .unwrap()
                .consecutive_failures,
            1
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "a timeout must not reconnect/replay an uncertain write"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the write must reach the wire once"
        );
        assert!(
            router.route_call("fixture__read", json!({})).is_ok(),
            "sibling connection survives the failed call"
        );
        stop.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        wire.join().unwrap();
    }

    #[test]
    fn header_timeout_counts_health_without_replaying_or_interrupting_sibling() {
        http_timeout_with_successful_sibling(false);
    }

    #[test]
    fn body_timeout_counts_health_without_replaying_or_interrupting_sibling() {
        http_timeout_with_successful_sibling(true);
    }

    #[test]
    fn post_send_io_failures_are_non_replayable_even_for_read_probes() {
        for kind in [
            CallFailureKind::Timeout { after_send: true },
            CallFailureKind::Unavailable { after_send: true },
        ] {
            let error = TransportError::Classified(kind, "opaque detail".into());
            assert!(error.is_health_failure());
            assert!(ReplayPolicy::ReadOnly.uncertain_failure(&error).is_some());
            assert!(ReplayPolicy::NoAmbiguousReplay
                .uncertain_failure(&error)
                .is_some());
        }
    }

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
        assert_eq!(
            retry_wait(Some(Duration::from_secs(3600)), 0),
            HTTP_RETRY_CAP
        );
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

    #[test]
    fn schema_compat_recursive_arguments_restore_below_cycles() {
        let mut server = mock_server("s");
        server.tools = vec![json!({"name": "echo", "inputSchema": {
            "$ref": "#/$defs/Node",
            "$defs": {"Node": {"properties": {
                "a b": {"type": "string"},
                "kids": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
            }}}
        }})]
        .into();
        let mut router = Router::new();
        router.add(server);
        let mut args = json!({"a_b": "top", "kids": [
            {"a_b": "child", "kids": [{"a_b": "grandchild"}]}
        ]});
        router.schema_arguments["s__echo"]
            .restore(&mut args)
            .unwrap();
        assert_eq!(
            args,
            json!({"a b": "top", "kids": [
                {"a b": "child", "kids": [{"a b": "grandchild"}]}
            ]})
        );
        let published = router.aggregated_tools();
        assert!(!published[0]["inputSchema"].to_string().contains("$ref"));
    }

    #[test]
    fn schema_compat_maps_survive_guarded_restoration_and_reindexing() {
        let mut server = mock_server("s");
        server.tools = vec![
            json!({"name": "echo", "inputSchema": {"properties": {"'x-Cwd'": {"type": "string"}}}}),
        ]
        .into();
        let raw = server.tools.clone();
        let mut previous = Router::new();
        previous.add(server);
        let plan = Arc::clone(&previous.schema_arguments["s__echo"]);
        for _ in 0..3 {
            assert_eq!(
                previous.aggregated_tools()[0]["inputSchema"]["properties"]["x-Cwd"],
                json!({"type": "string"})
            );
            assert!(Arc::ptr_eq(&plan, &previous.schema_arguments["s__echo"]));
        }
        assert_eq!(previous.raw_catalogs().unwrap()["s"], raw);
        let reindexed = previous.reindexed();
        let mut restored = Router::new();
        let mut empty = mock_server("s");
        empty.tools.clear();
        restored.add(empty);
        restored.adopt_restored_routes(&previous, &previous.aggregated_tools());
        assert!(Arc::ptr_eq(&plan, &restored.schema_arguments["s__echo"]));
        for view in [reindexed, restored.reindexed()] {
            let mut args = json!({"x-Cwd": "/tmp"});
            view.schema_arguments["s__echo"].restore(&mut args).unwrap();
            assert_eq!(args, json!({"'x-Cwd'": "/tmp"}));
        }
    }

    #[test]
    fn reindex_releases_slot_before_waiting_for_definition_cache() {
        let mut router = Router::new();
        router.add(mock_server("s"));
        let slot = router.servers[0].clone();
        let raw = slot.inner.lock().unwrap().tools.clone();
        let prior_sharers = raw.storage_sharers();
        let cache = slot.definitions.lock().unwrap();
        let snapshot = router.clone();
        let worker = std::thread::spawn(move || snapshot.reindexed());
        let deadline = Instant::now() + Duration::from_secs(5);
        while raw.storage_sharers() == prior_sharers && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let snapshotted = raw.storage_sharers() > prior_sharers;
        let callable = loop {
            if slot.inner.try_lock().is_ok() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        let persisted = router.raw_catalogs().is_some();
        drop(cache);
        let rebuilt = worker.join().unwrap();
        assert!(
            snapshotted,
            "reindex must snapshot serialized tools before indexing"
        );
        assert!(
            persisted,
            "catalog persistence must not skip an indexing slot"
        );
        assert!(callable, "dispatch must not wait on schema indexing");
        assert_eq!(rebuilt.shared_tools(), router.shared_tools());
    }

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
                    Ok(
                        json!({ "contents": [{ "uri": uri, "text": format!("{}-body", self.label) }] }),
                    )
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
                    Ok(
                        json!({ "messages": [{ "role": "user", "content": format!("{}:{}", self.label, name) }] }),
                    )
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
        DownstreamServer::connect(id.to_string(), Box::new(ExtensionTransport { extensions }))
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
                    self.seen
                        .lock()
                        .unwrap()
                        .push((method.to_string(), params.clone()));
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
                    self.seen
                        .lock()
                        .unwrap()
                        .push((method.to_string(), params.clone()));
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
        assert_ne!(
            alpha_id, beta_id,
            "same native id on two servers must not collide"
        );
        assert_eq!(router.task_server(alpha_id).as_deref(), Some("alpha"));
        assert_eq!(router.task_server(beta_id).as_deref(), Some("beta"));
        assert_eq!(
            Router::new().task_server(alpha_id).as_deref(),
            Some("alpha"),
            "task ownership must survive a router rebuild"
        );
        let mut tampered = alpha_id.to_string();
        let changed = TASK_HANDLE_PREFIX.len() + 5;
        let replacement = if &tampered[changed..=changed] == "A" {
            "B"
        } else {
            "A"
        };
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
        for (method, params) in seen
            .iter()
            .filter(|(method, _)| method.starts_with("tasks/"))
        {
            assert_eq!(
                params["taskId"], "same-native-id",
                "{method} must use native id"
            );
            assert_eq!(
                params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
                    ["io.modelcontextprotocol/tasks"],
                json!({})
            );
        }
        assert!(router
            .route_task(
                "tasks/get",
                json!({ "taskId": "forged" }),
                None,
                Some(&meta)
            )
            .is_err());
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

    /// A multiplexing transport: a `slow` call blocks until `gate` opens, any
    /// other call answers at once. With `slow_fails`, the slow call then times
    /// out instead of answering. A `late` call blocks until `late` opens, then
    /// answers.
    struct GatedTransport {
        gate: Arc<(Mutex<bool>, Condvar)>,
        slow_fails: bool,
        late: Gate,
    }

    struct GatedCall {
        gate: Arc<(Mutex<bool>, Condvar)>,
        slow_fails: bool,
        late: Gate,
    }

    impl Transport for GatedTransport {
        fn request(&mut self, method: &str, _params: Value) -> Result<Value, TransportError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18", "capabilities": {} })),
                "tools/list" => Ok(json!({
                    "tools": [{ "name": "slow" }, { "name": "fast" }, { "name": "late" }]
                })),
                _ => Ok(json!({})),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
        fn concurrent(&self) -> Option<Arc<dyn crate::downstream::ConcurrentTransport>> {
            Some(Arc::new(GatedCall {
                gate: Arc::clone(&self.gate),
                slow_fails: self.slow_fails,
                late: Arc::clone(&self.late),
            }))
        }
    }

    impl crate::downstream::ConcurrentTransport for GatedCall {
        fn request_with_cancel_and_headers(
            &self,
            _method: &str,
            params: Value,
            _cancel: Option<CancelContext>,
            _headers: &[(String, String)],
        ) -> Result<Value, TransportError> {
            if params["name"] == "late" {
                wait_for_gate(&self.late);
            }
            if params["name"] == "slow" {
                let (open, opened) = &*self.gate;
                let mut open = open.lock().unwrap();
                while !*open {
                    open = opened.wait(open).unwrap();
                }
                if self.slow_fails {
                    return Err(TransportError::Unavailable(
                        "timed out waiting for 'tools/call' response".to_string(),
                    ));
                }
            }
            Ok(json!({ "content": [{ "type": "text", "text": params["name"].clone() }] }))
        }
    }

    type Gate = Arc<(Mutex<bool>, Condvar)>;

    fn wait_for_gate(gate: &(Mutex<bool>, Condvar)) {
        let (open, opened) = gate;
        let mut open = open.lock().unwrap();
        while !*open {
            open = opened.wait(open).unwrap();
        }
    }

    fn closed_gate() -> Gate {
        Arc::new((Mutex::new(false), Condvar::new()))
    }

    /// A router over one [`GatedTransport`] whose re-spawns are counted, and the
    /// gate that releases its slow calls.
    fn gated_router(slow_fails: bool) -> (Arc<Router>, Gate, Arc<std::sync::atomic::AtomicUsize>) {
        let (router, gate, _late, spawns) = gated_router_with_late(slow_fails);
        (router, gate, spawns)
    }

    /// [`gated_router`], plus the gate that releases its `late` calls.
    fn gated_router_with_late(
        slow_fails: bool,
    ) -> (Arc<Router>, Gate, Gate, Arc<std::sync::atomic::AtomicUsize>) {
        let gate = closed_gate();
        let late = closed_gate();
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&spawns);
        let mut router = Router::new();
        router.add_with_reconnect(
            DownstreamServer::connect(
                "s".into(),
                Box::new(GatedTransport {
                    gate: Arc::clone(&gate),
                    slow_fails,
                    late: Arc::clone(&late),
                }),
            )
            .unwrap(),
            Some(Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Some(mock_server("s"))
            })),
        );
        (Arc::new(router), gate, late, spawns)
    }

    fn wait_for_in_flight(slot: &ServerSlot, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while *slot.in_flight.count.lock().unwrap() != count {
            assert!(Instant::now() < deadline, "{count} call(s) never started");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn open_gate(gate: &(Mutex<bool>, Condvar)) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }

    #[test]
    fn a_probe_timeout_does_not_respawn_while_other_calls_succeed() {
        let (router, gate, spawns) = gated_router(true);
        let slot = router.slot_for("s").unwrap();
        // The breaker's cooldown has elapsed: the next calls are half-open probes.
        slot.breaker.lock().unwrap().consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        let slow = {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("s__slow", json!({})))
        };
        wait_for_in_flight(&slot, 1);
        assert!(router.route_call("s__fast", json!({})).is_ok());

        open_gate(&gate);
        assert!(slow.join().unwrap().is_err());
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "one call's timeout must not re-spawn a connection other calls use"
        );
    }

    #[test]
    fn a_probe_timeout_leaves_a_sibling_call_that_is_still_running() {
        let (router, gate, late, spawns) = gated_router_with_late(true);
        let slot = router.slot_for("s").unwrap();
        slot.breaker.lock().unwrap().consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        let sibling = {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("s__late", json!({})))
        };
        wait_for_in_flight(&slot, 1);
        let probe = {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("s__slow", json!({})))
        };
        wait_for_in_flight(&slot, 2);

        // The probe times out while its sibling is still within its deadline:
        // neither succeeded yet, but the connection is still working on one.
        open_gate(&gate);
        assert!(probe.join().unwrap().is_err());
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "a probe timeout must not replace a connection another call is running on"
        );
        open_gate(&late);
        assert!(
            sibling.join().unwrap().is_ok(),
            "the sibling call was killed"
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        assert_eq!(slot.generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_server_that_answers_during_its_respawn_is_kept() {
        let gate = closed_gate();
        open_gate(&gate);
        let (spawning_tx, spawning_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel::<()>();
        let (spawning_tx, proceed_rx) = (Mutex::new(spawning_tx), Mutex::new(proceed_rx));
        let mut router = Router::new();
        router.add_with_reconnect(
            DownstreamServer::connect(
                "s".into(),
                Box::new(GatedTransport {
                    gate,
                    slow_fails: true,
                    late: closed_gate(),
                }),
            )
            .unwrap(),
            Some(Box::new(move || {
                spawning_tx.lock().unwrap().send(()).unwrap();
                proceed_rx.lock().unwrap().recv().unwrap();
                Some(mock_server("s"))
            })),
        );
        let router = Arc::new(router);
        let slot = router.slot_for("s").unwrap();
        slot.breaker.lock().unwrap().consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        // The probe fails with nothing else in flight, so it starts a re-spawn.
        let probe = {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("s__slow", json!({})))
        };
        spawning_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the probe never started a re-spawn");
        // The old connection answers while the fresh one starts.
        assert!(router.route_call("s__fast", json!({})).is_ok());
        proceed_tx.send(()).unwrap();
        assert!(probe.join().unwrap().is_err());
        assert_eq!(
            slot.generation.load(Ordering::SeqCst),
            0,
            "a connection that answered during the re-spawn must not be replaced"
        );
        assert!(router.route_call("s__fast", json!({})).is_ok());
    }

    #[test]
    fn a_probe_timeout_respawns_a_connection_nothing_got_through() {
        let (router, gate, spawns) = gated_router(true);
        let slot = router.slot_for("s").unwrap();
        slot.breaker.lock().unwrap().consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        open_gate(&gate);
        // The re-spawned server answers the replayed read-only call.
        let _ = router.route_call("s__slow", json!({}));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_timeouts_count_as_one_breaker_failure() {
        let (router, gate, spawns) = gated_router(true);
        let slot = router.slot_for("s").unwrap();
        let calls: Vec<_> = (0..BREAKER_FAILURE_THRESHOLD)
            .map(|_| {
                let router = Arc::clone(&router);
                std::thread::spawn(move || router.route_call("s__slow", json!({})))
            })
            .collect();
        wait_for_in_flight(&slot, BREAKER_FAILURE_THRESHOLD as usize);
        open_gate(&gate);
        for call in calls {
            assert!(call.join().unwrap().is_err());
        }
        let breaker = slot.breaker.lock().unwrap();
        assert_eq!(breaker.consecutive_failures, 1);
        assert!(breaker.open_until.is_none(), "one event must not trip it");
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_slow_concurrent_call_does_not_hold_the_slot_lock() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let mut router = Router::new();
        router.add(
            DownstreamServer::connect(
                "s".into(),
                Box::new(GatedTransport {
                    gate: Arc::clone(&gate),
                    slow_fails: false,
                    late: closed_gate(),
                }),
            )
            .unwrap(),
        );
        let router = Arc::new(router);
        let slow = {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("s__slow", json!({})))
        };
        let slot = router.slot_for("s").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while *slot.in_flight.count.lock().unwrap() == 0 {
            assert!(Instant::now() < deadline, "the slow call never started");
            std::thread::sleep(Duration::from_millis(1));
        }

        // A refresh (or anything else needing the lock) is not stuck behind the call,
        // and a second call to the same server completes while the first waits.
        assert!(slot.inner.try_lock().is_ok());
        assert!(router.route_call("s__fast", json!({})).is_ok());
        assert!(!slow.is_finished());

        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(slow.join().unwrap().is_ok());
        assert_eq!(*slot.in_flight.count.lock().unwrap(), 0);
    }

    #[test]
    fn in_flight_limit_fails_busy_after_the_deadline() {
        let limit = InFlightLimit::default();
        let mut permits: Vec<_> = (0..MAX_IN_FLIGHT_PER_SERVER)
            .map(|_| limit.acquire("s", Duration::ZERO, None).unwrap())
            .collect();
        let busy = limit
            .acquire("s", Duration::from_millis(20), None)
            .err()
            .expect("the limit is reached");
        assert!(matches!(busy, TransportError::Busy(_)), "{busy}");
        assert!(!busy.is_health_failure());
        permits.pop();
        assert!(limit.acquire("s", Duration::from_millis(20), None).is_ok());
    }

    fn dead_slot(reconnect: Option<Reconnect>) -> Arc<ServerSlot> {
        Arc::new(ServerSlot::new(
            "s".into(),
            DownstreamServer::connect("s".into(), Box::new(DeadOnCallTransport)).unwrap(),
            reconnect,
        ))
    }

    #[cfg(unix)]
    #[test]
    fn p09_rejected_frame_never_replays_and_next_call_reconnects() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Scratch(
            std::env::temp_dir().join(format!("toolport-p09-router-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let script = dir.0.join("oversized.py");
        let calls = dir.0.join("calls");
        std::fs::write(
            &script,
            r#"import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion':'2025-06-18','capabilities':{}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'echo'}]}
    elif method == 'tools/call':
        with open(sys.argv[1], 'a') as calls:
            calls.write('dispatch\n')
        for _ in range(2049):
            os.write(1, b'x' * 8192)
        os.write(1, b'\n')
        result = {'ok':True}
    else:
        result = {}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#,
        )
        .unwrap();
        let transport = crate::downstream::StdioTransport::spawn(
            "/usr/bin/python3",
            &[
                script.to_string_lossy().into_owned(),
                calls.to_string_lossy().into_owned(),
            ],
            &[],
            None,
            false,
        )
        .unwrap();
        struct ObservedReset {
            inner: crate::downstream::StdioTransport,
            seen: std::sync::mpsc::Sender<()>,
        }
        impl Transport for ObservedReset {
            fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
                self.inner.request(method, params)
            }
            fn notify(&mut self, method: &str, params: Value) -> Result<(), TransportError> {
                self.inner.notify(method, params)
            }
            fn concurrent(&self) -> Option<Arc<dyn crate::downstream::ConcurrentTransport>> {
                self.inner.concurrent()
            }
            fn connection_closed(&self) -> Option<bool> {
                self.inner.connection_closed()
            }
            fn connection_reset_reason(&self) -> Option<String> {
                let reason = self.inner.connection_reset_reason();
                if reason.is_some() {
                    self.seen.send(()).unwrap();
                }
                reason
            }
        }
        let (seen, observed) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let bad = DownstreamServer::connect(
            "s".into(),
            Box::new(ObservedReset {
                inner: transport,
                seen,
            }),
        )
        .unwrap();
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = spawns.clone();
        let mut router = Router::new();
        router.add_with_reconnect(
            bad,
            Some(Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Some(mock_server("s"))
            })),
        );
        let error = router
            .route_call("s__echo", json!({"text":"original"}))
            .unwrap_err();
        assert!(error.contains("16777216-byte limit"), "{error}");
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "must not recover by replaying the original call"
        );
        let router = Arc::new(router);
        let (done, completed) = std::sync::mpsc::channel();
        let mut callers = Vec::new();
        for _ in 0..10 {
            let router = router.clone();
            let done = done.clone();
            callers.push(std::thread::spawn(move || {
                done.send(router.route_call("s__echo", json!({"text":"fresh"})))
                    .unwrap();
            }));
        }
        for _ in 0..10 {
            observed.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        release.send(()).unwrap();
        for _ in 0..10 {
            assert!(completed
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .is_ok());
        }
        for caller in callers {
            caller.join().unwrap();
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_to_string(calls).unwrap(), "dispatch\n");
    }

    #[test]
    fn p09_read_only_probe_never_replays_a_rejected_frame() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let counted = spawns.clone();
        let slot = dead_slot(Some(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(mock_server("s"))
        })));
        slot.breaker.lock().unwrap().consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        let error = Router::new()
            .call_with_retry(
                &slot,
                None,
                false,
                ReplayPolicy::ReadOnly,
                SlotAccess::Shared,
                |_| {
                    Err::<Value, _>(TransportError::FrameRejected(
                        "oversized frame; connection reset".into(),
                    ))
                },
            )
            .unwrap_err();
        assert_eq!(error, "oversized frame; connection reset");
        assert_eq!(spawns.load(Ordering::SeqCst), 0);
        assert!(slot
            .breaker
            .lock()
            .unwrap()
            .open_remaining(Instant::now())
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn p09_always_oversized_server_trips_breaker_until_cooldown() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Scratch(
            std::env::temp_dir().join(format!("toolport-p09-breaker-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let script = dir.0.join("oversized.py");
        std::fs::write(
            &script,
            r#"import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request['method']
    if method == 'initialize': result = {'protocolVersion':'2025-06-18','capabilities':{}}
    elif method == 'tools/list': result = {'tools':[{'name':'echo'}]}
    elif method == 'tools/call':
        for _ in range(2049): os.write(1, b'x' * 8192)
        continue
    else: result = {}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#,
        )
        .unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));
        let counted = spawns.clone();
        let fail_reconnect = Arc::new(AtomicBool::new(false));
        let failed = fail_reconnect.clone();
        let factory = move || {
            if failed.load(Ordering::SeqCst) {
                return None;
            }
            counted.fetch_add(1, Ordering::SeqCst);
            let transport = crate::downstream::StdioTransport::spawn(
                "/usr/bin/python3",
                &[script.to_string_lossy().into_owned()],
                &[],
                None,
                false,
            )
            .unwrap();
            Some(DownstreamServer::connect("s".into(), Box::new(transport)).unwrap())
        };
        let mut router = Router::new();
        router.add_with_reconnect(factory().unwrap(), Some(Box::new(factory)));
        for _ in 0..BREAKER_FAILURE_THRESHOLD {
            let error = router.route_call("s__echo", json!({})).unwrap_err();
            assert!(error.contains("16777216-byte limit"), "{error}");
        }
        let started = Instant::now();
        for _ in 0..12 {
            let error = router.route_call("s__echo", json!({})).unwrap_err();
            assert!(
                error.contains("too many recent failures; retrying in"),
                "{error}"
            );
        }
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            BREAKER_FAILURE_THRESHOLD as usize
        );
        let slot = router.slot_for("s").unwrap();
        let mut breaker = slot.breaker.lock().unwrap();
        assert_eq!(breaker.consecutive_failures, BREAKER_FAILURE_THRESHOLD);
        let remaining = breaker.open_remaining(Instant::now()).unwrap();
        assert!(remaining <= BREAKER_COOLDOWN);
        breaker.open_until = Some(Instant::now());
        drop(breaker);
        let error = router.route_call("s__echo", json!({})).unwrap_err();
        assert!(error.contains("16777216-byte limit"), "{error}");
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            BREAKER_FAILURE_THRESHOLD as usize + 1
        );
        assert!(slot
            .breaker
            .lock()
            .unwrap()
            .open_remaining(Instant::now())
            .is_some());
        // A cooldown probe whose factory fails must reopen the breaker too.
        fail_reconnect.store(true, Ordering::SeqCst);
        slot.breaker.lock().unwrap().open_until = Some(Instant::now());
        let error = router.route_call("s__echo", json!({})).unwrap_err();
        assert!(error.contains("downstream reconnect required"), "{error}");
        assert!(slot
            .breaker
            .lock()
            .unwrap()
            .open_remaining(Instant::now())
            .is_some());
        let error = router.route_call("s__echo", json!({})).unwrap_err();
        assert!(
            error.contains("too many recent failures; retrying in"),
            "{error}"
        );
    }

    #[test]
    fn reconnect_and_retry_recovers_a_dead_server() {
        let router = Router::new();
        // Factory hands back a healthy connection, mirroring a re-spawn that succeeds.
        let slot = dead_slot(Some(Box::new(|| Some(mock_server("s")))));
        let out =
            router.reconnect_and_retry(&slot, None, None, 0, 0, SlotAccess::Shared, &mut |ds| {
                ds.call_with_cancel_and_mrtr("echo", json!({}), None, None, None)
            });
        // The probe re-spawned the server and the retried call went through.
        let value = out.expect("reconnect attempted").expect("call recovered");
        assert!(serde_json::to_string(&value).unwrap().contains("s:echo"));
        // The live connection was swapped in, so subsequent calls hit the healthy one.
        assert!(slot.inner.lock().unwrap().call("echo", json!({})).is_ok());
        // A successful recovery closes the breaker.
        assert!(slot
            .breaker
            .lock()
            .unwrap()
            .open_remaining(Instant::now())
            .is_none());
    }

    #[test]
    fn reconnect_and_retry_gives_up_when_respawn_fails() {
        let router = Router::new();
        // Factory still can't reach the server (returns None): no recovery, and the
        // caller must fall through to record the failure.
        let slot = dead_slot(Some(Box::new(|| None)));
        let out: Option<Result<Value, CallFailure>> =
            router.reconnect_and_retry(&slot, None, None, 0, 0, SlotAccess::Shared, &mut |ds| {
                ds.call_with_cancel_and_mrtr("echo", json!({}), None, None, None)
            });
        assert!(
            out.is_none(),
            "a failed re-spawn falls through to the breaker"
        );
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
            .reconnect_and_retry(
                &slot,
                Some(&cancel),
                None,
                0,
                0,
                SlotAccess::Shared,
                &mut move |ds| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    ds.call_with_cancel_and_mrtr("echo", json!({}), None, None, None)
                },
            )
            .expect("reconnect was attempted")
            .unwrap_err();

        assert!(
            result.detail.contains("cancelled"),
            "unexpected error: {result}"
        );
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

        let result: Result<Value, CallFailure> = router
            .reconnect_and_retry(
                &slot,
                Some(&cancel),
                None,
                0,
                0,
                SlotAccess::Shared,
                &mut move |_server| {
                    assert!(cancel_from_retry.cancel("retry-cancel", Some("user pressed stop")));
                    Err(TransportError::Cancelled(
                        "request cancelled during reconnected call".to_string(),
                    ))
                },
            )
            .expect("reconnect was attempted");

        assert!(result.unwrap_err().detail.contains("cancelled"));
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
        let out: Option<Result<Value, CallFailure>> =
            router.reconnect_and_retry(&slot, None, None, 0, 0, SlotAccess::Shared, &mut |ds| {
                ds.call_with_cancel_and_mrtr("echo", json!({}), None, None, None)
            });
        assert!(out.is_none());
    }

    /// An inert transport records a completed effect before losing the reply.
    /// Its healthy replacement uses the same counter, so a replay is observable.
    struct LostReplyTransport {
        inner: Box<dyn Transport>,
        operation: &'static str,
        lose_reply: bool,
        effects: Arc<AtomicU32>,
    }

    impl Transport for LostReplyTransport {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
            if method == self.operation {
                self.effects.fetch_add(1, Ordering::SeqCst);
                if self.lose_reply {
                    return Err(TransportError::Unavailable(
                        "reply lost after effect".into(),
                    ));
                }
            }
            let mut result = self.inner.request(method, params)?;
            if method == "tools/list" {
                // Even favorable server hints cannot prove mutation replay safe.
                for tool in result["tools"].as_array_mut().unwrap() {
                    tool["annotations"] = json!({
                        "readOnlyHint": true,
                        "idempotentHint": true,
                        "destructiveHint": false
                    });
                }
            }
            Ok(result)
        }

        fn notify(&mut self, method: &str, params: Value) -> Result<(), TransportError> {
            self.inner.notify(method, params)
        }
    }

    fn lost_reply_server(
        operation: &'static str,
        lose_reply: bool,
        effects: Arc<AtomicU32>,
    ) -> DownstreamServer {
        let inner: Box<dyn Transport> = if operation.starts_with("tasks/") {
            Box::new(TaskTransport {
                seen: Arc::new(Mutex::new(Vec::new())),
                advertise_tasks: true,
            })
        } else {
            Box::new(MockTransport { label: "s".into() })
        };
        let mut server = DownstreamServer::connect(
            "s".into(),
            Box::new(LostReplyTransport {
                inner,
                operation,
                lose_reply,
                effects,
            }),
        )
        .unwrap();
        server.load_resources_prompts();
        server
    }

    fn expired_probe_router(
        operation: &'static str,
        effects: Arc<AtomicU32>,
        reconnect: Option<Reconnect>,
    ) -> Router {
        let mut router = Router::new();
        router.add_with_reconnect(lost_reply_server(operation, true, effects), reconnect);
        let mut breaker = router.servers[0].breaker.lock().unwrap();
        breaker.consecutive_failures = BREAKER_FAILURE_THRESHOLD;
        breaker.open_until = Some(Instant::now() - Duration::from_secs(1));
        drop(breaker);
        router
    }

    #[test]
    fn replay_policy_ambiguous_tool_effect_is_not_replayed_but_next_call_uses_fresh_connection() {
        let effects = Arc::new(AtomicU32::new(0));
        let fresh_effects = Arc::clone(&effects);
        let reconnects = Arc::new(AtomicU32::new(0));
        let factory_calls = Arc::clone(&reconnects);
        let router = expired_probe_router(
            "tools/call",
            Arc::clone(&effects),
            Some(Box::new(move || {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                Some(lost_reply_server(
                    "tools/call",
                    false,
                    Arc::clone(&fresh_effects),
                ))
            })),
        );

        let revision_before = router.servers[0].tool_revision.load(Ordering::Acquire);
        let error = router
            .route_call("s__echo", json!({ "request": "first" }))
            .unwrap_err();
        assert_eq!(error, "reply lost after effect");
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "completed effect must not replay"
        );
        assert_eq!(reconnects.load(Ordering::SeqCst), 1);
        assert_eq!(
            router.servers[0].tool_revision.load(Ordering::Acquire),
            revision_before + 1,
            "fresh transport must invalidate tool identity before the uncertain error returns"
        );
        assert_eq!(
            router.servers[0]
                .breaker
                .lock()
                .unwrap()
                .consecutive_failures,
            0
        );
        assert!(router
            .route_call("s__echo", json!({ "request": "next" }))
            .is_ok());
        assert_eq!(
            effects.load(Ordering::SeqCst),
            2,
            "only the next independent call runs"
        );
        assert_eq!(reconnects.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn replay_policy_preserves_classified_retry_for_tool_calls() {
        let (server, attempts) = retry_server_inspectable("retry", 1);
        let mut router = Router::new();
        router.add(server);
        let result = router.route_call("retry__flaky", json!({})).unwrap();
        assert_eq!(result["content"][0]["text"], "ok-after-retry");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn replay_policy_read_probe_can_replay_after_transport_recovery() {
        let reads = Arc::new(AtomicU32::new(0));
        let fresh_reads = Arc::clone(&reads);
        let router = expired_probe_router(
            "resources/read",
            Arc::clone(&reads),
            Some(Box::new(move || {
                Some(lost_reply_server(
                    "resources/read",
                    false,
                    Arc::clone(&fresh_reads),
                ))
            })),
        );
        assert!(router.read_resource("s://readme").is_ok());
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "read probe retains safe recovery"
        );
        assert_eq!(
            router.servers[0]
                .breaker
                .lock()
                .unwrap()
                .consecutive_failures,
            0
        );
    }

    #[test]
    fn replay_policy_task_mutations_do_not_replay_while_task_get_recovers() {
        for method in ["tasks/update", "tasks/cancel", "tasks/get"] {
            let effects = Arc::new(AtomicU32::new(0));
            let fresh_effects = Arc::clone(&effects);
            let router = expired_probe_router(
                method,
                Arc::clone(&effects),
                Some(Box::new(move || {
                    Some(lost_reply_server(method, false, Arc::clone(&fresh_effects)))
                })),
            );
            let slot = router.slot_for("s").unwrap();
            // Raw task ids keep this fixture independent of the OS secret store.
            let result = router.call_with_retry(
                &slot,
                None,
                false,
                ReplayPolicy::for_task(method),
                SlotAccess::Shared,
                |server| server.task_request(method, json!({ "taskId": "fixture" }), None, None),
            );
            if method == "tasks/get" {
                assert!(result.is_ok());
                assert_eq!(effects.load(Ordering::SeqCst), 2);
            } else {
                assert_eq!(result.unwrap_err(), "reply lost after effect");
                assert_eq!(effects.load(Ordering::SeqCst), 1);
            }
            assert_eq!(slot.breaker.lock().unwrap().consecutive_failures, 0);
        }
    }

    #[test]
    fn replay_policy_uncertain_mutation_keeps_failure_when_factory_is_missing_or_fails() {
        for failed_factory in [false, true] {
            let effects = Arc::new(AtomicU32::new(0));
            let reconnect: Option<Reconnect> = if failed_factory {
                Some(Box::new(|| None))
            } else {
                None
            };
            let router = expired_probe_router("tools/call", Arc::clone(&effects), reconnect);
            assert_eq!(
                router.route_call("s__echo", json!({})).unwrap_err(),
                "reply lost after effect"
            );
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            assert!(router.servers[0]
                .breaker
                .lock()
                .unwrap()
                .open_until
                .is_some());
        }
    }

    #[test]
    fn replay_policy_cancellation_during_uncertain_reconnect_does_not_emit_another_effect() {
        let effects = Arc::new(AtomicU32::new(0));
        let fresh_effects = Arc::clone(&effects);
        let cancellations = CancelRegistry::new();
        assert!(cancellations.begin_client_request("uncertain-cancel".into()));
        let cancel = cancellations.context("uncertain-cancel".into());
        let cancel_from_factory = cancellations.clone();
        let router = expired_probe_router(
            "tools/call",
            Arc::clone(&effects),
            Some(Box::new(move || {
                assert!(cancel_from_factory.cancel("uncertain-cancel", Some("stop")));
                Some(lost_reply_server(
                    "tools/call",
                    false,
                    Arc::clone(&fresh_effects),
                ))
            })),
        );
        let error = router
            .route_call_with_cancel("s__echo", json!({}), Some(cancel), None)
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        // Cancellation adds no health penalty to the pre-existing probe streak.
        assert_eq!(
            router.servers[0]
                .breaker
                .lock()
                .unwrap()
                .consecutive_failures,
            BREAKER_FAILURE_THRESHOLD
        );
        cancellations.finish_client_request("uncertain-cancel");
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
        assert_eq!(
            router.resource_server("postgres://readme"),
            Some("postgres")
        );
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
        assert!(
            ttl > 0 && ttl <= 30_000,
            "minimum contributor TTL should win: {ttl}"
        );

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
    fn server_detail_resolves_exact_upstream_names_and_overrides() {
        let tools = vec![json!({"name":"get-item"}), json!({"name":"get_item"})];
        let aliases = Router::server_tool_aliases("server-a", &tools, HashMap::new());
        assert_eq!(aliases["get-item"], "server_a__get_item");
        assert_eq!(aliases["get_item"], "server_a__get_item_2");
        let overrides = HashMap::from([(
            "server-a".to_string(),
            HashMap::from([(
                "get-item".to_string(),
                ToolOverride {
                    name: Some("renamed".to_string()),
                    description: None,
                    unknown_fields: Default::default(),
                },
            )]),
        )]);
        let aliases = Router::server_tool_aliases("server-a", &tools, overrides);
        assert_eq!(aliases["get-item"], "renamed");
        let mut router = Router::new();
        router
            .routes
            .insert("renamed".into(), ("server-a".into(), "get-item".into()));
        router.routes.insert(
            "server_a__get_item_2".into(),
            ("server-a".into(), "get_item".into()),
        );
        assert_eq!(
            router.exposed_tool_name("server-a", "get-item"),
            Some("renamed")
        );
        assert_eq!(
            router.exposed_tool_name("server-a", "get_item"),
            Some("server_a__get_item_2")
        );
        assert_eq!(router.exposed_tool_name("server_a", "get-item"), None);
        assert_eq!(router.exposed_tool_name("server-a", "missing"), None);
    }

    #[test]
    fn routes_call_to_the_right_server() {
        let mut router = Router::new();
        router.add(mock_server("github"));
        router.add(mock_server("postgres"));

        let result = router
            .route_call("postgres__add", json!({ "a": 1 }))
            .unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "postgres:add");
    }

    #[test]
    fn cloned_router_shares_definitions_until_indexing_changes_its_view() {
        let _data = crate::registry::DataDirTestEnv::new(
            "cloned_router_shares_definitions_until_indexing_changes_its_view",
        );
        let mut base = Router::new();
        base.add(mock_server("shared"));
        let before = base.aggregated_tools();
        let mut view = base.clone();
        assert!(base
            .tools
            .0
            .iter()
            .zip(&view.tools.0)
            .all(|(a, b)| Arc::ptr_eq(a, b)));

        view.add(mock_server("another"));
        assert_eq!(view.tools.len(), base.tools.len() + 2);
        assert!(base
            .tools
            .0
            .iter()
            .zip(&view.tools.0)
            .all(|(a, b)| Arc::ptr_eq(a, b)));
        assert_eq!(base.aggregated_tools(), before);
        assert!(base.route_of("another__echo").is_none());
        assert!(view.route_of("another__echo").is_some());

        let snapshot = view.clone();
        view.requarantine(BTreeSet::from(["shared__echo".to_string()]));
        assert!(view.route_of("shared__echo").is_none());
        assert!(snapshot.route_of("shared__echo").is_some());
        assert_eq!(base.aggregated_tools(), before);
    }

    #[test]
    fn normalized_definitions_and_arguments_are_shared_across_profile_rebuilds() {
        let mut base = Router::new();
        base.add(DownstreamServer::stopped(
            "shared".into(),
            vec![json!({
                "name": "read", "inputSchema": {"type":"object", "properties": {
                    "'x-Cwd'": {"type":"string"}
                }}
            })],
        ));
        let original = base.shared_tools();
        let profile = base.with_tool_allow(HashMap::from([(
            "shared".into(),
            HashSet::from(["read".into()]),
        )]));
        let clone = profile.with_tool_allow(HashMap::new());
        assert!(Arc::ptr_eq(&original.0[0], &profile.tools.0[0]));
        assert!(Arc::ptr_eq(&original.0[0], &clone.tools.0[0]));
        assert!(Arc::ptr_eq(
            &base.schema_arguments["shared__read"],
            &profile.schema_arguments["shared__read"]
        ));
        let mut overridden = base.clone();
        overridden.set_overrides(HashMap::from([(
            "shared".into(),
            HashMap::from([(
                "read".into(),
                ToolOverride {
                    name: None,
                    description: Some("Changed".into()),
                    unknown_fields: Default::default(),
                },
            )]),
        )]));
        overridden.rebuild_aggregation();
        assert!(!Arc::ptr_eq(&original.0[0], &overridden.tools.0[0]));
        assert_ne!(original.0[0].digest, overridden.tools.0[0].digest);
        assert_eq!(original[0]["description"], Value::Null);
    }

    #[test]
    fn profile_views_share_downstreams_but_reindex_distinct_tool_scopes() {
        let mut base = Router::with_policy(ToolPolicy {
            allow: HashMap::from([("shared".to_string(), HashSet::new())]),
            ..ToolPolicy::default()
        });
        base.add(mock_server("shared"));
        assert!(base.aggregated_tools().is_empty());

        let echo = base.with_tool_allow(HashMap::from([(
            "shared".to_string(),
            HashSet::from(["echo".to_string()]),
        )]));
        let add = base.with_tool_allow(HashMap::from([(
            "shared".to_string(),
            HashSet::from(["add".to_string()]),
        )]));
        assert!(Arc::ptr_eq(&base.servers[0], &echo.servers[0]));
        assert!(Arc::ptr_eq(&echo.servers[0], &add.servers[0]));
        assert!(echo.route_of("shared__echo").is_some());
        assert!(echo.route_of("shared__add").is_none());
        assert!(add.route_of("shared__echo").is_none());
        assert!(add.route_of("shared__add").is_some());
    }

    #[test]
    fn replacing_a_root_slot_shares_unrelated_connections_and_rebuilds_its_catalog() {
        let mut base = Router::new();
        base.add(mock_server("ordinary"));
        base.add(mock_server("rooted"));
        let mut replacement = DownstreamServer::connect(
            "rooted".to_string(),
            Box::new(MockTransport {
                label: "another-root".to_string(),
            }),
        )
        .expect("replacement downstream");
        replacement.tools.push(json!({
            "name": "only_at_this_root",
            "inputSchema": { "type": "object" }
        }));
        let view = base.with_server_launch(replacement, None);

        assert!(Arc::ptr_eq(&base.servers[0], &view.servers[0]));
        assert!(!Arc::ptr_eq(&base.servers[1], &view.servers[1]));
        assert!(view.route_of("rooted__only_at_this_root").is_some());
        assert!(base.route_of("rooted__only_at_this_root").is_none());
        assert_eq!(
            view.route_call("ordinary__add", json!({})).unwrap()["content"][0]["text"],
            "ordinary:add"
        );
        assert_eq!(
            base.route_call("rooted__add", json!({})).unwrap()["content"][0]["text"],
            "rooted:add"
        );
        assert_eq!(
            view.route_call("rooted__add", json!({})).unwrap()["content"][0]["text"],
            "another-root:add"
        );

        let mut ordinary_only = Router::new();
        ordinary_only.add(mock_server("ordinary"));
        let inserted = ordinary_only.with_server_launch(mock_server("rooted"), None);
        assert!(Arc::ptr_eq(&ordinary_only.servers[0], &inserted.servers[0]));
        assert!(inserted.route_of("rooted__add").is_some());
        assert!(ordinary_only.route_of("rooted__add").is_none());
        let reused = ordinary_only
            .with_server_slot_from(&inserted, "rooted")
            .expect("inserted slot can be shared");
        assert!(Arc::ptr_eq(&inserted.servers[1], &reused.servers[1]));
    }

    #[test]
    fn tool_overrides_rename_and_redescribe() {
        let mut router = Router::new();
        // Keyed by (server id, ORIGINAL tool name), not the exposed name.
        let mut srv = HashMap::new();
        srv.insert(
            "echo".to_string(),
            ToolOverride {
                name: Some("say".into()),
                description: Some("say it back".into()),
                unknown_fields: Default::default(),
            },
        );
        srv.insert(
            "add".to_string(),
            ToolOverride {
                name: None,
                description: Some("cleaned".into()),
                unknown_fields: Default::default(),
            },
        );
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        let tools = router.aggregated_tools();
        let by_name: HashMap<&str, &Value> = tools
            .iter()
            .map(|t| (t["name"].as_str().unwrap(), t))
            .collect();

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
            ToolOverride {
                name: Some("say".into()),
                description: None,
                unknown_fields: Default::default(),
            },
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
        assert!(
            !names.contains(&"say".to_string()),
            "quarantined rename must be hidden"
        );
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
            ToolOverride {
                name: Some("say".into()),
                description: None,
                unknown_fields: Default::default(),
            },
        );
        let policy = ToolPolicy {
            quarantined: BTreeSet::from(["srv__echo".to_string()]),
            ..Default::default()
        };
        let mut router = Router::with_policy(policy);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        assert_eq!(router.route_of("say"), Some(("srv", "echo")));
        assert!(
            router.route_call("say", json!({})).is_ok(),
            "stale entry must not block"
        );
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
            ToolOverride {
                name: Some("search".into()),
                description: None,
                unknown_fields: Default::default(),
            },
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
        assert!(
            persisted.contains("search"),
            "quarantine is keyed by the exposed name"
        );

        // ...and feeding it to the router hides and blocks the renamed tool.
        router.requarantine(persisted);
        let visible: Vec<String> = router
            .aggregated_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert!(
            !visible.contains(&"search".to_string()),
            "quarantined rename must be hidden"
        );
        let err = router.route_call("search", json!({})).unwrap_err();
        assert!(
            err.contains("quarantine"),
            "a call to a quarantined rename must block: {err}"
        );

        // Re-approve: release clears the persisted set and the router restores the tool.
        assert!(
            integrity::release(profile, "search").unwrap(),
            "release must clear the entry"
        );
        let after = integrity::quarantined(profile).expect("quarantine store readable");
        assert!(
            !after.contains("search"),
            "a released tool leaves the persisted set"
        );
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
            ToolOverride {
                name: Some("srv__echo".into()),
                description: None,
                unknown_fields: Default::default(),
            },
        )]);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        let tools = router.aggregated_tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"srv__echo"), "the real echo keeps the name");
        assert!(names.contains(&"srv__add"), "add fell back to its own name");
        assert_eq!(
            router.route_call("srv__echo", json!({})).unwrap()["content"][0]["text"],
            "srv:echo"
        );
        assert_eq!(
            router.route_call("srv__add", json!({})).unwrap()["content"][0]["text"],
            "srv:add"
        );
    }

    #[test]
    fn route_of_resolves_renamed_tool_to_real_server_and_original_tool() {
        // The gate derives provenance/scoping from route_of, NOT by splitting the exposed
        // name. A renamed tool must still resolve to its real (server, original tool) so the
        // untrusted-source HITL check and per-client scoping aren't silently bypassed.
        let mut router = Router::new();
        let srv = HashMap::from([(
            "echo".to_string(),
            ToolOverride {
                name: Some("say".into()),
                description: None,
                unknown_fields: Default::default(),
            },
        )]);
        router.set_overrides(HashMap::from([("srv".to_string(), srv)]));
        router.add(mock_server("srv"));

        assert_eq!(
            router.route_of("say"),
            Some(("srv", "echo")),
            "renamed tool resolves to origin"
        );
        assert_eq!(
            router.route_of("srv__add"),
            Some(("srv", "add")),
            "normal tool resolves"
        );
        assert_eq!(
            router.route_of("nope"),
            None,
            "unknown name resolves to nothing"
        );
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
        assert_eq!(
            rebuilt.route_of("atlassian__t39"),
            Some(("atlassian", "t39"))
        );
        assert_eq!(
            rebuilt.route_of("atlassian__t37"),
            Some(("atlassian", "t37"))
        );
        assert_eq!(
            rebuilt.route_of("github__t4"),
            Some(("github", "t4")),
            "healthy server's own routes are untouched"
        );
        // A call to one of the 37 restored tools reaches the downstream server.
        let result = rebuilt.route_call("atlassian__t39", json!({})).unwrap();
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
    fn profile_view_retains_a_tool_hidden_from_the_base_during_a_guarded_shrink() {
        let mut previous = router_with_catalogs(&[("atlassian", 40)]);
        previous
            .policy
            .allow
            .insert("atlassian".to_string(), HashSet::from(["t0".to_string()]));
        previous.rebuild_aggregation();
        let mut rebuilt = router_with_catalogs(&[("atlassian", 3)]);
        rebuilt.policy.allow = previous.policy.allow.clone();
        rebuilt.rebuild_aggregation();
        rebuilt.adopt_restored_routes(&previous, &previous.aggregated_tools());

        let prior_live = rebuilt.clone();
        rebuilt.rebuild_preserving_restored();
        rebuilt.adopt_restored_routes(&prior_live, &prior_live.shared_tools());
        let profile = rebuilt.with_tool_allow(HashMap::from([(
            "atlassian".to_string(),
            HashSet::from(["t39".to_string()]),
        )]));
        assert_eq!(
            profile.route_of("atlassian__t39"),
            Some(("atlassian", "t39"))
        );
        assert!(profile
            .aggregated_tools()
            .iter()
            .any(|tool| tool["name"] == "atlassian__t39"));
        let refreshed = profile.reindexed();
        assert_eq!(
            refreshed.route_of("atlassian__t39"),
            Some(("atlassian", "t39")),
            "a rooted catalog refresh must retain guarded routes from unchanged slots"
        );
        let slot = profile.server_slot("atlassian").unwrap();
        let shared = profile.with_shared_server_slot(&slot);
        assert_eq!(
            shared.route_of("atlassian__t39"),
            Some(("atlassian", "t39"))
        );
        let mut quarantined = profile.clone();
        quarantined.requarantine(BTreeSet::from(["atlassian__t39".to_string()]));
        assert!(quarantined.route_of("atlassian__t39").is_none());
        assert!(quarantined.is_blocked("atlassian__t39"));
    }

    #[test]
    fn review_repeat_adoption_keeps_restored_candidates() {
        let previous = router_with_catalogs(&[("atlassian", 40)]);
        let mut guarded = router_with_catalogs(&[("atlassian", 3)]);
        let catalog = previous.aggregated_tools();
        guarded.adopt_restored_routes(&previous, &catalog);
        assert_eq!(guarded.route_of("atlassian__t39"), Some(("atlassian", "t39")));
        // Refresh path: next = live clone, rebuilt, published, then adopted again
        // with previous_router = the guarded live router.
        let prior_live = guarded.clone();
        let mut next = guarded.clone();
        next.rebuild_preserving_restored();
        next.adopt_restored_routes(&prior_live, &catalog);
        assert_eq!(next.route_of("atlassian__t39"), Some(("atlassian", "t39")));
        let profile = next.with_tool_allow(HashMap::new());
        assert_eq!(
            profile.route_of("atlassian__t39"),
            Some(("atlassian", "t39")),
            "profile view lost restored route"
        );
        let mut policy = next.clone();
        policy.requarantine(BTreeSet::new());
        assert_eq!(
            policy.route_of("atlassian__t39"),
            Some(("atlassian", "t39")),
            "policy rebuild lost restored route"
        );
        let mut registry = next.registry_policy();
        registry.deny_destructive = true;
        assert!(next.apply_registry_policy(registry));
        assert_eq!(next.route_of("atlassian__t39"), Some(("atlassian", "t39")));
        assert!(next.route_call("atlassian__t39", json!({})).is_ok());
        next.requarantine(BTreeSet::from(["atlassian__t39".to_string()]));
        assert!(next.route_of("atlassian__t39").is_none());
        assert!(next.is_blocked("atlassian__t39"));
    }

    #[test]
    fn guarded_route_keeps_its_name_when_a_new_server_collides() {
        let previous = router_with_catalogs(&[("a-b", 40)]);
        let mut guarded = router_with_catalogs(&[("a-b", 3)]);
        guarded.adopt_restored_routes(&previous, &previous.aggregated_tools());
        let new_server = router_with_catalogs(&[("a_b", 40)]);
        let slot = new_server.server_slot("a_b").unwrap();
        let combined = guarded.with_shared_server_slot(&slot);
        assert_eq!(combined.route_of("a_b__t39"), Some(("a-b", "t39")));
        assert_eq!(combined.route_of("a_b__t39_2"), Some(("a_b", "t39")));
    }

    #[test]
    fn confirmed_tool_refresh_expires_only_its_own_guarded_routes() {
        let previous = router_with_catalogs(&[("a", 40), ("b", 40)]);
        let mut guarded = router_with_catalogs(&[("a", 3), ("b", 3)]);
        guarded.adopt_restored_routes(&previous, &previous.aggregated_tools());
        assert!(guarded.reindexed().route_of("a__t39").is_some());

        let slot = guarded.server_slot("a").unwrap();
        slot.0.tool_revision.fetch_add(1, Ordering::AcqRel);
        let refreshed = guarded.reindexed();
        assert!(refreshed.route_of("a__t39").is_none());
        assert_eq!(refreshed.route_of("b__t39"), Some(("b", "t39")));
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
    fn healthy_catalogs_are_not_retained_as_restoration_candidates() {
        let _data = crate::registry::DataDirTestEnv::new(
            "healthy_catalogs_are_not_retained_as_restoration_candidates",
        );
        let previous = router_with_catalogs(&[("healthy", 40)]);
        let mut rebuilt = router_with_catalogs(&[("healthy", 40)]);
        let catalog = previous.aggregated_tools();
        rebuilt.adopt_restored_routes(&previous, &catalog);
        assert!(rebuilt.restored_candidates.is_empty());
        assert_eq!(rebuilt.aggregated_tools(), catalog);

        let profile = rebuilt.with_tool_allow(HashMap::from([(
            "healthy".to_string(),
            HashSet::from(["t39".to_string()]),
        )]));
        assert_eq!(profile.aggregated_tools().len(), 1);
        assert_eq!(profile.route_of("healthy__t39"), Some(("healthy", "t39")));
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
                    unknown_fields: Default::default(),
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
        assert!(is_destructive(
            &json!({ "annotations": { "destructiveHint": true } })
        ));
        assert!(is_destructive(&json!({ "destructiveHint": true }))); // top-level fallback
        assert!(!is_destructive(
            &json!({ "annotations": { "destructiveHint": false } })
        ));
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
        policy.disabled.insert(
            "github".to_string(),
            ["echo".to_string()].into_iter().collect(),
        );
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
    fn sbs871_fail_closed_catalog_hides_every_tool_until_requarantine() {
        // SBS-871: a cold-start store Err has no prior live set, so the whole
        // catalog stays hidden until a later successful read installs a set.
        let mut policy = ToolPolicy::default();
        policy.fail_closed_catalog = true;
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));
        assert!(
            router.aggregated_tools().is_empty(),
            "fail-closed catalog must hide every tool"
        );
        assert!(router.catalog_fail_closed());
        let err = router.route_call("github__echo", json!({})).unwrap_err();
        assert!(
            err.contains("quarantine store unreadable"),
            "unexpected: {err}"
        );

        router.requarantine_from_store(BTreeSet::new());
        assert!(!router.catalog_fail_closed());
        assert!(
            !router.aggregated_tools().is_empty(),
            "a later known set must re-expose the catalog"
        );
    }

    /// SBS-871: the whole point of the hide is that it survives every path that is
    /// NOT a successful store read. `requarantine` runs on store-error paths
    /// (integrity write failure, unreadable persisted set), so it must leave the
    /// catalog hidden; only `requarantine_from_store` lifts it.
    #[test]
    fn sbs871_requarantine_on_an_error_path_does_not_lift_fail_closed() {
        let mut policy = ToolPolicy::default();
        policy.fail_closed_catalog = true;
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));

        // The error paths union the live set with the names that triggered the write.
        router.requarantine(["github__echo".to_string()].into_iter().collect());
        assert!(
            router.catalog_fail_closed(),
            "a store-error requarantine must not re-expose the catalog"
        );
        assert!(
            router.aggregated_tools().is_empty(),
            "the catalog must stay hidden while the store is still unreadable"
        );
        // The reported set is not the whole picture, so callers have to consult
        // catalog_fail_closed() as well.
        assert_eq!(router.quarantined().len(), 1);

        router.requarantine_from_store(BTreeSet::new());
        assert!(!router.catalog_fail_closed());
        assert!(!router.aggregated_tools().is_empty());
    }

    /// SBS-871: `Router::new()` is the startup placeholder, but a router that was
    /// really built and connected nothing is still a prior quarantine decision.
    #[test]
    fn sbs871_built_flag_separates_a_placeholder_from_a_live_empty_router() {
        assert!(!Router::new().is_built(), "placeholder is not a build");
        let built = Router::with_policy(ToolPolicy::default());
        assert!(built.is_built(), "a policy build is a real build");
        assert_eq!(built.server_count(), 0, "even with zero connected servers");
    }

    #[test]
    fn block_reason_matches_route_call_blocked_map() {
        let mut policy = ToolPolicy::default();
        policy.quarantined = ["github__echo".to_string()].into_iter().collect();
        let mut router = Router::with_policy(policy);
        router.add(mock_server("github"));

        assert!(router.block_reason("github__add").is_none());
        assert_eq!(
            router
                .block_reason("github__echo")
                .map(|r| r.contains("quarantined")),
            Some(true)
        );
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
        assert_eq!(prompt["completion"]["values"][0], "github:prompt:greet:py");

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
        assert!(uri_matches_template("file:///a/b/c.txt", "file:///{+path}"));
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
    fn team_feature_protections_keep_destructive_tools_exposed_and_callable() {
        for policy in [
            serde_json::json!({"forceQuarantineOnDrift": true}),
            serde_json::json!({"forceBlockOnInjection": true}),
            serde_json::json!({"minSafetyLevel": "ask"}),
            serde_json::json!({"minSafetyLevel": "strict"}),
        ] {
            let mut reg = crate::registry::Registry::default();
            reg.set_safety_level(crate::registry::SafetyLevel::Off);
            crate::teams::apply_team_config(
                &mut reg,
                "t1",
                &serde_json::json!({"servers": [], "screeningPolicy": policy}),
            );
            let strict = reg.safety_level_effective() == crate::registry::SafetyLevel::Strict;
            let mut router = Router::with_policy(ToolPolicy {
                deny_destructive: reg.deny_destructive_effective(),
                ..Default::default()
            });
            router.add(DownstreamServer::connect("db".into(), Box::new(DestructiveMock)).unwrap());
            assert_eq!(
                router
                    .aggregated_tools()
                    .iter()
                    .any(|t| t["name"] == "db__drop_table"),
                !strict
            );
            assert_eq!(
                router
                    .route_call("db__drop_table", serde_json::json!({}))
                    .is_ok(),
                !strict
            );
        }
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
        assert_eq!(
            names,
            vec!["db__echo"],
            "only the allow-listed tool is exposed"
        );

        // Hidden, and also blocked on a direct call (not merely invisible).
        let err = router.route_call("db__add", json!({})).unwrap_err();
        assert!(
            err.contains("outside this client's tool scope"),
            "unexpected: {err}"
        );
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
        assert_eq!(
            names(&router),
            before,
            "refresh shuffled the collision suffixes"
        );
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
                        Ok(
                            json!({ "messages": [{ "role": "user", "content": format!("gp:{name}") }] }),
                        )
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
                "tools/list" => {
                    Ok(json!({ "tools": [{ "name": "boom", "description": "always fails" }] }))
                }
                "tools/call" => Err(TransportError::Fatal("HTTP 500: server error".to_string())),
                other => Err(TransportError::Fatal(format!("unexpected method {other}"))),
            }
        }
        fn notify(&mut self, _method: &str, _params: Value) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn retry_server(
        id: &str,
        tool_failures: u32,
        resource_failures: u32,
        prompt_failures: u32,
    ) -> DownstreamServer {
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
        assert!(
            fast_result.is_ok(),
            "fast server should not block behind slow retry"
        );

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
        assert!(
            result_b.is_ok(),
            "same-server call should succeed during backoff sleep"
        );
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
        assert_eq!(
            entries.load(Ordering::SeqCst),
            1,
            "first attempt must run once"
        );
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
        assert!(
            breaker.open_until.is_none(),
            "cancellation must leave the breaker closed"
        );
        cancellations.finish_client_request("cancel-retry-1");
    }

    fn failure(message: &str, needs_auth: bool) -> ConnectFailure {
        ConnectFailure {
            message: message.to_string(),
            needs_auth,
        }
    }

    /// A connect factory that fails `fail` times, then connects `mock_server(id)`.
    fn flaky_connect(id: &'static str, fail: usize, calls: Arc<AtomicU64>) -> Connect {
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) as usize;
            if n < fail {
                Err(failure("Temporary failure in name resolution", false))
            } else {
                Ok(mock_server(id))
            }
        })
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    fn supervised_fixture(connect: Connect) -> Router {
        let mut router = Router::new();
        router.add_supervised(
            "s".to_string(),
            vec![json!({"name":"echo"})],
            connect,
            ReconnectBackoff {
                base: Duration::from_millis(10),
                cap: Duration::from_millis(40),
            },
            json!({"revision":1}),
        );
        router
    }

    fn ready_supervisor(router: &mut Router) {
        assert!(wait_until(|| router.has_ready_reconnects()));
        router.adopt_ready_reconnects();
        router.activate_supervisors();
    }

    #[test]
    fn supervisor_lazy_start_is_single_flight_and_cached_discovery_stays_stopped() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = supervised_fixture(flaky_connect("s", 0, Arc::clone(&calls)));
        router.maintain_supervisors();
        router.discover_uncached(|_| true);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(router.aggregated_tools()[0]["name"], "s__echo");
        let slot = Arc::clone(&router.servers[0]);
        let attempts: Vec<_> = (0..16)
            .map(|_| {
                let slot = Arc::clone(&slot);
                std::thread::spawn(move || slot.start(true))
            })
            .collect();
        assert_eq!(
            attempts
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|started| *started)
                .count(),
            1
        );
        ready_supervisor(&mut router);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn supervisor_busy_inner_never_blocks_degrade_or_lifecycle_inspection() {
        let mut router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        router.prepare_lazy_use("s");
        ready_supervisor(&mut router);
        let slot = Arc::clone(&router.servers[0]);
        let inner = slot.inner.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_slot = Arc::clone(&slot);
        let worker = std::thread::spawn(move || {
            tx.send(worker_slot.degrade(
                worker_slot.generation.load(Ordering::Acquire),
                0,
                "offline",
                true,
            ))
            .unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(1));
        drop(inner);
        worker.join().unwrap();
        assert_eq!(
            result.unwrap(),
            false,
            "busy serial request must fall back to breaker"
        );

        let inner = slot.inner.lock().unwrap();
        let worker_slot = Arc::clone(&slot);
        let worker = std::thread::spawn(move || {
            Router::attempt(&worker_slot, SlotAccess::Shared, None, &mut |_| Ok(()))
        });
        assert!(wait_until(|| slot.handle_calls.load(Ordering::Acquire) == 1));
        let free = wait_until(|| slot.supervisor.as_ref().unwrap().try_lock().is_ok());
        drop(inner);
        worker.join().unwrap().0.unwrap();
        assert!(free, "queued call held the supervisor behind inner");
        router.maintain_supervisors();
        assert!(router.pending_statuses().is_empty());
        assert_eq!(slot.handle_calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn supervisor_demand_shortens_long_backoff_and_ready_resets_failures() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = supervised_fixture(flaky_connect("s", 1, Arc::clone(&calls)));
        let slot = Arc::clone(&router.servers[0]);
        slot.supervisor.as_ref().unwrap().lock().unwrap().backoff = ReconnectBackoff {
            base: Duration::from_secs(300),
            cap: Duration::from_secs(300),
        };
        slot.start(true);
        assert!(wait_until(|| slot.status().unwrap().failures == 1));
        let last = slot
            .supervisor
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .last_attempt;
        assert!(!slot.start_at(true, last + Duration::from_secs(14)));
        assert!(!slot.start_at(false, last + Duration::from_secs(15)));
        assert!(slot.start_at(true, last + Duration::from_secs(15)));
        ready_supervisor(&mut router);
        assert_eq!(slot.status().unwrap().failures, 0);
        assert!(slot.status().unwrap().last_error.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn supervisor_every_dispatch_waits_after_idle_stop_including_approved_tools() {
        let mut router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        router.prepare_lazy_use("s");
        ready_supervisor(&mut router);
        for method in [
            "resources/read",
            "prompts/get",
            "completion/complete",
            "tools/call",
        ] {
            let slot = Arc::clone(&router.servers[0]);
            let last = slot.supervisor.as_ref().unwrap().lock().unwrap().last_use;
            slot.maintain(last + SERVER_IDLE_TIMEOUT);
            assert_eq!(
                slot.supervisor.as_ref().unwrap().lock().unwrap().state,
                SupervisorState::Stopped
            );
            let snapshot = router.clone();
            let worker = std::thread::spawn(move || match method {
                "resources/read" => snapshot.read_resource("s://readme"),
                "prompts/get" => snapshot.get_prompt("s__greet", json!({})),
                "completion/complete" => snapshot.complete(json!({
                    "ref":{"type":"ref/prompt","name":"s__greet"},
                    "argument":{"name":"name","value":"a"}
                })),
                _ => snapshot.route_call("s__echo", json!({})),
            });
            assert!(wait_until(|| router.has_ready_reconnects()));
            router.adopt_ready_reconnects();
            router.activate_supervisors();
            assert!(
                worker.join().unwrap().is_ok(),
                "{method} failed its first post-idle dispatch"
            );
        }
    }

    #[test]
    fn supervisor_subscriptions_keep_idle_connections_warm_until_last_holder_leaves() {
        let subscribed = Arc::new(AtomicBool::new(true));
        let mut router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        let used = Arc::clone(&subscribed);
        router.set_subscription_use("s", Arc::new(move || used.load(Ordering::Acquire)));
        router.prepare_lazy_use("s");
        ready_supervisor(&mut router);
        let slot = &router.servers[0];
        let last = slot.supervisor.as_ref().unwrap().lock().unwrap().last_use;
        slot.maintain(last + SERVER_IDLE_TIMEOUT);
        assert_eq!(
            slot.supervisor.as_ref().unwrap().lock().unwrap().state,
            SupervisorState::Ready
        );
        subscribed.store(false, Ordering::Release);
        slot.maintain(last + SERVER_IDLE_TIMEOUT);
        assert_eq!(
            slot.supervisor.as_ref().unwrap().lock().unwrap().state,
            SupervisorState::Stopped
        );
    }

    #[test]
    fn supervisor_catalog_save_skips_busy_server_without_losing_its_cache() {
        let router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        let inner = router.servers[0].inner.lock().unwrap();
        assert!(router.raw_catalogs().is_none());
        drop(inner);
        assert_eq!(router.raw_catalogs().unwrap()["s"].materialize(0)["name"], "echo");
    }

    #[test]
    fn supervisor_background_retry_waits_for_backoff_and_auth_waits_for_replacement() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = supervised_fixture(flaky_connect("s", 1, Arc::clone(&calls)));
        router.servers[0].start(true);
        assert!(wait_until(
            || router.servers[0].status().unwrap().failures == 1
        ));
        assert!(router.servers[0]
            .unavailable()
            .contains("has not connected yet"));
        let (last_attempt, retry_at) = {
            let state = router.servers[0].supervisor.as_ref().unwrap().lock().unwrap();
            (state.last_attempt, state.next_attempt)
        };
        assert!(!router.servers[0].start_at(true, last_attempt));
        assert!(router.servers[0].start_at(false, retry_at));
        ready_supervisor(&mut router);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let auth_calls = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&auth_calls);
        let auth: Connect = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Err(failure("HTTP 401", true))
        });
        let router = supervised_fixture(auth);
        router.servers[0].start(true);
        assert!(wait_until(|| router.servers[0]
            .status()
            .unwrap()
            .needs_auth));
        for _ in 0..10 {
            router.maintain_supervisors();
            router.servers[0].start(true);
        }
        assert_eq!(auth_calls.load(Ordering::SeqCst), 1);
        assert!(router.servers[0].unavailable().contains("needs sign-in"));
    }

    #[test]
    fn supervisor_idle_stop_keeps_catalog_and_active_calls_keep_it_warm() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = supervised_fixture(flaky_connect("s", 0, Arc::clone(&calls)));
        router.servers[0].start(true);
        ready_supervisor(&mut router);
        let slot = &router.servers[0];
        let last_use = slot.supervisor.as_ref().unwrap().lock().unwrap().last_use;
        slot.handle_calls.store(1, Ordering::SeqCst);
        slot.maintain(last_use + SERVER_IDLE_TIMEOUT);
        assert_eq!(
            slot.supervisor.as_ref().unwrap().lock().unwrap().state,
            SupervisorState::Ready
        );
        slot.handle_calls.store(0, Ordering::SeqCst);
        slot.maintain(last_use + SERVER_IDLE_TIMEOUT);
        assert_eq!(
            slot.supervisor.as_ref().unwrap().lock().unwrap().state,
            SupervisorState::Stopped
        );
        assert!(slot.unavailable().contains("is restarting"));
        assert!(!router.aggregated_tools().is_empty());
        slot.start(true);
        ready_supervisor(&mut router);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn supervisor_idle_stop_keeps_prompts_and_resources_without_rediscovery() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = supervised_fixture(flaky_connect("s", 0, Arc::clone(&calls)));
        router.servers[0].start(true);
        ready_supervisor(&mut router);
        // A prompts and resources only server has no tools to cache.
        router.servers[0].inner.lock().unwrap().tools.clear();
        let slot = Arc::clone(&router.servers[0]);
        let last_use = slot.supervisor.as_ref().unwrap().lock().unwrap().last_use;
        router.maintain_supervisors_at(last_use + SERVER_IDLE_TIMEOUT);
        router.demand_servers(|_| true);
        router.discover_uncached(|_| true);
        assert_eq!(
            slot.supervisor.as_ref().unwrap().lock().unwrap().state,
            SupervisorState::Stopped,
            "a list restarted an idle server with a complete catalog"
        );
        assert!(!router.any_discovering(|_| true));
        assert!(!router.aggregated_prompts().is_empty());
        assert!(!router.aggregated_resources().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A server that never published still loads its first full catalog.
        let fresh = supervised_fixture(flaky_connect("s", 0, Arc::new(AtomicU64::new(0))));
        fresh.demand_servers(|_| true);
        assert!(fresh.any_discovering(|_| true));
    }

    #[test]
    fn supervisor_startup_result_wakes_a_waiting_publisher() {
        let router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        let mut seen = started_supervisors();
        let started = Instant::now();
        let deadline = started + Duration::from_secs(5);
        router.servers[0].start(true);
        while !router.has_ready_reconnects() && Instant::now() < deadline {
            seen = wait_for_started_supervisor(seen, deadline);
        }
        assert!(router.has_ready_reconnects());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the publisher waited for its timeout instead of the result"
        );
    }

    #[test]
    fn supervisor_scoped_adoption_leaves_other_servers_to_their_owner() {
        let mut router = supervised_fixture(Arc::new(|| Ok(mock_server("s"))));
        router.servers[0].start(true);
        assert!(wait_until(|| router.has_ready_reconnects()));
        assert!(!router.has_ready_reconnects_for(|id| id == "other"));
        assert!(router
            .adopt_ready_reconnects_for(|id| id == "other")
            .is_empty());
        router.activate_supervisors_for(|id| id == "other");
        assert!(router.has_ready_reconnects_for(|id| id == "s"));
        assert_eq!(router.adopt_ready_reconnects_for(|id| id == "s"), vec!["s"]);
        router.activate_supervisors_for(|id| id == "s");
        assert!(router.pending_statuses().is_empty());
    }

    #[test]
    fn supervisor_unsubscribe_never_starts_a_stopped_server() {
        let calls = Arc::new(AtomicU64::new(0));
        let router = supervised_fixture(flaky_connect("s", 0, Arc::clone(&calls)));
        let started = Instant::now();
        assert!(router
            .unsubscribe_resource_on_server("s", "s://readme")
            .is_ok());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!router.any_starting(|_| true));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn supervisor_waiter_on_a_replaced_start_fails_fast_and_discards_its_result() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let calls = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&calls);
        let old = supervised_fixture(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10));
            Ok(mock_server("s"))
        }));
        let waiter = {
            let old = old.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                (old.wait_for_server("s", None, false), started.elapsed())
            })
        };
        assert!(wait_until(|| old.lazy_starting("s")));
        let replacement = supervised_fixture(flaky_connect("s", 0, Arc::new(AtomicU64::new(0))));
        old.retire_replaced_supervisors(&replacement);
        let (result, waited) = waiter.join().unwrap();
        assert!(
            result.unwrap_err().contains("reconfigured while starting"),
            "waiter did not fail fast"
        );
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
        release_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(!old.has_ready_reconnects(), "a retired start published");
        assert!(
            !old.servers[0].start(true),
            "a retired supervisor started again"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn supervisor_unavailable_message_matches_the_demand_retry() {
        let calls = Arc::new(AtomicU64::new(0));
        let router = supervised_fixture(flaky_connect("s", 1, Arc::clone(&calls)));
        let slot = Arc::clone(&router.servers[0]);
        slot.supervisor.as_ref().unwrap().lock().unwrap().backoff = ReconnectBackoff {
            base: Duration::from_secs(300),
            cap: Duration::from_secs(300),
        };
        slot.start(true);
        assert!(wait_until(|| slot.status().unwrap().failures == 1));
        let message = slot.unavailable();
        let seconds: u64 = message
            .split("retry in ")
            .nth(1)
            .and_then(|rest| rest.split('s').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no retry delay in {message}"));
        assert!(
            (1..=15).contains(&seconds),
            "message promised the background schedule: {message}"
        );
    }

    #[test]
    fn supervisor_results_wait_for_integrity_publication_and_removed_starts_are_discarded() {
        let mut router = supervised_fixture(flaky_connect("s", 0, Arc::new(AtomicU64::new(0))));
        router.servers[0].start(true);
        assert!(wait_until(|| router.has_ready_reconnects()));
        router.adopt_ready_reconnects();
        // An adopted, unpublished slot refuses dispatch without waiting.
        let (result, _) = Router::attempt(
            &router.servers[0],
            SlotAccess::Shared,
            None,
            &mut |_| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("restarting"));
        router.activate_supervisors();
        assert!(router.route_call("s__echo", json!({})).is_ok());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let finish_rx = Arc::new(Mutex::new(finish_rx));
        let connect: Connect = Arc::new(move || {
            started_tx.send(()).unwrap();
            finish_rx.lock().unwrap().recv().unwrap();
            Ok(mock_server("s"))
        });
        let router = supervised_fixture(connect);
        let weak = Arc::downgrade(&router.servers[0]);
        router.servers[0].start(true);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(router);
        assert!(
            weak.upgrade().is_none(),
            "the start thread retained a removed supervisor"
        );
        finish_tx.send(()).unwrap();
    }

    #[test]
    fn supervisor_recovers_an_uncertain_call_without_replaying_its_effect() {
        let effects = Arc::new(AtomicU32::new(0));
        let fresh_effects = Arc::clone(&effects);
        let connect: Connect = Arc::new(move || {
            Ok(lost_reply_server(
                "tools/call",
                false,
                Arc::clone(&fresh_effects),
            ))
        });
        let mut router = Router::new().with_supervised_launch(
            lost_reply_server("tools/call", true, Arc::clone(&effects)),
            connect,
            ReconnectBackoff {
                base: Duration::from_millis(10),
                cap: Duration::from_millis(20),
            },
        );
        router.activate_supervisors();
        {
            let mut breaker = router.servers[0].breaker.lock().unwrap();
            breaker.consecutive_failures = BREAKER_FAILURE_THRESHOLD;
            breaker.open_until = Some(Instant::now() - Duration::from_secs(1));
        }
        let error = router.route_call("s__echo", json!({})).unwrap_err();
        assert!(error.contains("restarting"));
        assert!(error.contains("may have completed"));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert!(wait_until(|| {
            router.maintain_supervisors();
            router.has_ready_reconnects()
        }));
        ready_supervisor(&mut router);
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "recovery replayed the previous operation"
        );
        assert!(router.route_call("s__echo", json!({})).is_ok());
        assert_eq!(effects.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn supervisor_start_panic_enters_backoff_instead_of_staying_starting() {
        let router = supervised_fixture(Arc::new(|| panic!("fixture startup panic")));
        router.servers[0].start(true);
        assert!(wait_until(
            || router.servers[0].status().unwrap().failures == 1
        ));
        assert_eq!(
            router.servers[0]
                .supervisor
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .state,
            SupervisorState::Backoff
        );
    }

    #[test]
    fn supervisor_reuses_only_matching_launch_specs() {
        let previous = supervised_fixture(flaky_connect("s", 0, Arc::new(AtomicU64::new(0))));
        let mut same = Router::new();
        assert!(same.reuse_supervisor(&previous, "s", &json!({"revision":1})));
        assert!(same
            .server_slot("s")
            .unwrap()
            .ptr_eq(&previous.server_slot("s").unwrap()));
        let mut changed = Router::new();
        assert!(!changed.reuse_supervisor(&previous, "s", &json!({"revision":2})));
    }

    #[test]
    fn reconnect_backoff_doubles_from_base_and_caps() {
        let backoff = ReconnectBackoff::default();
        let secs: Vec<u64> = (1..=10).map(|n| backoff.delay(n).as_secs()).collect();
        assert_eq!(secs, vec![2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        assert_eq!(backoff.delay(u32::MAX), Duration::from_secs(300));
    }

    #[test]
    fn pending_state_waits_out_its_backoff_with_jitter() {
        let backoff = ReconnectBackoff::default();
        let t0 = Instant::now();
        let mut state = PendingState::new(failure("dns", false), &backoff, t0, 1.2);
        assert!(!state.due(t0 + Duration::from_millis(2399)));
        assert!(state.due(t0 + Duration::from_millis(2400)));
        state.begin(t0 + Duration::from_secs(3));
        assert!(
            !state.due(t0 + Duration::from_secs(60)),
            "one attempt at a time"
        );
        state.record_failure(
            failure("dns", false),
            &backoff,
            t0 + Duration::from_secs(4),
            0.8,
        );
        assert_eq!(state.failures, 2);
        assert!(!state.due(t0 + Duration::from_millis(7199)));
        assert!(state.due(t0 + Duration::from_millis(7200)));
    }

    #[test]
    fn a_kick_is_rate_limited_by_the_backoff_step() {
        let backoff = ReconnectBackoff::default();
        let t0 = Instant::now();
        let mut state = PendingState::new(failure("dns", false), &backoff, t0, 1.0);
        for _ in 0..7 {
            state.record_failure(failure("dns", false), &backoff, t0, 1.0);
        }
        // 8 failures: the schedule waits 256 s, a demand-driven retry only 30 s.
        assert!(!state.kickable(&backoff, t0 + Duration::from_secs(29)));
        assert!(state.kickable(&backoff, t0 + Duration::from_secs(30)));
        assert!(!state.due(t0 + Duration::from_secs(30)));
    }

    #[test]
    fn an_auth_failure_is_never_retried_on_a_schedule_or_by_demand() {
        let calls = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&calls);
        let connect: Connect = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(failure("HTTP 401 Unauthorized", true))
        });
        let mut router = Router::new();
        router.add_pending(
            "atlassian".into(),
            failure("HTTP 401 Unauthorized", true),
            connect,
            ReconnectBackoff {
                base: Duration::ZERO,
                cap: Duration::ZERO,
            },
        );
        for _ in 0..20 {
            assert_eq!(router.start_due_reconnects(), 0);
            let _ = router.route_call("atlassian__search", json!({}));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let err = router
            .route_call("atlassian__search", json!({}))
            .unwrap_err();
        assert!(err.contains("needs sign-in"), "{err}");
        let status = &router.pending_statuses()[0];
        assert!(status.needs_auth && status.retry_in.is_none());
    }

    #[test]
    fn a_pending_server_joins_the_catalog_in_build_order_after_retries() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = Router::new();
        router.add(mock_server("alpha"));
        router.add_pending(
            "beta".into(),
            failure("Temporary failure in name resolution", false),
            flaky_connect("beta", 1, Arc::clone(&calls)),
            ReconnectBackoff {
                base: Duration::ZERO,
                cap: Duration::ZERO,
            },
        );
        router.add(mock_server("gamma"));
        let err = router.route_call("beta__echo", json!({})).unwrap_err();
        assert!(err.contains("has not connected yet"), "{err}");
        assert!(err.contains("address could not be resolved"), "{err}");

        // First retry fails (call #1), the second connects (call #2).
        assert!(wait_until(|| {
            router.start_due_reconnects();
            router.has_ready_reconnects()
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let snapshot = router.clone();
        assert_eq!(router.adopt_ready_reconnects(), vec!["beta".to_string()]);
        assert!(router.pending_statuses().is_empty());
        let order: Vec<&str> = router.servers.iter().map(|slot| slot.id.as_str()).collect();
        assert_eq!(order, ["alpha", "beta", "gamma"]);
        let result = router.route_call("beta__echo", json!({})).unwrap();
        assert_eq!(result["content"][0]["text"], "beta:echo");

        // An older snapshot still holding the entry never starts another attempt.
        assert_eq!(snapshot.start_due_reconnects(), 0);
        let _ = snapshot.route_call("beta__echo", json!({}));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_cancelled_pending_server_stops_retrying_and_drops_its_result() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut router = Router::new();
        router.add_pending(
            "beta".into(),
            failure("dns", false),
            flaky_connect("beta", 0, Arc::clone(&calls)),
            ReconnectBackoff {
                base: Duration::ZERO,
                cap: Duration::ZERO,
            },
        );
        assert!(wait_until(|| {
            router.start_due_reconnects();
            router.has_ready_reconnects()
        }));
        for handle in router.pending_handles() {
            handle.cancel();
        }
        assert!(!router.has_ready_reconnects());
        assert_eq!(router.start_due_reconnects(), 0);
        assert!(router.adopt_ready_reconnects().is_empty());
        assert!(router.kick_pending("beta__echo", |_| true).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_kick_respects_the_callers_scope() {
        let mut router = Router::new();
        router.add_pending(
            "secret-team".into(),
            failure("dns", false),
            Arc::new(|| Err(failure("dns", false))),
            ReconnectBackoff::default(),
        );
        assert!(router
            .kick_pending("secret_team__x", |server| server != "secret-team")
            .is_none());
        let err = router.no_route_message_within("secret_team__x", |_| false);
        assert_eq!(err, "no route for tool 'secret_team__x'");
        assert!(router.kick_pending("secret_team__x", |_| true).is_some());
        assert!(router.kick_pending("secret-team", |_| true).is_some());
    }
    #[test]
    fn client_safe_error_never_repeats_downstream_text() {
        for (raw, expected) in [
            (
                "downstream server exited (status 3): invalid token ghp_secret123",
                "the server process exited (status 3)",
            ),
            (
                "HTTP 401 (needs authentication): token refresh failed: body sk-live-1",
                "the server answered HTTP 401",
            ),
            (
                "Temporary failure in name resolution",
                "the server's address could not be resolved",
            ),
            (
                "could not read secret 'API_KEY' from the vault: locked",
                "its stored credentials could not be read",
            ),
            (
                "failed to spawn 'npx': No such file",
                "the server command could not be started",
            ),
            (
                "timed out waiting for 'initialize' response",
                "the server did not answer in time",
            ),
            (
                "write failed: Broken pipe (os error 32)",
                "the server closed the connection",
            ),
            ("mock said: hunter2", "the connection failed"),
        ] {
            let shown = client_safe_error(raw);
            assert!(shown.starts_with(expected), "{raw} -> {shown}");
            for secret in ["ghp_secret123", "sk-live-1", "API_KEY", "hunter2", "npx"] {
                assert!(!shown.contains(secret), "{raw} -> {shown}");
            }
        }
    }

    #[test]
    fn jitter_never_pushes_a_retry_past_the_cap() {
        let backoff = ReconnectBackoff::default();
        let t0 = Instant::now();
        let mut state = PendingState::new(failure("dns", false), &backoff, t0, 1.2);
        for _ in 0..12 {
            state.record_failure(failure("dns", false), &backoff, t0, 1.2);
        }
        assert_eq!(state.next_attempt, t0 + backoff.cap);
    }

    fn destructive_db() -> DownstreamServer {
        DownstreamServer::connect("db".to_string(), Box::new(DestructiveMock)).unwrap()
    }

    fn exposed_names(router: &Router) -> Vec<String> {
        router
            .aggregated_tools()
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(String::from))
            .collect()
    }

    /// P1.3: every dispatch path goes through `authorize`, so a server outside the
    /// policy's server set is refused for tools, resources, subscriptions, prompts
    /// and completions alike. Unsubscribe cleanup still reaches it.
    #[test]
    fn authorize_refuses_every_dispatch_to_a_server_outside_the_policy() {
        let mut router = Router::with_policy(ToolPolicy {
            servers: Some(HashSet::from(["a".to_string()])),
            ..Default::default()
        });
        router.add(mock_server("a"));
        router.add(mock_server("b"));

        assert!(router.route_call("a__echo", json!({})).is_ok());
        let tool = router.route_call("b__echo", json!({})).unwrap_err();
        assert!(tool.contains("turned off"), "{tool}");
        assert!(!exposed_names(&router).contains(&"b__echo".to_string()));

        assert!(router.read_resource("a://readme").is_ok());
        let read = router.read_resource("b://readme").unwrap_err();
        assert!(read.contains("server 'b' is turned off"), "{read}");
        let sub = router.subscribe_resource("b://readme").unwrap_err();
        assert!(sub.contains("server 'b' is turned off"), "{sub}");
        assert!(router
            .unsubscribe_resource_on_server("b", "b://readme")
            .is_ok());

        assert!(router.get_prompt("a__greet", json!({})).is_ok());
        let prompt = router.get_prompt("b__greet", json!({})).unwrap_err();
        assert!(prompt.contains("server 'b' is turned off"), "{prompt}");

        let complete = |name: &str| {
            router.complete(json!({
                "ref": { "type": "ref/prompt", "name": name },
                "argument": { "name": "x", "value": "y" }
            }))
        };
        assert!(complete("a__greet").is_ok());
        let completion = complete("b__greet").unwrap_err();
        assert!(
            completion.contains("server 'b' is turned off"),
            "{completion}"
        );
    }

    /// P1.3 / REL-02: a registry policy republished on a live router takes effect on
    /// the connections it already holds, with no rebuild, and leaves quarantine alone.
    #[test]
    fn republished_registry_policy_applies_without_reconnecting() {
        let mut router = Router::with_policy(ToolPolicy {
            servers: Some(HashSet::from(["db".to_string()])),
            ..Default::default()
        });
        router.add(destructive_db());
        router.requarantine_from_store(BTreeSet::from(["db__list_tables".to_string()]));
        assert!(router.route_call("db__drop_table", json!({})).is_ok());

        let mut policy = router.registry_policy();
        policy.deny_destructive = true;
        assert!(router.apply_registry_policy(policy.clone()));
        assert!(
            !router.apply_registry_policy(policy),
            "an identical policy is not a change"
        );

        let err = router.route_call("db__drop_table", json!({})).unwrap_err();
        assert!(err.contains("destructive-tool policy"), "{err}");
        assert!(!exposed_names(&router).contains(&"db__drop_table".to_string()));
        assert!(
            router.route_call("db__list_tables", json!({})).is_err(),
            "quarantine must survive a registry policy republish"
        );
        assert_eq!(
            router.quarantined(),
            &BTreeSet::from(["db__list_tables".to_string()])
        );

        // Turning the switch back off restores the tool on the same connection.
        let mut policy = router.registry_policy();
        policy.deny_destructive = false;
        assert!(router.apply_registry_policy(policy));
        assert!(router.route_call("db__drop_table", json!({})).is_ok());
    }

    /// P1.3: a request keeps the router it arrived with. Rechecking against the live
    /// router refuses what the newer policy blocks, but ignores tool-granular scope,
    /// which daemon adapter views set for themselves.
    #[test]
    fn recheck_live_policy_applies_newer_policy_to_an_older_snapshot() {
        let mut snapshot = Router::with_policy(ToolPolicy {
            servers: Some(HashSet::from(["db".to_string()])),
            ..Default::default()
        });
        snapshot.add(destructive_db());

        let mut live = snapshot.clone();
        let mut policy = live.registry_policy();
        policy.deny_destructive = true;
        live.apply_registry_policy(policy);
        let err = snapshot
            .recheck_live_policy(&live, DispatchTarget::Tool("db__drop_table"))
            .unwrap_err();
        assert!(err.contains("destructive-tool policy"), "{err}");
        assert!(snapshot
            .recheck_live_policy(&live, DispatchTarget::Tool("db__list_tables"))
            .is_ok());

        let mut scoped = snapshot.clone();
        let mut policy = scoped.registry_policy();
        policy.allow =
            HashMap::from([("db".to_string(), HashSet::from(["list_tables".to_string()]))]);
        scoped.apply_registry_policy(policy);
        assert!(
            snapshot
                .recheck_live_policy(&scoped, DispatchTarget::Tool("db__drop_table"))
                .is_ok(),
            "the live base router's tool scope must not narrow an adapter view"
        );

        let mut off = snapshot.clone();
        let mut policy = off.registry_policy();
        policy.servers = Some(HashSet::new());
        off.apply_registry_policy(policy);
        assert!(snapshot
            .recheck_live_policy(&off, DispatchTarget::Tool("db__list_tables"))
            .unwrap_err()
            .contains("turned off"));
        assert!(snapshot
            .recheck_live_policy(&off, DispatchTarget::Server("db"))
            .is_err());
    }

    /// P1.3: a task on a server that was just turned off can no longer be polled or
    /// updated, but it can still be cancelled, like unsubscribe cleanup.
    #[test]
    fn task_cancel_still_reaches_a_server_that_was_turned_off() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router::with_policy(ToolPolicy {
            servers: Some(HashSet::from(["alpha".to_string()])),
            ..Default::default()
        });
        router.add(task_server("alpha", Arc::clone(&seen)));
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": crate::downstream::MODERN_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": { "io.modelcontextprotocol/tasks": {} }
            }
        });
        let started = router
            .route_call_with_cancel("alpha__job", json!({}), None, Some(&meta))
            .unwrap();
        let task_id = started["taskId"].as_str().unwrap();

        let mut policy = router.registry_policy();
        policy.servers = Some(HashSet::new());
        assert!(router.apply_registry_policy(policy));
        for method in ["tasks/get", "tasks/update"] {
            let err = router
                .route_task(method, json!({ "taskId": task_id }), None, Some(&meta))
                .unwrap_err();
            assert!(
                err.contains("server 'alpha' is turned off"),
                "{method}: {err}"
            );
        }
        let cancelled = router
            .route_task(
                "tasks/cancel",
                json!({ "taskId": task_id }),
                None,
                Some(&meta),
            )
            .unwrap();
        assert_eq!(cancelled["taskId"], task_id);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| method == "tasks/cancel"));
    }
}
