//! End-to-end guard for the stdio adapter role (one-gateway-per-host P2.2c).
//!
//! Starts `toolport-gateway --stdio-adapter` against a scratch data directory,
//! drives an MCP `initialize` + `tools/list` over its stdin, and reads the answers
//! from its stdout. The adapter is expected to rendezvous with a daemon it starts
//! itself, so this also proves the cold-start path in front of a real client.
//!
//! Unix only: cleanup kills the daemon the adapter detached into its own process
//! group, which the Windows path would have to do differently.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

static NEXT_SCRATCH_DIR_ID: AtomicU64 = AtomicU64::new(0);

struct Harness {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    dir: std::path::PathBuf,
    /// The adapter's stderr, so a failure says why instead of only that it happened.
    stderr: Arc<Mutex<String>>,
}

impl Harness {
    fn start() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-stdio-adapter-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT_SCRATCH_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create data dir");

        let mut child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
            .arg("--stdio-adapter")
            .env("TOOLPORT_DATA_DIR", &dir)
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped rather than null: the adapter's own lines are the only place a
            // broken exchange explains itself, and macOS CI has failed here before
            // with nothing recorded.
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the adapter");
        let stdin = child.stdin.take().expect("adapter stdin");
        let stdout = child.stdout.take().expect("adapter stdout");
        let stderr = child.stderr.take().expect("adapter stderr");

        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        let stderr_text = Arc::new(Mutex::new(String::new()));
        {
            let stderr_text = Arc::clone(&stderr_text);
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    let Ok(mut text) = stderr_text.lock() else {
                        return;
                    };
                    text.push_str(&line);
                    text.push('\n');
                    // Keep the last 4 KiB, including when one line is longer than the
                    // limit. Move to a UTF-8 boundary before trimming.
                    if text.len() > 4 * 1024 {
                        let mut from = text.len() - 4 * 1024;
                        while !text.is_char_boundary(from) {
                            from += 1;
                        }
                        text.drain(..from);
                    }
                }
            });
        }

        Self {
            child,
            stdin,
            lines,
            dir,
            stderr: stderr_text,
        }
    }

    /// What a failure needs to explain itself: the adapter's stderr, plus the tail of
    /// the gateway log the adapter and its daemon both append to. That log lives in
    /// this harness's scratch data directory, so no other process can be writing it.
    fn diagnostics(&self) -> String {
        let stderr = self
            .stderr
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        let log = std::fs::read_to_string(self.dir.join("gateway.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        format!(
            "adapter stderr:\n{}\ngateway.log (last 20 lines):\n{}",
            if stderr.is_empty() {
                "<empty>"
            } else {
                &stderr
            },
            if tail.trim().is_empty() {
                "<empty>"
            } else {
                &tail
            }
        )
    }

    fn send(&mut self, message: serde_json::Value) {
        writeln!(self.stdin, "{message}").expect("write to adapter stdin");
        self.stdin.flush().expect("flush adapter stdin");
    }

    fn send_raw(&mut self, raw: &str) {
        writeln!(self.stdin, "{raw}").expect("write to adapter stdin");
        self.stdin.flush().expect("flush adapter stdin");
    }

    fn next_response(&self) -> serde_json::Value {
        let line = match self.lines.recv_timeout(Duration::from_secs(60)) {
            Ok(line) => line,
            Err(error) => panic!(
                "no response before the deadline ({error})\n{}",
                self.diagnostics()
            ),
        };
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("invalid JSON ({e}): {line}"))
    }

    /// Wait for the response to `id`, forwarding (and ignoring) any server-initiated
    /// notifications the daemon interleaves first.
    fn response_to(&self, id: i64) -> serde_json::Value {
        loop {
            let message = self.next_response();
            if message["id"] == id {
                return message;
            }
            assert!(
                message.get("method").is_some() && message.get("id").is_none(),
                "unexpected non-notification before the answer to {id}: {message}\n{}",
                self.diagnostics()
            );
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        kill_daemon(&self.dir);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Kill the daemon the adapter detached, so it does not idle for its full grace
/// period after the test.
fn kill_daemon(dir: &std::path::Path) {
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
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if let Some(pid) = value["pid"].as_u64() {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
    }
}

#[test]
fn adapter_proxies_a_session_to_the_host_daemon() {
    let mut harness = Harness::start();

    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "stdio-adapter-test", "version": "1" }
        }
    }));
    let initialize = harness.response_to(1);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"], "toolport-gateway",
        "unexpected initialize result: {initialize}"
    );

    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list"
    }));
    let tools = harness.response_to(3);
    assert!(
        tools["result"]["tools"].is_array(),
        "tools/list did not return an array: {tools}"
    );

    // A malformed frame must be answered with a JSON-RPC parse error, not silence.
    harness.send_raw("this is not json");
    let parse_error = loop {
        let message = harness.next_response();
        if message.get("error").is_some() {
            break message;
        }
    };
    assert_eq!(parse_error["error"]["code"], -32700);
    assert!(
        parse_error["id"].is_null(),
        "a parse error carries a null id: {parse_error}"
    );
}

#[test]
fn the_adapter_recovers_after_the_daemon_dies() {
    let mut harness = Harness::start();

    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "stdio-adapter-recovery", "version": "1" }
        }
    }));
    let initialize = harness.response_to(1);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        "toolport-gateway",
        "the cold start did not complete: {initialize}\n{}",
        harness.diagnostics()
    );
    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    assert!(
        harness.response_to(2)["result"]["tools"].is_array(),
        "the session did not start\n{}",
        harness.diagnostics()
    );

    // Kill the daemon out from under the adapter.
    kill_daemon(&harness.dir);

    // The call that hits the dead daemon fails with an error, and is not replayed.
    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list"
    }));
    let failed = loop {
        let message = harness.next_response();
        if message["id"] == 3 {
            break message;
        }
    };
    assert!(
        failed.get("error").is_some(),
        "the call against a dead daemon should have failed: {failed}\n{}",
        harness.diagnostics()
    );

    // The next request re-rendezvouses, replays the handshake, and succeeds. The
    // replayed initialize answer is not forwarded, so only id 4 is expected back.
    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/list"
    }));
    let recovered = loop {
        let message = harness.next_response();
        if message["id"] == 4 {
            break message;
        }
        assert!(
            message.get("method").is_some() && message.get("id").is_none(),
            "unexpected message while recovering: {message}\n{}",
            harness.diagnostics()
        );
    };
    assert!(
        recovered["result"]["tools"].is_array(),
        "the adapter did not recover: {recovered}\n{}",
        harness.diagnostics()
    );
}
