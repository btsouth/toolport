//! Gateway side of the MCP Skills passthrough (`io.modelcontextprotocol/skills`).
//!
//! The URI codec and entry rewriting live in `conduit_lib::mcp_skills`. This
//! module applies the request's scope, routes to the owning server and builds
//! the client-facing results. A namespaced URI is decoded and its server is
//! checked against the scoped server set before any upstream request. An
//! unprefixed `skill://` URI (an absolute link inside verbatim file content)
//! resolves only when exactly one in-scope server serves it.
//!
//! Served on every client protocol version. Modern clients learn of it from
//! `server/discover`; a handshake client from `initialize`, when it declared
//! `capabilities.extensions` itself (the pre-2026-07-28 negotiation form).

use super::*;
use conduit_lib::mcp_skills;

/// Bound on waiting for a lazily started server to finish its first connect,
/// so a skill URI from before a restart can still reach its server.
const START_WAIT: Duration = Duration::from_secs(30);

fn in_scope(server: &str, allowed: Option<&HashSet<String>>) -> bool {
    allowed.is_none_or(|scope| server_in_allowed_scope(server, scope))
}

fn is_skill_scheme(uri: &str) -> bool {
    uri.get(..mcp_skills::SKILL_SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(mcp_skills::SKILL_SCHEME))
}

fn declaration(router: &Router, allowed: Option<&HashSet<String>>) -> Option<Value> {
    let declarations = router.mcp_skills_servers(|server| in_scope(server, allowed));
    mcp_skills::aggregate_declaration(declarations.iter().map(|(_, settings)| settings))
}

/// Advertise the extension when at least one in-scope server declares it,
/// with `directoryRead: true` when any of them does. A per-setting mismatch
/// must not drop the id, so this replaces the generic aggregate's value.
pub(super) fn advertise(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    extensions: &mut serde_json::Map<String, Value>,
) {
    match declaration(router, allowed) {
        Some(settings) => {
            extensions.insert(mcp_skills::EXTENSION.to_string(), settings);
        }
        None => {
            extensions.remove(mcp_skills::EXTENSION);
        }
    }
}

/// `initialize` (2025-11-25 and earlier): advertise MCP Skills under
/// `capabilities.extensions` when the client declared that field itself, the
/// handshake-era negotiation form. Other extensions stay modern-only: their
/// client settings are per-request on 2026-07-28 and cannot be relayed from a
/// one-time handshake over shared downstream connections. MCP Skills has no
/// client settings, so nothing needs relaying.
pub(super) fn advertise_on_initialize(
    req: &Value,
    router: &Router,
    allowed: Option<&HashSet<String>>,
    capabilities: &mut Value,
) {
    let client_negotiates = req
        .pointer("/params/capabilities/extensions")
        .is_some_and(Value::is_object);
    if !client_negotiates {
        return;
    }
    if let Some(settings) = declaration(router, allowed) {
        capabilities["extensions"] = json!({ mcp_skills::EXTENSION: settings });
    }
}

/// The server's MCP Skills settings, starting it first if it is a lazy server
/// that has not connected yet.
fn skills_settings(router: &Router, server: &str) -> Option<Value> {
    if let Some(settings) = router.server_extension_settings(server, mcp_skills::EXTENSION) {
        return Some(settings);
    }
    router.demand_servers(|id| id == server);
    let deadline = Instant::now() + START_WAIT;
    while router.any_discovering(|id| id == server) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    router.server_extension_settings(server, mcp_skills::EXTENSION)
}

/// A client-facing `skill://` URI resolved to (server, original URI, settings).
/// `None` unless the decoded server exists in this router, is in the request's
/// scope, and declares the extension. Scope is checked before the server is
/// started or contacted.
fn resolve_exposed(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    uri: &str,
) -> Option<(String, String, Value)> {
    let (server, original) = mcp_skills::parse_exposed_uri(uri)?;
    if !router.has_server(&server) || !in_scope(&server, allowed) {
        return None;
    }
    let settings = skills_settings(router, &server)?;
    Some((server, original, settings))
}

/// Owner for a skill URI under another scheme, which Toolport does not
/// namespace: the in-scope declaring server that listed it as a resource, else
/// the only in-scope declaring server. Several candidates are ambiguous, so
/// nothing is guessed.
fn resolve_other_scheme(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    uri: &str,
) -> Option<(String, String, Value)> {
    let servers = router.mcp_skills_servers(|server| in_scope(server, allowed));
    if let Some(owner) = router.resource_server(uri) {
        return servers
            .into_iter()
            .find(|(server, _)| server == owner)
            .map(|(server, settings)| (server, uri.to_string(), settings));
    }
    match servers.as_slice() {
        [(server, settings)] => Some((server.clone(), uri.to_string(), settings.clone())),
        _ => None,
    }
}

fn not_served(id: Value, uri: &str) -> Value {
    error(
        id,
        -32602,
        &format!(
            "Toolport: no skill is served at '{}'",
            integrity::sanitize_wrapper_label(uri)
        ),
    )
}

fn downstream_error(id: Value, server: &str, message: &str) -> Value {
    error(
        id,
        -32602,
        &format!(
            "Toolport: {}",
            integrity::defend_error_text(&integrity::sanitize_wrapper_label(server), message)
        ),
    )
}

/// Several in-scope servers serve the same unprefixed `skill://` URI. Toolport
/// never guesses, so the client is told the namespaced URI of each.
fn ambiguous(id: Value, uri: &str, servers: &[String]) -> Value {
    let candidates: Vec<String> = servers
        .iter()
        .map(|server| mcp_skills::expose_uri(server, uri))
        .collect();
    let printable = candidates.iter().all(|candidate| {
        !candidate
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    });
    let listed = if printable {
        candidates.join(", ")
    } else {
        "(see error data)".to_string()
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32602,
            "message": format!(
                "Toolport: several servers serve this skill:// URI; use one of: {listed}"
            ),
            "data": { "candidates": candidates }
        }
    })
}

/// What a request on an unprefixed `skill://` URI asks for, which decides how
/// a server is shown to serve it.
#[derive(Clone, Copy, PartialEq)]
enum Unprefixed {
    /// `resources/read`: a listed resource or a file in a skill manifest.
    File,
    /// `skills/get`: a listed skill, or one the server answers `skills/get` for.
    Skill,
    /// `resources/directory/read`: an ancestor of a listed resource or file.
    Directory,
}

fn serves_listed(target: &str, candidate: &str, kind: Unprefixed) -> bool {
    match kind {
        Unprefixed::Directory => candidate
            .strip_prefix(target)
            .is_some_and(|rest| rest.starts_with('/')),
        _ => candidate == target,
    }
}

/// Which in-scope servers serve an unprefixed `skill://` URI, from their
/// resource lists and their `skills/list` manifests (no file is read). For
/// `skills/get` with no listed owner, declaring servers are asked
/// `skills/get` itself, since a skill need not be listed.
fn unprefixed_owners(request: &SkillsRequest<'_>, uri: &str, kind: Unprefixed) -> Vec<String> {
    let router = request.router;
    let mut owners: Vec<String> = Vec::new();
    fn add(owners: &mut Vec<String>, server: &str) {
        if !owners.iter().any(|owner| owner == server) {
            owners.push(server.to_string());
        }
    }
    if kind != Unprefixed::Skill {
        for server in router.server_ids() {
            if in_scope(&server, request.allowed)
                && router.server_resources(&server).iter().any(|row| {
                    row.get("uri")
                        .and_then(Value::as_str)
                        .is_some_and(|listed| serves_listed(uri, listed, kind))
                })
            {
                add(&mut owners, &server);
            }
        }
    }
    let declaring = router.mcp_skills_servers(|server| in_scope(server, request.allowed));
    for (server, _) in &declaring {
        let Ok(walked) = mcp_skills::walk_skill_pages(|cursor| {
            let params = cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            request.forward(server, "skills/list", params)
        }) else {
            continue;
        };
        let listed = walked.skills.iter().any(|entry| {
            let skill_uri = entry.get("uri").and_then(Value::as_str).unwrap_or("");
            match kind {
                Unprefixed::Skill => skill_uri == uri,
                _ => {
                    serves_listed(uri, skill_uri, kind)
                        || entry
                            .get("resources")
                            .and_then(Value::as_array)
                            .is_some_and(|files| {
                                files.iter().any(|file| {
                                    file.get("uri")
                                        .and_then(Value::as_str)
                                        .is_some_and(|file| serves_listed(uri, file, kind))
                                })
                            })
                }
            }
        });
        if listed {
            add(&mut owners, server);
        }
    }
    if kind == Unprefixed::Skill && owners.is_empty() {
        for (server, _) in &declaring {
            if request
                .forward(server, "skills/get", json!({ "uri": uri }))
                .is_ok()
            {
                add(&mut owners, server);
            }
        }
    }
    owners
}

/// Rows of `key` (`uri` or `uriTemplate`) a declaring server publishes under
/// `skill://` are replaced by their namespaced form, taken from that server's
/// own list so a first-writer collision cannot hide one. Another server's raw
/// row that decodes to a declaring server's namespace is dropped: requests for
/// that address route to the declaring server, so the row could not be used.
fn namespace_rows(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    mut rows: Vec<Value>,
    key: &str,
    owner_of: impl Fn(&str) -> Option<String>,
    rows_of: impl Fn(&str) -> Vec<Value>,
) -> Vec<Value> {
    let servers: Vec<String> = router
        .mcp_skills_servers(|server| in_scope(server, allowed))
        .into_iter()
        .map(|(server, _)| server)
        .collect();
    if servers.is_empty() {
        return rows;
    }
    let declaring: HashSet<&str> = servers.iter().map(String::as_str).collect();
    rows.retain(|row| {
        let Some(uri) = row.get(key).and_then(Value::as_str) else {
            return true;
        };
        if !uri.starts_with(mcp_skills::SKILL_SCHEME) {
            return true;
        }
        let owned_by_declaring =
            owner_of(uri).is_some_and(|owner| declaring.contains(owner.as_str()));
        let shadows = mcp_skills::parse_exposed_uri(uri)
            .is_some_and(|(server, _)| declaring.contains(server.as_str()));
        !owned_by_declaring && !shadows
    });
    for server in &servers {
        for mut row in rows_of(server) {
            let Some(uri) = row.get(key).and_then(Value::as_str) else {
                continue;
            };
            if !uri.starts_with(mcp_skills::SKILL_SCHEME) {
                continue;
            }
            row[key] = json!(mcp_skills::expose_uri(server, uri));
            rows.push(row);
        }
    }
    rows
}

/// `resources/list` rows with declaring servers' `skill://` URIs namespaced.
pub(super) fn list_resources(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    resources: Vec<Value>,
) -> Vec<Value> {
    namespace_rows(
        router,
        allowed,
        resources,
        "uri",
        |uri| router.resource_server(uri).map(str::to_string),
        |server| router.server_resources(server),
    )
}

/// `resources/templates/list` rows with declaring servers' `skill://`
/// templates namespaced, so an expanded URI has the same form as a listed one.
pub(super) fn list_resource_templates(
    router: &Router,
    allowed: Option<&HashSet<String>>,
    templates: Vec<Value>,
) -> Vec<Value> {
    namespace_rows(
        router,
        allowed,
        templates,
        "uriTemplate",
        |template| {
            router
                .resource_template_server(template)
                .map(str::to_string)
        },
        |server| router.server_resource_templates(server),
    )
}

/// Everything a skills handler needs from the dispatch.
pub(super) struct SkillsRequest<'a> {
    pub id: Value,
    pub req: &'a Value,
    pub reg: &'a Registry,
    pub router: &'a Router,
    pub allowed: Option<&'a HashSet<String>>,
    pub cancel: Option<downstream::CancelContext>,
    pub live_router: Option<&'a Arc<Mutex<Arc<Router>>>>,
    pub cache_scoped: bool,
}

impl SkillsRequest<'_> {
    fn params(&self) -> Option<&Value> {
        self.req.get("params")
    }

    fn uri(&self) -> Option<&str> {
        self.params()
            .and_then(|params| params.get("uri"))
            .and_then(Value::as_str)
    }

    fn client_meta(&self) -> Option<Value> {
        self.params()
            .and_then(|params| params.get("_meta"))
            .cloned()
    }

    /// Forward one request to `server`, with live-policy checks and progress
    /// relaying as `resources/read` has.
    fn forward(&self, server: &str, method: &str, params: Value) -> Result<Value, String> {
        let mrtr = MrtrRequest::from_params(self.params());
        let client_meta = self.client_meta();
        let (_progress_route, relay_owned) = prepare_progress(client_meta.as_ref(), server);
        let client_meta = relay_owned.or(client_meta);
        prepare_dispatch(
            self.router,
            self.live_router,
            DispatchTarget::Server(server),
            self.cancel.as_ref(),
            !mrtr.is_empty(),
        )?;
        self.router.mcp_skills_request(
            server,
            method,
            params,
            self.cancel.clone(),
            client_meta.as_ref(),
            (!mrtr.is_empty()).then_some(&mrtr),
        )
    }

    /// Resolve the request's `uri` to (server, original URI, settings), or
    /// the error response to send.
    fn resolve(&self, kind: Unprefixed) -> Result<(String, String, Value), Value> {
        let Some(uri) = self.uri() else {
            return Err(error(
                self.id.clone(),
                -32602,
                "Toolport: params.uri is required",
            ));
        };
        if let Some(target) = resolve_exposed(self.router, self.allowed, uri) {
            return Ok(target);
        }
        if !is_skill_scheme(uri) {
            return resolve_other_scheme(self.router, self.allowed, uri)
                .ok_or_else(|| not_served(self.id.clone(), uri));
        }
        let owners = unprefixed_owners(self, uri, kind);
        match owners.as_slice() {
            [server] => match skills_settings(self.router, server) {
                Some(settings) => Ok((server.clone(), uri.to_string(), settings)),
                None => Err(not_served(self.id.clone(), uri)),
            },
            [] => Err(not_served(self.id.clone(), uri)),
            _ => Err(ambiguous(self.id.clone(), uri, &owners)),
        }
    }
}

/// `skills/list`, `skills/get` and `resources/directory/read`.
pub(super) fn handle(method: &str, request: SkillsRequest<'_>) -> Value {
    if declaration(request.router, request.allowed).is_none() {
        return error(request.id, -32601, &format!("Method not found: {method}"));
    }
    match method {
        "skills/list" => list_skills(request),
        "skills/get" => get_skill(request),
        _ => read_directory(request),
    }
}

fn list_skills(request: SkillsRequest<'_>) -> Value {
    let id = request.id.clone();
    if request
        .params()
        .and_then(|params| params.get("cursor"))
        .is_some_and(|cursor| !cursor.is_null())
    {
        // The merged listing is never paginated, so Toolport issued no cursor.
        return error(id, -32602, "Toolport: invalid skills/list cursor");
    }
    let servers = request
        .router
        .mcp_skills_servers(|server| in_scope(server, request.allowed));
    let mut skills = Vec::new();
    let mut hints = Vec::new();
    let mut partial = false;
    let mut other_scheme: HashSet<String> = HashSet::new();
    for (server, _) in &servers {
        let walked = mcp_skills::walk_skill_pages(|cursor| {
            let params = cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            request.forward(server, "skills/list", params)
        });
        let walked = match walked {
            Ok(walked) => walked,
            Err(e) => {
                gtrace(&format!("skills/list: skipping '{server}': {e}"));
                partial = true;
                continue;
            }
        };
        partial |= !walked.complete;
        hints.push(walked.hint);
        for mut entry in walked.skills {
            let uri = entry.get("uri").and_then(Value::as_str).unwrap_or("");
            // Another scheme is not namespaced, so the same URI from two servers
            // would be one identity. Keep the first server's, as resources do.
            if !uri.starts_with(mcp_skills::SKILL_SCHEME) && !other_scheme.insert(uri.to_string()) {
                eprintln!(
                    "toolport: MCP Skills URI collision on '{}': keeping the first server's entry, refusing '{server}'",
                    integrity::sanitize_wrapper_label(uri)
                );
                continue;
            }
            mcp_skills::expose_skill_entry(server, &mut entry);
            skills.push(entry);
        }
    }
    gtrace(&format!(
        "skills/list -> {} skills from {} servers{}",
        skills.len(),
        servers.len(),
        if partial { " (partial)" } else { "" }
    ));
    success(
        id,
        cacheable_for_upstream(
            json!({ "skills": skills }),
            mcp_skills::merged_listing_hint(hints, partial),
            request.cache_scoped,
        ),
    )
}

fn get_skill(request: SkillsRequest<'_>) -> Value {
    let id = request.id.clone();
    let (server, original, _) = match request.resolve(Unprefixed::Skill) {
        Ok(target) => target,
        Err(response) => return response,
    };
    match request.forward(&server, "skills/get", json!({ "uri": original })) {
        Ok(result) => {
            let Some(mut skill) = result.get("skill").cloned().filter(Value::is_object) else {
                return downstream_error(id, &server, "skills/get result has no skill object");
            };
            mcp_skills::expose_skill_entry(&server, &mut skill);
            success(
                id,
                cacheable_for_upstream(
                    json!({ "skill": skill }),
                    CacheHint::from_result(&result),
                    request.cache_scoped,
                ),
            )
        }
        Err(e) => downstream_error(id, &server, &e),
    }
}

fn read_directory(request: SkillsRequest<'_>) -> Value {
    let id = request.id.clone();
    let (server, original, settings) = match request.resolve(Unprefixed::Directory) {
        Ok(target) => target,
        Err(response) => return response,
    };
    if !mcp_skills::declares_directory_read(&settings) {
        // The owning server did not declare directoryRead, so for it this is
        // an unknown method, and it must not be called (extension rule).
        return error(id, -32601, "Method not found: resources/directory/read");
    }
    let mut params = json!({ "uri": original });
    if let Some(cursor) = request
        .params()
        .and_then(|params| params.get("cursor"))
        .filter(|cursor| !cursor.is_null())
    {
        // A directory cursor is the owning server's own token; the URI already
        // names the server, so it passes through unchanged.
        params["cursor"] = cursor.clone();
    }
    match request.forward(&server, "resources/directory/read", params) {
        Ok(mut result) => {
            let hint = CacheHint::from_result(&result);
            mcp_skills::expose_directory_result(&server, &mut result);
            let mut out = json!({
                "resources": result.get("resources").cloned().unwrap_or_else(|| json!([]))
            });
            if let Some(cursor) = result.get("nextCursor").filter(|cursor| cursor.is_string()) {
                out["nextCursor"] = cursor.clone();
            }
            out["_meta"] = json!({
                mcp_skills::PROVENANCE_KEY: result["_meta"][mcp_skills::PROVENANCE_KEY].clone()
            });
            success(id, cacheable_for_upstream(out, hint, request.cache_scoped))
        }
        Err(e) => downstream_error(id, &server, &e),
    }
}

/// `resources/read` of a skill file: a namespaced URI, or an unprefixed
/// `skill://` URI exactly one in-scope declaring server serves. `None` when it
/// is neither, so the ordinary resource path (with its own scope guard)
/// handles it; that includes an unprefixed URI only a server without the
/// extension lists.
///
/// A skill file is delivered verbatim, byte for byte, so the digest and size
/// its listing published stay true and the skill loads. Skill files are
/// server-published instructions, not user data, so PII pseudonymization is
/// not applied. Toolport-voice neutralization and content defense run only as
/// detectors on a copy: the read is refused (with an audit entry) when the
/// file forges Toolport's own voice or content defense would block it.
/// Altered bytes are never returned.
pub(super) fn read_skill_file(request: SkillsRequest<'_>) -> Option<Value> {
    let uri = request.uri()?.to_string();
    let (server, original) = match resolve_exposed(request.router, request.allowed, &uri) {
        Some((server, original, _)) => (server, original),
        None if is_skill_scheme(&uri) => {
            let owners = unprefixed_owners(&request, &uri, Unprefixed::File);
            let declaring: Vec<String> = owners
                .iter()
                .filter(|owner| skills_settings(request.router, owner).is_some())
                .cloned()
                .collect();
            match (owners.len(), declaring.as_slice()) {
                (1, [server]) => (server.clone(), uri.clone()),
                (n, _) if n > 1 => return Some(ambiguous(request.id.clone(), &uri, &owners)),
                _ => return None,
            }
        }
        None => return None,
    };
    let id = request.id.clone();
    let mut result = match request.forward(&server, "resources/read", json!({ "uri": original })) {
        Ok(result) => result,
        Err(e) => return Some(downstream_error(id, &server, &e)),
    };
    if let Some(reason) = skill_file_refusal(request.reg, &server, &result) {
        integrity::record_skill_file_refusal(&server, &original, reason);
        return Some(error(
            id,
            -32603,
            &format!(
                "Toolport: refused skill file '{}' from server '{}': {reason}. Skill files are delivered unchanged or not at all.",
                integrity::sanitize_wrapper_label(&uri),
                integrity::sanitize_wrapper_label(&server)
            ),
        ));
    }
    integrity::label_untrusted_result(&server, &mut result);
    mcp_skills::expose_read_result(&server, &mut result);
    let hint = CacheHint::from_result(&result);
    Some(success(
        id,
        cacheable_for_upstream(result, hint, request.cache_scoped),
    ))
}

/// Why a skill file read must be refused, if it must. Runs the detectors on a
/// copy; the caller delivers the untouched original otherwise.
fn skill_file_refusal(reg: &Registry, server: &str, result: &Value) -> Option<&'static str> {
    let delivered = mcp_skills::content_bytes(result);
    let mut probe = result.clone();
    integrity::neutralize_untrusted_result(&mut probe);
    if mcp_skills::content_bytes(&probe) != delivered {
        return Some("it contains text forging Toolport's own voice");
    }
    if reg.content_defense_effective() || reg.block_on_injection_effective() {
        let block = reg.should_block_injection_for(server);
        let mut probe = result.clone();
        let label = integrity::sanitize_wrapper_label(server);
        if integrity::defend_content(&label, "resource", &mut probe, block).is_some() {
            return Some("content defense blocked it as a high-confidence injection");
        }
    }
    None
}
