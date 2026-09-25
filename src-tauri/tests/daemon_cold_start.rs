//! End-to-end guard for the host daemon role (one-gateway-per-host Phase 2).
//!
//! Starts `toolport-gateway --daemon` against a scratch data directory, waits for
//! the rendezvous descriptor, proves the authenticated identity handshake, and
//! drives an MCP `initialize` + `tools/list` over the internal endpoint. The unit
//! tests cover the rendezvous primitives and the identity payload; this is the
//! only test that exercises the two together over a real socket.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "toolport-daemon-cold-start-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// Read the rendezvous descriptor the daemon publishes into the data directory.
fn read_descriptor(dir: &std::path::Path) -> Option<serde_json::Value> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("daemon-") && name.ends_with(".json") {
            let raw = std::fs::read_to_string(entry.path()).ok()?;
            if let Ok(value) = serde_json::from_str(&raw) {
                return Some(value);
            }
        }
    }
    None
}

/// A JSON-RPC response body, whether the server answered with `application/json`
/// or a one-shot `text/event-stream` frame.
fn json_body(response: ureq::Response) -> serde_json::Value {
    let content_type = response
        .header("Content-Type")
        .unwrap_or_default()
        .to_string();
    let body = response.into_string().expect("response body");
    if content_type.contains("text/event-stream") {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .last()
            .and_then(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| panic!("no JSON-RPC payload in SSE body: {body:?}"))
    } else {
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("invalid JSON body ({e}): {body:?}"))
    }
}

fn post_mcp(
    endpoint: &str,
    token: &str,
    session: Option<&str>,
    body: &serde_json::Value,
) -> ureq::Response {
    let mut request = ureq::post(&format!("http://{endpoint}/mcp"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .timeout(Duration::from_secs(20));
    if let Some(session) = session {
        request = request.set("Mcp-Session-Id", session);
    }
    request.send_json(body.clone()).expect("MCP request")
}

#[test]
fn daemon_cold_start_serves_identity_and_an_mcp_session() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");

    let child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .arg("--daemon")
        .env("TOOLPORT_DATA_DIR", &dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let mut child = ChildGuard(child);

    // 1. The daemon publishes a descriptor once its runtime is up.
    let deadline = Instant::now() + Duration::from_secs(60);
    let descriptor = loop {
        if let Some(value) = read_descriptor(&dir) {
            break value;
        }
        assert!(
            child.0.try_wait().expect("daemon status").is_none(),
            "daemon exited before publishing a descriptor"
        );
        assert!(
            Instant::now() < deadline,
            "daemon did not publish a descriptor within the deadline"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let endpoint = descriptor["endpoint"]
        .as_str()
        .expect("endpoint")
        .to_string();
    let token = descriptor["token"].as_str().expect("token").to_string();
    let compat = descriptor["compat"].as_str().expect("compat").to_string();

    // 2. The authenticated identity handshake matches the descriptor.
    let identity = json_body(
        ureq::get(&format!("http://{endpoint}/host/identity"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
            .expect("identity handshake"),
    );
    assert_eq!(identity["compat"], compat);
    assert_eq!(identity["pid"], child.0.id());
    assert!(identity["gatewayVersion"].is_string());

    // 3. Without the bearer, the identity route is refused.
    let unauthorized = ureq::get(&format!("http://{endpoint}/host/identity"))
        .timeout(Duration::from_secs(10))
        .call();
    assert!(
        unauthorized.is_err(),
        "the internal identity route must require the bearer"
    );

    // 4. A full MCP session over the internal endpoint: initialize, then tools/list.
    let initialize_response = post_mcp(
        &endpoint,
        &token,
        None,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "daemon-cold-start", "version": "1" }
            }
        }),
    );
    let session = initialize_response
        .header("Mcp-Session-Id")
        .map(str::to_string);
    let initialize = json_body(initialize_response);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"], "toolport-gateway",
        "unexpected initialize result: {initialize}"
    );

    let tools = json_body(post_mcp(
        &endpoint,
        &token,
        session.as_deref(),
        &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
    ));
    assert!(
        tools["result"]["tools"].is_array(),
        "tools/list did not return an array: {tools}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
