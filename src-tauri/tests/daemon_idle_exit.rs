//! End-to-end guard for the host daemon idle exit (one-gateway-per-host P2.3).
//!
//! Starts `toolport-gateway --daemon` with a short idle grace against a scratch
//! data directory. A daemon with nothing connected exits on its own and clears its
//! descriptor; a daemon holding a live MCP session does not until that session is
//! deleted. The unit tests cover the rendezvous; this is the only test that watches
//! the process actually leave.
use conduit_lib::http_client::{RequestHeaderExt as _, ResponseResultExt as _};

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_SCRATCH_DIR_ID: AtomicU64 = AtomicU64::new(0);

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "toolport-daemon-idle-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        NEXT_SCRATCH_DIR_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Read the rendezvous descriptor the daemon publishes into the data directory.
fn read_descriptor(dir: &Path) -> Option<serde_json::Value> {
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

/// What a failure needs to explain itself: the tail of the gateway log the daemon
/// appends to inside this scratch directory. The daemon's stdout and stderr are
/// null, so this is the only record a failing CI run can leave behind.
fn diagnostics(dir: &Path) -> String {
    let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    if tail.trim().is_empty() {
        "<empty gateway.log>".to_string()
    } else {
        format!("gateway.log (last 20 lines):\n{tail}")
    }
}

fn spawn_daemon(dir: &Path, grace_ms: u64) -> ChildGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .arg("--daemon")
        .env("TOOLPORT_DATA_DIR", dir)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", grace_ms.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    ChildGuard(child)
}

fn wait_for_descriptor(child: &mut ChildGuard, dir: &Path) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = read_descriptor(dir) {
            return value;
        }
        assert!(
            child.0.try_wait().expect("daemon status").is_none(),
            "daemon exited before publishing a descriptor\n{}",
            diagnostics(dir)
        );
        assert!(
            Instant::now() < deadline,
            "no descriptor within the deadline\n{}",
            diagnostics(dir)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_exit(child: &mut ChildGuard, dir: &Path, within: Duration) -> ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.0.try_wait().expect("daemon status") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon did not exit within the deadline\n{}",
            diagnostics(dir)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn post_mcp(
    endpoint: &str,
    token: &str,
    session: Option<&str>,
    body: &serde_json::Value,
) -> ureq::http::Response<ureq::Body> {
    let mut request = conduit_lib::http_client::agent()
        .post(&format!("http://{endpoint}/mcp"))
        .set_header("Authorization", &format!("Bearer {token}"))
        .set_header("Content-Type", "application/json")
        .set_header("Accept", "application/json, text/event-stream")
        .config()
        .timeout_global(Some(Duration::from_secs(20)))
        .build();
    if let Some(session) = session {
        request = request.set_header("Mcp-Session-Id", session);
    }
    request
        .send_json(body.clone())
        .retain_status_body()
        .expect("MCP request")
}

#[test]
fn an_idle_daemon_exits_and_clears_its_descriptor() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    // Long enough for a loaded runner's 50 ms poll to see the descriptor before the
    // daemon gives up and clears it; 300 ms lost that race on Windows CI.
    let mut child = spawn_daemon(&dir, 2_000);

    wait_for_descriptor(&mut child, &dir);
    let status = wait_for_exit(&mut child, &dir, Duration::from_secs(30));
    assert!(
        status.success(),
        "unexpected daemon exit: {status:?}\n{}",
        diagnostics(&dir)
    );
    assert!(
        read_descriptor(&dir).is_none(),
        "the descriptor outlived the daemon\n{}",
        diagnostics(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_open_listen_stream_prevents_idle_exit() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    // Long enough for a loaded runner to read the descriptor and open a session before
    // the daemon gives up on an empty host; 300 ms lost that race on Windows CI.
    const GRACE_MS: u64 = 3_000;
    let mut child = spawn_daemon(&dir, GRACE_MS);

    let descriptor = wait_for_descriptor(&mut child, &dir);
    let endpoint = descriptor["endpoint"]
        .as_str()
        .expect("endpoint")
        .to_string();
    let token = descriptor["token"].as_str().expect("token").to_string();

    let initialize = post_mcp(
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
                "clientInfo": { "name": "daemon-idle-test", "version": "1" }
            }
        }),
    );
    let session = initialize
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .expect("a session id");

    // Hold a listen stream open, the way the adapter does. The open connection is the
    // lease that keeps the daemon alive.
    let mut listen = TcpStream::connect(&endpoint).expect("connect for the listen stream");
    listen
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set a read timeout");
    write!(
        listen,
        "GET /mcp HTTP/1.1\r\nHost: {endpoint}\r\nAuthorization: Bearer {token}\r\nAccept: text/event-stream\r\nMcp-Session-Id: {session}\r\n\r\n"
    )
    .expect("write the listen request");
    listen.flush().expect("flush the listen request");
    let mut head = [0u8; 128];
    let read = listen.read(&mut head).expect("read the listen response");
    let head = String::from_utf8_lossy(&head[..read]);
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "the listen stream was refused: {head}"
    );

    // Past the grace: the open stream holds the daemon up. This only has to prove
    // the lease holds it; the idle-exit test above covers the exit path. Waiting for
    // the exit here would mean waiting out the listen stream's 30s keepalive, which
    // is how long the daemon can take to notice the socket closed.
    std::thread::sleep(Duration::from_millis(GRACE_MS + 1_500));
    assert!(
        child.0.try_wait().expect("daemon status").is_none(),
        "the daemon exited while a listen stream was open\n{}",
        diagnostics(&dir)
    );
}

#[test]
fn updater_shutdown_waits_for_a_client_session_then_exits_without_the_idle_grace() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let mut child = spawn_daemon(&dir, 30_000);
    let descriptor = wait_for_descriptor(&mut child, &dir);
    let endpoint = descriptor["endpoint"].as_str().expect("endpoint");
    let token = descriptor["token"].as_str().expect("token");
    let shutdown_url = format!(
        "http://{endpoint}{}",
        conduit_lib::daemon::SHUTDOWN_IF_IDLE_PATH
    );
    assert!(matches!(
        conduit_lib::http_client::agent()
            .post(&shutdown_url)
            .send_empty()
            .retain_status_body(),
        Err(conduit_lib::http_client::Error::Status(401, _))
    ));
    let initialize = post_mcp(
        endpoint,
        token,
        None,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "updater-idle-test", "version": "1" }
            }
        }),
    );
    let session = initialize
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|value| value.to_str().ok())
        .expect("session id")
        .to_string();
    let mut listen = TcpStream::connect(endpoint).expect("connect session listen stream");
    listen
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set listen timeout");
    write!(
        listen,
        "GET /mcp HTTP/1.1\r\nHost: {endpoint}\r\nAuthorization: Bearer {token}\r\nAccept: text/event-stream\r\nMcp-Session-Id: {session}\r\n\r\n"
    )
    .expect("write listen request");
    listen.flush().expect("flush listen request");
    let mut head = [0u8; 128];
    let read = listen.read(&mut head).expect("read listen response");
    assert!(String::from_utf8_lossy(&head[..read]).starts_with("HTTP/1.1 200"));
    conduit_lib::http_client::agent()
        .post(&shutdown_url)
        .set_header("Authorization", &format!("Bearer {token}"))
        .send_empty()
        .retain_status_body()
        .expect("authenticated updater request");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.0.try_wait().expect("daemon status").is_none(),
        "the updater stopped a daemon with a live session"
    );
    conduit_lib::http_client::agent()
        .delete(&format!("http://{endpoint}/mcp"))
        .set_header("Authorization", &format!("Bearer {token}"))
        .set_header("Mcp-Session-Id", &session)
        .call()
        .retain_status_body()
        .expect("close client session");
    drop(listen);
    let status = wait_for_exit(&mut child, &dir, Duration::from_secs(5));
    assert!(status.success(), "unexpected daemon exit: {status:?}");
    assert!(
        read_descriptor(&dir).is_none(),
        "descriptor was left behind"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A copy of the gateway in its own folder, standing in for an installed app the
/// user can delete or move to the Trash while its daemon runs.
fn installed_copy(dir: &Path) -> PathBuf {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("create the app folder");
    let name = Path::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .file_name()
        .expect("gateway file name");
    let copy = app.join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_toolport-gateway"), &copy).expect("copy the gateway");
    copy
}

fn spawn_daemon_from(binary: &Path, dir: &Path, grace_ms: u64) -> ChildGuard {
    // A copy just written can be briefly "text file busy" under `cargo test`: a
    // test on another thread forking at that moment inherits the copy's write
    // handle until its exec. Retry only that error, for a bounded time.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match Command::new(binary)
            .arg("--daemon")
            .env("TOOLPORT_DATA_DIR", dir)
            .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
            .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", grace_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return ChildGuard(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("spawn the daemon: {error:?}"),
        }
    }
}

/// "Uninstall" the copy the way each platform allows while it runs: Unix
/// deletes it, Windows can only move a running image (as the Recycle Bin does).
fn uninstall_copy(binary: &Path) {
    #[cfg(unix)]
    std::fs::remove_file(binary).expect("delete the running gateway");
    #[cfg(not(unix))]
    std::fs::rename(binary, binary.with_extension("trashed")).expect("move the running gateway");
}

#[test]
fn a_daemon_whose_app_was_removed_exits_without_waiting_out_the_idle_grace() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let binary = installed_copy(&dir);
    // Far longer than the deadline below: only the removal can end it in time.
    let mut child = spawn_daemon_from(&binary, &dir, 600_000);
    wait_for_descriptor(&mut child, &dir);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.0.try_wait().expect("daemon status").is_none(),
        "the daemon exited before its app was removed\n{}",
        diagnostics(&dir)
    );
    uninstall_copy(&binary);
    let status = wait_for_exit(&mut child, &dir, Duration::from_secs(15));
    assert!(
        status.success(),
        "unexpected daemon exit: {status:?}\n{}",
        diagnostics(&dir)
    );
    assert!(
        read_descriptor(&dir).is_none(),
        "the descriptor outlived the daemon\n{}",
        diagnostics(&dir)
    );
    assert!(
        diagnostics(&dir).contains("reason=binary_removed"),
        "the exit was not attributed to the removed binary\n{}",
        diagnostics(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_removed_app_keeps_serving_a_live_session_until_it_ends() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let binary = installed_copy(&dir);
    let mut child = spawn_daemon_from(&binary, &dir, 600_000);
    let descriptor = wait_for_descriptor(&mut child, &dir);
    let endpoint = descriptor["endpoint"].as_str().expect("endpoint");
    let token = descriptor["token"].as_str().expect("token");
    let initialize = post_mcp(
        endpoint,
        token,
        None,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "removed-app-test", "version": "1" }
            }
        }),
    );
    let session = initialize
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|value| value.to_str().ok())
        .expect("session id")
        .to_string();
    let mut listen = TcpStream::connect(endpoint).expect("connect session listen stream");
    listen
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set listen timeout");
    write!(
        listen,
        "GET /mcp HTTP/1.1\r\nHost: {endpoint}\r\nAuthorization: Bearer {token}\r\nAccept: text/event-stream\r\nMcp-Session-Id: {session}\r\n\r\n"
    )
    .expect("write listen request");
    listen.flush().expect("flush listen request");
    let mut head = [0u8; 128];
    let read = listen.read(&mut head).expect("read listen response");
    assert!(String::from_utf8_lossy(&head[..read]).starts_with("HTTP/1.1 200"));

    uninstall_copy(&binary);
    // Several binary checks later, the connected client is still served.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        child.0.try_wait().expect("daemon status").is_none(),
        "the daemon dropped a live client when its app was removed\n{}",
        diagnostics(&dir)
    );
    conduit_lib::http_client::agent()
        .delete(&format!("http://{endpoint}/mcp"))
        .set_header("Authorization", &format!("Bearer {token}"))
        .set_header("Mcp-Session-Id", &session)
        .call()
        .retain_status_body()
        .expect("close client session");
    drop(listen);
    let status = wait_for_exit(&mut child, &dir, Duration::from_secs(10));
    assert!(status.success(), "unexpected daemon exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Assert the daemon is still running, then remove its app and expect the
/// usual `binary_removed` exit: the watch stays armed after a false alarm.
fn still_running_then_exits_on_removal(child: &mut ChildGuard, dir: &Path, binary: &Path) {
    assert!(
        child.0.try_wait().expect("daemon status").is_none(),
        "the daemon exited on a mismatch that recovered\n{}",
        diagnostics(dir)
    );
    assert!(
        !diagnostics(dir).contains("reason=binary_removed"),
        "{}",
        diagnostics(dir)
    );
    uninstall_copy(binary);
    let status = wait_for_exit(child, dir, Duration::from_secs(20));
    assert!(status.success(), "unexpected daemon exit: {status:?}");
    assert!(
        diagnostics(dir).contains("reason=binary_removed"),
        "{}",
        diagnostics(dir)
    );
}

#[test]
fn a_binary_that_disappears_briefly_and_comes_back_does_not_end_the_daemon() {
    // A volume that drops for a moment, or an installer that moves the app
    // aside and back, is not an uninstall.
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let binary = installed_copy(&dir);
    let mut child = spawn_daemon_from(&binary, &dir, 600_000);
    wait_for_descriptor(&mut child, &dir);
    std::thread::sleep(Duration::from_millis(500));
    let aside = binary.with_extension("aside");
    std::fs::rename(&binary, &aside).expect("move the gateway aside");
    std::thread::sleep(Duration::from_millis(1500));
    std::fs::rename(&aside, &binary).expect("put the gateway back");
    // Well past the confirmation window.
    std::thread::sleep(Duration::from_secs(8));
    still_running_then_exits_on_removal(&mut child, &dir, &binary);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_unreadable_binary_that_becomes_readable_again_does_not_end_the_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let binary = installed_copy(&dir);
    let app = binary.parent().expect("app folder").to_path_buf();
    let mut child = spawn_daemon_from(&binary, &dir, 600_000);
    wait_for_descriptor(&mut child, &dir);
    std::thread::sleep(Duration::from_millis(500));
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o000)).expect("lock the app");
    if std::fs::metadata(&binary).is_ok() {
        // Root can stat through a 000 folder; there is nothing to test.
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: this user can stat through an unreadable folder");
        return;
    }
    // Unreadable for longer than the confirmation window: unknown is not gone.
    std::thread::sleep(Duration::from_secs(8));
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).expect("unlock the app");
    std::thread::sleep(Duration::from_secs(3));
    still_running_then_exits_on_removal(&mut child, &dir, &binary);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_when_idle_stops_an_idle_daemon_at_once() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create data dir");
    let mut child = spawn_daemon(&dir, 600_000);
    let descriptor = wait_for_descriptor(&mut child, &dir);
    let pid = descriptor["pid"].as_u64().expect("pid") as u32;
    // Reap it as it exits, the way its real parent would: an unreaped child is a
    // zombie, and a zombie still has a pid.
    let (exited, status) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = exited.send(child.0.wait());
        drop(child);
    });
    let outcomes = conduit_lib::daemon::stop_when_idle(&dir, Duration::from_secs(10));
    let status = status
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            panic!("the daemon did not exit\n{}", diagnostics(&dir))
        })
        .expect("daemon status");
    assert!(status.success(), "unexpected daemon exit: {status:?}");
    assert!(
        matches!(
            outcomes.as_slice(),
            [conduit_lib::daemon::IdleStop::Stopped(stopped)
                | conduit_lib::daemon::IdleStop::Serving(stopped)] if *stopped == pid
        ),
        "{outcomes:?}\n{}",
        diagnostics(&dir)
    );
    // Nothing is left to stop on a second call.
    assert!(conduit_lib::daemon::stop_when_idle(&dir, Duration::ZERO).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
