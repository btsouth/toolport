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

mod discovery_support;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use conduit_lib::approval::{self, BrokerChallenge, BrokerProof, EndpointDescriptor};
use conduit_lib::daemon::{descriptor_path, election_lock_base};
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
            if std::thread::panicking() {
                if let Some(output) = std::env::var_os("TOOLPORT_TEST_FAILURE_LOG_DIR") {
                    let output = PathBuf::from(output).join(dir.file_name().unwrap());
                    let _ = std::fs::create_dir_all(&output);
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.file_name().is_some_and(|name| name == "gateway.log")
                                || path.extension().is_some_and(|ext| ext == "jsonl")
                            {
                                let _ = std::fs::copy(&path, output.join(entry.file_name()));
                            }
                        }
                    }
                    eprintln!("conformance failure logs: {}", output.display());
                }
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct HttpProxyChild(Child);

impl Drop for HttpProxyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    roots: Arc<Mutex<Vec<PathBuf>>>,
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
    /// Exercise the registry-selected role with no explicit gateway flag.
    default_role: bool,
    /// Test-only startup publication control for the two notification rows.
    startup_catalog_servers: &'a [&'a str],
    topology_override: Option<&'a str>,
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
            default_role: false,
            startup_catalog_servers: &[],
            topology_override: None,
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
    let client_id = options
        .client_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("matrix-{index}"));
    discovery_support::select_full(dir, &client_id);
    spawn_configured_adapter(dir, options, &client_id)
}

fn spawn_configured_adapter(
    dir: &Path,
    options: &AdapterOptions,
    client_id: &str,
) -> AdapterClient {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    if !options.default_role {
        command.arg("--stdio-adapter");
    }
    command
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env_remove("TOOLPORT_GATEWAY_TOPOLOGY")
        .env_remove("CONDUIT_GATEWAY_TOPOLOGY")
        .env("TOOLPORT_CLIENT_ID", client_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !options.startup_catalog_servers.is_empty() {
        command.env(
            "TOOLPORT_TEST_STARTUP_CATALOG_SERVERS",
            serde_json::to_string(options.startup_catalog_servers).unwrap(),
        );
    }
    if let Some(topology) = options.topology_override {
        command.env("TOOLPORT_GATEWAY_TOPOLOGY", topology);
    }
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

    let roots = Arc::new(Mutex::new(options.roots.clone()));
    let roots_reader = Arc::clone(&roots);
    let declares_roots = !options.roots.is_empty();
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
                                "roots": roots_reader
                                    .lock()
                                    .unwrap()
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
        roots,
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
    fn set_roots(&mut self, roots: Vec<PathBuf>) {
        *self.roots.lock().unwrap() = roots;
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/roots/list_changed"
        }));
    }

    fn send(&mut self, message: Value) {
        let result = {
            let mut guard = self.stdin.lock().unwrap();
            let stdin = guard.as_mut().expect("adapter stdin still open");
            writeln!(stdin, "{message}").and_then(|()| stdin.flush())
        };
        result.unwrap_or_else(|error| {
            panic!(
                "could not write to the adapter ({error})\n{}",
                self.diagnostics()
            )
        });
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
                "no tool matching {label} was exposed within {within:?}: {names:?}\n{}",
                self.diagnostics()
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
                .unwrap_or_else(|| {
                    panic!(
                        "no {method} notification before the deadline\n{}",
                        self.diagnostics()
                    )
                });
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
            panic!(
                "{label}: unexpected message {message}\n{}",
                self.diagnostics()
            );
        }
    }
}

impl Drop for AdapterClient {
    fn drop(&mut self) {
        self.stdin
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A downstream `mock-mcp-server` registry entry with a transcript, so cases
/// can count real downstream launches by counting `initialize` lines.
///
/// The transcript path is also passed as the one argument. `mock-mcp-server`
/// ignores argv, but the path puts this case's scratch directory into every
/// child's command line, which is what lets [`mock_child_process_count`] count
/// only the children this case launched. Without it the count is machine-wide
/// and a concurrent run in another worktree (or a leaked child from an earlier
/// test binary) shifts it out from under the delta assertion.
fn mock_server_entry(id: &str, transcript: &Path, cwd: Option<&str>) -> ServerEntry {
    ServerEntry {
        enabled: true,
        inherit_env: false,
        id: id.to_string(),
        name: format!("Mock {id}"),
        transport: "stdio".to_string(),
        command: Some(env!("CARGO_BIN_EXE_mock-mcp-server").to_string()),
        args: vec![transcript.display().to_string()],
        env: vec![EnvVar {
            key: "MOCK_MCP_TRANSCRIPT".to_string(),
            value: Some(transcript.display().to_string()),
            secret: false,
            unknown_fields: Default::default(),
        }],
        url: None,
        source: Some("manual".to_string()),
        disabled_tools: vec![],
        cwd: cwd.map(str::to_string),
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: None,
        launch: None,
        unknown_fields: serde_json::Map::new(),
    }
}

fn profile(id: &str, enabled: &[&str]) -> Profile {
    Profile {
        id: id.to_string(),
        name: id.to_string(),
        enabled_server_ids: enabled.iter().map(|s| s.to_string()).collect(),
        tool_scope: std::collections::HashMap::new(),
        instructions: None,
        unknown_fields: Default::default(),
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

fn approval_broker(
    dir: &Path,
    expected_server: &str,
    expected_tool: &str,
) -> std::thread::JoinHandle<()> {
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
    let expected_server = expected_server.to_string();
    let expected_tool = expected_tool.to_string();
    std::thread::spawn(move || {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // BSD may inherit nonblocking mode from the listener.
                    stream
                        .set_nonblocking(false)
                        .expect("blocking approval socket");
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
                    assert_eq!(request.server, expected_server);
                    assert_eq!(request.tool, expected_tool);
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
    })
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

/// Live daemons started for `dir`. The daemon inherits `TOOLPORT_DATA_DIR` from
/// the adapter that spawned it, so scoping the count to that value keeps a
/// concurrent suite in another worktree, or a leaked daemon from an earlier
/// binary, from shifting a case's delta assertion.
#[cfg(target_os = "linux")]
fn daemon_process_count(dir: &Path) -> usize {
    fixture_daemon_rows(dir).len()
}

/// Whether `pid` was started with `TOOLPORT_DATA_DIR` set to exactly `dir`. The
/// daemon is spawned with the adapter's environment, so this is what pins a
/// process to this case's fixture rather than any other run's.
#[cfg(target_os = "linux")]
fn process_data_dir_is(pid: u32, dir: &Path) -> bool {
    let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let wanted = format!("TOOLPORT_DATA_DIR={}", dir.display());
    environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == wanted.as_bytes())
}

/// Live `--daemon` processes whose environment names this case's data dir.
#[cfg(target_os = "linux")]
fn fixture_daemon_rows(dir: &Path) -> Vec<(u32, String)> {
    process_rows()
        .into_iter()
        .filter(|(pid, command)| {
            command.contains("toolport-gateway")
                && command.contains("--daemon")
                && process_data_dir_is(*pid, dir)
        })
        .collect()
}

/// Daemon process-table lines for `dir`, for the panic message a failed
/// election writes.
#[cfg(target_os = "linux")]
fn daemon_process_report(dir: &Path) -> Vec<String> {
    fixture_daemon_rows(dir)
        .into_iter()
        .map(|(pid, command)| format!("pid={pid} command={command}"))
        .collect()
}

/// Platforms without `/proc` cannot scope by environment, so the report stays
/// machine-wide as it was before.
#[cfg(not(target_os = "linux"))]
fn daemon_process_report(_dir: &Path) -> Vec<String> {
    process_report("--daemon")
}

/// Pids paired with their command lines, for a `/proc` lookup of the
/// environment each was started with.
#[cfg(target_os = "linux")]
fn process_rows() -> Vec<(u32, String)> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            Some((pid.parse().ok()?, command.trim_start().to_string()))
        })
        .collect()
}

/// Live daemon processes for this build. Platforms without `/proc` keep the
/// machine-wide count as it was before; Linux scopes it to `dir`.
#[cfg(not(target_os = "linux"))]
fn daemon_process_count(_dir: &Path) -> usize {
    process_command_lines()
        .iter()
        .filter(|line| line.contains("toolport-gateway") && line.contains("--daemon"))
        .count()
}

/// Live `mock-mcp-server` children launched for THIS case, recognized by the
/// scratch directory in their command line (see [`mock_server_entry`]).
///
/// Counting every `mock-mcp-server` on the machine also sees children from a
/// concurrent run in another worktree and children leaked by an earlier test
/// binary that are still exiting. A delta assertion on that count then fails
/// for processes the case never launched, which is what made the pooling rows
/// flake under load.
fn mock_child_process_count(dir: &Path) -> usize {
    let needle = dir.to_string_lossy();
    process_command_lines()
        .iter()
        .filter(|line| line.contains("mock-mcp-server") && line.contains(needle.as_ref()))
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
    // `-ww` is load-bearing, not cosmetic: without it `ps` truncates the command
    // to the inherited `COLUMNS` width, which cuts the scratch directory off the
    // tail of `mock-mcp-server <data-dir>/downstream.jsonl`. The per-case child
    // count then reads zero and the pooling rows flake on any machine whose
    // environment exports COLUMNS (a terminal, a CI runner). `process_report`
    // already asks for unlimited width for the same reason.
    let output = Command::new("ps")
        .args(["-ww", "-axo", "command="])
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

/// Regression: the process table is machine-wide, so a per-case count is only
/// right when the case's scratch directory survives into the `ps` output. `ps`
/// clips `command=` to the inherited `COLUMNS` width, so a probe whose marker
/// sits at the end of a long command line must still be visible.
#[cfg(unix)]
#[test]
fn process_command_lines_are_not_truncated_by_columns() {
    // Serialized with the matrix rows so the global COLUMNS set below cannot race
    // a case that is counting processes.
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::env::var_os("COLUMNS");
    struct RestoreColumns(Option<std::ffi::OsString>);
    impl Drop for RestoreColumns {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("COLUMNS", value),
                None => std::env::remove_var("COLUMNS"),
            }
        }
    }
    // Restore even on failure, so a regression cannot leak the narrow width.
    let _restore = RestoreColumns(previous);
    std::env::set_var("COLUMNS", "40");

    let marker = format!(
        "toolport-columns-probe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let pad = "x".repeat(240);
    // `read` is a shell builtin, so the shell stays in the process table with
    // this exact argv. A `sh -c "sleep 30"` probe execs the lone command
    // instead, and the kernel replaces the argv with `sleep 30`, dropping the
    // marker. bash does that exec (it is macOS `/bin/sh`), dash does not (it is
    // the Linux CI `/bin/sh`), so the exec version only tested the width on
    // Linux. stdin stays open in the guard below, so `read` blocks until the
    // probe is killed.
    let child = Command::new("sh")
        .arg("-c")
        .arg("read _line")
        .arg(&pad)
        .arg(&marker)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn a long-command probe");
    struct ProbeGuard(std::process::Child);
    impl Drop for ProbeGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _probe = ProbeGuard(child);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if process_command_lines()
            .iter()
            .any(|line| line.contains(marker.as_str()))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "COLUMNS truncated the probe's long command line out of the process table"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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

fn demand_root_replacement(
    client: &mut AdapterClient,
    transcript: &Path,
    tool: &str,
    completed_method: &str,
    failure: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "{failure}");
        // Demand once even if the watcher has already started the replacement.
        // Its initialize frame precedes catalog and subscription restoration.
        client.call_tool(tool, json!({}));
        // A successful demand can outlive this scheduling deadline. Observe its
        // completion before deciding whether to start another RPC.
        if transcript_method_count(transcript, completed_method) >= 2 {
            break;
        }
    }
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

/// Only completed publication markers count, including timestamped gateway logs.
fn initial_catalog_announced(log: &str, servers: &[&str]) -> bool {
    let startup = log.lines().find_map(|line| {
        let (_, ids) = line.split_once("background build: initial catalog announced; servers=")?;
        serde_json::from_str::<Vec<String>>(ids).ok()
    });
    startup.is_some()
        && servers.iter().all(|server| {
            startup.as_ref().unwrap().iter().any(|id| id == server)
                || log.lines().any(|line| {
                    line.split_once("catalog_publish reason=reconnect_adoption servers=")
                        .and_then(|(_, rest)| rest.split_once(" tools="))
                        .is_some_and(|(ids, _)| ids.split(", ").any(|id| id == *server))
                })
        })
}

#[test]
fn initial_catalog_barrier_accepts_startup_reconnect_and_mixed_publications() {
    let startup = "2026-10-07T00:00:00Z pid=1 role=daemon background build: initial catalog announced; servers=[\"one\"]\n";
    let reconnect = "2026-10-07T00:00:01Z pid=1 role=daemon catalog_publish reason=reconnect_adoption servers=two tools=8; scoped notification delivery is recorded per session\n";
    assert!(initial_catalog_announced(startup, &["one"]));
    assert!(!initial_catalog_announced(startup, &["two"]));
    assert!(!initial_catalog_announced(reconnect, &["two"]));
    assert!(initial_catalog_announced(
        &format!("{startup}{reconnect}"),
        &["one", "two"]
    ));
    let empty = "background build: initial catalog announced; servers=[]\n";
    assert!(initial_catalog_announced(
        &format!("{empty}{reconnect}"),
        &["two"]
    ));
    assert!(!initial_catalog_announced(
        "background build: 8 tools from 1 servers\nconnected 'one' (8 tools)\n",
        &["one"],
    ));
}

/// Finish startup and each server's first publication before opening the
/// sessions whose later notifications a case attributes to one scoped change.
/// Seeing a tool in tools/list is insufficient: persistence and SSE fanout can
/// still be running, and a quiet period cannot prove they finished.
fn warm_initial_catalog(dir: &Path, servers: &[&str]) -> AdapterClient {
    let mode = std::env::var("TOOLPORT_TEST_INITIAL_CATALOG_PATH").unwrap_or_default();
    assert!(
        matches!(mode.as_str(), "" | "fast" | "slow"),
        "unknown catalog path: {mode}"
    );
    if mode == "slow" {
        let path = dir.join("registry.json");
        let mut reg = registry::load_from(&path).expect("load warmup registry");
        for server in &mut reg.servers {
            server.env.push(EnvVar {
                key: "MOCK_MCP_START_DELAY_MS".to_string(),
                value: Some("250".to_string()),
                secret: false,
                unknown_fields: Default::default(),
            });
        }
        registry::save_to(&path, &reg).expect("delay fixture startups");
    }
    let mut warmup = spawn_adapter(
        dir,
        &AdapterOptions {
            profile: Some(registry::ALL_ENABLED_ACCESS),
            startup_catalog_servers: if mode == "fast" { servers } else { &[] },
            ..AdapterOptions::default()
        },
    );
    warmup.initialize("matrix-catalog-warmup");
    for server in servers {
        let prefix = format!("{server}__");
        warmup.wait_for_tool_where(&prefix, |name| name.starts_with(&prefix), RESPONSE_TIMEOUT);
    }
    wait_until(
        || {
            let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
            initial_catalog_announced(&log, servers)
        },
        "the startup and first-server catalog announcements",
        RESPONSE_TIMEOUT,
    );
    if !mode.is_empty() {
        let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap();
        let startup = log
            .lines()
            .find_map(|line| {
                let (_, ids) =
                    line.split_once("background build: initial catalog announced; servers=")?;
                serde_json::from_str::<Vec<String>>(ids).ok()
            })
            .unwrap();
        for server in servers {
            assert_eq!(
                startup.iter().any(|id| id == server),
                mode == "fast",
                "wrong startup path for {server}: {log}"
            );
        }
        eprintln!("confirmed {mode} initial catalog path for {servers:?}");
    }
    warmup
}

fn tool_publication_count(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("gateway.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.ends_with("downstream tool catalog publication completed"))
        .count()
}

/// Start the delivery deadline after this refresh has published and fanned out.
/// The tool-call reply precedes persistence, which can take longer under load.
fn wait_for_tool_publication(dir: &Path, previous: usize) {
    wait_until(
        || tool_publication_count(dir) > previous,
        "the downstream tool catalog publication",
        RESPONSE_TIMEOUT,
    );
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
    let before = daemon_process_count(&dir);

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
    // The panic carries this case's daemon processes so a failure is evidence.
    let elected_pid = &descriptor["pid"];
    let after = daemon_process_count(&dir);
    assert_eq!(
        after.saturating_sub(before),
        1,
        "twenty simultaneous adapters must elect exactly one daemon \
         (elected pid {elected_pid}, {before} before, {after} after, \
         data dir {}); matching processes:\n{}",
        dir.display(),
        daemon_process_report(&dir).join("\n")
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
    let before = mock_child_process_count(&dir);

    // Complete fixture writes before the first adapter starts its daemon.
    // Rewriting discovery choices while it boots can hold the registry lock
    // across fsync and charge fixture setup against the daemon's load budget.
    let ids = ["pool-one", "pool-two", "pool-three"];
    for id in ids {
        discovery_support::select_full(&dir, id);
    }
    let mut clients: Vec<AdapterClient> = ids
        .iter()
        .map(|id| spawn_configured_adapter(&dir, &AdapterOptions::default(), id))
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
        mock_child_process_count(&dir).saturating_sub(before),
        1,
        "three sessions on one ordinary stdio server must share one child; \
         matching processes:\n{}",
        process_report("mock-mcp-server").join("\n")
    );
}

#[test]
fn matrix_rollout_default_selects_the_shared_daemon() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("rollout-default");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).expect("load registry");
    reg.http_clients.push(registry::HttpClient {
        id: "probe-client".into(),
        label: "Probe client".into(),
        token_sha256: registry::sha256_hex("registered-probe-token"),
        profile: String::new(),
        unknown_fields: Default::default(),
    });
    registry::save_to(&path, &reg).expect("register probe client");

    let options = AdapterOptions {
        default_role: true,
        ..AdapterOptions::default()
    };
    let mut a = spawn_adapter(&dir, &options);
    let mut b = spawn_adapter(&dir, &options);
    a.initialize("matrix-default-a");
    b.initialize("matrix-default-b");
    let tool_a = a.wait_for_tool("__echo", Duration::from_secs(30));
    let tool_b = b.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(text_of(&a.call_tool(&tool_a, json!({ "text": "a" }))), "a");
    assert_eq!(text_of(&b.call_tool(&tool_b, json!({ "text": "b" }))), "b");
    let descriptor = first_descriptor(&dir).expect("daemon descriptor");
    assert_eq!(transcript_initialize_count(&transcript), 1);
    let endpoint = descriptor["endpoint"].as_str().expect("daemon endpoint");
    let token = descriptor["token"].as_str().expect("daemon bearer");
    let url = format!("http://{endpoint}{}", conduit_lib::daemon::TOPOLOGY_PATH);
    assert!(
        matches!(ureq::get(&url).call(), Err(ureq::Error::Status(401, _))),
        "topology probe must require the private bearer"
    );
    let public_route = ureq::get(&format!("http://{endpoint}/openapi.json"))
        .set("Authorization", "Bearer registered-probe-token")
        .call()
        .expect("registered client can reach an ordinary HTTP route");
    assert_eq!(public_route.status(), 200);
    for path in [
        conduit_lib::daemon::IDENTITY_PATH,
        conduit_lib::daemon::TOPOLOGY_PATH,
    ] {
        let response = ureq::get(&format!("http://{endpoint}{path}"))
            .set("Authorization", "Bearer registered-probe-token")
            .call();
        assert!(
            matches!(&response, Err(ureq::Error::Status(401, _))),
            "a registered HTTP client token reached private {path}: {response:?}"
        );
    }
    let topology = json_body(
        ureq::get(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
            .expect("authenticated topology probe"),
    );
    assert_eq!(topology["role"], "daemon");
    assert_eq!(topology["compat"], descriptor["compat"]);
    assert_eq!(topology["pid"], descriptor["pid"]);
    assert_eq!(topology["sessions"], 2);
    assert_eq!(topology["ordinaryLaunches"], 1);
    assert_eq!(topology["rootedLaunches"], 0);
    assert_eq!(topology["launches"], 1);

    let lease_url = format!(
        "http://{endpoint}{}",
        conduit_lib::daemon::HTTP_SERVICE_LEASE_PATH
    );
    let bridge_token = "leased-public-bridge-token";
    let lease_body = json!({
        "tokenSha256": registry::sha256_hex(bridge_token),
        "bindHost": "bridge.example"
    });
    assert!(matches!(
        ureq::post(&lease_url)
            .set("Authorization", "Bearer registered-probe-token")
            .send_json(lease_body.clone()),
        Err(ureq::Error::Status(401, _))
    ));
    ureq::post(&lease_url)
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(lease_body.clone())
        .expect("private bearer acquires public bridge lease");
    ureq::get(&format!("http://{endpoint}/openapi.json"))
        .set("Authorization", "Bearer registered-probe-token")
        .set("Origin", "http://127.0.0.1")
        .call()
        .expect("service lease keeps registered clients' Origin policy");
    ureq::get(&format!("http://{endpoint}/openapi.json"))
        .set("Authorization", &format!("Bearer {bridge_token}"))
        .set("Origin", "http://bridge.example")
        .call()
        .expect("leased bridge token reaches its public origin");
    assert!(matches!(
        ureq::get(&format!("http://{endpoint}/openapi.json"))
            .set("Authorization", &format!("Bearer {bridge_token}"))
            .set("Origin", "http://attacker.example")
            .call(),
        Err(ureq::Error::Status(403, _))
    ));
    for path in [
        conduit_lib::daemon::IDENTITY_PATH,
        conduit_lib::daemon::TOPOLOGY_PATH,
        conduit_lib::daemon::HTTP_SERVICE_LEASE_PATH,
        conduit_lib::daemon::SHUTDOWN_IF_IDLE_PATH,
    ] {
        let response = ureq::get(&format!("http://{endpoint}{path}"))
            .set("Authorization", &format!("Bearer {bridge_token}"))
            .call();
        assert!(
            matches!(response, Err(ureq::Error::Status(401, _))),
            "leased public bearer reached private {path}"
        );
    }
    for bearer in [bridge_token, "registered-probe-token"] {
        let response = ureq::post(&format!(
            "http://{endpoint}{}",
            conduit_lib::daemon::SHUTDOWN_IF_IDLE_PATH
        ))
        .set("Authorization", &format!("Bearer {bearer}"))
        .call();
        assert!(
            matches!(response, Err(ureq::Error::Status(401, _))),
            "a non-private bearer reached the shutdown endpoint: {response:?}"
        );
    }
    ureq::delete(&lease_url)
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(lease_body)
        .expect("private bearer releases public bridge lease");
    assert!(matches!(
        ureq::get(&format!("http://{endpoint}/openapi.json"))
            .set("Authorization", &format!("Bearer {bridge_token}"))
            .call(),
        Err(ureq::Error::Status(401, _))
    ));
}

#[test]
fn matrix_desktop_http_proxy_shares_daemon_and_releases_lease() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("desktop-http-proxy");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );
    let registry_path = dir.join("registry.json");
    let mut reg = registry::load_from(&registry_path).expect("load bridge registry");
    reg.http_clients.push(registry::HttpClient {
        id: "proxy-client".into(),
        label: "Proxy client".into(),
        token_sha256: registry::sha256_hex("registered-proxy-token"),
        profile: String::new(),
        unknown_fields: Default::default(),
    });
    registry::save_to(&registry_path, &reg).expect("register HTTP proxy client");
    let mut adapter = spawn_adapter(&dir, &AdapterOptions::default());
    adapter.initialize("matrix-http-proxy-stdio");
    let tool = adapter.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(transcript_initialize_count(&transcript), 1);

    let port_listener = TcpListener::bind("127.0.0.1:0").expect("allocate HTTP proxy port");
    let port = port_listener.local_addr().unwrap().port();
    drop(port_listener);
    let public_token = "matrix-desktop-bridge-token";
    let child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .arg("--http-proxy")
        .arg(port.to_string())
        .env("TOOLPORT_DATA_DIR", &dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env("TOOLPORT_HTTP_TOKEN", public_token)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start lightweight desktop HTTP proxy");
    let mut proxy = HttpProxyChild(child);
    let public_url = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            proxy.0.try_wait().unwrap().is_none(),
            "HTTP proxy exited before readiness"
        );
        if let Ok(response) = ureq::get(&format!("{public_url}/"))
            .timeout(Duration::from_millis(300))
            .set("Authorization", &format!("Bearer {public_token}"))
            .call()
        {
            let banner = response.into_string().expect("read proxy readiness banner");
            assert!(
                banner.starts_with("Toolport gateway (HTTP mode)."),
                "desktop readiness requires the HTTP-mode banner: {banner}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "HTTP proxy did not become ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        ureq::get(&format!("{public_url}/openapi.json"))
            .set("Authorization", "Bearer registered-proxy-token")
            .set("Origin", "http://127.0.0.1")
            .call()
            .expect("registered client reaches Shared HTTP through the proxy")
            .status(),
        200
    );

    let request = |id: u64, method: &str, mut params: Value| {
        params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28" });
        let mut request = ureq::post(&format!("{public_url}/mcp"))
            .timeout(Duration::from_secs(10))
            .set("Authorization", &format!("Bearer {public_token}"))
            .set("MCP-Protocol-Version", "2026-07-28")
            .set("Mcp-Method", method)
            .set("Accept", "application/json");
        if let Some(name) = params.get("name").and_then(Value::as_str) {
            request = request.set("Mcp-Name", name);
        }
        json_body(
            request
                .send_json(json!({
                    "jsonrpc": "2.0", "id": id, "method": method, "params": params
                }))
                .expect("public MCP request through proxy"),
        )
    };
    let listed = request(1, "tools/list", json!({}));
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|entry| entry["name"] == tool)),
        "public MCP catalog missing shared tool: {listed}"
    );
    let called = request(
        2,
        "tools/call",
        json!({ "name": tool, "arguments": { "text": "via-http" } }),
    );
    assert_eq!(text_of(&called["result"]), "via-http");
    assert_eq!(
        transcript_initialize_count(&transcript),
        1,
        "the public bridge started a second downstream copy"
    );

    let descriptor = first_descriptor(&dir).expect("shared daemon descriptor");
    let endpoint = descriptor["endpoint"].as_str().unwrap();
    let private_token = descriptor["token"].as_str().unwrap();
    let identity_url = format!("http://{endpoint}{}", conduit_lib::daemon::IDENTITY_PATH);
    drop(proxy.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(4);
    while proxy.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "HTTP proxy ignored parent EOF");
        std::thread::sleep(Duration::from_millis(25));
    }
    ureq::get(&identity_url)
        .set("Authorization", &format!("Bearer {private_token}"))
        .call()
        .expect("shared daemon survives desktop bridge exit");
    assert!(matches!(
        ureq::get(&format!("http://{endpoint}/openapi.json"))
            .set("Authorization", &format!("Bearer {public_token}"))
            .call(),
        Err(ureq::Error::Status(401, _))
    ));
}

#[test]
fn matrix_retired_legacy_override_uses_shared_daemon() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("rollout-legacy");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );

    let options = AdapterOptions {
        default_role: true,
        topology_override: Some("legacy"),
        ..AdapterOptions::default()
    };
    let mut a = spawn_adapter(&dir, &options);
    let mut b = spawn_adapter(&dir, &options);
    a.initialize("matrix-legacy-a");
    b.initialize("matrix-legacy-b");
    let tool_a = a.wait_for_tool("__echo", Duration::from_secs(30));
    let tool_b = b.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(text_of(&a.call_tool(&tool_a, json!({ "text": "a" }))), "a");
    assert_eq!(text_of(&b.call_tool(&tool_b, json!({ "text": "b" }))), "b");
    assert_eq!(
        descriptor_files(&dir).len(),
        1,
        "retired legacy must use the daemon"
    );
    assert_eq!(transcript_initialize_count(&transcript), 1);
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap();
    assert!(log.contains("Legacy gateway topology was retired in 2.0"));
}

#[test]
fn matrix_retired_legacy_registry_uses_shared_daemon() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("rollout-registry-legacy");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );
    // A 1.x registry that pinned the legacy topology. The v2 migration drops it.
    let path = dir.join("registry.json");
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    document["version"] = json!(1);
    document["gatewayTopology"] = json!("legacy");
    std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            default_role: true,
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-registry-legacy");
    let tool = client.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(
        text_of(&client.call_tool(&tool, json!({ "text": "legacy" }))),
        "legacy"
    );
    assert_eq!(
        descriptor_files(&dir).len(),
        1,
        "retired legacy must use the daemon"
    );
    assert_eq!(transcript_initialize_count(&transcript), 1);
    let migrated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], Registry::default().version);
    assert_eq!(migrated["servers"][0]["enabled"], true);
    assert!(migrated.get("gatewayTopology").is_none());
}

#[test]
fn matrix_rollout_ambiguous_daemon_startup_refuses_standalone_fallback() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("rollout-startup-fallback");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, None)],
        vec![],
    );
    let compat = CompatKey::new(env!("CARGO_PKG_VERSION"), dir.display().to_string());
    let blocked_lock = election_lock_base(&dir, &compat).with_extension("lock");
    std::fs::create_dir(&blocked_lock).expect("block daemon election lock");

    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            default_role: true,
            topology_override: Some("daemon"),
            ..AdapterOptions::default()
        },
    );
    assert!(!client.wait_exit(Duration::from_secs(25)).success());
    assert!(
        descriptor_files(&dir).is_empty(),
        "blocked election started a daemon"
    );
    assert_eq!(transcript_initialize_count(&transcript), 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if client
            .stderr
            .lock()
            .unwrap()
            .contains("refusing an in-process fallback")
        {
            break;
        }
        assert!(Instant::now() < deadline, "refusal was not reported");
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let before = mock_child_process_count(&dir);

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
        mock_child_process_count(&dir).saturating_sub(before),
        2,
        "two distinct roots must shard into two downstream children; \
         matching processes:\n{}",
        process_report("mock-mcp-server").join("\n")
    );
}

#[test]
fn matrix_pooling_sessionless_modern_requests_keep_a_warm_root_launch() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("sessionless-root-lease");
    let root = std::fs::canonicalize(fixture.add("project")).unwrap();
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("rooted", &transcript, Some("${ROOT}"))],
        vec![],
    );
    discovery_support::select_full(&dir, "sessionless-root");
    let mut bootstrap = spawn_adapter(&dir, &AdapterOptions::default());
    bootstrap.initialize("matrix-sessionless-bootstrap");
    let descriptor = wait_for_descriptor(&dir, Duration::from_secs(10));
    let endpoint = descriptor["endpoint"].as_str().unwrap();
    let token = descriptor["token"].as_str().unwrap();
    let encoded_root =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(root.to_str().unwrap().as_bytes());
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}
    });
    let request = || {
        let response = ureq::post(&format!("http://{endpoint}/mcp"))
            .set("Authorization", &format!("Bearer {token}"))
            .set(
                conduit_lib::stdio_adapter::ADAPTER_CLIENT_ID_HEADER,
                "sessionless-root",
            )
            .set(
                conduit_lib::stdio_adapter::ADAPTER_CWD_HEADER,
                &encoded_root,
            )
            .set("MCP-Protocol-Version", "2026-07-28")
            .set("Mcp-Method", "tools/list")
            .set("Accept", "application/json")
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .expect("modern rooted tools/list");
        let value: Value = serde_json::from_reader(response.into_reader()).unwrap();
        assert!(
            value["result"]["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "rooted__pwd")),
            "sessionless root must expose the rooted child: {value}"
        );
    };
    request();
    let launches = transcript_initialize_count(&transcript);
    std::thread::sleep(Duration::from_millis(2200));
    request();
    assert_eq!(transcript_initialize_count(&transcript), launches);
}

#[test]
fn matrix_pooling_integrity_pins_are_independent_per_root() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-integrity-scope");
    let root_a = std::fs::canonicalize(fixture.add("root-a")).unwrap();
    let root_b = std::fs::canonicalize(fixture.add("root-b")).unwrap();
    std::fs::write(root_a.join("toolport-mock-schema.txt"), "project-a").unwrap();
    std::fs::write(root_b.join("toolport-mock-schema.txt"), "project-b").unwrap();
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("rooted", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).unwrap();
    reg.set_safety_level(registry::SafetyLevel::Strict);
    reg.quarantine_on_drift = true;
    registry::save_to(&path, &reg).unwrap();
    let mut a = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a.clone()],
            ..AdapterOptions::default()
        },
    );
    let mut b = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_b.clone()],
            ..AdapterOptions::default()
        },
    );
    a.initialize("matrix-root-integrity-a");
    b.initialize("matrix-root-integrity-b");
    let tool = a.wait_for_tool("__pwd", Duration::from_secs(30));
    assert_eq!(b.wait_for_tool("__pwd", Duration::from_secs(30)), tool);
    assert_eq!(transcript_initialize_count(&transcript), 2);

    let resolved_root = conduit_lib::downstream::file_uri_to_path(&file_uri(&root_a))
        .expect("declared root URI decodes on this host");
    let scope = format!("root:{}", registry::sha256_hex(&resolved_root));
    let quarantine = dir.join(format!(
        "quarantine-v2-{}.json",
        registry::profile_store_key(&scope)
    ));
    std::fs::write(
        quarantine,
        json!({(tool.clone()): {"change": "changed"}}).to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while a.tool_names().contains(&tool) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !a.tool_names().contains(&tool),
        "root A quarantine was ignored"
    );
    assert!(
        b.tool_names().contains(&tool),
        "root A quarantine leaked to B"
    );
}

#[test]
fn matrix_pooling_quarantine_reaches_every_cached_root_view() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-quarantine");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let registry_path = dir.join("registry.json");
    let mut registry_value: Registry =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    registry_value.set_safety_level(registry::SafetyLevel::Strict);
    registry_value.quarantine_on_drift = true;
    registry::save_to(&registry_path, &registry_value).unwrap();

    let mut a = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a],
            ..AdapterOptions::default()
        },
    );
    let mut b = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_b],
            ..AdapterOptions::default()
        },
    );
    a.initialize("matrix-root-quarantine-a");
    b.initialize("matrix-root-quarantine-b");
    let tool = a.wait_for_tool("__pwd", Duration::from_secs(30));
    assert_eq!(b.wait_for_tool("__pwd", Duration::from_secs(30)), tool);
    std::fs::write(
        dir.join("quarantine.json"),
        json!({(tool.clone()): {"change": "changed"}}).to_string(),
    )
    .unwrap();

    for client in [&mut a, &mut b] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.tool_names().contains(&tool) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !client.tool_names().contains(&tool),
            "quarantined tool remained in a cached root view: {}",
            client.diagnostics()
        );
    }
}

fn quarantine_release_restores_calls(reserved_alias: bool, product_release: bool) {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _data_lock = registry::data_dir_test_lock();
    let (_fixture, dir) = Fixture::new("quarantine-release");
    let _data_dir = registry::DataDirOverride::set(&dir);
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("files", &transcript, None)],
        vec![],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).unwrap();
    reg.set_safety_level(registry::SafetyLevel::Strict);
    reg.quarantine_on_drift = true;
    let exposed = "files__echo";
    let policy_name = if reserved_alias {
        reg.tool_overrides.insert(
            "files".into(),
            std::collections::HashMap::from([(
                "echo".into(),
                registry::ToolOverride {
                    name: Some("toolport_custom_echo".into()),
                    description: None,
                    unknown_fields: Default::default(),
                },
            )]),
        );
        "toolport_custom_echo"
    } else {
        exposed
    };
    registry::save_to(&path, &reg).unwrap();
    let profile = reg.active_profile_id.as_deref().unwrap();
    let stores = [
        dir.join("quarantine.json"),
        dir.join(format!(
            "quarantine-v2-{}.json",
            registry::profile_store_key(profile)
        )),
    ];
    for store in &stores {
        std::fs::write(
            store,
            json!({(policy_name): {"server": "files", "tool": "echo", "change": "changed"}})
                .to_string(),
        )
        .unwrap();
    }
    let mut client = spawn_adapter(&dir, &AdapterOptions::default());
    client.initialize("matrix-quarantine-release");
    client.wait_for_tool("__add", Duration::from_secs(30));
    assert!(!client.tool_names().contains(&exposed.to_string()));
    for name in [policy_name, exposed] {
        assert_eq!(
            client.call_tool(name, json!({"text": "held"}))["isError"],
            true
        );
        assert_eq!(
            client.call_tool(
                "toolport_call_tool",
                json!({"name": name, "arguments": {"text": "held"}}),
            )["isError"],
            true
        );
    }

    if product_release {
        // Both React IPC and GTK re-approve call this controller. The daemon
        // must observe its store write without a registry edit or reconnect.
        for scope in [None, Some(profile)] {
            conduit_lib::registry_controller::release_quarantine(scope, policy_name).unwrap();
            assert!(conduit_lib::integrity::quarantined(scope)
                .unwrap()
                .is_empty());
        }
    } else {
        // The watcher explicitly supports hand edits as well as product release.
        for store in &stores {
            std::fs::write(store, "{}").unwrap();
        }
    }
    client.wait_for_tool_where(exposed, |name| name == exposed, Duration::from_secs(10));
    for (name, arguments) in [
        (exposed, json!({"text": "released"})),
        (
            "toolport_call_tool",
            json!({"name": exposed, "arguments": {"text": "released"}}),
        ),
    ] {
        let result = client.call_tool(name, arguments);
        assert_ne!(result["isError"], true, "released call failed: {result}");
        assert!(result.to_string().contains("released"), "{result}");
    }
    if reserved_alias {
        assert!(!client.tool_names().contains(&policy_name.to_string()));
        assert_eq!(client.call_tool(policy_name, json!({}))["isError"], true);
        assert_eq!(
            client.call_tool(
                "toolport_call_tool",
                json!({"name": policy_name, "arguments": {}}),
            )["isError"],
            true
        );
    }
}

#[test]
fn matrix_quarantine_release_reserved_alias_product() {
    quarantine_release_restores_calls(true, true);
}

#[test]
fn matrix_quarantine_release_reserved_alias_disk() {
    quarantine_release_restores_calls(true, false);
}

#[test]
fn matrix_quarantine_release_normal_product() {
    quarantine_release_restores_calls(false, true);
}

#[test]
fn matrix_quarantine_release_normal_disk() {
    quarantine_release_restores_calls(false, false);
}

#[test]
fn matrix_pooling_rooted_servers_launch_only_for_authorized_profiles() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-profile-launches");
    let root = fixture.add("root");
    let first_transcript = dir.join("first.jsonl");
    let second_transcript = dir.join("second.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("first", &first_transcript, Some("${ROOT}")),
            mock_server_entry("second", &second_transcript, Some("${ROOT}")),
        ],
        vec![
            profile("first-profile", &["first"]),
            profile("second-profile", &["second"]),
        ],
    );
    let options = |profile: &'static str| AdapterOptions {
        profile: Some(profile),
        roots: vec![root.clone()],
        ..AdapterOptions::default()
    };
    let mut first = spawn_adapter(&dir, &options("first-profile"));
    first.initialize("matrix-root-first-profile");
    first.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(transcript_initialize_count(&first_transcript), 1);
    assert_eq!(
        transcript_initialize_count(&second_transcript),
        0,
        "the first profile must not launch the other profile's rooted server"
    );

    let mut second = spawn_adapter(&dir, &options("second-profile"));
    second.initialize("matrix-root-second-profile");
    second.wait_for_tool("__echo", Duration::from_secs(30));
    assert_eq!(transcript_initialize_count(&first_transcript), 1);
    assert_eq!(transcript_initialize_count(&second_transcript), 1);
}

#[test]
fn matrix_pooling_rooted_catalog_change_reaches_only_its_root() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-catalog-change");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let mut a = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a],
            ..AdapterOptions::default()
        },
    );
    let mut b = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_b],
            ..AdapterOptions::default()
        },
    );
    a.initialize("matrix-root-catalog-a");
    b.initialize("matrix-root-catalog-b");
    let grow = a.wait_for_tool("__grow", Duration::from_secs(30));
    b.wait_for_tool("__grow", Duration::from_secs(30));
    for client in [&a, &b] {
        while client.lines.try_recv().is_ok() {}
        client.pending_notifications.lock().unwrap().clear();
    }

    a.call_tool(&grow, json!({}));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = a
            .observed_message(remaining.max(Duration::from_millis(1)))
            .expect("root A did not receive tools/list_changed");
        if message["method"] == "notifications/tools/list_changed" {
            break;
        }
    }
    a.wait_for_tool("__greet", Duration::from_secs(30));
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(message) = b.observed_message(remaining) else {
            break;
        };
        assert_ne!(
            message["method"], "notifications/tools/list_changed",
            "root B received root A's tool catalog change"
        );
    }
    assert!(!b.tool_names().iter().any(|name| name.ends_with("__greet")));
    assert_eq!(transcript_initialize_count(&transcript), 2);
}

#[test]
fn matrix_pooling_equal_resource_uris_keep_rooted_subscriptions_separate() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-resource-subscriptions");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let options = |root| AdapterOptions {
        roots: vec![root],
        ..AdapterOptions::default()
    };
    let mut a = spawn_adapter(&dir, &options(root_a));
    let mut b = spawn_adapter(&dir, &options(root_b));
    a.initialize("matrix-root-sub-a");
    b.initialize("matrix-root-sub-b");
    let grow = a.wait_for_tool("__grow", Duration::from_secs(30));
    b.wait_for_tool("__grow", Duration::from_secs(30));
    for client in [&mut a, &mut b] {
        let reply = client.request("resources/subscribe", json!({ "uri": "mock://base" }));
        assert_eq!(reply["result"], json!({}), "subscribe failed: {reply}");
        while client.lines.try_recv().is_ok() {}
    }

    a.next_id += 1;
    let grow_id = a.next_id;
    a.send(json!({
        "jsonrpc": "2.0",
        "id": grow_id,
        "method": "tools/call",
        "params": { "name": grow, "arguments": {} }
    }));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_reply = false;
    let mut got_update = false;
    loop {
        if got_reply && got_update {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = a
            .lines
            .recv_timeout(remaining.max(Duration::from_millis(1)))
            .unwrap_or_else(|error| {
                panic!(
                    "root A did not receive its resource update ({error})\ntranscript:\n{}\n{}",
                    std::fs::read_to_string(&transcript).unwrap_or_default(),
                    a.diagnostics()
                )
            });
        let message: Value = serde_json::from_str(&line).expect("valid notification");
        if message["id"] == grow_id {
            assert!(message.get("result").is_some(), "grow failed: {message}");
            got_reply = true;
        }
        if message["method"] == "notifications/resources/updated" {
            assert_eq!(message["params"]["uri"], "mock://base");
            got_update = true;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = b.lines.recv_timeout(remaining) else {
            break;
        };
        let message: Value = serde_json::from_str(&line).expect("valid notification");
        assert_ne!(
            message["method"], "notifications/resources/updated",
            "root B received root A's resource update"
        );
    }
    assert_eq!(
        transcript_initialize_count(&transcript),
        2,
        "unexpected rooted downstream initialization\ntranscript:\n{}\nroot A:\n{}\nroot B:\n{}",
        std::fs::read_to_string(&transcript).unwrap_or_default(),
        a.diagnostics(),
        b.diagnostics()
    );
}

#[test]
fn matrix_pooling_rooted_server_request_returns_to_the_originating_adapter() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("rooted-server-request");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let options = |root| AdapterOptions {
        roots: vec![root],
        elicitation: true,
        ..AdapterOptions::default()
    };
    let mut origin = spawn_adapter(&dir, &options(root_a));
    let mut other = spawn_adapter(&dir, &options(root_b));
    origin.initialize("matrix-rooted-origin");
    other.initialize("matrix-rooted-other");
    let tool = origin.wait_for_tool("__legacy_elicitation", Duration::from_secs(30));
    other.wait_for_tool("__legacy_elicitation", Duration::from_secs(30));
    let result = origin.call_tool(&tool, json!({}));
    assert!(
        text_of(&result).contains("legacy confirmed"),
        "the rooted server request did not reach its caller: {result}"
    );
    assert_eq!(origin.elicitation_queries.load(Ordering::Relaxed), 1);
    assert_eq!(other.elicitation_queries.load(Ordering::Relaxed), 0);
    assert_eq!(transcript_initialize_count(&transcript), 2);
}

#[test]
fn matrix_pooling_unused_root_launch_exits_while_another_root_stays_live() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-launch-retirement");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let before = mock_child_process_count(&dir);
    let mut a = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a],
            ..AdapterOptions::default()
        },
    );
    let mut b = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_b],
            ..AdapterOptions::default()
        },
    );
    a.initialize("matrix-retire-a");
    b.initialize("matrix-retire-b");
    let a_pwd = a.wait_for_tool("__pwd", Duration::from_secs(30));
    let b_pwd = b.wait_for_tool("__pwd", Duration::from_secs(30));
    a.call_tool(&a_pwd, json!({}));
    b.call_tool(&b_pwd, json!({}));
    assert_eq!(mock_child_process_count(&dir).saturating_sub(before), 2);

    a.close_stdin();
    assert!(a.wait_exit(Duration::from_secs(10)).success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while mock_child_process_count(&dir).saturating_sub(before) > 1 {
        assert!(
            Instant::now() < deadline,
            "unused root child stayed live:\n{}",
            process_report("mock-mcp-server").join("\n")
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!b.call_tool(&b_pwd, json!({}))["isError"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(transcript_initialize_count(&transcript), 2);
}

#[test]
fn matrix_pooling_root_views_keep_one_ordinary_child() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("mixed-root-pool");
    let root_a = fixture.add("root-a");
    let root_b = fixture.add("root-b");
    let ordinary_log = dir.join("ordinary.jsonl");
    let rooted_log = dir.join("rooted.jsonl");
    write_registry(
        &dir,
        vec![
            mock_server_entry("ordinary", &ordinary_log, None),
            mock_server_entry("rooted", &rooted_log, Some("${ROOT}")),
        ],
        vec![],
    );
    let before = mock_child_process_count(&dir);
    let mut a = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a],
            ..AdapterOptions::default()
        },
    );
    let mut b = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_b],
            ..AdapterOptions::default()
        },
    );
    a.initialize("matrix-mixed-a");
    b.initialize("matrix-mixed-b");
    for client in [&mut a, &mut b] {
        let ordinary = client.wait_for_tool("ordinary__echo", Duration::from_secs(30));
        let rooted = client.wait_for_tool("rooted__pwd", Duration::from_secs(30));
        assert_eq!(
            text_of(&client.call_tool(&ordinary, json!({ "text": "shared" }))),
            "shared"
        );
        client.call_tool(&rooted, json!({}));
    }
    assert_eq!(transcript_initialize_count(&ordinary_log), 1);
    assert_eq!(transcript_initialize_count(&rooted_log), 2);
    assert_eq!(mock_child_process_count(&dir).saturating_sub(before), 3);
}

#[test]
fn matrix_pooling_root_restarts_only_when_its_effective_spec_changes() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-secret-generation");
    let root = fixture.add("project");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let before = mock_child_process_count(&dir);
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root],
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-root-secret-generation");
    let pwd = client.wait_for_tool("__pwd", Duration::from_secs(30));
    client.call_tool(&pwd, json!({}));
    assert_eq!(transcript_initialize_count(&transcript), 1);

    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).expect("load fixture registry");
    reg.secrets_generation += 1;
    registry::save_to(&path, &reg).expect("rotate secret generation");
    // An unrelated vault revision must leave this plain server warm.
    std::thread::sleep(Duration::from_millis(2200));
    assert!(client.call_tool(&pwd, json!({}))["isError"] != true);
    assert_eq!(transcript_initialize_count(&transcript), 1);
    // Changing a connection input retires only that effective launch.
    reg.servers[0].env.push(registry::EnvVar {
        key: "ROOT_LAUNCH_REVISION".into(),
        value: Some("2".into()),
        secret: false,
        unknown_fields: Default::default(),
    });
    registry::save_to(&path, &reg).expect("change root launch spec");
    demand_root_replacement(
        &mut client,
        &transcript,
        &pwd,
        "initialize",
        "the old rooted launch survived an effective spec change",
    );
    assert!(
        client.call_tool(&pwd, json!({}))["isError"] != true,
        "replacement child must remain callable"
    );
    let settled = Instant::now() + Duration::from_secs(10);
    while mock_child_process_count(&dir).saturating_sub(before) != 1 {
        assert!(
            Instant::now() < settled,
            "root launch did not settle to one live child: {}\n{}",
            process_report("mock-mcp-server").join("\n"),
            client.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        client.call_tool(&pwd, json!({}))["isError"] != true,
        "replacement child must remain callable"
    );
}

#[test]
fn matrix_pooling_rooted_subscription_survives_an_effective_spec_change() {
    rooted_subscription_rollover(false);
}

#[test]
fn matrix_pooling_rooted_subscription_survives_a_slow_replacement_call() {
    rooted_subscription_rollover(true);
}

fn rooted_subscription_rollover(slow_call: bool) {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-subscription-rollover");
    let root = fixture.add("project");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root],
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-root-subscription-rollover");
    let pwd = client.wait_for_tool("__pwd", Duration::from_secs(30));
    let grow = client.wait_for_tool("__grow", Duration::from_secs(30));
    let subscribe = client.request("resources/subscribe", json!({ "uri": "mock://base" }));
    assert_eq!(
        subscribe["result"],
        json!({}),
        "subscribe failed: {subscribe}"
    );
    assert_eq!(
        transcript_method_count(&transcript, "resources/subscribe"),
        1
    );

    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).expect("load fixture registry");
    reg.secrets_generation += 1;
    reg.servers[0].env.push(registry::EnvVar {
        key: "ROOT_LAUNCH_REVISION".into(),
        value: Some("2".into()),
        secret: false,
        unknown_fields: Default::default(),
    });
    if slow_call {
        // Delay only the replacement. Its first demand completes after the
        // rollover deadline, but inside the existing RPC budget.
        reg.servers[0].env.push(EnvVar {
            key: "MOCK_MCP_CALL_DELAY_MS".into(),
            value: Some("11000".into()),
            secret: false,
            unknown_fields: Default::default(),
        });
    }
    registry::save_to(&path, &reg).expect("rotate secret generation");
    demand_root_replacement(
        &mut client,
        &transcript,
        &pwd,
        "resources/subscribe",
        "replacement rooted child did not resume the subscription",
    );
    assert_eq!(
        transcript_method_count(&transcript, "resources/subscribe"),
        2,
        "the replacement child did not resume the subscription"
    );
    if slow_call {
        return;
    }
    while client.lines.try_recv().is_ok() {}

    client.next_id += 1;
    let grow_id = client.next_id;
    client.send(json!({
        "jsonrpc": "2.0",
        "id": grow_id,
        "method": "tools/call",
        "params": { "name": grow, "arguments": {} }
    }));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_reply = false;
    let mut got_update = false;
    while !got_reply || !got_update {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = client
            .lines
            .recv_timeout(remaining.max(Duration::from_millis(1)))
            .expect("replacement child did not deliver its resource update");
        let message: Value = serde_json::from_str(&line).expect("valid gateway frame");
        if message["id"] == grow_id {
            assert!(message.get("result").is_some(), "grow failed: {message}");
            got_reply = true;
        }
        if message["method"] == "notifications/resources/updated" {
            assert_eq!(message["params"]["uri"], "mock://base");
            got_update = true;
        }
    }
}

#[test]
fn matrix_pooling_live_root_change_drops_the_old_resource_subscription() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-subscription-change");
    let root_a = std::fs::canonicalize(fixture.add("root-a")).expect("root A");
    let root_b = std::fs::canonicalize(fixture.add("root-b")).expect("root B");
    let transcript = dir.join("downstream.jsonl");
    write_registry(
        &dir,
        vec![mock_server_entry("mock", &transcript, Some("${ROOT}"))],
        vec![],
    );
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            roots: vec![root_a.clone()],
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-live-root-subscription");
    let pwd = client.wait_for_tool("__pwd", Duration::from_secs(30));
    let grow = client.wait_for_tool("__grow", Duration::from_secs(30));
    let subscribe = client.request("resources/subscribe", json!({ "uri": "mock://base" }));
    assert_eq!(
        subscribe["result"],
        json!({}),
        "subscribe failed: {subscribe}"
    );
    let queries_before = client.roots_queries.load(Ordering::Relaxed);

    client.set_roots(vec![root_b.clone()]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = client.call_tool(&pwd, json!({}));
        let current = text_of(&reply);
        if std::fs::canonicalize(&current).ok().as_ref() == Some(&root_b) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root change never selected B: {reply}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(client.roots_queries.load(Ordering::Relaxed) > queries_before);
    assert_eq!(transcript_initialize_count(&transcript), 2);
    assert_eq!(
        transcript_method_count(&transcript, "resources/subscribe"),
        1,
        "a subscription for root A must not silently move to root B"
    );

    let subscribe = client.request("resources/subscribe", json!({ "uri": "mock://base" }));
    assert_eq!(
        subscribe["result"],
        json!({}),
        "B subscribe failed: {subscribe}"
    );
    assert_eq!(
        transcript_method_count(&transcript, "resources/subscribe"),
        2
    );
    while client.lines.try_recv().is_ok() {}
    client.next_id += 1;
    let grow_id = client.next_id;
    client.send(json!({
        "jsonrpc": "2.0",
        "id": grow_id,
        "method": "tools/call",
        "params": { "name": grow, "arguments": {} }
    }));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got_reply = false;
    let mut got_update = false;
    while !got_reply || !got_update {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = client
            .lines
            .recv_timeout(remaining.max(Duration::from_millis(1)))
            .expect("new root did not deliver its resource update");
        let message: Value = serde_json::from_str(&line).expect("valid gateway frame");
        if message["id"] == grow_id {
            assert!(message.get("result").is_some(), "grow failed: {message}");
            got_reply = true;
        }
        if message["method"] == "notifications/resources/updated" {
            assert_eq!(message["params"]["uri"], "mock://base");
            got_update = true;
        }
    }
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
    discovery_support::select_full(&dir, "matrix-rescope");
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
    // The daemon refuses the old session before dispatch. The adapter reopens it
    // and sends the refused request again, so the client never sees the rescope
    // as an error.
    let names = list_until_no_error(
        &mut client,
        |names| names.iter().any(|name| name.starts_with("two__")),
        "the two__ prefix after the rescope",
    );
    assert!(
        !names.iter().any(|name| name.starts_with("one__")),
        "the reopened session still exposes the old profile: {names:?}"
    );
}

/// Poll tools/list until `done` holds, failing on any error reply. A scope change
/// must reach the client as the new catalog, never as a failed request.
fn list_until_no_error(
    client: &mut AdapterClient,
    done: impl Fn(&[String]) -> bool,
    label: &str,
) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let reply = client.request("tools/list", json!({}));
        assert!(
            reply.get("error").is_none(),
            "a scope change surfaced as an error: {reply}\n{}",
            client.diagnostics()
        );
        let names: Vec<String> = reply["result"]["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect();
        if done(&names) {
            return names;
        }
        assert!(
            Instant::now() < deadline,
            "no catalog matching {label} before the deadline: {names:?}\n{}",
            client.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Turn one server off in the active profile, the way a Team handoff swaps a
/// personal server for its Team copy, then wait until `observer` sees the change.
/// The observer is a separate session, so the session under test sends nothing
/// until the daemon has loaded the new enabled set.
fn disable_and_observe(dir: &Path, server: &str, observer: &mut AdapterClient) {
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).expect("load fixture registry");
    let active = reg.active_profile_id();
    reg.set_server_enabled(&active, server, false)
        .expect("disable the server");
    registry::save_to(&path, &reg).expect("save the enabled-set change");
    let prefix = format!("{server}__");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let reply = observer.request("tools/list", json!({}));
        let visible = reply["result"]["tools"].as_array().is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with(&prefix))
            })
        });
        if reply.get("result").is_some() && !visible {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never loaded the enabled-set change\n{}",
            observer.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn enabled_set_fixture(tag: &str) -> (Fixture, PathBuf, AdapterClient, AdapterClient) {
    let (fixture, dir) = Fixture::new(tag);
    write_registry(
        &dir,
        vec![
            mock_server_entry("one", &dir.join("one.jsonl"), None),
            mock_server_entry("two", &dir.join("two.jsonl"), None),
        ],
        vec![],
    );
    let mut client = spawn_adapter(&dir, &AdapterOptions::default());
    let mut observer = spawn_adapter(&dir, &AdapterOptions::default());
    client.initialize(tag);
    observer.initialize(&format!("{tag}-observer"));
    for session in [&mut client, &mut observer] {
        session.wait_for_tool_where(
            "both servers",
            |name| name.starts_with("two__"),
            Duration::from_secs(30),
        );
        session.wait_for_tool_where(
            "both servers",
            |name| name.starts_with("one__"),
            Duration::from_secs(30),
        );
    }
    (fixture, dir, client, observer)
}

/// Wait for a downstream transcript to settle at exactly `expected` calls.
fn assert_call_count(path: &Path, expected: usize, label: &str) {
    wait_until(
        || transcript_method_count(path, "tools/call") >= expected,
        label,
        Duration::from_secs(10),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        transcript_method_count(path, "tools/call"),
        expected,
        "{label}"
    );
}

#[test]
fn matrix_routing_enabled_set_change_resends_a_refused_call_once() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir, mut client, mut observer) = enabled_set_fixture("enabled-set");
    let transcript = dir.join("one.jsonl");
    let echo = client.wait_for_tool_where(
        "one__echo",
        |name| name.starts_with("one__") && name.ends_with("__echo"),
        Duration::from_secs(30),
    );
    assert_eq!(
        text_of(&client.call_tool(&echo, json!({ "text": "before" }))),
        "before"
    );
    let calls = transcript_method_count(&transcript, "tools/call");

    disable_and_observe(&dir, "two", &mut observer);

    // The client's session belongs to the old enabled set. Its next request is
    // refused before dispatch, then sent once more on a fresh session.
    let reply = client.request(
        "tools/call",
        json!({ "name": echo, "arguments": { "text": "after" } }),
    );
    assert!(
        reply.get("error").is_none() && reply["result"]["isError"] != true,
        "the refused call was not recovered: {reply}\n{}",
        client.diagnostics()
    );
    assert_eq!(text_of(&reply["result"]), "after");
    assert_call_count(&transcript, calls + 1, "the resent call ran exactly once");
}

#[test]
fn matrix_routing_resent_call_uses_the_narrowed_enabled_set() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir, mut client, mut observer) = enabled_set_fixture("narrowed-set");
    let transcript = dir.join("two.jsonl");
    let removed = client.wait_for_tool_where(
        "two__echo",
        |name| name.starts_with("two__") && name.ends_with("__echo"),
        Duration::from_secs(30),
    );
    let calls = transcript_method_count(&transcript, "tools/call");

    disable_and_observe(&dir, "two", &mut observer);

    // The fresh session no longer includes the disabled server, so the gateway
    // itself refuses the call. It must not reach the downstream, and it must not
    // look like a transport failure.
    let reply = client.request(
        "tools/call",
        json!({ "name": removed, "arguments": { "text": "blocked" } }),
    );
    assert!(
        !reply.to_string().contains("host daemon request failed"),
        "the refused session reached the client: {reply}\n{}",
        client.diagnostics()
    );
    assert!(
        reply["result"]["isError"] == true || reply.get("error").is_some(),
        "a call to a disabled server must be refused: {reply}"
    );
    assert_call_count(&transcript, calls, "the disabled server never ran the call");
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
    // Strict asks before non-destructive calls to untrusted servers.
    server.source = Some("shared".to_string());
    write_registry(&dir, vec![server], vec![echo_only, add_only]);
    let registry_path = dir.join("registry.json");
    let mut reg = registry::load_from(&registry_path).expect("load registry");
    reg.set_safety_level(registry::SafetyLevel::Strict);
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
    let broker_stop = Arc::new(AtomicBool::new(false));
    let broker_requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::clone(&broker_stop);
    let requests = Arc::clone(&broker_requests);
    let broker = std::thread::spawn(move || {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // BSD may inherit nonblocking mode from the listener. The
                    // broker waits for the request after answering the challenge.
                    stream
                        .set_nonblocking(false)
                        .expect("blocking approval socket");
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
                    requests.lock().unwrap().push(request.tool);
                    writeln!(stream, "\"approved\"").expect("approval decision");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline || !requests.lock().unwrap().is_empty(),
                        "no approval request"
                    );
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
    let before = transcript_method_count(&transcript, "tools/call");
    let rejected = echo.call_tool(&add_tool, json!({ "a": 2, "b": 3 }));
    broker_stop.store(true, Ordering::Release);
    broker.join().expect("approval broker");
    assert_eq!(*broker_requests.lock().unwrap(), ["echo"]);
    assert_eq!(transcript_method_count(&transcript, "tools/call"), before);
    assert!(
        rejected["isError"] == true,
        "approval did not preserve the caller's tool scope"
    );
}

#[test]
fn matrix_pooling_approved_rooted_call_rebinds_to_its_root_view() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut fixture, dir) = Fixture::new("root-hitl");
    let root = std::fs::canonicalize(fixture.add("project")).expect("project root");
    let transcript = dir.join("rooted.jsonl");
    let mut server = mock_server_entry("rooted", &transcript, Some("${ROOT}"));
    server.source = Some("shared".to_string());
    write_registry(
        &dir,
        vec![server],
        vec![profile("rooted-only", &["rooted"])],
    );
    let registry_path = dir.join("registry.json");
    let mut reg = registry::load_from(&registry_path).expect("load registry");
    reg.set_safety_level(registry::SafetyLevel::Strict);
    registry::save_to(&registry_path, &reg).expect("enable approval");
    let broker = approval_broker(&dir, "rooted", "pwd");

    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("rooted-only"),
            roots: vec![root.clone()],
            ..AdapterOptions::default()
        },
    );
    client.initialize("matrix-root-hitl");
    let tool = client.wait_for_tool("__pwd", Duration::from_secs(30));
    let result = client.call_tool(&tool, json!({}));
    broker.join().expect("approval broker");
    assert_eq!(
        std::fs::canonicalize(text_of(&result)).ok().as_ref(),
        Some(&root),
        "approved rooted call lost its route: {result}"
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
    let names = list_until_no_error(
        &mut client,
        |names| names.iter().any(|name| name.ends_with("__add")),
        "the new tool scope",
    );
    assert!(!names.contains(&echo));
    let add = client.wait_for_tool("__add", Duration::from_secs(30));
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
    let _warmup = warm_initial_catalog(&dir, &["shared"]);
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

    let publications = tool_publication_count(&dir);
    grow.call_tool(&grow_tool, json!({}));
    wait_for_tool_publication(&dir, publications);
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
        unknown_fields: Default::default(),
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
    let _warmup = warm_initial_catalog(&dir, &["one", "two"]);
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

    // A and B share the same downstream server. Its catalog change belongs to
    // both of them; C is scoped to another server and must not learn about it.
    let publications = tool_publication_count(&dir);
    session_a.call_tool(&grow, json!({}));
    wait_for_tool_publication(&dir, publications);
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

#[test]
fn matrix_routing_server_request_is_refused_while_another_client_has_a_call_in_flight() {
    let _guard = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_fixture, dir) = Fixture::new("concurrent-server-request");
    let transcript = dir.join("shared.jsonl");
    // Concurrent mode keeps both calls in flight on the one child at once.
    let mut server = mock_server_entry("shared", &transcript, None);
    server.env.push(EnvVar {
        key: "MOCK_MCP_CONCURRENT".to_string(),
        value: Some("1".to_string()),
        secret: false,
        unknown_fields: Default::default(),
    });
    write_registry(&dir, vec![server], vec![]);
    let options = AdapterOptions {
        elicitation: true,
        ..AdapterOptions::default()
    };
    let mut origin = spawn_adapter(&dir, &options);
    let mut other = spawn_adapter(&dir, &options);
    origin.initialize("matrix-concurrent-origin");
    other.initialize("matrix-concurrent-other");
    let tool = origin.wait_for_tool("__legacy_elicitation", Duration::from_secs(30));
    let sleep = other.wait_for_tool("__sleep", Duration::from_secs(30));
    let other = std::thread::spawn(move || {
        let result = other.call_tool(&sleep, json!({ "ms": 3000 }));
        (other, result)
    });
    wait_until(
        || transcript_method_count(&transcript, "tools/call") == 1,
        "the other client's call to reach the server",
        Duration::from_secs(10),
    );

    // JSON-RPC does not say which call the elicitation belongs to, and two
    // clients have calls in flight: neither may be asked, so it is refused.
    let started = Instant::now();
    let result = origin.call_tool(&tool, json!({}));
    assert!(
        text_of(&result).contains("legacy refused"),
        "the server request was not refused: {result}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let (other, slept) = other.join().unwrap();
    assert_eq!(text_of(&slept), "slept 3000 ms");
    assert_eq!(origin.elicitation_queries.load(Ordering::Relaxed), 0);
    assert_eq!(other.elicitation_queries.load(Ordering::Relaxed), 0);
}

// Protocol lane: every call below crosses the real adapter and daemon boundary.
fn protocol_lane_tool(name: &str, destructive: bool) -> Value {
    json!({"name":name, "description":"Protocol fixture tool.",
        "annotations":{"destructiveHint":destructive},
        "inputSchema":{"type":"object","properties":{}}})
}

fn protocol_lane_server(dir: &Path, id: &str, tools: &[Value]) -> ServerEntry {
    let catalog = dir.join(format!("catalog-{}.json", registry::sha256_hex(id)));
    std::fs::write(&catalog, serde_json::to_vec(tools).unwrap()).unwrap();
    let mut server = mock_server_entry(id, &dir.join(format!("transcript-{id}.jsonl")), None);
    server.env.push(EnvVar {
        key: "MOCK_MCP_TOOLS_FILE".into(),
        value: Some(catalog.display().to_string()),
        secret: false,
        unknown_fields: Default::default(),
    });
    server
}

fn protocol_lane_error(result: &Value, expected: &str) -> String {
    assert_eq!(result["isError"], true, "{result}");
    let text = text_of(result);
    assert!(text.contains(expected), "expected {expected:?}, got {text}");
    let tokens = conduit_lib::savings::count_tokens(&text);
    assert!(tokens <= 140, "error grew to {tokens} tokens: {text}");
    println!("PROTOCOL_ERROR tokens={tokens} text={text:?}");
    text
}

#[test]
fn protocol_lane_unknown_names_never_request_approval_or_leak_hidden_matches() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_fixture, dir) = Fixture::new("protocol-unknown");
    let tools = [
        protocol_lane_tool("read_item", false),
        protocol_lane_tool("read_items", false),
        protocol_lane_tool("read_itam", false),
        protocol_lane_tool("read_itum", false),
        {
            let mut app = protocol_lane_tool("read_itma", false);
            app["_meta"] = json!({"ui":{"visibility":["app"]}});
            app
        },
        protocol_lane_tool("delete_item", true),
    ];
    let mut scoped = profile("visible", &["files"]);
    scoped.tool_scope.insert(
        "files".into(),
        vec!["read_item".into(), "delete_item".into()],
    );
    write_registry(
        &dir,
        vec![protocol_lane_server(&dir, "files", &tools)],
        vec![scoped, profile("full", &["files"])],
    );
    let mut client = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("visible"),
            ..Default::default()
        },
    );
    client.initialize("protocol-unknown");
    client.wait_for_tool("__read_item", Duration::from_secs(30));
    let message = protocol_lane_error(
        &client.call_tool("files__read_itm", json!({})),
        "Unknown tool: files__read_itm",
    );
    assert!(
        message.contains("Close matches: files__read_item"),
        "{message}"
    );
    assert!(
        !message.contains("read_items") && !message.contains("read_itam"),
        "{message}"
    );
    assert!(message.contains("toolport_search_tools"));
    protocol_lane_error(
        &client.call_tool(
            "toolport_call_tool",
            json!({"name":"invented","arguments":{}}),
        ),
        "Unknown tool: invented",
    );
    let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
    assert!(
        !audit.contains("\"kind\":\"approval\""),
        "unknown call raised approval: {audit}"
    );
    let mut full = spawn_adapter(
        &dir,
        &AdapterOptions {
            profile: Some("full"),
            ..Default::default()
        },
    );
    full.initialize("protocol-unknown-full");
    full.wait_for_tool("__read_item", Duration::from_secs(30));
    let text = protocol_lane_error(
        &full.call_tool("files__read_itm", json!({})),
        "Unknown tool:",
    );
    let matches = text
        .lines()
        .find(|line| line.starts_with("Close matches:"))
        .unwrap();
    assert_eq!(matches.split(',').count(), 3, "{text}");
    assert!(
        !text.contains("read_itma"),
        "app-only suggestion leaked: {text}"
    );
    // A known destructive tool still fails closed without a broker.
    protocol_lane_error(
        &client.call_tool("files__delete_item", json!({})),
        "approval",
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
        if let Some(row) = rows
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|row| row["decision"] == "requested")
        {
            assert_eq!(row["safetyLevel"], "ask");
            assert_eq!(row["safetySource"], "personal");
            assert_eq!(row["gatewayVersion"], env!("CARGO_PKG_VERSION"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no approval safety snapshot: {rows}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn protocol_lane_policy_refusals_explain_the_reason_and_fix() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for case in [
        "strict",
        "client",
        "disabled",
        "quarantine",
        "reserved",
        "team",
    ] {
        let (_fixture, dir) = Fixture::new(&format!("protocol-{case}"));
        let tools = [
            protocol_lane_tool("read_item", false),
            protocol_lane_tool("delete_item", true),
        ];
        write_registry(
            &dir,
            vec![protocol_lane_server(&dir, "files", &tools)],
            vec![],
        );
        let path = dir.join("registry.json");
        let mut reg = registry::load_from(&path).unwrap();
        let profile_id = reg.active_profile_id.clone().unwrap();
        let (name, reason, fix) = match case {
            "strict" => {
                reg.set_safety_level(registry::SafetyLevel::Strict);
                ("files__delete_item", "Strict safety", "Toolport > Safety")
            }
            "team" => {
                reg.set_safety_level(registry::SafetyLevel::Off);
                reg.team_min_safety_level = registry::SafetyLevel::Strict;
                ("files__delete_item", "team's Strict safety", "team admin")
            }
            "client" => {
                reg.profiles
                    .iter_mut()
                    .find(|p| p.id == profile_id)
                    .unwrap()
                    .tool_scope
                    .insert("files".into(), vec!["read_item".into()]);
                (
                    "files__delete_item",
                    "turned off for this client",
                    "Toolport > Clients",
                )
            }
            "disabled" => {
                reg.servers[0].disabled_tools.push("delete_item".into());
                ("files__delete_item", "turned off", "Toolport > Servers")
            }
            _ => {
                let legacy = if case == "reserved" {
                    reg.tool_overrides.insert(
                        "files".into(),
                        std::collections::HashMap::from([(
                            "delete_item".into(),
                            registry::ToolOverride {
                                name: Some("toolport_old_delete".into()),
                                description: None,
                                unknown_fields: Default::default(),
                            },
                        )]),
                    );
                    "toolport_old_delete"
                } else {
                    "files__delete_item"
                };
                reg.team_forced_quarantine_on_drift = true;
                for store in [
                    dir.join("quarantine.json"),
                    dir.join(format!(
                        "quarantine-v2-{}.json",
                        registry::profile_store_key(&profile_id)
                    )),
                ] {
                    std::fs::write(store, json!({(legacy):{"server":"files","tool":"delete_item","change":"changed"}}).to_string()).unwrap();
                }
                (
                    "files__delete_item",
                    "quarantined after a tool change",
                    "Toolport > Activity",
                )
            }
        };
        registry::save_to(&path, &reg).unwrap();
        let mut client = spawn_adapter(
            &dir,
            &AdapterOptions {
                profile: Some(&profile_id),
                ..AdapterOptions::default()
            },
        );
        client.initialize(&format!("protocol-{case}"));
        client.wait_for_tool("__read_item", Duration::from_secs(30));
        for result in [
            client.call_tool(name, json!({})),
            client.call_tool("toolport_call_tool", json!({"name":name,"arguments":{}})),
        ] {
            let text = protocol_lane_error(&result, "Blocked by Toolport:");
            assert!(
                text.contains(reason) && text.contains(fix),
                "{case}: {text}"
            );
        }
        assert_eq!(
            transcript_method_count(&dir.join("transcript-files.jsonl"), "tools/call"),
            0
        );
    }
}

#[test]
fn protocol_lane_cold_calls_wait_for_catalog_or_report_starting() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for delay in [400, 4000] {
        let (_fixture, dir) = Fixture::new("protocol-cold");
        let mut server =
            protocol_lane_server(&dir, "files", &[protocol_lane_tool("read_item", false)]);
        server.args.push(format!("--start-delay-ms={delay}"));
        write_registry(&dir, vec![server], vec![]);
        let mut client = spawn_adapter(&dir, &AdapterOptions::default());
        client.initialize("protocol-cold");
        let started = Instant::now();
        let result = client.call_tool("files__read_item", json!({}));
        if delay == 400 {
            assert_eq!(text_of(&result), "read_item", "{result}");
            assert_ne!(result["isError"], true);
        } else {
            let text = protocol_lane_error(&result, "has not connected yet");
            assert!(
                text.contains("connecting") && !text.contains("approval"),
                "{text}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "first catalog budget was exceeded"
            );
            client.wait_for_tool("__read_item", Duration::from_secs(30));
            assert_eq!(
                text_of(&client.call_tool("files__read_item", json!({}))),
                "read_item"
            );
        }
        let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
        assert!(
            !audit.contains("\"kind\":\"approval\""),
            "cold read raised approval: {audit}"
        );
    }
}

#[test]
fn protocol_lane_long_aliases_route_and_survive_reorder_restart_and_old_policy() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _data_lock = registry::data_dir_test_lock();
    let (_fixture, dir) = Fixture::new("protocol-long");
    let _data_dir = registry::DataDirOverride::set(&dir);
    let long = format!("read_{}", "customer_account_details_".repeat(4));
    let twin = long.replace('_', "-");
    let unicode = format!("read_{}", "用戶".repeat(40));
    let override_name = format!("custom_{}", "account_".repeat(12));
    let tools = [
        protocol_lane_tool(&long, false),
        protocol_lane_tool(&twin, false),
        protocol_lane_tool(&unicode, false),
        protocol_lane_tool("short", false),
        protocol_lane_tool("renamed", false),
    ];
    write_registry(
        &dir,
        vec![protocol_lane_server(&dir, "files", &tools)],
        vec![],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).unwrap();
    reg.set_safety_level(registry::SafetyLevel::Strict);
    reg.tool_overrides.insert(
        "files".into(),
        std::collections::HashMap::from([(
            "renamed".into(),
            registry::ToolOverride {
                name: Some(override_name.clone()),
                description: None,
                unknown_fields: Default::default(),
            },
        )]),
    );
    registry::save_to(&path, &reg).unwrap();
    let profile = reg.active_profile_id.as_deref();
    // Seed pins using the exact old client-facing definitions. Bounding the name
    // must keep these records and fingerprints rather than silently re-pin them.
    let mut old_tools = tools.to_vec();
    for tool in &mut old_tools {
        let original = tool["name"].as_str().unwrap();
        let old_name = if original == "renamed" {
            override_name.clone()
        } else {
            let suffix = if original == long { "_2" } else { "" };
            format!(
                "files__{}{suffix}",
                conduit_lib::router::sanitize_segment(original)
            )
        };
        tool["name"] = json!(old_name);
        conduit_lib::router::normalize_tool_schema(&mut tool["inputSchema"]);
    }
    conduit_lib::integrity::check(profile, &old_tools).unwrap();
    let old_pins = serde_json::to_value(conduit_lib::integrity::baselines(profile)).unwrap();
    let aliases = conduit_lib::router::Router::server_tool_aliases(
        "files",
        &tools,
        reg.tool_overrides.clone(),
    );
    assert_eq!(aliases["short"], "files__short");
    assert_eq!(aliases.len(), tools.len());
    assert_ne!(aliases[&long], aliases[&twin]);
    assert!(aliases.values().all(
        |name| name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    ));
    for restart in 0..2 {
        let mut client = spawn_adapter(&dir, &AdapterOptions::default());
        client.initialize("protocol-long");
        client.wait_for_tool("__short", Duration::from_secs(30));
        let names = client.tool_names();
        for (original, alias) in &aliases {
            assert!(names.contains(alias), "alias missing: {alias}: {names:?}");
            assert_eq!(text_of(&client.call_tool(alias, json!({}))), *original);
        }
        assert_eq!(
            serde_json::to_value(conduit_lib::integrity::baselines(profile)).unwrap(),
            old_pins,
            "alias change rewrote old pins"
        );
        drop(client);
        kill_daemons(&dir);
        if restart == 0 {
            let mut reversed = tools.to_vec();
            reversed.reverse();
            protocol_lane_server(&dir, "files", &reversed);
        }
    }
    // Existing quarantine names still block their bounded aliases after restart.
    let legacy = format!("files__{}_2", conduit_lib::router::sanitize_segment(&long));
    for store in [
        dir.join("quarantine.json"),
        dir.join(format!(
            "quarantine-v2-{}.json",
            registry::profile_store_key(profile.unwrap())
        )),
    ] {
        std::fs::write(store, json!({(legacy.clone()):{"server":"files","tool":long,"change":"changed"}, (override_name.clone()):{"server":"files","tool":"renamed","change":"changed"}}).to_string()).unwrap();
    }
    let mut client = spawn_adapter(&dir, &AdapterOptions::default());
    client.initialize("protocol-long-quarantine");
    client.wait_for_tool("__short", Duration::from_secs(30));
    for original in [&long, &"renamed".to_string()] {
        protocol_lane_error(
            &client.call_tool(&aliases[original], json!({})),
            "quarantined after a tool change",
        );
    }
    assert_eq!(text_of(&client.call_tool(&aliases[&twin], json!({}))), twin);
}

#[test]
fn protocol_lane_long_destructive_names_keep_approval_and_team_source() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_fixture, dir) = Fixture::new("protocol-long-approval");
    let original = format!("{}_delete", "account_".repeat(12));
    let mut tool = protocol_lane_tool(&original, true);
    tool.as_object_mut().unwrap().remove("annotations");
    let aliases = conduit_lib::router::Router::server_tool_aliases(
        "files",
        &[tool.clone()],
        Default::default(),
    );
    write_registry(
        &dir,
        vec![protocol_lane_server(&dir, "files", &[tool])],
        vec![],
    );
    let path = dir.join("registry.json");
    let mut reg = registry::load_from(&path).unwrap();
    reg.set_safety_level(registry::SafetyLevel::Off);
    reg.team_min_safety_level = registry::SafetyLevel::Ask;
    registry::save_to(&path, &reg).unwrap();
    let mut client = spawn_adapter(&dir, &AdapterOptions::default());
    client.initialize("protocol-long-approval");
    let alias = &aliases[&original];
    client.wait_for_tool_where("long alias", |name| name == alias, Duration::from_secs(30));
    protocol_lane_error(
        &client.call_tool(alias, json!({})),
        "approval service was unreachable",
    );
    assert_eq!(
        transcript_method_count(&dir.join("transcript-files.jsonl"), "tools/call"),
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
        if let Some(row) = rows
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|row| row["decision"] == "requested")
        {
            assert_eq!(row["safetyLevel"], "ask");
            assert_eq!(row["safetySource"], "team_floor");
            assert_eq!(row["gatewayVersion"], env!("CARGO_PKG_VERSION"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no team approval snapshot: {rows}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn protocol_lane_long_server_aliases_keep_both_identity_parts_and_cached_routes() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_fixture, dir) = Fixture::new("protocol-long-server");
    let server = format!("service_{}", "account_".repeat(12));
    let tools = [protocol_lane_tool("read_item", false)];
    let aliases =
        conduit_lib::router::Router::server_tool_aliases(&server, &tools, Default::default());
    let alias = &aliases["read_item"];
    assert!(
        alias.len() <= 64 && alias.contains("__read_item_"),
        "{alias}"
    );
    write_registry(
        &dir,
        vec![protocol_lane_server(&dir, &server, &tools)],
        vec![],
    );
    let mut first = spawn_adapter(&dir, &AdapterOptions::default());
    first.initialize("protocol-long-server");
    first.wait_for_tool_where(
        "long server alias",
        |name| name == alias,
        Duration::from_secs(30),
    );
    let found = first.call_tool("toolport_search_tools", json!({"query":"","server":server}));
    assert!(
        text_of(&found).contains(alias),
        "raw server selector lost its bounded alias: {found}"
    );
    assert_eq!(text_of(&first.call_tool(alias, json!({}))), "read_item");
    drop(first);
    kill_daemons(&dir);
    let mut second = spawn_adapter(&dir, &AdapterOptions::default());
    second.initialize("protocol-long-server-restart");
    assert_eq!(text_of(&second.call_tool(alias, json!({}))), "read_item");
}
