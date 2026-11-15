// MCP Skills routing under a per-client server scope (included into the
// gateway's test module). End-to-end coverage against the real fixture lives
// in `tests/mcp_skills.rs`; these pin the security rule with fake transports
// that record every request, so a test can prove an out-of-scope server was
// never contacted.

/// A modern downstream that declares MCP Skills and serves one skill,
/// `skill://x/SKILL.md`, whose text names the server.
struct SkillsFake {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

impl SkillsFake {
    fn new(name: &'static str) -> (Self, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                name,
                log: Arc::clone(&log),
            },
            log,
        )
    }
}

impl Transport for SkillsFake {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, downstream::TransportError> {
        self.log.lock().unwrap().push(method.to_string());
        let uri = params["uri"].as_str().unwrap_or("").to_string();
        let text = format!("---\nname: x\ndescription: from {}\n---\n", self.name);
        match method {
            "initialize" => Err(downstream::TransportError::Rpc(
                json!({ "code": -32601, "message": "method not found" }),
            )),
            "server/discover" => Ok(json!({
                "supportedVersions": [MODERN_PROTOCOL_VERSION],
                "capabilities": {
                    "resources": {},
                    "extensions": { conduit_lib::mcp_skills::EXTENSION: { "directoryRead": true } }
                }
            })),
            "tools/list" => Ok(json!({ "tools": [] })),
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "skills/list" => Ok(json!({
                "skills": [{
                    "uri": "skill://x/SKILL.md",
                    "frontmatter": { "name": "x", "description": format!("from {}", self.name) },
                    "resources": [{ "uri": "skill://x/SKILL.md", "digest": "sha256:00", "size": text.len() }]
                }],
                "ttlMs": 60000,
                "cacheScope": "public"
            })),
            "skills/get" if uri == "skill://x/SKILL.md" => Ok(json!({
                "skill": {
                    "uri": uri,
                    "frontmatter": { "name": "x", "description": "d" },
                    "resources": "dynamic"
                }
            })),
            "resources/read" if uri == "skill://x/SKILL.md" => Ok(json!({
                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
            })),
            "resources/directory/read" if uri == "skill://x" => Ok(json!({
                "resources": [{ "uri": "skill://x/SKILL.md", "name": "SKILL.md" }]
            })),
            _ => Err(downstream::TransportError::Rpc(
                json!({ "code": -32602, "message": format!("unknown {uri}") }),
            )),
        }
    }

    fn notify(&mut self, _method: &str, _params: Value) -> Result<(), downstream::TransportError> {
        Ok(())
    }
}

fn skills_req(id: i64, method: &str, params: Value) -> Value {
    let mut extra = params;
    extra["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "TestClient", "version": "1.0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": extra })
}

type RequestLog = Arc<Mutex<Vec<String>>>;

/// Two declaring servers, `a` and `b`, with the client scoped to `a`.
fn scoped_fixture() -> (Router, RequestLog, RequestLog) {
    let (a, a_log) = SkillsFake::new("a");
    let (b, b_log) = SkillsFake::new("b");
    let mut router = Router::new();
    router.add(DownstreamServer::connect("a".into(), Box::new(a)).unwrap());
    router.add(DownstreamServer::connect("b".into(), Box::new(b)).unwrap());
    a_log.lock().unwrap().clear();
    b_log.lock().unwrap().clear();
    (router, a_log, b_log)
}

fn skills_methods(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|method| {
            matches!(
                method.as_str(),
                "skills/list" | "skills/get" | "resources/read" | "resources/directory/read"
            )
        })
        .cloned()
        .collect()
}

#[test]
fn a_crafted_uri_never_reaches_a_server_outside_the_client_scope() {
    let _lock = registry::data_dir_test_lock();
    let dir = std::env::temp_dir().join(format!("toolport-mcp-skills-{}", new_correlation_id()));
    let _data = registry::DataDirOverride::set(&dir);
    let host = dispatch_host(false);
    let reg = Registry::default();
    let (router, a_log, b_log) = scoped_fixture();
    let allowed = HashSet::from(["a".to_string()]);
    let send = |id: i64, method: &str, params: Value| {
        handle_request(
            &host,
            &skills_req(id, method, params),
            &reg,
            &router,
            &[],
            true,
            None,
            &SearchGuard::default(),
            Some(&allowed),
            None,
        )
        .unwrap()
    };

    let listed = send(1, "skills/list", json!({}));
    let uris: Vec<&str> = listed["result"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, vec!["skill://a/x/SKILL.md"]);

    let read = send(2, "resources/read", json!({ "uri": "skill://a/x/SKILL.md" }));
    assert!(read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("from a"));
    assert_eq!(read["result"]["contents"][0]["uri"], "skill://a/x/SKILL.md");

    // Every way of naming server b: its canonical prefix, case variants a
    // client could produce, a non-canonical escape, and the directory root.
    let crafted = [
        ("skills/get", "skill://b/x/SKILL.md"),
        ("resources/read", "skill://b/x/SKILL.md"),
        ("resources/directory/read", "skill://b/x"),
        ("resources/read", "SKILL://b/x/SKILL.md"),
        ("resources/read", "skill://B/x/SKILL.md"),
        ("resources/read", "skill://_62/x/SKILL.md"),
        ("skills/get", "skill://b/../a/x/SKILL.md"),
    ];
    for (index, (method, uri)) in crafted.iter().enumerate() {
        let response = send(10 + index as i64, method, json!({ "uri": uri }));
        assert!(
            response.get("error").is_some(),
            "{method} {uri} must fail for a client scoped to a: {response}"
        );
    }
    assert!(
        skills_methods(&b_log).is_empty(),
        "server b was contacted: {:?}",
        b_log.lock().unwrap()
    );
    // The unprefixed form has one in-scope owner here, so it resolves to a.
    let unprefixed = send(30, "resources/read", json!({ "uri": "skill://x/SKILL.md" }));
    assert!(unprefixed["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("from a"));
    assert!(skills_methods(&b_log).is_empty());
    assert!(!skills_methods(&a_log).is_empty());
}

#[test]
fn an_unscoped_client_gets_the_ambiguity_error_for_a_shared_unprefixed_uri() {
    let _lock = registry::data_dir_test_lock();
    let dir = std::env::temp_dir().join(format!("toolport-mcp-skills-{}", new_correlation_id()));
    let _data = registry::DataDirOverride::set(&dir);
    let host = dispatch_host(false);
    let reg = Registry::default();
    let (router, _, _) = scoped_fixture();
    for (id, method, uri) in [
        (1, "resources/read", "skill://x/SKILL.md"),
        (2, "skills/get", "skill://x/SKILL.md"),
        (3, "resources/directory/read", "skill://x"),
    ] {
        let response = handle_request(
            &host,
            &skills_req(id, method, json!({ "uri": uri })),
            &reg,
            &router,
            &[],
            true,
            None,
            &SearchGuard::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32602, "{method}: {response}");
        let prefixed = |server: &str| conduit_lib::mcp_skills::expose_uri(server, uri);
        assert_eq!(
            response["error"]["data"]["candidates"],
            json!([prefixed("a"), prefixed("b")]),
            "{method}: {response}"
        );
    }
}
