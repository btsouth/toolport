//! Conformance harness for the verification matrix in
//! `docs/design/one-gateway-per-host.md`, tracked in issue #910.
//!
//! Every row of the matrix is a named case here, so the daemon phases land
//! against an executable checklist instead of a prose table. The mapping to the
//! plan's acceptance notes: the rendezvous rows are the cold-start and
//! partitioning cases below; the P2.3 lifecycle rows are the EOF and
//! stale-descriptor cases plus the crash/no-replay rows already pinned in
//! `tests/stdio_adapter.rs` and `tests/daemon_idle_exit.rs`; and the Phase 3
//! rows target behavior that does not exist yet.
//!
//! Phase 3 cases are `#[ignore]`d acceptance criteria rather than absent: they
//! run (and fail loudly) with `cargo test ... -- --ignored`, and the attribute
//! comes off in the same PR that lands the phase — the pattern
//! `tests/spec_conformance.rs` used while the dual-era work was in flight. The
//! case table with per-case commands lives in
//! `docs/design/one-gateway-per-host-plan.md`.
//!
//! Everything drives real processes: the real gateway binary in `--daemon` and
//! `--stdio-adapter` roles, the real rendezvous files, and the real
//! `mock-mcp-server` fixture as the downstream. Every wait is bounded, so a
//! case that hangs fails its own deadline rather than the CI job.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use conduit_lib::approval::{self, BrokerChallenge, BrokerProof, EndpointDescriptor};
use conduit_lib::daemon::descriptor_path;
use conduit_lib::registry::{self, EnvVar, FolderProfile, Profile, Registry, ServerEntry};
use conduit_lib::topology::CompatKey;
use serde_json::{json, Value};

/// The matrix races processes and then counts them, so the cases in this file
/// run one at a time. Cargo already serializes separate test binaries; this
/// lock covers the test threads inside this one.
static CASE_LOCK: Mutex<()> = Mutex::new(());

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Generous on purpose: a cold-start daemon on a loaded CI runner can take tens
/// of seconds to publish its descriptor, and the adapter blocks on that.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A scratch data directory per case, plus cleanup that kills every daemon the
/// case left a descriptor for. Daemons are detached process-group leaders, so
/// killing the adapters alone would leave them idling for the full grace.
struct Fixture {
    dirs: Vec<PathBuf>,
}

impl Fixture {
    fn new(tag: &str) -> (Self, PathBuf) {
        let dir = scratch_dir(tag);
        (
            Self {
                dirs: vec![dir.clone()],
            },
            dir,
        )
    }

    /// Add one more scratch directory to the same case (partitioning needs two).
    fn add(&mut self, tag: &str) -> PathBuf {
        let dir = scratch_dir(tag);
        self.dirs.push(dir.clone());
        dir
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for dir in &self.dirs {
            kill_daemons(dir);
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolport-matrix-{tag}-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch data dir");
    dir
}

/// One AI client session: a real `--stdio-adapter` process with a line-based
/// MCP client on its stdin/stdout. Server-initiated requests (the daemon asking
/// for `roots/list`) are answered by the reader thread, because this harness
/// is the whole client.
struct AdapterClient {
    child: Child,
    /// Shared with the reader thread, which answers the daemon's
    /// server-initiated requests on the same pipe. `None` means the client
    /// side closed: dropping the handle is what delivers EOF.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    lines: mpsc::Receiver<String>,
    pending_notifications: Mutex<VecDeque<Value>>,
    next_id: i64,
    /// Whether `initialize` will declare the roots capability. Kept beside the
    /// reader thread that answers `roots/list`, so the two cannot disagree.
    declares_roots: bool,
    roots_queries: Arc<AtomicUsize>,
    declares_elicitation: bool,
    elicitation_queries: Arc<AtomicUsize>,
    /// This client's data directory, for the gateway log a failure has to quote.
    dir: PathBuf,
    /// The adapter's stderr, captured rather than discarded: an adapter that exits
    /// answers nothing on stdout, and its own lines are the only place that says why.
    stderr: Arc<Mutex<String>>,
}

struct AdapterOptions<'a> {
    client_id: Option<&'a str>,
    /// Named registry profile this client runs under (the per-principal row).
    profile: Option<&'a str>,
    /// Process cwd, which may differ from the client's declared MCP root.
    cwd: Option<&'a Path>,
    /// Idle grace the daemon inherits through the adapter's environment.
    grace_ms: Option<u64>,
    /// Project roots this client declares, for the `${ROOT}` rows.
    roots: Vec<PathBuf>,
    /// Whether this client can answer legacy form elicitation requests.
    elicitation: bool,
}

impl Default for AdapterOptions<'_> {
    fn default() -> Self {
        Self {
            client_id: None,
            profile: None,
            cwd: None,
            grace_ms: None,
            roots: Vec::new(),
            elicitation: false,
        }
    }
}

fn spawn_adapter(dir: &Path, options: &AdapterOptions) -> AdapterClient {
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    command
        .arg("--stdio-adapter")
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env(
            "TOOLPORT_CLIENT_ID",
            options
                .client_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("matrix-{index}")),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(profile) = options.profile {
        command.env("TOOLPORT_PROFILE", profile);
    }
    if let Some(cwd) = options.cwd {
        command.current_dir(cwd);
    }
    if let Some(grace_ms) = options.grace_ms {
        command.env("TOOLPORT_DAEMON_IDLE_GRACE_MS", grace_ms.to_string());
    }
    let mut child = command.spawn().expect("spawn the stdio adapter");
    let stdin = child.stdin.take().expect("adapter stdin");
    let stdout = child.stdout.take().expect("adapter stdout");
    let stderr = child.stderr.take().expect("adapter stderr");
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

    let roots = options.roots.clone();
    let declares_roots = !roots.is_empty();
    let roots_queries = Arc::new(AtomicUsize::new(0));
    let roots_queries_reader = Arc::clone(&roots_queries);
    let elicitation_queries = Arc::new(AtomicUsize::new(0));
    let elicitation_queries_reader = Arc::clone(&elicitation_queries);
    let stdin = Arc::new(Mutex::new(Some(stdin)));
    let responder = Arc::clone(&stdin);
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("method").is_some() && value.get("id").is_some() {
                let id = value["id"].clone();
                let reply = match value["method"].as_str() {
                    Some("roots/list") => {
                        roots_queries_reader.fetch_add(1, Ordering::Relaxed);
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "roots": roots
                                    .iter()
                                    .map(|root| json!({
                                        "uri": file_uri(root),
                                        "name": "project",
                                    }))
                                    .collect::<Vec<_>>()
                            }
                        })
                    }
                    Some("elicitation/create") => {
                        elicitation_queries_reader.fetch_add(1, Ordering::Relaxed);
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "action": "accept",
                                "content": {"approved": true}
                            }
                        })
                    }
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": "harness client answers roots/list and elicitation/create only"
                        }
                    }),
                };
                if let Some(handle) = responder.lock().unwrap().as_mut() {
                    let _ = writeln!(handle, "{reply}");
                    let _ = handle.flush();
                }
                continue;
            }
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    AdapterClient {
        child,
        stdin,
        lines,
        pending_notifications: Mutex::new(VecDeque::new()),
        next_id: 0,
        declares_roots,
        roots_queries,
        declares_elicitation: options.elicitation,
        elicitation_queries,
        dir: dir.to_path_buf(),
        stderr: stderr_text,
    }
}

/// The tail of the gateway log this suite's data directory collects. Both the
/// adapters and the daemon they rendezvous with append to it.
fn gateway_log_tail(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    format!(
        "gateway.log (last 20 lines):\n{}",
        if tail.trim().is_empty() {
            "<empty>"
        } else {
            &tail
        }
    )
}

/// The `file://` root URI form the gateway decodes back into a path.
fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("absolute project root has a file URI")
        .to_string()
}

impl AdapterClient {
    fn send(&mut self, message: Value) {
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard.as_mut().expect("adapter stdin still open");
        writeln!(stdin, "{message}").expect("write to the adapter");
        stdin.flush().expect("flush the adapter");
    }

    fn next_message(&self) -> Value {
        let line = match self.lines.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => line,
            Err(error) => panic!(
                "no message before the deadline ({error})\n{}",
                self.diagnostics()
            ),
        };
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("invalid JSON ({e}): {line}"))
    }

    /// Everything a failure needs to explain itself: this adapter's stderr plus the
    /// gateway log the data directory collects. An adapter that exits mid-run
    /// answers nothing, so a bare \"no message\" panic says nothing about why.
    fn diagnostics(&self) -> String {
        let stderr = self
            .stderr
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        format!(
            "adapter stderr:\n{}\n{}",
            if stderr.is_empty() {
                "<empty>"
            } else {
                &stderr
            },
            gateway_log_tail(&self.dir)
        )
    }

    /// The response to `id`, skipping whatever notifications arrive first.
    fn response_to(&self, id: i64) -> Value {
        loop {
            let message = self.next_message();
            if message["id"] == id {
                return message;
            }
            assert!(
                message.get("method").is_some() && message.get("id").is_none(),
                "unexpected message before the answer to {id}: {message}\n{}",
                self.diagnostics()
            );
            self.pending_notifications
                .lock()
                .unwrap()
                .push_back(message);
        }
    }

    fn observed_message(&self, within: Duration) -> Option<Value> {
        if let Some(message) = self.pending_notifications.lock().unwrap().pop_front() {
            return Some(message);
        }
        let line = self.lines.recv_timeout(within).ok()?;
        Some(
            serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("invalid JSON ({error}): {line}")),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        self.response_to(id)
    }

    fn initialize(&mut self, name: &str) -> Value {
        let mut capabilities = serde_json::Map::new();
        if self.declares_roots {
            capabilities.insert("roots".to_string(), json!({}));
        }
        if self.declares_elicitation {
            capabilities.insert("elicitation".to_string(), json!({}));
        }
        let reply = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": capabilities,
                "clientInfo": { "name": name, "version": "1" }
            }),
        );
        assert!(reply.get("result").is_some(), "initialize failed: {reply}");
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        reply
    }

    fn tool_names(&mut self) -> Vec<String> {
        let reply = self.request("tools/list", json!({}));
        assert!(
            reply["result"]["tools"].is_array(),
            "tools/list did not return an array: {reply}"
        );
        reply["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect()
    }

    /// Poll tools/list until some tool name matches.
    ///
    /// The daemon publishes its descriptor and serves immediately while its
    /// router builds on a background thread, so the first list can
    /// legitimately show only the gateway built-ins. A real client learns the
    /// finished catalog from notifications/tools/list_changed; a conformance
    /// case polls instead, bounded, so a catalog that never lands still fails
    /// the row rather than hanging it.
    fn wait_for_tool_where(
        &mut self,
        label: &str,
        matches: impl Fn(&str) -> bool,
        within: Duration,
    ) -> String {
        let deadline = Instant::now() + within;
        loop {
            let names = self.tool_names();
            if let Some(found) = names.iter().find(|name| matches(name.as_str())).cloned() {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "no tool matching {label} was exposed within {within:?}: {names:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_tool(&mut self, suffix: &str, within: Duration) -> String {
        self.wait_for_tool_where(
            &format!("the suffix {suffix}"),
            |name| name.ends_with(suffix),
            within,
        )
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(
            reply.get("result").is_some(),
            "tools/call {name} failed: {reply}"
        );
        reply["result"].clone()
    }

    /// Close the client side of the stdio session (an AI client shutting down)
    /// without killing the adapter, so EOF cleanup can be observed.
    fn close_stdin(&mut self) {
        self.stdin.lock().unwrap().take();
    }

    fn wait_exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().expect("adapter status") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the adapter did not exit within the deadline"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The next notification with this method, proving it reached this session.
    fn next_notification(&self, method: &str, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .observed_message(remaining.max(Duration::from_millis(1)))
                .unwrap_or_else(|| panic!("no {method} notification before the deadline"));
            if message.get("method").is_some() && message.get("id").is_none() {
                assert_eq!(
                    message["method"], method,
                    "expected a {method} notification, got another"
                );
                return message;
            }
        }
    }

    fn assert_no_notification(&self, within: Duration, label: &str) {
        if let Some(message) = self.observed_message(within) {
            panic!("{label}: unexpected message {message}");
        }
    }
}

impl Drop for AdapterClient {
    fn drop(&mut self) {
        self.stdin.lock().unwrap().take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A downstream `mock-mcp-server` registry entry with a transcript, so cases
/// can count real downstream launches by counting `initialize` lines.
fn mock_server_entry(id: &str, transcript: &Path, cwd: Option<&str>) -> ServerEntry {
    ServerEntry {
        id: id.to_string(),
        name: format!("Mock {id}"),
        transport: "stdio".to_string(),
        command: Some(env!("CARGO_BIN_EXE_mock-mcp-server").to_string()),
        args: vec![],
        env: vec![EnvVar {
            key: "MOCK_MCP_TRANSCRIPT".to_string(),
            value: Some(transcript.display().to_string()),
            secret: false,
        }],
        url: None,
        source: Some("manual".to_string()),
        disabled_tools: vec![],
        cwd: cwd.map(str::to_string),
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: None,
        unknown_fields: serde_json::Map::new(),
    }
}

fn profile(id: &str, enabled: &[&str]) -> Profile {
    Profile {
        id: id.to_string(),
        name: id.to_string(),
        enabled_server_ids: enabled.iter().map(|s| s.to_string()).collect(),
        tool_scope: std::collections::HashMap::new(),
    }
}

fn write_registry(dir: &Path, servers: Vec<ServerEntry>, profiles: Vec<Profile>) {
    let mut registry_value = Registry::default();
    // Registry::default() runs lazy discovery, where tools/list serves only
    // the gateway's meta-tools. The matrix rows are full-catalog rows, so the
    // fixture opts out the way a user's full-discovery install would.
    registry_value.set_lazy_discovery(false);
    // The active default profile gates what a session sees, and it starts
    // empty, so every fixture server must be enabled there or tools/list
    // answers with only the gateway's built-ins. Scoped profiles opt in to
    // their own subsets via TOOLPORT_PROFILE.
    let server_ids: Vec<String> = servers.iter().map(|s| s.id.clone()).collect();
    registry_value.servers = servers;
    if let Some(active) = registry_value.active_profile_id.clone() {
        if let Some(profile) = registry_value.profiles.iter_mut().find(|p| p.id == active) {
            profile.enabled_server_ids = server_ids;
        }
    }
    if !profiles.is_empty() {
        registry_value.profiles = profiles;
    }
    registry::save_to(&dir.join("registry.json"), &registry_value).expect("write registry");
}

// ---------------------------------------------------------------------------
// Daemon observation
// ---------------------------------------------------------------------------

fn descriptor_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|e| e == "json")
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("daemon-"))
        })
        .collect();
    paths.sort();
    paths
}

fn read_descriptor(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn first_descriptor(dir: &Path) -> Option<Value> {
    descriptor_files(dir)
        .iter()
        .find_map(|p| read_descriptor(p))
}

fn wait_for_descriptor(dir: &Path, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        if let Some(descriptor) = first_descriptor(dir) {
            return descriptor;
        }
        assert!(
            Instant::now() < deadline,
            "no daemon descriptor within the deadline"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A JSON-RPC response body, whether the server answered with `application/json`
/// or a one-shot `text/event-stream` frame (mirrors `tests/daemon_cold_start.rs`).
fn json_body(response: ureq::Response) -> Value {
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

/// The authenticated identity handshake. `Err` covers transport failure, HTTP
/// errors, and a wrong bearer alike: from a caller's point of view none of them
/// is a daemon it may talk to.
fn probe_identity(endpoint: &str, token: &str) -> Result<Value, String> {
    let response = ureq::get(&format!("http://{endpoint}/host/identity"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("identity probe failed: {e}"))?;
    Ok(json_body(response))
}

/// The compat domain a gateway process computes for this data directory: the
/// build version plus the directory exactly as the environment spells it (the
/// gateway never canonicalizes). The descriptor filename follows from it, so
/// cases that plant a descriptor use the gateway's own path computation.
fn compat_for(dir: &Path) -> CompatKey {
    CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string())
}

/// Live daemon processes for this build. Counting a delta around a case makes
/// the number immune to daemons leaked by other runs on the same machine.
fn daemon_process_count() -> usize {
    process_command_lines()
        .iter()
        .filter(|line| line.contains("toolport-gateway") && line.contains("--daemon"))
        .count()
}

fn mock_child_process_count() -> usize {
    process_command_lines()
        .iter()
        .filter(|line| line.contains("mock-mcp-server"))
        .count()
}

/// Full process-table lines (pid, parent, age, command) matching `needle`.
/// A process-count assertion that fails on a loaded runner is evidence about
/// a race, so its panic message carries every matching process: which ones,
/// whose children, how old, with which full command line.
#[cfg(unix)]
fn process_report(needle: &str) -> Vec<String> {
    let output = Command::new("ps")
        .args(["-ww", "-axo", "pid=,ppid=,etime=,command="])
        .output()
        .expect("run ps for the process report");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_string)
        .collect()
}

#[cfg(windows)]
fn process_report(needle: &str) -> Vec<String> {
    let script = format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{needle}*' }} | \
         ForEach-Object {{ \"pid=$($_.ProcessId) ppid=$($_.ParentProcessId) created=$($_.CreationDate) command=$($_.CommandLine)\" }}"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .expect("run powershell for the process report");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn process_command_lines() -> Vec<String> {
    let output = Command::new("ps")
        .args(["-axo", "command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(windows)]
fn process_command_lines() -> Vec<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine } | ForEach-Object { $_.CommandLine }",
        ])
        .output()
        .expect("run powershell for process command lines");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn kill_daemons(dir: &Path) {
    for path in descriptor_files(dir) {
        let Some(descriptor) = read_descriptor(&path) else {
            continue;
        };
        let Some(pid) = descriptor["pid"].as_u64() else {
            continue;
        };
        #[cfg(unix)]
        let _ = Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}

/// Whether a pid still has a process. The daemon is a detached process-group
/// leader, so this (not parenthood) is what "the daemon exited" means.
#[cfg(unix)]
fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn pid_alive(pid: u64) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

fn transcript_method_count(path: &Path, method: &str) -> usize {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 0;
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry.get("method").is_some_and(|m| m == method))
        .count()
}

/// One `initialize` line per spawned downstream child.
fn transcript_initialize_count(path: &Path) -> usize {
    transcript_method_count(path, "initialize")
}

fn wait_until(mut predicate: impl FnMut() -> bool, label: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {label}");
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// Family 1: cold-start dedupe
// ---------------------------------------------------------------------------

#[test]
fn matrix_cold_start_twenty_simultaneous_adapters_elect_exactly_one_daemon() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("cold-start");
    let before = daemon_process_count();

    // Spawn every adapter first, then drive them: the rendezvous race happens
    // at process start, before any of them reads stdin, so this is the
    // simultaneous cold start the matrix row is about.
    let mut clients: Vec<AdapterClient> = (0..20)
        .map(|_| spawn_adapter(&dir, &AdapterOptions::default()))
        .collect();
    std::thread::scope(|scope| {
        for client in &mut clients {
            scope.spawn(move || {
                let name = "matrix-cold-start";
                client.initialize(name);
                let reply = client.request("tools/list", json!({}));
                assert!(
                    reply["result"]["tools"].is_array(),
                    "session did not serve tools/list: {reply}\n{}",
                    client.diagnostics()
                );
            });
        }
    });

    // Exactly one daemon-<fingerprint>.json exists, it answers the
    // authenticated identity handshake, and it is the daemon it claims.
    let paths = descriptor_files(&dir);
    assert_eq!(paths.len(), 1, "expected exactly one descriptor: {paths:?}");
    let descriptor = read_descriptor(&paths[0]).expect("read the descriptor");
    let endpoint = descriptor["endpoint"]
        .as_str()
        .expect("endpoint")
        .to_string();
    let token = descriptor["token"].as_str().expect("token").to_string();
    let identity = probe_identity(&endpoint, &token).expect("probe the elected daemon");
    assert_eq!(identity["compat"], descriptor["compat"]);

    // The process table agrees: twenty adapters cold-started exactly one
    // daemon. Deliberately one-shot: a double election under load leaves the
    // loser serving its sessions until its idle grace, so waiting for the
    // count to converge would only mask the defect this row exists to catch.
    // The panic carries the whole process table so a failure is evidence.
    let elected_pid = &descriptor["pid"];
    let after = daemon_process_count();
    assert_eq!(
        after.saturating_sub(before),
        1,
        "twenty simultaneous adapters must elect exactly one daemon \
         (elected pid {elected_pid}, {before} before, {after} after); \
         matching processes:\n{}",
        process_report("--daemon").join("\n")
    );

    // Scoped teardown: the clients drop first (their adapters die), then the
    // fixture kills the daemon they elected and removes the scratch directory.
    drop(clients);
}

// ---------------------------------------------------------------------------
// Family 2: version and data-dir partitioning
// ---------------------------------------------------------------------------

#[test]
fn matrix_partitioning_separate_data_dirs_run_separate_daemons_without_cross_talk() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir_a) = Fixture::new("partition-a");
    let dir_b = fixture.add("partition-b");

    let mut client_a = spawn_adapter(&dir_a, &AdapterOptions::default());
    let mut client_b = spawn_adapter(&dir_b, &AdapterOptions::default());
    client_a.initialize("matrix-partition-a");
    client_b.initialize("matrix-partition-b");
    for (label, client) in [("A", &mut client_a), ("B", &mut client_b)] {
        assert!(
            !client
                .tool_names()
                .iter()
                .any(|name| name.ends_with("__echo")),
            "session {label} with an empty registry must not expose downstream tools"
        );
    }

    let descriptor_a = first_descriptor(&dir_a).expect("descriptor in data dir A");
    let descriptor_b = first_descriptor(&dir_b).expect("descriptor in data dir B");
    assert_ne!(
        descriptor_a["compat"], descriptor_b["compat"],
        "different data directories must not share a compatibility domain"
    );
    assert_ne!(
        descriptor_a["endpoint"], descriptor_b["endpoint"],
        "each data directory gets its own daemon endpoint"
    );
    assert_ne!(descriptor_a["pid"], descriptor_b["pid"]);

    // Both daemons are alive and answer their own identity handshake.
    for descriptor in [&descriptor_a, &descriptor_b] {
        let endpoint = descriptor["endpoint"].as_str().expect("endpoint");
        let token = descriptor["token"].as_str().expect("token");
        let identity = probe_identity(endpoint, token).expect("probe own daemon");
        assert_eq!(identity["compat"], descriptor["compat"]);
    }

    // No cross-talk: A's bearer is worthless against B's daemon.
    let endpoint_b = descriptor_b["endpoint"].as_str().expect("endpoint B");
    let token_a = descriptor_a["token"].as_str().expect("token A");
    assert!(
        probe_identity(endpoint_b, token_a).is_err(),
        "a token from one data directory must not authorize against another"
    );
}

#[test]
fn matrix_partitioning_a_foreign_compat_descriptor_is_rejected_not_adopted() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir_a) = Fixture::new("foreign-a");
    let dir_b = fixture.add("foreign-b");

    // A real daemon from another compatibility domain (a different data dir
    // stands in for a different build, since the version is compile-time).
    let foreign = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .arg("--daemon")
        .env("TOOLPORT_DATA_DIR", &dir_b)
        .env("TOOLPORT_REGISTRY", dir_b.join("registry.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the foreign daemon");
    let mut foreign = foreign;
    let descriptor_b = wait_for_descriptor(&dir_b, Duration::from_secs(60));
    let endpoint_b = descriptor_b["endpoint"]
        .as_str()
        .expect("endpoint B")
        .to_string();
    let token_b = descriptor_b["token"].as_str().expect("token B").to_string();

    let raw_b = std::fs::read_to_string(
        descriptor_files(&dir_b)
            .first()
            .expect("foreign descriptor path"),
    )
    .expect("read foreign descriptor");
    // Plant the foreign descriptor at A's own descriptor path: a live daemon,
    // a valid token, but the wrong compatibility domain.
    std::fs::write(descriptor_path(&dir_a, &compat_for(&dir_a)), &raw_b)
        .expect("plant the foreign descriptor");

    let mut client = spawn_adapter(&dir_a, &AdapterOptions::default());
    client.initialize("matrix-foreign-compat");
    let reply = client.request("tools/list", json!({}));
    assert!(
        reply["result"]["tools"].is_array(),
        "the adapter must elect its own daemon, not adopt the foreign one: {reply}"
    );

    // The adapter's live descriptor claims its own domain and a different
    // endpoint than the foreign daemon's.
    let live = descriptor_files(&dir_a)
        .iter()
        .filter_map(|p| read_descriptor(p))
        .find(|d| {
            d["compat"] != descriptor_b["compat"] && d["endpoint"] != descriptor_b["endpoint"]
        })
        .expect("a descriptor for this domain, distinct from the foreign one");
    let endpoint_a = live["endpoint"].as_str().expect("endpoint A").to_string();
    let token_a = live["token"].as_str().expect("token A").to_string();
    assert_ne!(live["pid"], descriptor_b["pid"]);
    probe_identity(&endpoint_a, &token_a).expect("probe the elected daemon");

    // Cross-talk is still refused in both directions.
    assert!(probe_identity(&endpoint_a, &token_b).is_err());
    assert!(probe_identity(&endpoint_b, token_a.as_str()).is_err());

    let _ = foreign.kill();
    let _ = foreign.wait();
}

// ---------------------------------------------------------------------------
// Family: landed lifecycle rows (P2.3)
// ---------------------------------------------------------------------------

#[test]
fn matrix_lifecycle_adapter_eof_releases_the_session_and_lets_the_daemon_exit() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("eof");

    // The grace the daemon inherits through the adapter's environment.
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            grace_ms: Some(800),
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-eof");
    assert!(
        !client
            .tool_names()
            .iter()
            .any(|name| name.ends_with("__echo")),
        "a session with an empty registry must not expose downstream tools"
    );
    let descriptor = first_descriptor(&dir).expect("descriptor before EOF");
    let daemon_pid = descriptor["pid"].as_u64().expect("daemon pid");

    // A client shutting down closes stdin; the adapter releases the session and
    // exits, and with nothing left in flight the daemon idles out and clears
    // its descriptor rather than waiting for its full default grace.
    client.close_stdin();
    client.wait_exit(Duration::from_secs(20));
    wait_until(
        || first_descriptor(&dir).is_none() && !pid_alive(daemon_pid),
        "the daemon to idle out after client EOF",
        Duration::from_secs(30),
    );
    assert!(
        first_descriptor(&dir).is_none(),
        "the descriptor outlived the daemon"
    );
}

#[test]
fn matrix_lifecycle_a_stale_descriptor_does_not_stall_startup() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("stale");

    // A descriptor that claims the right domain but points at a dead endpoint:
    // grab a port and release it, so the probe cannot connect.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a scratch port");
    let port = listener.local_addr().expect("scratch port").port();
    drop(listener);
    let stale = json!({
        "endpoint": format!("127.0.0.1:{port}"),
        "token": "stale-token",
        "pid": 4_000_000u64,
        "compat": compat_for(&dir).fingerprint(),
        "protocol": 1u32,
        "createdAtMs": 0u64
    });
    std::fs::write(descriptor_path(&dir, &compat_for(&dir)), stale.to_string())
        .expect("plant the stale descriptor");

    // Bounded by construction: initialize answers within RESPONSE_TIMEOUT or
    // the case fails, which is the row itself — startup never waits forever.
    let mut client = spawn_adapter(&dir, &AdapterOptions::default());
    client.initialize("matrix-stale");
    assert!(
        !client
            .tool_names()
            .iter()
            .any(|name| name.ends_with("__echo")),
        "a session with an empty registry must not expose downstream tools"
    );

    // The stale pointer was replaced by a live daemon's descriptor.
    let live = first_descriptor(&dir).expect("a live descriptor after startup");
    assert_ne!(
        live["token"], "stale-token",
        "the stale descriptor survived"
    );
    let endpoint = live["endpoint"].as_str().expect("endpoint");
    let token = live["token"].as_str().expect("token");
    probe_identity(endpoint, token).expect("probe the replacement daemon");
}

// ---------------------------------------------------------------------------
// Family 3: downstream launch pooling (P3.2)
// ---------------------------------------------------------------------------

#[test]
fn matrix_pooling_sessions_share_one_downstream_child() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("pool-share");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );
    let before = mock_child_process_count();

    let mut clients: Vec<AdapterClient> = (0..3)
        .map(|_| spawn_adapter(&dir, &AdapterOptions::default()))
        .collect();
    let mut echo_tools = Vec::new();
    for client in clients.iter_mut() {
        client.initialize("matrix-pool-share");
        // Every session waits for the server's tool to be exposed: the wait
        // itself is the visibility assertion, and it is bounded so a catalog
        // that never lands fails the row instead of hanging it.
        echo_tools.push(client.wait_for_tool("__echo", Duration::from_secs(30)));
    }
    let echoes = ["from-session-one", "from-session-two", "from-session-three"];
    for ((client, tool), expected) in clients.iter_mut().zip(&echo_tools).zip(echoes) {
        let result = client.call_tool(tool, json!({ "text": expected }));
        assert_eq!(text_of(&result), expected);
    }

    // The row: three ordinary client sessions, one downstream child. Counted
    // both as live processes and as initialize lines in the child's transcript.
    assert_eq!(transcript_initialize_count(&transcript), 1);
    assert_eq!(
        mock_child_process_count().saturating_sub(before),
        1,
        "three sessions on one ordinary stdio server must share one child; \
         matching processes:\n{}",
        process_report("mock-mcp-server").join("\n")
    );
}

#[test]
fn matrix_adapter_root_is_queried_on_its_own_session() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("session-roots");
    let root = std::fs::canonicalize(fixture.add("project")).expect("project root");
    write_registry(&dir, vec![], vec![]);
    let mut with_roots = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root],
            ..AdapterOptions::default()
        },
    );
    let mut without_roots = spawn_adapter(&dir, &AdapterOptions::default());
    with_roots.initialize("matrix-with-roots");
    without_roots.initialize("matrix-without-roots");
    let deadline = Instant::now() + Duration::from_secs(10);
    while with_roots.roots_queries.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        with_roots.roots_queries.load(Ordering::Relaxed),
        1,
        "daemon did not ask the capable adapter for roots\n{}",
        with_roots.diagnostics()
    );
    assert_eq!(without_roots.roots_queries.load(Ordering::Relaxed), 0);
}

#[ignore = "P3.2 root-aware downstream slot selection is pending (see #910 and docs/design/one-gateway-per-host-plan.md). Run with --ignored once it lands, and remove this attribute in the PR that lands it."]
#[test]
fn matrix_pooling_root_sharding_two_roots_two_children() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("pool-roots");
    let root_a = std::fs::canonicalize(fixture.add("root-a")).expect("canonical root A");
    let root_b = std::fs::canonicalize(fixture.add("root-b")).expect("canonical root B");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let before = mock_child_process_count();

    let options = |root: PathBuf| AdapterOptions {
        roots: vec![root],
        ..AdapterOptions::default()
    };
    let mut client_a = spawn_adapter(&dir, &options(root_a.clone()));
    let mut client_b = spawn_adapter(&dir, &options(root_b.clone()));
    let mut client_a2 = spawn_adapter(&dir, &options(root_a.clone()));
    for client in [&mut client_a, &mut client_b, &mut client_a2] {
        client.initialize("matrix-root-shard");
    }

    let pwd_of = |client: &mut AdapterClient| {
        let tool = client.wait_for_tool("__pwd", Duration::from_secs(30));
        text_of(&client.call_tool(&tool, json!({})))
    };
    assert_eq!(
        std::fs::canonicalize(pwd_of(&mut client_a)).expect("canonical cwd"),
        root_a,
        "a session rooted at A must run ${{ROOT}} servers in A"
    );
    assert_eq!(
        std::fs::canonicalize(pwd_of(&mut client_a2)).expect("canonical cwd"),
        root_a,
        "an equal root shares A's placement"
    );
    assert_eq!(
        std::fs::canonicalize(pwd_of(&mut client_b)).expect("canonical cwd"),
        root_b,
        "a session rooted at B must run ${{ROOT}} servers in B"
    );

    // Two distinct roots, two children; the third session shares A's child.
    assert_eq!(transcript_initialize_count(&transcript), 2);
    assert_eq!(
        mock_child_process_count().saturating_sub(before),
        2,
        "two distinct roots must shard into two downstream children; \
         matching processes:\n{}",
        process_report("mock-mcp-server").join("\n")
    );
}

// ---------------------------------------------------------------------------
// Family 4: per-principal routing (P3.1 / P3.3)
// ---------------------------------------------------------------------------

#[test]
fn matrix_routing_identical_request_ids_stay_per_session() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("id-collision");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );

    let mut client_a = spawn_adapter(&dir, &AdapterOptions::default());
    let mut client_b = spawn_adapter(&dir, &AdapterOptions::default());
    client_a.initialize("matrix-ids-a");
    client_b.initialize("matrix-ids-b");
    let tool_a = client_a.wait_for_tool("__echo", Duration::from_secs(30));
    let tool_b = client_b.wait_for_tool("__echo", Duration::from_secs(30));

    // Both clients choose the same id for in-flight calls with different
    // payloads; each answer must come back on the session that asked.
    client_a.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": tool_a, "arguments": { "text": "from-a" } }
    }));
    client_b.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": tool_b, "arguments": { "text": "from-b" } }
    }));
    assert_eq!(text_of(&client_a.response_to(7)["result"]), "from-a");
    assert_eq!(text_of(&client_b.response_to(7)["result"]), "from-b");
}

#[test]
fn matrix_routing_profiles_cannot_reach_servers_outside_their_scope() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("profile-scope");
    let transcript_one = dir.join("one.jsonl");
    let transcript_two = dir.join("two.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &transcript_one, None),
            mock_server_entry("two", &transcript_two, None),
        ],
        vec![
            profile("scope-one", &["one"]),
            profile("scope-two", &["two"]),
        ],
    );

    let mut scoped_one = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("scope-one"),
            ..AdapterOptions::default()
        },
    );
    let mut scoped_two = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("scope-two"),
            ..AdapterOptions::default()
        },
    );
    scoped_one.initialize("matrix-scope-one");
    scoped_two.initialize("matrix-scope-two");

    // A partial adapter claim must not fall back to the daemon bearer's
    // unscoped administrative identity.
    let descriptor = wait_for_descriptor(&dir, Duration::from_secs(10));
    let endpoint = descriptor["endpoint"].as_str().expect("endpoint");
    let token = descriptor["token"].as_str().expect("token");
    let rejected = ureq::post(&format!("http://{endpoint}/mcp"))
        .set("Authorization", &format!("Bearer {token}"))
        .set(
            conduit_lib::stdio_adapter::ADAPTER_PROFILE_HEADER,
            "scope-two",
        )
        .set("Content-Type", "application/json")
        .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert!(
        matches!(rejected, Err(ureq::Error::Status(401, _))),
        "a profile claim without a client id must be refused"
    );
    let rejected_root = ureq::post(&format!("http://{endpoint}/mcp"))
        .set("Authorization", &format!("Bearer {token}"))
        .set(conduit_lib::stdio_adapter::ADAPTER_CWD_HEADER, "!!!")
        .set("Content-Type", "application/json")
        .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert!(
        matches!(rejected_root, Err(ureq::Error::Status(401, _))),
        "a root claim without a client id must be refused"
    );
    let malformed_root = ureq::post(&format!("http://{endpoint}/mcp"))
        .set("Authorization", &format!("Bearer {token}"))
        .set(
            conduit_lib::stdio_adapter::ADAPTER_CLIENT_ID_HEADER,
            "matrix-root",
        )
        .set(conduit_lib::stdio_adapter::ADAPTER_CWD_HEADER, "!!!")
        .set("Content-Type", "application/json")
        .send_string(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert!(
        matches!(malformed_root, Err(ureq::Error::Status(401, _))),
        "a malformed root claim must be refused"
    );

    scoped_one.wait_for_tool_where(
        "the one__ prefix",
        |name| name.starts_with("one__"),
        Duration::from_secs(30),
    );
    scoped_two.wait_for_tool_where(
        "the two__ prefix",
        |name| name.starts_with("two__"),
        Duration::from_secs(30),
    );
    let names_one = scoped_one.tool_names();
    let names_two = scoped_two.tool_names();
    assert!(
        names_one.iter().any(|n| n.starts_with("one__"))
            && !names_one.iter().any(|n| n.starts_with("two__")),
        "scope-one must see only its own server: {names_one:?}"
    );
    assert!(
        names_two.iter().any(|n| n.starts_with("two__"))
            && !names_two.iter().any(|n| n.starts_with("one__")),
        "scope-two must see only its own server: {names_two:?}"
    );

    // And not just hidden: a direct call to an out-of-scope tool is refused.
    let out_of_scope = names_two
        .iter()
        .find(|name| name.starts_with("two__"))
        .expect("scope-two exposes its downstream tool")
        .clone();
    let before = transcript_method_count(&transcript_two, "tools/call");
    let reply = scoped_one.request(
        "tools/call",
        json!({ "name": out_of_scope, "arguments": {} }),
    );
    assert!(
        reply["result"]["isError"] == true || reply.get("error").is_some(),
        "an out-of-scope call must be refused: {reply}"
    );
    assert_eq!(
        transcript_method_count(&transcript_two, "tools/call"),
        before,
        "the out-of-scope call reached the downstream server"
    );
}

#[test]
fn matrix_routing_live_profile_change_reopens_the_adapter_session() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("live-rescope");
    let path = dir.join("registry.json");
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &dir.join("one.jsonl"), None),
            mock_server_entry("two", &dir.join("two.jsonl"), None),
        ],
        vec![
            profile("scope-one", &["one"]),
            profile("scope-two", &["two"]),
        ],
    );
    let mut reg = registry::load_from(&path).expect("load fixture registry");
    reg.client_scopes
        .insert("matrix-rescope".to_string(), "scope-one".to_string());
    registry::save_to(&path, &reg).expect("set initial client scope");

    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            client_id: Some("matrix-rescope"),
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-rescope");
    client.wait_for_tool_where(
        "the one__ prefix",
        |name| name.starts_with("one__"),
        Duration::from_secs(30),
    );

    reg.client_scopes
        .insert("matrix-rescope".to_string(), "scope-two".to_string());
    registry::save_to(&path, &reg).expect("change client scope");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = client.request("tools/list", json!({}));
        if reply.get("error").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the old session was never invalidated"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    client.wait_for_tool_where(
        "the two__ prefix after recovery",
        |name| name.starts_with("two__"),
        Duration::from_secs(30),
    );
    let names = client.tool_names();
    assert!(
        !names.iter().any(|name| name.starts_with("one__")),
        "the reopened session still exposes the old profile: {names:?}"
    );
}

#[test]
fn matrix_routing_profile_tool_scopes_are_independent_on_one_shared_server() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("tool-scope");
    let transcript = dir.join("shared.jsonl");
    let mut echo_only = profile("echo-only", &["shared"]);
    echo_only
        .tool_scope
        .insert("shared".to_string(), vec!["echo".to_string()]);
    let mut add_only = profile("add-only", &["shared"]);
    add_only
        .tool_scope
        .insert("shared".to_string(), vec!["add".to_string()]);
    write_registry(
        &dir,
        vec![mock_server_entry("shared", &transcript, None)],
        vec![echo_only, add_only],
    );

    let mut echo = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("echo-only"),
            ..AdapterOptions::default()
        },
    );
    let mut add = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("add-only"),
            ..AdapterOptions::default()
        },
    );
    echo.initialize("matrix-tool-scope-echo");
    add.initialize("matrix-tool-scope-add");
    let echo_tool = echo.wait_for_tool("__echo", Duration::from_secs(30));
    let add_tool = add.wait_for_tool("__add", Duration::from_secs(30));
    assert!(!echo.tool_names().contains(&add_tool));
    assert!(!add.tool_names().contains(&echo_tool));
    assert_eq!(
        text_of(&echo.call_tool(&echo_tool, json!({ "text": "yes" }))),
        "yes"
    );
    assert!(add.call_tool(&add_tool, json!({ "a": 2, "b": 3 }))["isError"] != true);
    assert_eq!(
        echo.call_tool(&add_tool, json!({ "a": 2, "b": 3 }))["isError"],
        true,
        "a direct call must not bypass the profile's tool allowlist"
    );
    assert_eq!(transcript_initialize_count(&transcript), 1);
}

#[test]
fn matrix_routing_approved_call_rebinds_to_its_profile_view() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("profile-hitl");
    let transcript = dir.join("shared.jsonl");
    let mut echo_only = profile("echo-only", &["shared"]);
    echo_only
        .tool_scope
        .insert("shared".to_string(), vec!["echo".to_string()]);
    let mut add_only = profile("add-only", &["shared"]);
    add_only
        .tool_scope
        .insert("shared".to_string(), vec!["add".to_string()]);
    let mut server = mock_server_entry("shared", &transcript, None);
    // Imported servers require human approval even for a non-destructive tool.
    server.source = Some("shared".to_string());
    write_registry(&dir, vec![server], vec![echo_only, add_only]);
    let registry_path = dir.join("registry.json");
    let mut reg = registry::load_from(&registry_path).expect("load registry");
    reg.set_human_approval(true);
    registry::save_to(&registry_path, &reg).expect("enable approval");

    let listener = TcpListener::bind("127.0.0.1:0").expect("approval listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let token = "matrix-approval-token".to_string();
    let descriptor = EndpointDescriptor {
        endpoint: listener.local_addr().unwrap().to_string(),
        unix_endpoint: None,
        token: token.clone(),
    };
    std::fs::write(
        dir.join(approval::ENDPOINT_FILE),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .expect("approval descriptor");
    let broker = std::thread::spawn(move || {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("approval challenge");
                    let challenge: BrokerChallenge =
                        serde_json::from_str(&line).expect("valid challenge");
                    let proof = BrokerProof {
                        toolport_approval_proof: approval::challenge_proof(
                            &token,
                            &challenge.toolport_approval_challenge,
                        ),
                    };
                    writeln!(stream, "{}", serde_json::to_string(&proof).unwrap())
                        .expect("approval proof");
                    line.clear();
                    reader.read_line(&mut line).expect("approval request");
                    let request: approval::ApprovalRequest =
                        serde_json::from_str(&line).expect("valid approval request");
                    assert_eq!(request.server, "shared");
                    assert_eq!(request.tool, "echo");
                    writeln!(stream, "\"approved\"").expect("approval decision");
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no approval request");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("approval accept failed: {error}"),
            }
        }
    });

    let mut echo = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("echo-only"),
            ..AdapterOptions::default()
        },
    );
    let mut add = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("add-only"),
            ..AdapterOptions::default()
        },
    );
    echo.initialize("matrix-hitl-echo");
    add.initialize("matrix-hitl-add");
    let echo_tool = echo.wait_for_tool("__echo", Duration::from_secs(30));
    let add_tool = add.wait_for_tool("__add", Duration::from_secs(30));
    assert!(!add.tool_names().contains(&echo_tool));
    assert_eq!(
        text_of(&echo.call_tool(&echo_tool, json!({ "text": "approved" }))),
        "approved"
    );
    broker.join().expect("approval broker");
    assert!(
        echo.call_tool(&add_tool, json!({ "a": 2, "b": 3 }))["isError"] == true,
        "approval did not preserve the caller's tool scope"
    );
}

#[test]
fn matrix_routing_live_tool_scope_change_reopens_the_adapter_session() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("live-tool-scope");
    let path = dir.join("registry.json");
    let transcript = dir.join("shared.jsonl");
    let mut scoped = profile("scoped", &["shared"]);
    scoped
        .tool_scope
        .insert("shared".to_string(), vec!["echo".to_string()]);
    write_registry(
        &dir,
        vec![mock_server_entry("shared", &transcript, None)],
        vec![scoped],
    );
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("scoped"),
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-live-tool-scope");
    let echo = client.wait_for_tool("__echo", Duration::from_secs(30));

    let mut reg = registry::load_from(&path).expect("load fixture registry");
    reg.profiles[0]
        .tool_scope
        .insert("shared".to_string(), vec!["add".to_string()]);
    registry::save_to(&path, &reg).expect("change tool scope");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = client.request("tools/list", json!({}));
        if reply.get("error").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old tool scope was never invalidated"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let add = client.wait_for_tool("__add", Duration::from_secs(30));
    assert!(!client.tool_names().contains(&echo));
    assert_eq!(
        client.call_tool(&echo, json!({ "text": "blocked" }))["isError"],
        true
    );
    assert!(client.call_tool(&add, json!({ "a": 2, "b": 3 }))["isError"] != true);
}

#[test]
fn matrix_routing_tool_change_notifies_only_profiles_that_can_see_it() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("tool-scope-notifications");
    let transcript = dir.join("shared.jsonl");
    let mut grow_profile = profile("grow-profile", &["shared"]);
    grow_profile.tool_scope.insert(
        "shared".to_string(),
        vec!["grow".to_string(), "greet".to_string()],
    );
    let mut echo_profile = profile("echo-profile", &["shared"]);
    echo_profile
        .tool_scope
        .insert("shared".to_string(), vec!["echo".to_string()]);
    write_registry(
        &dir,
        vec![mock_server_entry("shared", &transcript, None)],
        vec![grow_profile, echo_profile],
    );
    let mut grow = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("grow-profile"),
            ..AdapterOptions::default()
        },
    );
    let mut echo = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("echo-profile"),
            ..AdapterOptions::default()
        },
    );
    grow.initialize("matrix-grow-profile");
    echo.initialize("matrix-echo-profile");
    let grow_tool = grow.wait_for_tool("__grow", Duration::from_secs(30));
    echo.wait_for_tool("__echo", Duration::from_secs(30));
    // The daemon may finish its initial catalog fanout after a tools/list
    // response, especially on Windows. Establish a quiet baseline before
    // attributing a later notification to the grow call.
    for client in [&grow, &echo] {
        while client
            .observed_message(Duration::from_millis(500))
            .is_some()
        {}
    }

    grow.call_tool(&grow_tool, json!({}));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = grow
            .observed_message(remaining.max(Duration::from_millis(1)))
            .expect("grow profile did not receive tools/list_changed");
        if message["method"] == "notifications/tools/list_changed" {
            break;
        }
    }
    grow.wait_for_tool("__greet", Duration::from_secs(30));
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(message) = echo.observed_message(remaining) else {
            break;
        };
        assert_ne!(
            message["method"], "notifications/tools/list_changed",
            "the other tool profile received a tool catalog notification"
        );
    }
    assert!(!echo
        .tool_names()
        .iter()
        .any(|name| name.ends_with("__greet")));
    assert_eq!(transcript_initialize_count(&transcript), 1);
}

#[test]
fn matrix_routing_declared_root_selects_folder_profile() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("folder-scope");
    let cwd = fixture.add("launch-cwd");
    let root = fixture.add("mapped-project");
    let transcript_one = dir.join("one.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &transcript_one, None),
            mock_server_entry("two", &dir.join("two.jsonl"), None),
        ],
        vec![
            profile("scope-one", &["one"]),
            profile("scope-two", &["two"]),
        ],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).expect("load fixture registry");
    reg.client_scopes
        .insert("matrix-folder".to_string(), "scope-one".to_string());
    reg.folder_profiles.push(FolderProfile {
        path: root.display().to_string(),
        profile: "scope-two".to_string(),
    });
    let reported_root = conduit_lib::downstream::file_uri_to_path(&file_uri(&root))
        .expect("declared root URI must decode on this platform");
    assert_eq!(
        reg.profile_for_root(&reported_root),
        Some("scope-two".to_string())
    );
    registry::save_to(&path, &reg).expect("set folder profile");

    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            client_id: Some("matrix-folder"),
            cwd: Some(&cwd),
            roots: vec![root],
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-folder");
    let deadline = Instant::now() + Duration::from_secs(30);
    let names = loop {
        let reply = client.request("tools/list", json!({}));
        if let Some(tools) = reply["result"]["tools"].as_array() {
            let names: Vec<String> = tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect();
            if names.iter().any(|name| name.starts_with("two__")) {
                break names;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the declared root never selected its folder profile: {reply}\n{}",
            client.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        !names.iter().any(|name| name.starts_with("one__")),
        "the configured profile leaked into the folder scope: {names:?}"
    );
    let before = transcript_method_count(&transcript_one, "tools/call");
    let refused = client.request(
        "tools/call",
        json!({"name": "one__echo", "arguments": {"text": "blocked"}}),
    );
    assert!(
        refused["result"]["isError"] == true || refused.get("error").is_some(),
        "a call outside the folder profile was accepted: {refused}"
    );
    assert_eq!(
        transcript_method_count(&transcript_one, "tools/call"),
        before
    );
}

#[test]
fn matrix_routing_server_change_notifies_only_authorized_sessions() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("route-notifications");
    let transcript_one = dir.join("one.jsonl");
    let transcript_two = dir.join("two.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &transcript_one, None),
            mock_server_entry("two", &transcript_two, None),
        ],
        vec![
            profile("scope-one", &["one"]),
            profile("scope-two", &["two"]),
        ],
    );

    let options = |profile| AdapterOptions {
        profile: Some(profile),
        ..AdapterOptions::default()
    };
    let mut session_a = spawn_adapter(&dir, &options("scope-one"));
    let mut session_b = spawn_adapter(&dir, &options("scope-one"));
    let mut session_c = spawn_adapter(&dir, &options("scope-two"));
    session_a.initialize("matrix-route-a");
    session_b.initialize("matrix-route-b");
    session_c.initialize("matrix-route-c");
    let grow = session_a.wait_for_tool("__grow", Duration::from_secs(30));
    session_b.wait_for_tool_where(
        "the one__ prefix",
        |name| name.starts_with("one__"),
        Duration::from_secs(30),
    );
    session_c.wait_for_tool_where(
        "the two__ prefix",
        |name| name.starts_with("two__"),
        Duration::from_secs(30),
    );
    // Ignore the initial catalog's own change notifications. Only the grow
    // below is relevant to this assertion.
    for client in [&session_a, &session_b, &session_c] {
        // The catalog notification can trail the first successful tools/list
        // on slower runners, so wait for a short quiet period before the call.
        while client
            .observed_message(Duration::from_millis(500))
            .is_some()
        {}
    }

    // A and B share the same downstream server. Its catalog change belongs to
    // both of them; C is scoped to another server and must not learn about it.
    session_a.call_tool(&grow, json!({}));
    session_a.next_notification("notifications/tools/list_changed", Duration::from_secs(10));
    session_b.next_notification("notifications/tools/list_changed", Duration::from_secs(10));
    session_b.wait_for_tool("__greet", Duration::from_secs(30));
    session_c.assert_no_notification(
        Duration::from_secs(3),
        "out-of-scope session received a notification",
    );
    assert!(
        !session_c
            .tool_names()
            .iter()
            .any(|n| n.ends_with("__greet")),
        "an out-of-scope tool leaked into C's catalog"
    );
}

#[test]
fn matrix_routing_root_change_notifies_only_authorized_downstreams() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-change-scope");
    let root = fixture.add("project");
    let transcript_one = dir.join("one.jsonl");
    let transcript_two = dir.join("two.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &transcript_one, None),
            mock_server_entry("two", &transcript_two, None),
        ],
        vec![
            profile("scope-one", &["one"]),
            profile("scope-two", &["two"]),
        ],
    );
    let mut one = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("scope-one"),
            roots: vec![root],
            ..AdapterOptions::default()
        },
    );
    let mut two = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("scope-two"),
            ..AdapterOptions::default()
        },
    );
    one.initialize("matrix-root-one");
    two.initialize("matrix-root-two");
    one.wait_for_tool_where(
        "one's catalog",
        |name| name.starts_with("one__"),
        Duration::from_secs(30),
    );
    let two_echo = two.wait_for_tool("__echo", Duration::from_secs(30));
    one.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/roots/list_changed"
    }));
    let deadline = Instant::now() + Duration::from_secs(10);
    while transcript_method_count(&transcript_one, "notifications/roots/list_changed") == 0 {
        assert!(
            Instant::now() < deadline,
            "the authorized server saw no root change"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The next request on adapter one acknowledges that its preceding
    // notification has finished traversing the daemon. A call on adapter two
    // then places an acknowledgement after any wrongly forwarded notification
    // on server two's single stdio stream.
    let acknowledged = one.request("tools/list", json!({}));
    assert!(acknowledged.get("result").is_some());
    assert_eq!(
        text_of(&two.call_tool(&two_echo, json!({ "text": "barrier" }))),
        "barrier"
    );
    assert_eq!(
        transcript_method_count(&transcript_two, "notifications/roots/list_changed"),
        0,
        "an out-of-scope server received the other client's root change"
    );
}

#[test]
fn matrix_routing_server_request_reaches_only_the_originating_adapter() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("originating-server-request");
    let transcript = dir.join("shared.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("shared", &transcript, None)],
        vec![],
    );
    let options = AdapterOptions {
        elicitation: true,
        ..AdapterOptions::default()
    };
    let mut origin = spawn_adapter(&dir, &options);
    let mut other = spawn_adapter(&dir, &options);
    origin.initialize("matrix-origin");
    other.initialize("matrix-other");
    let tool = origin.wait_for_tool("__legacy_elicitation", Duration::from_secs(30));
    other.wait_for_tool("__legacy_elicitation", Duration::from_secs(30));
    let result = origin.call_tool(&tool, json!({}));
    assert!(
        text_of(&result).contains("legacy confirmed"),
        "the originating client did not complete elicitation: {result}"
    );
    assert_eq!(origin.elicitation_queries.load(Ordering::Relaxed), 1);
    assert_eq!(other.elicitation_queries.load(Ordering::Relaxed), 0);
    assert_eq!(transcript_initialize_count(&transcript), 1);
}
