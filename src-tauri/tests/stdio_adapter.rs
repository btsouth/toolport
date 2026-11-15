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
        Self::start_with_env(&[])
    }

    /// Start the adapter with extra environment. The daemon it spawns inherits it.
    fn start_with_env(env: &[(&str, &str)]) -> Self {
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

        // With `TOOLPORT_TEST_UNDER_T3CODE`, the adapter runs under a shell named
        // `t3code`, as it does under T3 Code's own process.
        let gateway = env!("CARGO_BIN_EXE_toolport-gateway");
        let mut command = if env
            .iter()
            .any(|(key, _)| *key == "TOOLPORT_TEST_UNDER_T3CODE")
        {
            let t3code = dir.join("t3code");
            std::fs::copy("/bin/sh", &t3code).expect("copy a shell named t3code");
            let mut command = Command::new(t3code);
            command.args(["-c", "\"$0\" --stdio-adapter; exit $?", gateway]);
            command
        } else {
            let mut command = Command::new(gateway);
            command.arg("--stdio-adapter");
            command
        };
        let mut child = command
            .env("TOOLPORT_DATA_DIR", &dir)
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            .envs(env.iter().copied())
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
            // Until it is gone its listener can still accept a connection.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline
                && Command::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            {
                std::thread::sleep(Duration::from_millis(20));
            }
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

    // Nothing reached the dead daemon (its port refuses the connection), so the
    // call re-rendezvouses, replays the handshake and is sent to the new daemon.
    // A call the daemon dies during is never replayed; see
    // one_gateway_conformance's installer mid-call case.
    harness.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list"
    }));
    let resent = loop {
        let message = harness.next_response();
        if message["id"] == 3 {
            break message;
        }
    };
    assert!(
        resent["result"]["tools"].is_array(),
        "the call after the daemon died was not sent to a new one: {resent}\n{}",
        harness.diagnostics()
    );

    // Later requests use the recovered session. The replayed initialize answer
    // is not forwarded, so only id 4 is expected back.
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

const MODERN: &str = "2026-07-28";

/// A 2026-07-28 request. It has no session: the version rides in every body.
fn modern_request(id: i64, method: &str, mut params: serde_json::Value) -> serde_json::Value {
    params["_meta"] = serde_json::json!({ "io.modelcontextprotocol/protocolVersion": MODERN });
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The pid in the daemon descriptor the adapter published, if one is live.
fn daemon_pid(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.starts_with("daemon-") && name.ends_with(".json")) {
            return None;
        }
        let raw = std::fs::read_to_string(entry.path()).ok()?;
        serde_json::from_str::<serde_json::Value>(&raw).ok()?["pid"].as_u64()
    })
}

/// A modern stdio client has no handshake, so each POST must carry the routing
/// headers the daemon requires of a modern Streamable HTTP request.
#[test]
fn a_modern_client_is_served_through_the_adapter() {
    let mut harness = Harness::start();

    harness.send(modern_request(1, "server/discover", serde_json::json!({})));
    let discover = harness.response_to(1);
    assert!(
        discover["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.iter().any(|version| version == MODERN)),
        "server/discover did not answer: {discover}\n{}",
        harness.diagnostics()
    );

    harness.send(modern_request(2, "tools/list", serde_json::json!({})));
    let tools = harness.response_to(2);
    assert!(
        tools["result"]["tools"].is_array(),
        "tools/list did not return an array: {tools}\n{}",
        harness.diagnostics()
    );

    // A call also routes on its tool name.
    harness.send(modern_request(
        3,
        "tools/call",
        serde_json::json!({ "name": "toolport_status", "arguments": {} }),
    ));
    let status = harness.response_to(3);
    assert!(
        status.get("result").is_some(),
        "tools/call did not answer: {status}\n{}",
        harness.diagnostics()
    );

    // A name that is not safe as a raw header value still reaches the router.
    harness.send(modern_request(
        4,
        "tools/call",
        serde_json::json!({ "name": "wetter_überblick", "arguments": {} }),
    ));
    let unknown = harness.response_to(4);
    assert!(
        !unknown.to_string().contains("header"),
        "a non-ASCII tool name was rejected at the transport: {unknown}\n{}",
        harness.diagnostics()
    );
}

/// Modern protocol errors are answered with a non-2xx status and a JSON-RPC body.
/// The client needs that body as is: `-32022` lists the versions to retry with.
#[test]
fn modern_protocol_errors_reach_the_client_intact() {
    let mut harness = Harness::start();

    let mut unsupported = modern_request(1, "tools/list", serde_json::json!({}));
    unsupported["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] =
        serde_json::json!("2099-01-01");
    harness.send(unsupported);
    let rejected = harness.response_to(1);
    assert_eq!(
        rejected["error"]["code"],
        -32022,
        "an unsupported version must be reported as such: {rejected}\n{}",
        harness.diagnostics()
    );
    assert!(
        rejected["error"]["data"]["supported"]
            .as_array()
            .is_some_and(|versions| versions.iter().any(|version| version == MODERN)),
        "the error must list the supported versions: {rejected}"
    );

    harness.send(modern_request(2, "no/such-method", serde_json::json!({})));
    let unknown = harness.response_to(2);
    assert_eq!(
        unknown["error"]["code"],
        -32601,
        "an unknown method must be reported as such: {unknown}\n{}",
        harness.diagnostics()
    );
}

/// `subscriptions/listen` stays open for the life of the subscription. Its frames
/// must arrive as they are sent, other requests must not queue behind it, and the
/// client must hear when the stream ends underneath it.
#[test]
fn a_modern_subscription_streams_beside_other_requests() {
    let mut harness = Harness::start();

    harness.send(modern_request(
        1,
        "subscriptions/listen",
        serde_json::json!({ "notifications": { "toolsListChanged": true } }),
    ));
    let acknowledged = harness.next_response();
    assert_eq!(
        acknowledged["method"],
        "notifications/subscriptions/acknowledged",
        "the acknowledgement did not arrive while the stream is open: {acknowledged}\n{}",
        harness.diagnostics()
    );

    harness.send(modern_request(2, "server/discover", serde_json::json!({})));
    let discover = harness.response_to(2);
    assert!(
        discover.get("result").is_some(),
        "a request behind an open subscription was not answered: {discover}\n{}",
        harness.diagnostics()
    );

    kill_daemon(&harness.dir);
    let ended = harness.response_to(1);
    assert!(
        ended.get("error").is_some(),
        "the client must be told its subscription ended: {ended}\n{}",
        harness.diagnostics()
    );
}

/// A legacy session holds the daemon open with its listen stream. A modern client
/// has no such stream, and must not lose its daemon to the idle timer between calls.
#[test]
fn a_modern_client_keeps_its_daemon_alive_while_idle() {
    let mut harness = Harness::start_with_env(&[("TOOLPORT_DAEMON_IDLE_GRACE_MS", "2500")]);

    harness.send(modern_request(1, "server/discover", serde_json::json!({})));
    assert!(
        harness.response_to(1).get("result").is_some(),
        "the first request failed\n{}",
        harness.diagnostics()
    );
    let first_daemon = daemon_pid(&harness.dir);
    assert!(first_daemon.is_some(), "no daemon descriptor was published");

    std::thread::sleep(Duration::from_millis(6_000));

    harness.send(modern_request(2, "server/discover", serde_json::json!({})));
    let later = harness.response_to(2);
    assert!(
        later.get("result").is_some(),
        "a call after an idle pause failed: {later}\n{}",
        harness.diagnostics()
    );
    assert_eq!(
        daemon_pid(&harness.dir),
        first_daemon,
        "the daemon idled out under a connected client"
    );
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Quitting the app (or removing Toolport from every client) asks the daemon to
/// stop. A client that is connected right now keeps its daemon; the daemon goes
/// as soon as that client does, without waiting out the idle grace.
#[test]
fn a_stop_request_spares_a_connected_client_and_leaves_with_it() {
    let mut harness = Harness::start_with_env(&[("TOOLPORT_DAEMON_IDLE_GRACE_MS", "600000")]);
    harness.send(modern_request(1, "server/discover", serde_json::json!({})));
    assert!(
        harness.response_to(1).get("result").is_some(),
        "the first request failed\n{}",
        harness.diagnostics()
    );
    let pid = daemon_pid(&harness.dir).expect("a daemon descriptor");
    let outcomes = conduit_lib::daemon::stop_when_idle(&harness.dir, Duration::from_secs(3));
    assert_eq!(
        outcomes,
        vec![conduit_lib::daemon::IdleStop::Serving(pid as u32)],
        "{}",
        harness.diagnostics()
    );
    // Well past several watchdog polls, the same daemon still answers.
    std::thread::sleep(Duration::from_secs(3));
    harness.send(modern_request(2, "server/discover", serde_json::json!({})));
    assert!(
        harness.response_to(2).get("result").is_some(),
        "a call after the stop request failed\n{}",
        harness.diagnostics()
    );
    assert_eq!(
        daemon_pid(&harness.dir),
        Some(pid),
        "the daemon was replaced"
    );
    // The client disconnects: the adapter exits and closes its lifetime stream.
    let _ = harness.child.kill();
    let _ = harness.child.wait();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while pid_alive(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon outlived its last client\n{}",
            harness.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What a started adapter leaves behind once it has served `initialize` and `tools/list`:
/// every path in its data directory (digits and hex folded, so pids and hashes compare)
/// and the names of its own threads.
#[cfg(target_os = "linux")]
fn start_footprint(
    env: &[(&str, &str)],
) -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    let mut harness = Harness::start_with_env(env);
    harness.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "stdio-adapter-test", "version": "1" }
        }
    }));
    harness.response_to(1);
    harness.send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    harness.send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = harness.response_to(2);
    assert!(tools["result"]["tools"].is_array(), "{tools}");
    let fold = |text: &str| -> String {
        let mut out = String::new();
        for c in text.chars() {
            let c = if c.is_ascii_hexdigit() { '0' } else { c };
            if !(c == '0' && out.ends_with('0')) {
                out.push(c);
            }
        }
        out
    };
    let mut paths = std::collections::BTreeSet::new();
    let mut pending = vec![harness.dir.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(&harness.dir).unwrap().to_string_lossy();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(path.clone());
            }
            let tag = if kind.is_dir() {
                "dir"
            } else if std::os::unix::fs::FileTypeExt::is_socket(&kind) {
                "socket"
            } else {
                "file"
            };
            // The Activity log is written in the background as calls end, so whether it
            // exists yet is timing, not start-up work. Host attribution only adds fields
            // to its rows.
            if relative.starts_with("audit.jsonl") {
                continue;
            }
            paths.insert(format!("{tag} {}", fold(&relative)));
        }
    }
    let threads = std::fs::read_dir(format!("/proc/{}/task", harness.child.id()))
        .expect("read the adapter's threads")
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
        .map(|name| fold(name.trim()))
        .collect();
    (paths, threads)
}

#[cfg(target_os = "linux")]
#[test]
fn host_detection_adds_no_files_sockets_or_threads_at_start() {
    // Telling that T3 Code hosts the client only reads the environment and, with T3 Code
    // installed, the parent processes: the adapter starts the same way for every client.
    let (plain_paths, plain_threads) = start_footprint(&[]);
    let cases: &[&[(&str, &str)]] = &[
        &[("TOOLPORT_CLIENT_ID", "claude-code")],
        &[("TOOLPORT_CLIENT_ID", "codex")],
        &[("TOOLPORT_CLIENT_ID", "cursor")],
        // Claude Code started by T3 Code.
        &[
            ("TOOLPORT_CLIENT_ID", "claude-code"),
            ("CLAUDE_CODE_ENTRYPOINT", "t3-code"),
            ("T3CODE_THREAD_ID", "465774fd"),
        ],
        // T3 Code installed: the ancestor walk runs.
        &[
            ("TOOLPORT_CLIENT_ID", "codex"),
            ("TOOLPORT_TEST_T3_INSTALLED", "1"),
        ],
        &[
            ("TOOLPORT_CLIENT_ID", "pi"),
            ("TOOLPORT_TEST_T3_INSTALLED", "1"),
            ("T3CODE_THREAD_ID", "465774fd"),
        ],
    ];
    for env in cases {
        let (paths, threads) = start_footprint(env);
        assert_eq!(paths, plain_paths, "{env:?}: data dir differs");
        assert_eq!(threads, plain_threads, "{env:?}: threads differ");
    }
}

/// The daemon records which app hosts the client from the adapter's headers. They are on
/// every request, so a call resent after the daemon stopped, with its replayed handshake
/// on the new daemon, is still recorded as T3 Code's.
#[cfg(target_os = "linux")]
#[test]
fn the_host_survives_a_daemon_restart() {
    let mut harness = Harness::start_with_env(&[
        ("TOOLPORT_TEST_UNDER_T3CODE", "1"),
        ("TOOLPORT_TEST_T3_INSTALLED", "1"),
        ("TOOLPORT_CLIENT_ID", "claude-code"),
        ("CLAUDE_CODE_ENTRYPOINT", "t3-code"),
        ("T3CODE_THREAD_ID", "465774fd"),
    ]);
    harness.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "stdio-adapter-host", "version": "1" }
        }
    }));
    harness.response_to(1);
    harness.send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    harness.send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    assert!(harness.response_to(2)["result"]["tools"].is_array());
    // Session rows the daemon writes in the background, with the client's name.
    let dir = harness.dir.clone();
    let rows = move || -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.join("audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| row["kind"] == "session" && row["clientName"].is_string())
            .collect()
    };
    let wait_for = |count: usize| {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let found = rows();
            if found.len() >= count || std::time::Instant::now() > deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let before = wait_for(1);
    assert!(
        !before.is_empty(),
        "no session row\n{}",
        harness.diagnostics()
    );
    let first_daemon = daemon_pid(&harness.dir);
    kill_daemon(&harness.dir);
    // Never reached the dead daemon, so it is resent to a new one.
    harness.send(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
    let resent = loop {
        let message = harness.next_response();
        if message["id"] == 3 {
            break message;
        }
    };
    assert!(
        resent["result"]["tools"].is_array(),
        "{resent}\n{}",
        harness.diagnostics()
    );
    let second_daemon = daemon_pid(&harness.dir);
    assert_ne!(second_daemon, first_daemon, "the daemon was not replaced");
    // The new daemon wrote rows of its own, and every row carries the host and thread.
    let recorded = wait_for(before.len() + 1);
    assert!(
        recorded.len() > before.len(),
        "the new daemon recorded no session: {recorded:?}\n{}",
        harness.diagnostics()
    );
    for row in &recorded {
        assert_eq!(row["host"], "t3-code", "{row}");
        assert_eq!(row["hostThread"], "465774fd", "{row}");
    }
}
