//! Shared harness for the P1.7 chaos suite.
//!
//! Every chaos case drives the real `toolport-gateway` the way a real MCP client
//! does: a stdio adapter in front of the shared host daemon, with
//! `mock-mcp-server` (and its `MOCK_MCP_*` knobs) as the downstream server. All
//! state lives in a fresh scratch data directory, and each case cleans up the
//! daemon, its adapters and the scratch tree on drop, including after an early
//! assertion failure.
//!
//! The cases are deliberately bounded: every wait has a hard deadline, and the
//! harness kills the daemon by the pid in its descriptor instead of a process
//! name pattern.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const GATEWAY: &str = env!("CARGO_BIN_EXE_toolport-gateway");
pub const MOCK: &str = env!("CARGO_BIN_EXE_mock-mcp-server");

/// A full round trip budget for one request under the chaos cases.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A per-case scratch data directory that removes itself, and any daemon it
/// started, on drop.
pub struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-chaos-{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch data dir");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    /// Names of the scratch files whose name contains `needle`.
    pub fn matching(&self, needle: &str) -> Vec<String> {
        std::fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.contains(needle))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        kill_daemon(&self.dir);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A stdio server entry that launches the mock with the given `MOCK_MCP_*` env.
pub fn mock_entry(id: &str, env: &[(&str, &str)]) -> Value {
    json!({
        "id": id,
        "name": id,
        "transport": "stdio",
        "command": MOCK,
        "args": [],
        "env": env
            .iter()
            .map(|(key, value)| json!({ "key": key, "value": value, "secret": false }))
            .collect::<Vec<_>>(),
        "source": "manual",
        "disabledTools": []
    })
}

/// An HTTP server entry pointing at a URL the test controls.
pub fn http_entry(id: &str, url: &str) -> Value {
    json!({
        "id": id,
        "name": id,
        "transport": "http",
        "url": url,
        "args": [],
        "env": [],
        "source": "manual",
        "disabledTools": []
    })
}

/// Write `registry.json` through a temp file and rename, so the gateway's
/// watcher never reads a half-written document.
pub fn write_registry(dir: &Path, servers: &[Value], enabled: &[&str], legacy: bool) {
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
    let tmp = dir.join("registry.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&registry).unwrap()).expect("write registry");
    std::fs::rename(&tmp, dir.join("registry.json")).expect("publish registry");
}

/// Rewrite only the enabled-server list, leaving the rest of the document alone.
pub fn set_enabled(dir: &Path, enabled: &[&str]) {
    let path = dir.join("registry.json");
    let raw = std::fs::read_to_string(&path).expect("read registry");
    let mut registry: Value = serde_json::from_str(&raw).expect("parse registry");
    registry["profiles"][0]["enabledServerIds"] = json!(enabled);
    let tmp = dir.join("registry.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&registry).unwrap()).expect("write registry");
    std::fs::rename(&tmp, &path).expect("publish registry");
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

/// Send `which` (a `kill` argument such as `-KILL` or `-STOP`) to `pid`.
pub fn signal(pid: u64, which: &str) {
    let _ = Command::new("kill")
        .arg(which)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// True when `pid` exists and is not a zombie. A SIGKILLed daemon whose parent
/// has not reaped it still answers `kill -0`, which would make a "wait until it
/// died" check hang forever.
pub fn pid_running(pid: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        // `/proc/<pid>/stat` is `pid (comm) state ...`; `comm` may contain
        // spaces and parentheses, so split on the last ')'.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some((_, rest)) = stat.rsplit_once(')') {
                if rest.split_whitespace().next() == Some("Z") {
                    return false;
                }
            }
        }
    }
    pid_alive(pid)
}

fn daemon_descriptor_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|e| e == "json")
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("daemon-"))
        })
        .collect()
}

pub fn descriptor(dir: &Path) -> Option<Value> {
    daemon_descriptor_files(dir)
        .first()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

pub fn daemon_pid(dir: &Path) -> Option<u64> {
    descriptor(dir).and_then(|d| d["pid"].as_u64())
}

/// Every daemon pid recorded in the scratch dir, live or not.
pub fn daemon_pids(dir: &Path) -> Vec<u64> {
    daemon_descriptor_files(dir)
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter_map(|value| value["pid"].as_u64())
        .collect()
}

/// A daemon pid from the scratch dir that is still running.
pub fn live_daemon_pid(dir: &Path) -> Option<u64> {
    daemon_pids(dir)
        .into_iter()
        .find(|pid| pid_running(*pid))
}

pub fn wait_for_descriptor(dir: &Path) -> Value {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if let Some(descriptor) = descriptor(dir) {
            return descriptor;
        }
        assert!(
            Instant::now() < deadline,
            "no daemon descriptor\n{}",
            log_tail(dir)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill every daemon recorded in the scratch dir, if any is still alive.
pub fn kill_daemon(dir: &Path) {
    for pid in daemon_pids(dir) {
        signal(pid, "-CONT");
        signal(pid, "-KILL");
    }
}

pub fn log_tail(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    format!(
        "gateway.log (last 30 lines):\n{}",
        lines[lines.len().saturating_sub(30)..].join("\n")
    )
}

pub fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Direct children of `pid`, read from `/proc`. Linux only.
#[cfg(target_os = "linux")]
pub fn direct_children(pid: u64) -> Vec<u64> {
    let mut children = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return children;
    };
    for task in tasks.flatten() {
        if let Ok(raw) = std::fs::read_to_string(task.path().join("children")) {
            for token in raw.split_whitespace() {
                if let Ok(child) = token.parse::<u64>() {
                    children.push(child);
                }
            }
        }
    }
    children.sort_unstable();
    children.dedup();
    children
}

/// The first child of `pid` whose command line contains `needle`. Linux only.
#[cfg(target_os = "linux")]
pub fn find_child(pid: u64, needle: &str) -> Option<u64> {
    direct_children(pid).into_iter().find(|child| {
        std::fs::read_to_string(format!("/proc/{child}/cmdline"))
            .map(|raw| raw.replace('\0', " ").contains(needle))
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

fn base_gateway_command(dir: &Path) -> Command {
    let mut command = Command::new(GATEWAY);
    command
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env("TOOLPORT_SECRET_KEY", "chaos-test-key")
        .env_remove("TOOLPORT_GATEWAY_TOPOLOGY")
        .env_remove("CONDUIT_GATEWAY_TOPOLOGY")
        .env_remove("TOOLPORT_HTTP_MAX_CONNECTIONS");
    command
}

/// Start the shared host daemon and wait for its rendezvous descriptor.
pub fn start_daemon(dir: &Path) -> ChildGuard {
    start_daemon_with_env(dir, &[])
}

/// Start the daemon with extra environment, for cases that need to control what
/// the spawned servers could inherit (the env-allowlist case).
pub fn start_daemon_with_env(dir: &Path, env: &[(&str, &str)]) -> ChildGuard {
    ChildGuard(
        base_gateway_command(dir)
            .envs(env.iter().copied())
            .arg("--daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon"),
    )
}

// ---------------------------------------------------------------------------
// A line-based MCP client on one stdio adapter
// ---------------------------------------------------------------------------

pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    pending: HashMap<i64, Value>,
    pub notes: Vec<Value>,
    stderr: Arc<Mutex<String>>,
    next_id: i64,
    dir: PathBuf,
}

impl Client {
    /// Start an adapter in front of the shared host daemon, and complete the
    /// MCP handshake.
    pub fn start(dir: &Path, tag: &str) -> Self {
        let mut command = base_gateway_command(dir);
        command.env("TOOLPORT_GATEWAY_TOPOLOGY", "daemon");
        command.env("TOOLPORT_CLIENT_ID", tag);
        Self::spawn(command, dir)
    }

    fn spawn(mut command: Command, dir: &Path) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the adapter");
        let stdin = child.stdin.take().expect("adapter stdin");
        let stdout = child.stdout.take().expect("adapter stdout");
        let stderr = child.stderr.take().expect("adapter stderr");
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
        let mut client = Self {
            stdin: Some(stdin),
            child,
            lines,
            pending: HashMap::new(),
            notes: Vec::new(),
            stderr: stderr_text,
            next_id: 0,
            dir: dir.to_path_buf(),
        };
        client.initialize();
        client
    }

    pub fn diagnostics(&self) -> String {
        let stderr = self.stderr.lock().map(|t| t.clone()).unwrap_or_default();
        format!("adapter stderr:\n{stderr}\n{}", log_tail(&self.dir))
    }

    pub fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{message}").expect("write to the adapter");
        stdin.flush().expect("flush the adapter");
    }

    /// Send a request without waiting for its response, returning its id.
    pub fn send_request(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Send a `tools/call` without waiting for its response.
    pub fn call_async(&mut self, name: &str, arguments: Value) -> i64 {
        self.send_request("tools/call", json!({ "name": name, "arguments": arguments }))
    }

    /// Wait for the response to `id`, buffering any other in-flight responses.
    pub fn wait_for_id(&mut self, id: i64, timeout: Duration) -> Value {
        if let Some(message) = self.pending.remove(&id) {
            return message;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(error) => panic!("no answer to id {id} ({error})\n{}", self.diagnostics()),
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                // The adapter filters downstream noise; a non-JSON line here is
                // itself a finding, but is not the answer we are waiting for.
                continue;
            };
            match message["id"].as_i64() {
                Some(got) if got == id && message.get("method").is_none() => return message,
                Some(got) => {
                    self.pending.insert(got, message);
                }
                None => self.notes.push(message),
            }
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.wait_for_id(id, RESPONSE_TIMEOUT)
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": arguments }))
    }

    fn initialize(&mut self) {
        let reply = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "chaos", "version": "1" }
            }),
        );
        assert!(
            reply.get("result").is_some(),
            "initialize failed: {reply}\n{}",
            self.diagnostics()
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    /// The text of a tool result, or the JSON-RPC error serialized as text.
    pub fn call_text(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let reply = self.call(name, arguments);
        if let Some(error) = reply.get("error") {
            return (true, error.to_string());
        }
        let result = &reply["result"];
        (
            result["isError"] == true,
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    }

    pub fn tool_names(&mut self) -> Vec<String> {
        self.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn status(&mut self) -> String {
        self.call_text("toolport_status", json!({})).1
    }

    /// Poll `tools/list` until `tool` is routable, or the deadline passes.
    pub fn wait_for_tool(&mut self, tool: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.tool_names().contains(&tool.to_string()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Poll a tool call until it succeeds, returning the last error otherwise.
    /// Success means a non-error reply whose text leads with `prefix`: the
    /// gateway may append a repeated-call advisor note, so exact equality is
    /// wrong.
    pub fn wait_for_call(
        &mut self,
        name: &str,
        prefix: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let (is_error, text) = self.call_text(name, json!({ "text": "chaos" }));
            if !is_error && text.starts_with(prefix) {
                return Ok(text);
            }
            if Instant::now() >= deadline {
                return Err(text);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Kills its child process on drop.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// True when a `tools/call` reply succeeded and its text leads with `prefix`.
/// The gateway may append a repeated-call advisor note, so exact equality is
/// wrong.
pub fn reply_ok(reply: &Value, prefix: &str) -> bool {
    reply.get("error").is_none()
        && reply["result"]["isError"] != true
        && reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .starts_with(prefix)
}
