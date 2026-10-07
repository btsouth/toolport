//! Daemon health under load and when wedged, end to end.
//!
//! - A daemon whose request workers are all busy still answers its identity
//!   probe, so no client elects a second daemon beside it.
//! - A daemon that is alive but stopped (SIGSTOP, a stand-in for a deadlock or a
//!   hung keychain call) is not an outage: a new client falls back to its own
//!   gateway, and an attached client moves to a private one, while the shared
//!   daemon's descriptor stays untouched.
//! - The HTTP bridge takes a burst of hundreds of clients, and sheds load with a
//!   503 and `Retry-After` only past its configured cap.
//!
//! Real processes throughout: the gateway binary in its daemon, adapter and HTTP
//! roles, and `mock-mcp-server` downstream, each against a scratch data
//! directory. Unix only: the wedge is a SIGSTOP.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use conduit_lib::daemon::Rendezvous;
use conduit_lib::registry::{self, EnvVar, Registry, ServerEntry};
use conduit_lib::topology::CompatKey;
use serde_json::{json, Value};

/// These cases count daemons and stop processes, so they run one at a time.
static CASE_LOCK: Mutex<()> = Mutex::new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-health-{tag}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch data dir");
        Self { dir }
    }

    fn descriptor(&self) -> Option<Value> {
        descriptor_files(&self.dir)
            .first()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str(&raw).ok())
    }

    fn wait_for_descriptor(&self) -> Value {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            if let Some(descriptor) = self.descriptor() {
                return descriptor;
            }
            assert!(
                Instant::now() < deadline,
                "no daemon descriptor\n{}",
                log_tail(&self.dir)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn log_contains(&self, needle: &str) -> bool {
        std::fs::read_to_string(self.dir.join("gateway.log"))
            .unwrap_or_default()
            .contains(needle)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The daemon is detached into its own process group; a stopped one must
        // be continued before it can act on anything but SIGKILL.
        if let Some(pid) = self.descriptor().and_then(|d| d["pid"].as_u64()) {
            signal(pid, "-CONT");
            signal(pid, "-KILL");
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn signal(pid: u64, which: &str) {
    let _ = Command::new("kill")
        .arg(which)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn descriptor_files(dir: &Path) -> Vec<PathBuf> {
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

fn log_tail(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    format!(
        "gateway.log (last 30 lines):\n{}",
        lines[lines.len().saturating_sub(30)..].join("\n")
    )
}

fn write_registry(dir: &Path, extra_env: &[(&str, &str)]) {
    let transcript = dir.join("downstream.jsonl");
    let mut env = vec![EnvVar {
        key: "MOCK_MCP_TRANSCRIPT".to_string(),
        value: Some(transcript.display().to_string()),
        secret: false,
        unknown_fields: Default::default(),
    }];
    env.extend(extra_env.iter().map(|(key, value)| EnvVar {
        key: key.to_string(),
        value: Some(value.to_string()),
        secret: false,
        unknown_fields: Default::default(),
    }));
    let server = ServerEntry {
        id: "mock".to_string(),
        name: "Mock mock".to_string(),
        transport: "stdio".to_string(),
        command: Some(env!("CARGO_BIN_EXE_mock-mcp-server").to_string()),
        args: vec![transcript.display().to_string()],
        env,
        url: None,
        source: Some("manual".to_string()),
        disabled_tools: vec![],
        cwd: None,
        client_credentials: None,
        request_timeout_ms: None,
        initialize_timeout_ms: None,
        launch: None,
        inherit_env: false,
        unknown_fields: serde_json::Map::new(),
    };
    let mut registry_value = Registry::default();
    registry_value.set_lazy_discovery(false);
    registry_value.servers = vec![server];
    if let Some(active) = registry_value.active_profile_id.clone() {
        if let Some(profile) = registry_value.profiles.iter_mut().find(|p| p.id == active) {
            profile.enabled_server_ids = vec!["mock".to_string()];
        }
    }
    registry::save_to(&dir.join("registry.json"), &registry_value).expect("write registry");
}

fn gateway(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    command
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env_remove("TOOLPORT_GATEWAY_TOPOLOGY")
        .env_remove("CONDUIT_GATEWAY_TOPOLOGY")
        .env_remove("TOOLPORT_HTTP_MAX_CONNECTIONS");
    command
}

// ---------------------------------------------------------------------------
// A line-based MCP client on one adapter process.
// ---------------------------------------------------------------------------

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
    next_id: i64,
    dir: PathBuf,
}

impl Client {
    /// `explicit` runs `--stdio-adapter`; otherwise the registry-selected role a
    /// real client gets, pinned to the daemon topology.
    fn spawn(dir: &Path, explicit: bool) -> Self {
        let mut command = gateway(dir);
        if explicit {
            command.arg("--stdio-adapter");
        } else {
            command.env("TOOLPORT_GATEWAY_TOPOLOGY", "daemon");
        }
        let mut child = command
            .env("ADAPTER_AMBIENT_CREDENTIAL", "must-not-leak")
            // Force the inherit-env fallback to expose the gateway process boundary.
            .env("SHELL", dir.join("missing-login-shell"))
            .env(
                "TOOLPORT_CLIENT_ID",
                format!("health-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the adapter");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
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
                    let mut text = stderr_text.lock().unwrap();
                    text.push_str(&line);
                    text.push('\n');
                }
            });
        }
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            stderr: stderr_text,
            next_id: 0,
            dir: dir.to_path_buf(),
        }
    }

    fn diagnostics(&self) -> String {
        format!(
            "adapter stderr:\n{}\n{}",
            self.stderr.lock().unwrap(),
            log_tail(&self.dir)
        )
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{message}").expect("write to the adapter");
        stdin.flush().expect("flush the adapter");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).unwrap_or_else(|error| {
                panic!("no answer to {method} ({error})\n{}", self.diagnostics())
            });
            let message: Value = serde_json::from_str(&line).expect("JSON from the adapter");
            if message["id"] == id {
                return message;
            }
        }
    }

    fn initialize(&mut self) {
        let reply = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "daemon-health", "version": "1" }
            }),
        );
        assert!(
            reply.get("result").is_some(),
            "initialize failed: {reply}\n{}",
            self.diagnostics()
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    /// Call the mock's `echo`, waiting out a catalog that is still building.
    fn echo(&mut self, text: &str) -> String {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let reply = self.request(
                "tools/call",
                json!({ "name": "mock__echo", "arguments": { "text": text } }),
            );
            let content = reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if reply["result"]["isError"] != true && content.contains(text) {
                return content;
            }
            assert!(
                Instant::now() < deadline,
                "echo never succeeded: {reply}\n{}",
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn status(&mut self) -> String {
        let reply = self.request(
            "tools/call",
            json!({ "name": "toolport_status", "arguments": {} }),
        );
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Daemon HTTP helpers
// ---------------------------------------------------------------------------

fn post_mcp(
    endpoint: &str,
    token: &str,
    session: Option<&str>,
    body: &Value,
) -> Result<ureq::Response, ureq::Error> {
    let mut request = ureq::post(&format!("http://{endpoint}/mcp"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .timeout(Duration::from_secs(60));
    if let Some(session) = session {
        request = request.set("Mcp-Session-Id", session);
    }
    request.send_json(body.clone())
}

fn body_json(response: ureq::Response) -> Value {
    let sse = response
        .header("Content-Type")
        .unwrap_or_default()
        .contains("text/event-stream");
    let body = response.into_string().expect("body");
    if sse {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .last()
            .and_then(|line| serde_json::from_str(line).ok())
            .unwrap_or(Value::Null)
    } else {
        serde_json::from_str(&body).unwrap_or(Value::Null)
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn a_saturated_daemon_still_answers_its_probe_and_is_never_duplicated() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = Fixture::new("saturated");
    write_registry(&fixture.dir, &[("MOCK_MCP_CALL_DELAY_MS", "3000")]);
    let _daemon = ChildGuard(
        gateway(&fixture.dir)
            .arg("--daemon")
            // Four request workers, so a handful of slow calls saturates them.
            .env("TOOLPORT_HTTP_MAX_CONNECTIONS", "4")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon"),
    );
    let descriptor = fixture.wait_for_descriptor();
    let endpoint = descriptor["endpoint"].as_str().unwrap().to_string();
    let token = descriptor["token"].as_str().unwrap().to_string();

    let initialize = post_mcp(
        &endpoint,
        &token,
        None,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                        "clientInfo": { "name": "saturate", "version": "1" } }
        }),
    )
    .expect("initialize");
    let session = initialize
        .header("Mcp-Session-Id")
        .expect("session id")
        .to_string();
    // Wait for the catalog so the slow calls below reach the server.
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let listed = body_json(
            post_mcp(
                &endpoint,
                &token,
                Some(&session),
                &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            )
            .expect("tools/list"),
        );
        let ready = listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "mock__echo"));
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "catalog never built\n{}",
            log_tail(&fixture.dir)
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Six slow calls against four workers: four hold the workers for seconds,
    // the rest are shed.
    let shed = Arc::new(AtomicUsize::new(0));
    let callers: Vec<_> = (0..6)
        .map(|index| {
            let (endpoint, token, session, shed) = (
                endpoint.clone(),
                token.clone(),
                session.clone(),
                Arc::clone(&shed),
            );
            std::thread::spawn(move || {
                let result = post_mcp(
                    &endpoint,
                    &token,
                    Some(&session),
                    &json!({ "jsonrpc": "2.0", "id": 10 + index, "method": "tools/call",
                             "params": { "name": "mock__echo", "arguments": { "text": "slow" } } }),
                );
                if let Err(ureq::Error::Status(503, response)) = result {
                    assert_eq!(response.header("Retry-After"), Some("1"));
                    shed.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(700));

    let started = Instant::now();
    let identity = ureq::get(&format!(
        "http://{endpoint}{}",
        conduit_lib::daemon::IDENTITY_PATH
    ))
    .set("Authorization", &format!("Bearer {token}"))
    .timeout(Duration::from_secs(2))
    .call()
    .map_err(|error| error.to_string());
    assert!(
        identity.is_ok(),
        "a saturated daemon must answer its probe: {identity:?}\n{}",
        log_tail(&fixture.dir)
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    // The rendezvous a new client runs must reuse this daemon, not elect another.
    let compat = CompatKey::new(env!("CARGO_PKG_VERSION"), fixture.dir.display().to_string());
    let spawns = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&spawns);
    let reused = Rendezvous::new(&fixture.dir, compat)
        .ensure(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Err("must not spawn beside a live daemon".to_string())
        })
        .expect("a saturated daemon is reused");
    assert_eq!(reused.endpoint, endpoint);
    assert_eq!(spawns.load(Ordering::SeqCst), 0);

    for caller in callers {
        caller.join().expect("caller thread");
    }
    assert!(
        shed.load(Ordering::SeqCst) > 0,
        "the case must actually saturate the daemon's workers"
    );
    assert_eq!(descriptor_files(&fixture.dir).len(), 1);
}

#[test]
fn a_wedged_daemon_moves_clients_to_their_own_gateways() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = Fixture::new("wedged");
    write_registry(&fixture.dir, &[]);
    // A real downstream launcher records its environment after private recovery.
    let launcher = fixture.dir.join("env-launcher.sh");
    let dump = fixture.dir.join("private-child-env.txt");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nenv > '{}'\nexec '{}' \"$@\"\n",
            dump.display(),
            env!("CARGO_BIN_EXE_mock-mcp-server")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = fixture.dir.join("registry.json");
    let mut reg = registry::load_from(&path).unwrap();
    reg.servers[0].command = Some(launcher.display().to_string());
    reg.servers[0].inherit_env = true;
    registry::save_to(&path, &reg).unwrap();

    let mut attached = Client::spawn(&fixture.dir, false);
    attached.initialize();
    attached.echo("before");
    let descriptor = fixture.wait_for_descriptor();
    let daemon_pid = descriptor["pid"].as_u64().expect("daemon pid");

    signal(daemon_pid, "-STOP");

    // A new client in the default role falls back to its own in-process gateway
    // instead of failing to start.
    let started = Instant::now();
    let mut fresh = Client::spawn(&fixture.dir, false);
    fresh.initialize();
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "fallback took {:?}",
        started.elapsed()
    );
    assert!(fresh.echo("fresh").contains("fresh"));
    assert!(
        fresh.status().contains("in-process gateway"),
        "toolport_status must say why: {}",
        fresh.status()
    );
    assert!(fresh
        .stderr
        .lock()
        .unwrap()
        .contains("using its own in-process gateway"));

    // An explicit adapter starts a private gateway instead.
    let mut explicit = Client::spawn(&fixture.dir, true);
    explicit.initialize();
    assert!(explicit.echo("explicit").contains("explicit"));
    assert!(explicit.status().contains("private gateway"));
    let dumped = std::fs::read_to_string(&dump).unwrap();
    assert!(
        !dumped.contains("ADAPTER_AMBIENT_CREDENTIAL"),
        "private gateway child inherited adapter credentials"
    );
    assert!(
        dumped.contains("MOCK_MCP_TRANSCRIPT="),
        "configured env was lost"
    );

    // The attached client notices the silence on its own and moves its session.
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    while !fixture.log_contains("moving to a private gateway") {
        assert!(
            Instant::now() < deadline,
            "no wedge detected\n{}",
            attached.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(attached.echo("after").contains("after"));
    assert!(attached.status().contains("private gateway"));

    // Nothing replaced or duplicated the shared daemon.
    assert_eq!(descriptor_files(&fixture.dir).len(), 1);
    assert_eq!(fixture.descriptor(), Some(descriptor));
    assert!(pid_alive(daemon_pid));

    // Each private gateway ends with its adapter.
    let log = std::fs::read_to_string(fixture.dir.join("gateway.log")).unwrap_or_default();
    let private_pids: Vec<u64> = log
        .lines()
        .filter_map(|line| line.split("role=private-gateway pid=").nth(1))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect();
    assert_eq!(private_pids.len(), 2, "{}", log_tail(&fixture.dir));
    drop(explicit);
    drop(attached);
    let deadline = Instant::now() + Duration::from_secs(15);
    while private_pids.iter().any(|pid| pid_alive(*pid)) {
        assert!(
            Instant::now() < deadline,
            "a private gateway outlived its adapter"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_http(fixture: &Fixture, cap: Option<&str>) -> (ChildGuard, u16) {
    let port = free_port();
    let mut command = gateway(&fixture.dir);
    command
        .args(["--http", &port.to_string()])
        .env("TOOLPORT_HTTP_HOST", "127.0.0.1")
        .env("TOOLPORT_HTTP_TOKEN", "health-token")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cap) = cap {
        command.env("TOOLPORT_HTTP_MAX_CONNECTIONS", cap);
    }
    let child = ChildGuard(command.spawn().expect("spawn the HTTP gateway"));
    // Wait for a whole answer, so no readiness connection still holds a slot.
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let answered = ureq::get(&format!("http://127.0.0.1:{port}/"))
            .timeout(Duration::from_secs(5))
            .call();
        if !matches!(answered, Err(ureq::Error::Transport(_))) {
            break;
        }
        assert!(Instant::now() < deadline, "HTTP gateway never answered");
        std::thread::sleep(Duration::from_millis(50));
    }
    (child, port)
}

#[test]
fn the_http_bridge_takes_a_burst_and_sheds_only_past_its_cap() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = Fixture::new("http-burst");
    write_registry(&fixture.dir, &[]);
    let (_gateway, port) = spawn_http(&fixture, None);

    let barrier = Arc::new(std::sync::Barrier::new(300));
    let clients: Vec<_> = (0..300)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let initialize = || {
                    ureq::post(&format!("http://127.0.0.1:{port}/mcp"))
                        .set("Authorization", "Bearer health-token")
                        .set("Content-Type", "application/json")
                        .set("Accept", "application/json, text/event-stream")
                        .timeout(Duration::from_secs(60))
                        .send_json(json!({
                            "jsonrpc": "2.0", "id": index, "method": "initialize",
                            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                                        "clientInfo": { "name": "burst", "version": "1" } }
                        }))
                };
                // A loaded CI runner's loopback stack can drop or reset a few of
                // 300 simultaneous connects before the gateway ever sees them. A
                // real client reconnects, so one retry is allowed for that, never
                // for an answer: a 503 or 429 is the gateway shedding load, which
                // is exactly what this burst must not cause.
                let mut transport_errors = Vec::new();
                for _ in 0..2 {
                    match initialize() {
                        Ok(response) => return (response.status(), transport_errors),
                        Err(ureq::Error::Status(code, _)) => return (code, transport_errors),
                        Err(ureq::Error::Transport(error)) => {
                            transport_errors.push(error.to_string());
                        }
                    }
                }
                (0, transport_errors)
            })
        })
        .collect();
    let mut statuses = std::collections::BTreeMap::new();
    let mut transport_errors = Vec::new();
    for client in clients {
        let (status, errors) = client.join().unwrap();
        *statuses.entry(status).or_insert(0) += 1;
        transport_errors.extend(errors);
    }
    if !transport_errors.is_empty() {
        eprintln!(
            "{} connection(s) needed a retry: {:?}",
            transport_errors.len(),
            transport_errors
        );
    }
    assert_eq!(
        statuses.get(&200),
        Some(&300),
        "300 concurrent initialize calls must all succeed: {statuses:?}; \
         transport errors: {transport_errors:?}"
    );

    // With a cap of two, two clients still sending their request hold both slots,
    // and the third is told when to retry.
    let capped = Fixture::new("http-capped");
    write_registry(&capped.dir, &[]);
    let (_capped_gateway, capped_port) = spawn_http(&capped, Some("2"));
    let held: Vec<TcpStream> = (0..2)
        .map(|_| {
            let mut stream = TcpStream::connect(("127.0.0.1", capped_port)).unwrap();
            stream
                .write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\n")
                .unwrap();
            stream
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    for mut stream in &held {
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut scratch = [0u8; 64];
        assert!(
            stream.read(&mut scratch).is_err(),
            "a client within the cap must not be answered while it is still sending"
        );
    }
    let mut third = TcpStream::connect(("127.0.0.1", capped_port)).unwrap();
    third
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    third
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    let _ = third.read_to_string(&mut answer);
    assert!(
        answer.starts_with("HTTP/1.1 503") && answer.contains("Retry-After: 1"),
        "past the cap: {answer}"
    );
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = ureq::get(&format!("http://127.0.0.1:{capped_port}/"))
            .set("Authorization", "Bearer health-token")
            .timeout(Duration::from_secs(5))
            .call();
        match response {
            Ok(response) => {
                assert_eq!(response.status(), 200);
                break;
            }
            Err(error) => {
                assert!(Instant::now() < deadline, "the cap never released: {error}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
