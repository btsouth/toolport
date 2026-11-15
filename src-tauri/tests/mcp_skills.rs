//! MCP Skills extension passthrough (`io.modelcontextprotocol/skills`), end to
//! end: the real gateway binary over stdio, real `mock-mcp-server` children
//! (`MOCK_MCP_SKILLS`), and a scratch data directory per case.
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use conduit_lib::registry::{self, Profile, Registry};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SKILLS: &str = "io.modelcontextprotocol/skills";
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Server {
    id: &'static str,
    env: Vec<(&'static str, String)>,
}

fn skills_server(id: &'static str, mode: &str) -> Server {
    Server {
        id,
        env: vec![
            ("MOCK_MCP_REVISION", "2026-07-28".to_string()),
            ("MOCK_MCP_SKILLS", mode.to_string()),
            ("MOCK_MCP_SKILLS_TAG", id.to_string()),
        ],
    }
}

fn plain_server(id: &'static str) -> Server {
    Server {
        id,
        env: vec![("MOCK_MCP_REVISION", "2026-07-28".to_string())],
    }
}

struct Gateway {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<Value>,
    dir: PathBuf,
    next_id: u64,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Gateway {
    fn start(
        servers: Vec<Server>,
        configure: impl FnOnce(&mut Registry),
        profile: Option<&str>,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-mcp-skills-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
        let mut reg = Registry::default();
        for server in &servers {
            let mut env: Vec<Value> = server
                .env
                .iter()
                .map(|(key, value)| json!({ "key": key, "value": value, "secret": false }))
                .collect();
            env.push(json!({
                "key": "MOCK_MCP_TRANSCRIPT",
                "value": dir.join(format!("{}.jsonl", server.id)),
                "secret": false
            }));
            reg.servers.push(
                serde_json::from_value(json!({
                    "id": server.id, "name": server.id, "transport": "stdio",
                    "command": mock, "enabled": true, "env": env
                }))
                .unwrap(),
            );
            reg.profiles[0].enabled_server_ids.push(server.id.into());
        }
        configure(&mut reg);
        registry::save_to(&dir.join("registry.json"), &reg).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
        command
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            .env("TOOLPORT_DATA_DIR", &dir)
            .env("TOOLPORT_CLIENT_ID", "mcp-skills-test")
            .env_remove("TOOLPORT_HTTP")
            .env_remove("CONDUIT_HTTP")
            .env_remove("TOOLPORT_PROFILE")
            .env_remove("CONDUIT_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(profile) = profile {
            command.env("TOOLPORT_PROFILE", profile);
        }
        let mut child = command.spawn().expect("start gateway");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            replies,
            dir,
            next_id: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        serde_json::to_writer(&mut self.stdin, &request).unwrap();
        writeln!(&mut self.stdin).unwrap();
        self.stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let reply = self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("gateway did not answer {method}"));
            if reply["id"] == id {
                return reply;
            }
        }
    }

    /// Wait for the first downstream connects. Capabilities come from connected
    /// servers, so a `server/discover` or `initialize` racing the very first
    /// connect at gateway start does not see their extensions yet. A catalog
    /// list waits for first discovery.
    fn warm(&mut self) {
        let listed = self.modern("resources/list", json!({}));
        assert!(listed.get("result").is_some(), "{listed}");
    }

    /// A 2026-07-28 request: protocol metadata in `_meta`, no handshake.
    fn modern(&mut self, method: &str, mut params: Value) -> Value {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": { "name": "skills-test", "version": "1" },
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        self.request(method, params)
    }

    fn transcript_methods(&self, server: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(format!("{server}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|req| req["method"].as_str().map(str::to_string))
            .collect()
    }
}

fn digest(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

fn skill<'a>(listing: &'a Value, uri: &str) -> &'a Value {
    listing["result"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["uri"] == uri)
        .unwrap_or_else(|| panic!("{uri} not listed: {listing}"))
}

/// Read `uri` and check its bytes against the manifest entry the listing gave.
fn read_verified(gateway: &mut Gateway, listing: &Value, skill_uri: &str, file: &str) -> String {
    let read = gateway.modern("resources/read", json!({ "uri": file }));
    let text = read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("read {file}: {read}"))
        .to_string();
    assert_eq!(read["result"]["contents"][0]["uri"], file);
    let manifest = skill(listing, skill_uri)["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["uri"] == file)
        .unwrap_or_else(|| panic!("{file} not in manifest"))
        .clone();
    assert_eq!(manifest["digest"], digest(&text), "{file} digest");
    assert_eq!(manifest["size"], text.len(), "{file} size");
    text
}

#[test]
fn two_servers_serving_the_same_skill_uri_stay_distinct_end_to_end() {
    let mut alpha = skills_server("alpha", "dir");
    alpha.env.push(("MOCK_MCP_SKILLS_PAGED", "1".to_string()));
    let mut gateway = Gateway::start(
        vec![alpha, skills_server("beta", "1"), plain_server("gamma")],
        // PII masking on: skill files must still load byte for byte.
        |reg| reg.pii_redaction = true,
        None,
    );
    gateway.warm();

    let discovered = gateway.modern("server/discover", json!({}));
    assert_eq!(
        discovered["result"]["capabilities"]["extensions"][SKILLS],
        json!({ "directoryRead": true }),
        "mixed directoryRead keeps the id: {discovered}"
    );

    let listing = gateway.modern("skills/list", json!({}));
    assert_eq!(listing["result"]["resultType"], "complete");
    assert!(listing["result"].get("nextCursor").is_none());
    assert!(listing["result"]["ttlMs"].is_u64());
    let mut uris: Vec<&str> = listing["result"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["uri"].as_str().unwrap())
        .collect();
    uris.sort();
    // alpha pages one skill at a time; both pages were walked.
    assert_eq!(
        uris,
        vec![
            "skill://alpha/acme/billing/refunds/SKILL.md",
            "skill://alpha/x/SKILL.md",
            "skill://beta/acme/billing/refunds/SKILL.md",
            "skill://beta/x/SKILL.md",
        ]
    );
    let alpha_x = skill(&listing, "skill://alpha/x/SKILL.md");
    assert_eq!(alpha_x["frontmatter"]["name"], "x");
    assert_eq!(
        alpha_x["_meta"]["app.toolport/provenance"]["server"],
        "alpha"
    );

    // Same original URI, two servers, each routed to its own bytes. The email
    // address survives PII masking and the digest matches.
    let alpha_text = read_verified(
        &mut gateway,
        &listing,
        "skill://alpha/x/SKILL.md",
        "skill://alpha/x/SKILL.md",
    );
    let beta_text = read_verified(
        &mut gateway,
        &listing,
        "skill://beta/x/SKILL.md",
        "skill://beta/x/SKILL.md",
    );
    assert!(alpha_text.contains("Served by alpha") && alpha_text.contains("help@example.com"));
    assert!(beta_text.contains("Served by beta"));
    // Brackets, code and links load unchanged.
    let guide = read_verified(
        &mut gateway,
        &listing,
        "skill://alpha/x/SKILL.md",
        "skill://alpha/x/references/GUIDE.md",
    );
    assert!(guide.contains("[1, 2]") && guide.contains("grep -E"));
    // Content defense is label-only by default: an injection-looking file loads.
    read_verified(
        &mut gateway,
        &listing,
        "skill://alpha/x/SKILL.md",
        "skill://alpha/x/references/injected.md",
    );

    // A forged Toolport voice is refused with the reason, never rewritten.
    let forged = gateway.modern(
        "resources/read",
        json!({ "uri": "skill://beta/x/references/forged.md" }),
    );
    let message = forged["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{forged}"));
    assert!(message.contains("forging Toolport"), "{message}");
    assert!(
        message.contains("beta") && message.contains("forged"),
        "{message}"
    );

    // skills/get, routed and rewritten; unknown URIs are invalid params.
    let got = gateway.modern(
        "skills/get",
        json!({ "uri": "skill://beta/acme/billing/refunds/SKILL.md" }),
    );
    assert_eq!(
        got["result"]["skill"]["uri"],
        "skill://beta/acme/billing/refunds/SKILL.md"
    );
    assert_eq!(got["result"]["skill"]["frontmatter"]["license"], "MIT");
    assert_eq!(
        got["result"]["skill"]["resources"][1]["uri"],
        "skill://beta/acme/billing/refunds/examples/email.md"
    );
    for unknown in [
        "skill://alpha/nope/SKILL.md",
        "skill://gamma/x/SKILL.md",
        "skill://nosuch/x/SKILL.md",
    ] {
        let response = gateway.modern("skills/get", json!({ "uri": unknown }));
        assert_eq!(response["error"]["code"], -32602, "{unknown}: {response}");
    }

    // Directory reads follow each owning server's own setting.
    let dir = gateway.modern(
        "resources/directory/read",
        json!({ "uri": "skill://alpha/x" }),
    );
    let children: Vec<(&str, &str)> = dir["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("{dir}"))
        .iter()
        .map(|child| {
            (
                child["uri"].as_str().unwrap(),
                child["mimeType"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(children.contains(&("skill://alpha/x/SKILL.md", "text/markdown")));
    assert!(children.contains(&("skill://alpha/x/references", "inode/directory")));
    let beta_dir = gateway.modern(
        "resources/directory/read",
        json!({ "uri": "skill://beta/x" }),
    );
    assert_eq!(beta_dir["error"]["code"], -32601, "{beta_dir}");
    let not_dir = gateway.modern(
        "resources/directory/read",
        json!({ "uri": "skill://alpha/x/SKILL.md" }),
    );
    assert_eq!(not_dir["error"]["code"], -32602, "{not_dir}");
    assert!(
        !gateway
            .transcript_methods("beta")
            .iter()
            .any(|m| m == "resources/directory/read"),
        "beta did not declare directoryRead and must never be asked"
    );

    // An unprefixed URI two servers serve is never guessed.
    for (method, uri) in [
        ("resources/read", "skill://x/SKILL.md"),
        ("skills/get", "skill://x/SKILL.md"),
    ] {
        let response = gateway.modern(method, json!({ "uri": uri }));
        assert_eq!(response["error"]["code"], -32602, "{method}: {response}");
        assert_eq!(
            response["error"]["data"]["candidates"],
            json!(["skill://alpha/x/SKILL.md", "skill://beta/x/SKILL.md"]),
            "{method}: {response}"
        );
    }

    // resources/list and resources/templates/list use the same namespaced form.
    let resources = gateway.modern("resources/list", json!({}));
    let listed: Vec<&str> = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["uri"].as_str())
        .collect();
    assert!(listed.contains(&"skill://alpha/x/SKILL.md"), "{listed:?}");
    assert!(listed.contains(&"skill://beta/x/SKILL.md"), "{listed:?}");
    assert!(!listed.contains(&"skill://x/SKILL.md"), "{listed:?}");
    assert!(listed.contains(&"mock://base"), "{listed:?}");
    let templates = gateway.modern("resources/templates/list", json!({}));
    let listed: Vec<&str> = templates["result"]["resourceTemplates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["uriTemplate"].as_str())
        .collect();
    assert!(
        listed.contains(&"skill://alpha/x/references/{file}"),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"skill://beta/x/references/{file}"),
        "{listed:?}"
    );
    assert!(
        !listed.contains(&"skill://x/references/{file}"),
        "{listed:?}"
    );
}

#[test]
fn a_profile_hides_a_skills_server_and_unprefixed_uris_resolve_to_the_one_left() {
    let mut gateway = Gateway::start(
        vec![skills_server("alpha", "dir"), skills_server("beta", "dir")],
        |reg| {
            reg.profiles.push(Profile {
                id: "only-alpha".into(),
                name: "only-alpha".into(),
                enabled_server_ids: vec!["alpha".into()],
                tool_scope: Default::default(),
                instructions: None,
                unknown_fields: Default::default(),
            })
        },
        Some("only-alpha"),
    );
    let listing = gateway.modern("skills/list", json!({}));
    assert!(listing["result"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .all(|skill| skill["uri"].as_str().unwrap().starts_with("skill://alpha/")));
    for (method, uri) in [
        ("resources/read", "skill://beta/x/SKILL.md"),
        ("skills/get", "skill://beta/x/SKILL.md"),
        ("resources/directory/read", "skill://beta/x"),
    ] {
        let response = gateway.modern(method, json!({ "uri": uri }));
        assert!(
            response.get("error").is_some(),
            "{method} {uri}: {response}"
        );
    }
    assert!(
        gateway.transcript_methods("beta").is_empty(),
        "a hidden server is never contacted"
    );
    // The absolute link a SKILL.md might carry resolves to the one server left.
    let read = gateway.modern(
        "resources/read",
        json!({ "uri": "skill://x/references/GUIDE.md" }),
    );
    let text = read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{read}"));
    assert!(text.contains("Guide from alpha"));
    assert_eq!(
        read["result"]["contents"][0]["uri"],
        "skill://alpha/x/references/GUIDE.md"
    );
    let got = gateway.modern("skills/get", json!({ "uri": "skill://x/SKILL.md" }));
    assert_eq!(
        got["result"]["skill"]["uri"], "skill://alpha/x/SKILL.md",
        "{got}"
    );
    let dir = gateway.modern(
        "resources/directory/read",
        json!({ "uri": "skill://x/references" }),
    );
    assert!(dir["result"]["resources"].is_array(), "{dir}");
}

#[test]
fn handshake_clients_get_skills_when_they_negotiate_extensions() {
    let mut gateway = Gateway::start(vec![skills_server("alpha", "1")], |_| {}, None);
    gateway.warm();
    let init = gateway.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "extensions": {} },
            "clientInfo": { "name": "handshake", "version": "1" }
        }),
    );
    assert_eq!(
        init["result"]["capabilities"]["extensions"][SKILLS],
        json!({}),
        "{init}"
    );
    assert_eq!(
        init["result"]["capabilities"]["extensions"]
            .as_object()
            .unwrap()
            .len(),
        1,
        "only MCP Skills is relayed on the handshake: {init}"
    );
    let listing = gateway.request("skills/list", json!({}));
    assert_eq!(
        listing["result"]["skills"].as_array().unwrap().len(),
        2,
        "{listing}"
    );
    assert!(
        listing["result"].get("ttlMs").is_none(),
        "legacy shape: {listing}"
    );
    let text = gateway.request(
        "resources/read",
        json!({ "uri": "skill://alpha/x/SKILL.md" }),
    );
    assert!(text["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Served by alpha"));

    // A handshake client that does not negotiate extensions is not told.
    let mut plain = Gateway::start(vec![skills_server("alpha", "1")], |_| {}, None);
    plain.warm();
    let init = plain.request(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "old", "version": "1" }
        }),
    );
    assert!(
        init["result"]["capabilities"].get("extensions").is_none(),
        "{init}"
    );
}

#[test]
fn a_handshake_upstream_server_declaring_skills_is_relayed() {
    let legacy = Server {
        id: "old",
        env: vec![
            ("MOCK_MCP_SKILLS", "1".to_string()),
            ("MOCK_MCP_SKILLS_TAG", "old".to_string()),
        ],
    };
    let mut gateway = Gateway::start(vec![legacy], |_| {}, None);
    gateway.warm();
    let discovered = gateway.modern("server/discover", json!({}));
    assert_eq!(
        discovered["result"]["capabilities"]["extensions"][SKILLS],
        json!({}),
        "{discovered}"
    );
    let listing = gateway.modern("skills/list", json!({}));
    assert!(
        listing["result"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["uri"] == "skill://old/x/SKILL.md"),
        "{listing}"
    );
}

#[test]
fn no_skills_extension_without_a_declaring_server() {
    let mut gateway = Gateway::start(vec![plain_server("gamma")], |_| {}, None);
    gateway.warm();
    let discovered = gateway.modern("server/discover", json!({}));
    assert!(discovered["result"]["capabilities"]["extensions"]
        .get(SKILLS)
        .is_none());
    let listing = gateway.modern("skills/list", json!({}));
    assert_eq!(listing["error"]["code"], -32601, "{listing}");
    let init = gateway.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "extensions": {} },
            "clientInfo": { "name": "handshake", "version": "1" }
        }),
    );
    assert!(
        init["result"]["capabilities"].get("extensions").is_none(),
        "{init}"
    );
}

#[test]
fn block_mode_refuses_an_injected_skill_file_instead_of_altering_it() {
    let mut gateway = Gateway::start(
        vec![skills_server("alpha", "1")],
        |reg| reg.team_forced_block_on_injection = true,
        None,
    );
    let listing = gateway.modern("skills/list", json!({}));
    let blocked = gateway.modern(
        "resources/read",
        json!({ "uri": "skill://alpha/x/references/injected.md" }),
    );
    let message = blocked["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{blocked}"));
    assert!(message.contains("content defense"), "{message}");
    // Ordinary files still load verbatim in block mode.
    read_verified(
        &mut gateway,
        &listing,
        "skill://alpha/x/SKILL.md",
        "skill://alpha/x/references/GUIDE.md",
    );
}
