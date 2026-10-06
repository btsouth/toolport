//! End-to-end guard for REL-03: a downstream server that fails its first connect
//! is retried in the background and joins the catalog without a gateway restart.
//!
//! Drives the real gateway over stdio, in the legacy per-client topology and in the
//! default daemon topology (stdio adapter in front of a host daemon), against
//! `mock-mcp-server` started in its fail-the-first-N-starts mode. The start counter
//! file it keeps is how these tests see each retry.
//!
//! Unix only: cleanup kills the daemon the adapter detached into its own process
//! group, which the Windows path would have to do differently.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static NEXT_SCRATCH_DIR_ID: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolport-reconnect-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        NEXT_SCRATCH_DIR_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    dir
}

fn mock_entry(id: &str, env: &[(&str, &str)]) -> Value {
    json!({
        "id": id,
        "name": id,
        "transport": "stdio",
        "command": env!("CARGO_BIN_EXE_mock-mcp-server"),
        "args": [],
        "env": env
            .iter()
            .map(|(key, value)| json!({ "key": key, "value": value, "secret": false }))
            .collect::<Vec<_>>(),
        "source": "manual",
        "disabledTools": []
    })
}

fn write_registry(dir: &Path, servers: &[Value], enabled: &[&str], legacy: bool) {
    let mut registry = json!({
        "version": 1,
        "servers": servers,
        "profiles": [{ "id": "default", "name": "Default", "enabledServerIds": enabled }],
        "activeProfileId": "default",
        "lazyDiscovery": false
    });
    if legacy {
        registry["gatewayTopology"] = json!("legacy");
    }
    // Write then rename, so the watcher never reads a half-written file.
    let tmp = dir.join("registry.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&registry).unwrap()).expect("write registry");
    std::fs::rename(&tmp, dir.join("registry.json")).expect("publish registry");
}

fn read_count(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .map(|raw| raw.lines().count() as u64)
        .unwrap_or(0)
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Gateway {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    dir: PathBuf,
    stderr: Arc<Mutex<String>>,
    next_id: i64,
    /// Server-initiated notifications seen while waiting for responses.
    notes: Vec<Value>,
}

impl Gateway {
    fn start(dir: &Path) -> Self {
        Self::start_with_backoff(dir, "300")
    }

    fn start_with_backoff(dir: &Path, base_ms: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
            .env("TOOLPORT_DATA_DIR", dir)
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            // File vault in the scratch dir: never the OS keychain.
            .env("TOOLPORT_SECRET_KEY", "reconnect-test")
            // Short steps so the schedule fits a test: 300 ms, then one try per watcher tick.
            .env("TOOLPORT_RECONNECT_BASE_MS", base_ms)
            .env("TOOLPORT_RECONNECT_CAP_MS", base_ms)
            .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", "5000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the gateway");
        let stdin = child.stdin.take().expect("gateway stdin");
        let stdout = child.stdout.take().expect("gateway stdout");
        let stderr = child.stderr.take().expect("gateway stderr");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_text = Arc::new(Mutex::new(String::new()));
        {
            let stderr_text = Arc::clone(&stderr_text);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let Ok(mut text) = stderr_text.lock() else {
                        return;
                    };
                    text.push_str(&line);
                    text.push('\n');
                    if text.len() > 8 * 1024 {
                        let mut from = text.len() - 8 * 1024;
                        while !text.is_char_boundary(from) {
                            from += 1;
                        }
                        text.drain(..from);
                    }
                }
            });
        }
        let mut gateway = Gateway {
            child,
            stdin,
            lines,
            dir: dir.to_path_buf(),
            stderr: stderr_text,
            next_id: 1,
            notes: Vec::new(),
        };
        let init = gateway.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "reconnect-test", "version": "1" }
            }),
        );
        assert!(init.get("result").is_some(), "initialize failed: {init}");
        gateway.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        gateway
    }

    fn diagnostics(&self) -> String {
        let stderr = self.stderr.lock().map(|t| t.clone()).unwrap_or_default();
        let log = std::fs::read_to_string(self.dir.join("gateway.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        format!(
            "gateway stderr:\n{stderr}\ngateway.log (last 30 lines):\n{}",
            lines[lines.len().saturating_sub(30)..].join("\n")
        )
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").expect("write to gateway stdin");
        self.stdin.flush().expect("flush gateway stdin");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = match self.lines.recv_timeout(Duration::from_secs(60)) {
                Ok(line) => line,
                Err(error) => panic!("no answer to {method} ({error})\n{}", self.diagnostics()),
            };
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("invalid JSON ({e}): {line}"));
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
            if message.get("method").is_some() && message.get("id").is_none() {
                self.notes.push(message);
            }
        }
    }

    /// The text of a tool result, and whether it is an error.
    fn call(&mut self, name: &str) -> (bool, String) {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": { "text": "hi" } }),
        );
        if let Some(error) = response.get("error") {
            return (true, error.to_string());
        }
        let result = &response["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (result["isError"] == true, text)
    }

    fn tool_names(&mut self) -> Vec<String> {
        let response = self.request("tools/list", json!({}));
        response["result"]["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn status(&mut self) -> String {
        self.call("toolport_status").1
    }

    fn list_changed_seen(&self) -> bool {
        self.notes
            .iter()
            .any(|note| note["method"] == "notifications/tools/list_changed")
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        kill_daemon(&self.dir);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn daemon_running(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.starts_with("daemon-") && name.ends_with(".json")
        })
    })
}

/// Kill the daemon the adapter detached, so it does not idle after the test.
fn kill_daemon(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.starts_with("daemon-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(pid) = value["pid"].as_u64() {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
    }
}

/// `good` connects at once; `flaky` fails its first four starts, then works.
fn failing_server_joins_without_a_restart(legacy: bool) {
    let dir = scratch_dir();
    let counter = dir.join("flaky-starts");
    let counter_path = counter.to_string_lossy().to_string();
    write_registry(
        &dir,
        &[
            mock_entry("good", &[]),
            mock_entry(
                "flaky",
                &[
                    ("MOCK_MCP_FAIL_STARTS", "4"),
                    ("MOCK_MCP_START_COUNTER", &counter_path),
                ],
            ),
        ],
        &["good", "flaky"],
        legacy,
    );
    let mut gateway = Gateway::start(&dir);

    wait_for("the first build", Duration::from_secs(60), || {
        gateway.tool_names().contains(&"good__echo".to_string())
    });
    assert_eq!(
        daemon_running(&dir),
        !legacy,
        "wrong topology\n{}",
        gateway.diagnostics()
    );
    // The failed server is reported as retrying with its real error, not guessed at.
    let status = gateway.status();
    assert!(
        status.contains("Not connected yet, retrying in the background")
            && status.contains("flaky"),
        "status should report the retry: {status}\n{}",
        gateway.diagnostics()
    );
    assert!(!status.contains("Conduit"), "{status}");
    let (is_error, text) = gateway.call("flaky__echo");
    assert!(is_error, "flaky should not route yet: {text}");
    assert!(
        text.contains("has not connected yet") || text.contains("connecting"),
        "the call error should explain the retry: {text}"
    );
    gateway.notes.clear();

    // No restart, no registry change: the retries alone bring it in.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (is_error, text) = gateway.call("flaky__echo");
        if !is_error {
            assert_eq!(text, "hi");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "flaky never joined; last error: {text}\n{}",
            gateway.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    // The catalog cache and the notification follow the router swap.
    wait_for("tools/list_changed", Duration::from_secs(10), || {
        let _ = gateway.tool_names();
        gateway.list_changed_seen()
    });
    let names = gateway.tool_names();
    assert!(
        names.contains(&"flaky__echo".to_string()),
        "tools/list after the join: {names:?}\n{}",
        gateway.diagnostics()
    );
    let status = gateway.status();
    assert!(!status.contains("retrying"), "{status}");
    assert!(read_count(&counter) >= 5);

    // Once connected, retrying stops.
    let starts = read_count(&counter);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        read_count(&counter),
        starts,
        "a connected server is not respawned"
    );
}

#[test]
fn legacy_gateway_picks_up_a_server_that_failed_at_startup() {
    failing_server_joins_without_a_restart(true);
}

#[test]
fn daemon_picks_up_a_server_that_failed_at_startup() {
    failing_server_joins_without_a_restart(false);
}

/// A late client of the daemon sees a server an earlier client watched fail.
#[test]
fn daemon_shares_the_recovered_server_with_a_later_client() {
    let dir = scratch_dir();
    let counter = dir.join("flaky-starts");
    let counter_path = counter.to_string_lossy().to_string();
    write_registry(
        &dir,
        &[
            mock_entry("good", &[]),
            mock_entry(
                "flaky",
                &[
                    ("MOCK_MCP_FAIL_STARTS", "3"),
                    ("MOCK_MCP_START_COUNTER", &counter_path),
                ],
            ),
        ],
        &["good", "flaky"],
        false,
    );
    let mut first = Gateway::start(&dir);
    wait_for("the first build", Duration::from_secs(60), || {
        first.tool_names().contains(&"good__echo".to_string())
    });
    wait_for("the retries", Duration::from_secs(60), || {
        read_count(&counter) >= 4
    });
    wait_for("flaky to join", Duration::from_secs(30), || {
        !first.call("flaky__echo").0
    });

    let mut second = Gateway::start(&dir);
    assert!(second.tool_names().contains(&"flaky__echo".to_string()));
    assert_eq!(second.call("flaky__echo"), (false, "hi".to_string()));
    // Both clients share the daemon's one connection: no extra starts.
    assert_eq!(read_count(&counter), 4);
    drop(second);
}

/// Every request answered 401, like an OAuth server nobody signed into yet.
fn unauthorized_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; 8192];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = r#"{"error":"unauthorized"}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, hits)
}

#[test]
fn a_server_that_needs_sign_in_is_not_retried_in_a_loop() {
    let dir = scratch_dir();
    let (url, hits) = unauthorized_server();
    let locked = json!({
        "id": "locked",
        "name": "locked",
        "transport": "http",
        "url": url,
        "args": [],
        "env": [],
        "source": "manual",
        "disabledTools": []
    });
    write_registry(
        &dir,
        &[mock_entry("good", &[]), locked],
        &["good", "locked"],
        true,
    );
    let mut gateway = Gateway::start(&dir);
    wait_for("the first build", Duration::from_secs(60), || {
        gateway.tool_names().contains(&"good__echo".to_string())
    });
    let status = gateway.status();
    assert!(
        status.contains("Needs sign-in") && status.contains("locked"),
        "status should ask for a sign-in: {status}\n{}",
        gateway.diagnostics()
    );
    assert!(!status.contains("Conduit"), "{status}");
    let after_connect = hits.load(Ordering::SeqCst);
    assert!(after_connect > 0, "the first connect reached the server");

    // Several backoff steps pass, and calls ask for the server, but nothing retries.
    for _ in 0..4 {
        let (is_error, text) = gateway.call("locked__anything");
        assert!(is_error);
        assert!(text.contains("needs sign-in"), "{text}");
        std::thread::sleep(Duration::from_secs(1));
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after_connect,
        "an auth-required server must wait for new credentials"
    );
}

#[test]
fn disabling_a_failing_server_stops_its_retries() {
    let dir = scratch_dir();
    let counter = dir.join("broken-starts");
    let counter_path = counter.to_string_lossy().to_string();
    let servers = [
        mock_entry("good", &[]),
        mock_entry(
            "broken",
            &[
                ("MOCK_MCP_FAIL_STARTS", "1000000"),
                ("MOCK_MCP_START_COUNTER", &counter_path),
            ],
        ),
    ];
    write_registry(&dir, &servers, &["good", "broken"], true);
    let mut gateway = Gateway::start(&dir);
    wait_for("retries", Duration::from_secs(60), || {
        read_count(&counter) >= 3
    });

    write_registry(&dir, &servers, &["good"], true);
    wait_for("the disable to apply", Duration::from_secs(30), || {
        !gateway.status().contains("broken")
    });
    // One attempt may already have been running when the rebuild landed.
    std::thread::sleep(Duration::from_secs(1));
    let settled = read_count(&counter);
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(
        read_count(&counter),
        settled,
        "a disabled server must not be retried\n{}",
        gateway.diagnostics()
    );
    assert!(gateway.tool_names().contains(&"good__echo".to_string()));
}

/// When every server failed, status checks and calls must not respawn them: they
/// wait for the background schedule. The schedule here is a minute, so any extra
/// start inside the test came from a request. The error shown to the client names
/// the failure class, never the server's own output.
#[test]
fn calls_do_not_respawn_servers_that_are_waiting_to_retry() {
    for legacy in [true, false] {
        let dir = scratch_dir();
        let counter = dir.join("down-starts");
        let counter_path = counter.to_string_lossy().to_string();
        write_registry(
            &dir,
            &[mock_entry(
                "down",
                &[
                    ("MOCK_MCP_FAIL_STARTS", "1000"),
                    ("MOCK_MCP_START_COUNTER", &counter_path),
                ],
            )],
            &["down"],
            legacy,
        );
        let mut gateway = Gateway::start_with_backoff(&dir, "60000");
        wait_for("the failed first connect", Duration::from_secs(60), || {
            gateway.status().contains("Not connected yet")
        });
        let after_first = read_count(&counter);
        assert!(after_first >= 1, "the first build started the server");
        for _ in 0..5 {
            let status = gateway.status();
            assert!(status.contains("down"), "{status}");
            assert!(
                !status.contains("failing starts"),
                "the server's own stderr must not reach the client: {status}"
            );
            let (is_error, text) = gateway.call("down__echo");
            assert!(is_error, "{text}");
            assert!(text.contains("exited (status 3)"), "{text}");
            assert!(!text.contains("failing starts"), "{text}");
        }
        assert_eq!(
            read_count(&counter),
            after_first,
            "requests must not respawn a pending server (legacy: {legacy})\n{}",
            gateway.diagnostics()
        );
    }
}
