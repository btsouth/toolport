//! A shared setup is an untrusted launch proposal. Importing one into a
//! running gateway must not start its local commands or contact its remote
//! servers, and must not turn off servers the user already runs. Every path
//! here is a private scratch directory; the OS keychain is never used.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use conduit_lib::{registry, sharing_controller};
use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-shared-import-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("reserve scratch dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Counts every connection made to a loopback port.
struct Listener {
    url: String,
    accepted: Arc<AtomicUsize>,
}

impl Listener {
    fn start(path: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                count.fetch_add(1, Ordering::SeqCst);
                // Close at once: the gateway sees a failed server, which is fine.
                drop(stream);
            }
        });
        Self { url, accepted }
    }

    fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

struct Gateway {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: i64,
}

impl Gateway {
    fn start(dir: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("TOOLPORT_") || name.starts_with("CONDUIT_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .env("TOOLPORT_DATA_DIR", dir)
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            .env("TOOLPORT_SECRET_KEY", "shared-import-test")
            .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", "5000")
            .env("HOME", dir)
            .env("XDG_CONFIG_HOME", dir)
            .env("XDG_DATA_HOME", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the gateway");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut gateway = Self {
            child,
            stdin,
            lines,
            next_id: 1,
        };
        let init = gateway.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "shared-import-test", "version": "1" }
            }),
        );
        assert!(init.get("result").is_some(), "initialize failed: {init}");
        writeln!(
            gateway.stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        )
        .unwrap();
        gateway
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|error| panic!("no answer to {method} ({error})"));
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The end of the gateway's log, for failure messages.
fn gateway_log(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A local command that records each time it is started, then serves MCP.
fn recording_command(dir: &Path, name: &str, marker: &Path) -> (String, Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join(format!("{name}.sh"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho started >> '{}'\nexec '{}'\n",
            marker.display(),
            env!("CARGO_BIN_EXE_mock-mcp-server")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script.display().to_string(), Vec::new())
}

#[test]
fn importing_a_shared_setup_starts_nothing_and_keeps_existing_servers_on() {
    let scratch = Scratch::new();
    let dir = &scratch.0;
    let path = dir.join("registry.json");

    // Servers the user already runs: one local, one remote. They prove this
    // gateway does start enabled servers, so silence below means something.
    let existing_marker = dir.join("existing-started");
    let (existing_command, existing_args) = recording_command(dir, "existing", &existing_marker);
    let existing_remote = Listener::start("/mcp");
    let registry_json = json!({
        "version": 2,
        "servers": [
            {"id": "keep-local", "name": "Keep local", "enabled": true, "transport": "stdio",
             "command": &existing_command, "args": existing_args, "env": [], "source": "manual"},
            {"id": "keep-remote", "name": "Keep remote", "enabled": true, "transport": "http",
             "url": existing_remote.url, "env": [], "source": "manual"}
        ],
        "profiles": [{"id": "default", "name": "Default",
                      "enabledServerIds": ["keep-local", "keep-remote"]}],
        "activeProfileId": "default",
        "lazyDiscovery": false
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&registry_json).unwrap()).unwrap();

    let mut gateway = Gateway::start(dir);
    gateway.request("tools/list", json!({}));
    wait_for(
        "the existing local server to start",
        Duration::from_secs(20),
        || existing_marker.exists(),
    );
    wait_for(
        "the existing remote server to be contacted",
        Duration::from_secs(20),
        || existing_remote.connections() > 0,
    );

    // A shared setup asking to run a command and contact two remote servers,
    // all marked enabled, plus a same-name copy of a server the user runs.
    let imported_marker = dir.join("imported-started");
    let (imported_command, imported_args) = recording_command(dir, "imported", &imported_marker);
    let imported_http = Listener::start("/mcp");
    let imported_sse = Listener::start("/sse");
    let setup = json!({"kind": "conduit-setup", "version": 1, "servers": [
        {"name": "Imported local", "enabled": true, "transport": "stdio",
         "command": imported_command, "args": imported_args, "env": []},
        {"name": "Imported HTTP", "enabled": true, "transport": "http",
         "url": imported_http.url, "env": []},
        {"name": "Imported SSE", "enabled": true, "transport": "sse",
         "url": imported_sse.url, "env": []},
        {"name": "Keep local", "enabled": false, "transport": "stdio",
         "command": "/bin/false", "args": [], "env": []}
    ]})
    .to_string();
    let (imported, added) = registry::update_at(&path, |registry| {
        sharing_controller::apply_import(registry, &setup)
    })
    .unwrap();
    assert_eq!(added, 3, "the same-name copy is skipped");

    // Give the gateway time to pick up the new registry and act on it, then
    // report the local and remote outcomes together.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        gateway.request("tools/list", json!({}));
        std::thread::sleep(Duration::from_millis(250));
    }
    let local_started = imported_marker.exists();
    let remote_contacts = imported_http.connections() + imported_sse.connections();
    // Loopback is a private address, which the gateway refuses for shared
    // servers even when they are on. Its log still shows any attempt to start
    // one, which is what an import must never cause.
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let remote_starts = log
        .lines()
        .filter(|line| {
            line.contains("supervisor_transition")
                && (line.contains("server=imported_http") || line.contains("server=imported_sse"))
        })
        .count();
    assert!(
        !local_started && remote_contacts == 0 && remote_starts == 0,
        "importing a shared setup must start nothing: local command started: {local_started}, \
         connections to its remote servers: {remote_contacts}, attempts to start them: \
         {remote_starts}\n{}",
        gateway_log(dir)
    );

    for server in &imported.servers {
        let on = imported.is_enabled("default", &server.id);
        if server.id.starts_with("keep-") {
            assert!(on, "{} must stay on", server.name);
        } else {
            assert!(!on && !server.enabled, "{} must import off", server.name);
        }
    }
    let keep = imported
        .servers
        .iter()
        .find(|server| server.id == "keep-local")
        .unwrap();
    assert_eq!(
        keep.command.as_deref(),
        Some(existing_command.as_str()),
        "not replaced"
    );

    // The window above meant something only if the gateway follows registry
    // changes: turning the imported local server on now must start it.
    let local = imported
        .servers
        .iter()
        .find(|server| server.name == "Imported local")
        .unwrap()
        .id
        .clone();
    registry::update_at(&path, |registry| {
        registry.set_global_server_enabled(&local, true)
    })
    .unwrap();
    wait_for(
        "the imported server to start once the user turns it on",
        Duration::from_secs(20),
        || {
            gateway.request("tools/list", json!({}));
            imported_marker.exists()
        },
    );
}
