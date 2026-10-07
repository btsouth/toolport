//! What a killed or crashed host daemon leaves behind, end to end.
//!
//! - Its stdio servers do not outlive it: on Linux the direct child gets
//!   SIGTERM at once, and whatever is left in a server's process group is
//!   stopped by the next gateway to start.
//! - The next daemon says the last one ended without a clean shutdown, in
//!   `toolport_status` and in `last-daemon-exit.json` for the app, and clears
//!   dead daemons' descriptors.
//! - A panic is written to `daemon.log` with its location.
//!
//! The servers used here ignore stdin EOF, like ones holding a listener or a
//! pool, so only an explicit kill stops them. Everything runs against a scratch
//! data directory, and every process signalled is one this test started.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use conduit_lib::child_ledger::ProcessId;
use conduit_lib::registry::{self, EnvVar, Registry, ServerEntry};
use serde_json::{json, Value};

static CASE_LOCK: Mutex<()> = Mutex::new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);
const DEADLINE: Duration = Duration::from_secs(60);

struct Scratch {
    dir: PathBuf,
    pid_file: PathBuf,
    /// Servers seen so far, named by start time, so cleanup after a failed
    /// case only ever signals the exact processes this case started.
    seen: Mutex<Vec<ProcessId>>,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-crash-{tag}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let pid_file = dir.join("server-pids.txt");
        Self {
            dir,
            pid_file,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn server_pids(&self) -> Vec<u32> {
        let pids: Vec<u32> = std::fs::read_to_string(&self.pid_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        let mut seen = self.seen.lock().unwrap();
        for pid in &pids {
            if !seen.iter().any(|known| known.pid == *pid) {
                if let Some(id) = ProcessId::of(*pid) {
                    seen.push(id);
                }
            }
        }
        pids
    }

    /// SIGKILL every server this case started that is still the same process.
    fn kill_servers(&self) {
        self.server_pids();
        for id in self.seen.lock().unwrap().iter() {
            if id.is_alive() {
                signal(id.pid, "-KILL");
            }
        }
    }

    fn log(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.dir.join(name)).unwrap_or_default();
        format!(
            "gateway.log:\n{}\ndaemon.log:\n{}",
            read("gateway.log"),
            read("daemon.log")
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.kill_servers();
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

fn signal(pid: u32, which: &str) {
    let _ = Command::new("kill")
        .arg(which)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Alive and not a zombie: a reparented zombie waiting for init is gone.
fn running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z"),
        // No /proc (macOS): fall back to whether the pid can be signalled.
        Err(_) => Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
    }
}

fn wait_until(what: &str, scratch: &Scratch, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out: {what}\n{}",
            scratch.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One stdio server that ignores stdin EOF. `wrapped` starts it from a shell
/// that stays as its parent, the way `npx` stays in front of node.
fn write_registry(scratch: &Scratch, wrapped: bool) {
    let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
    let (command, args) = if wrapped {
        let script = scratch.dir.join("launcher.sh");
        std::fs::write(&script, format!("#!/bin/sh\n'{mock}' <&0 &\nwait\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script.display().to_string(), Vec::new())
    } else {
        (mock.to_string(), vec![scratch.dir.display().to_string()])
    };
    let env = [
        ("MOCK_MCP_IGNORE_EOF", "1".to_string()),
        ("MOCK_MCP_PID_FILE", scratch.pid_file.display().to_string()),
    ]
    .into_iter()
    .map(|(key, value)| EnvVar {
        key: key.to_string(),
        value: Some(value),
        secret: false,
    })
    .collect();
    let server = ServerEntry {
        id: "mock".to_string(),
        name: "Mock".to_string(),
        transport: "stdio".to_string(),
        command: Some(command),
        args,
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
    registry::save_to(&scratch.dir.join("registry.json"), &registry_value).expect("registry");
}

fn spawn_daemon(scratch: &Scratch) -> ChildGuard {
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
            .arg("--daemon")
            .env("TOOLPORT_DATA_DIR", &scratch.dir)
            .env("TOOLPORT_REGISTRY", scratch.dir.join("registry.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon"),
    )
}

fn descriptor_of(scratch: &Scratch, pid: u32) -> Value {
    let mut found = None;
    wait_until("the daemon's descriptor", scratch, || {
        found = std::fs::read_dir(&scratch.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("daemon-"))
            })
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
            .find(|descriptor| descriptor["pid"] == pid);
        found.is_some()
    });
    found.unwrap()
}

/// A daemon serving the mock, with the mock's pids once it has started.
fn start_serving(scratch: &Scratch, servers: usize) -> ChildGuard {
    let daemon = spawn_daemon(scratch);
    descriptor_of(scratch, daemon.0.id());
    wait_until("the server to start", scratch, || {
        scratch.server_pids().len() >= servers
    });
    daemon
}

fn status_text(scratch: &Scratch, daemon: &ChildGuard) -> String {
    let descriptor = descriptor_of(scratch, daemon.0.id());
    let endpoint = descriptor["endpoint"].as_str().unwrap().to_string();
    let token = descriptor["token"].as_str().unwrap().to_string();
    let post = |body: Value, session: Option<&str>| {
        let mut request = ureq::post(&format!("http://{endpoint}/mcp"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .timeout(Duration::from_secs(30));
        if let Some(session) = session {
            request = request.set("Mcp-Session-Id", session);
        }
        request.send_json(body).expect("daemon request")
    };
    let initialize = post(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"crash","version":"1"}}}),
        None,
    );
    let session = initialize.header("Mcp-Session-Id").unwrap().to_string();
    let reply = post(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":"toolport_status","arguments":{}}}),
        Some(&session),
    );
    let sse = reply
        .header("Content-Type")
        .unwrap_or_default()
        .contains("text/event-stream");
    let body = reply.into_string().unwrap();
    let message: Value = if sse {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|line| serde_json::from_str(line.trim()).ok())
            .last()
            .unwrap()
    } else {
        serde_json::from_str(&body).unwrap()
    };
    message["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(target_os = "linux")]
#[test]
fn a_killed_daemon_takes_its_servers_with_it() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = Scratch::new("pdeathsig");
    write_registry(&scratch, false);
    let mut daemon = start_serving(&scratch, 1);
    let server = scratch.server_pids()[0];

    signal(daemon.0.id(), "-KILL");
    let _ = daemon.0.wait();
    wait_until("the server to die with its daemon", &scratch, || {
        !running(server)
    });
}

#[cfg(target_os = "linux")]
#[test]
fn the_next_gateway_stops_what_a_killed_daemon_left() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = Scratch::new("reap");
    write_registry(&scratch, true);
    let mut daemon = start_serving(&scratch, 1);
    let server = scratch.server_pids()[0];
    let ledger = scratch
        .dir
        .join("children")
        .join(format!("{}.json", daemon.0.id()));
    wait_until("the daemon to record its server", &scratch, || {
        ledger.exists()
    });

    signal(daemon.0.id(), "-KILL");
    let _ = daemon.0.wait();
    // The shell in front of the server dies with the daemon; the server it
    // started does not, until a gateway starts and finds it.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        running(server),
        "precondition: the wrapped server survives the kill"
    );

    let _next = spawn_daemon(&scratch);
    wait_until("the next gateway to stop the orphan", &scratch, || {
        !running(server)
    });
    assert!(!ledger.exists(), "the dead daemon's ledger is removed");
}

#[test]
fn the_next_daemon_reports_an_unclean_end_and_clears_dead_pointers() {
    let _guard = CASE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = Scratch::new("report");
    write_registry(&scratch, false);
    let mut first = spawn_daemon(&scratch);
    descriptor_of(&scratch, first.0.id());
    let first_pid = first.0.id();
    // An older build's daemon that died long ago.
    let stale = scratch.dir.join("daemon-0000000000000000.json");
    std::fs::write(
        &stale,
        json!({"endpoint":"127.0.0.1:9","token":"t","pid":999_999_999u32,
               "compat":"0000","protocol":1,"createdAtMs":0})
        .to_string(),
    )
    .unwrap();

    scratch.server_pids();
    signal(first_pid, "-KILL");
    let _ = first.0.wait();
    scratch.kill_servers();

    let second = spawn_daemon(&scratch);
    descriptor_of(&scratch, second.0.id());
    let status = status_text(&scratch, &second);
    assert!(
        status.contains(&format!("previous host daemon (pid {first_pid})"))
            && status.contains("without a clean shutdown"),
        "toolport_status must say why: {status}\n{}",
        scratch.log()
    );
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(scratch.dir.join("last-daemon-exit.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(recorded["pid"], first_pid);
    assert_eq!(recorded["cleanShutdown"], false);
    assert!(!stale.exists(), "a dead daemon's descriptor is cleared");
}

#[test]
fn a_panic_is_written_to_the_daemon_log_with_its_location() {
    let scratch = Scratch::new("panic");
    conduit_lib::daemon_log::install_panic_hook(scratch.dir.clone());
    let line = line!() + 3;
    let joined = std::thread::Builder::new()
        .name("crash-probe".to_string())
        .spawn(|| panic!("the probe gave up"))
        .unwrap()
        .join();
    assert!(joined.is_err());
    // Back to the default hook before the scratch directory goes.
    let _ = std::panic::take_hook();
    let log = std::fs::read_to_string(conduit_lib::daemon_log::log_path(&scratch.dir)).unwrap();
    assert!(
        log.contains("panic in thread 'crash-probe'")
            && log.contains(&format!("daemon_crash_recovery.rs:{line}"))
            && log.contains("the probe gave up"),
        "{log}"
    );
}
