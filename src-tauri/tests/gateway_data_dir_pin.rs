//! A client run with its own `XDG_CONFIG_HOME` (T3 Code's OpenCode instances pass it on
//! to MCP servers) starts the gateway with the entry Toolport wrote for that row. The
//! gateway must open the user's registry, not an empty one under that folder.
#![cfg(all(unix, not(target_os = "macos")))]

use conduit_lib::{clients, registry};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolport-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

struct Gateway {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    dirs: Vec<PathBuf>,
    next_id: i64,
}

impl Gateway {
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
        )
        .expect("write to gateway");
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|e| panic!("no answer to {method}: {e}"));
            let message: Value = serde_json::from_str(&line).expect("JSON from gateway");
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
        }
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        // A host daemon the gateway started records itself in the data folder.
        for dir in &self.dirs {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.starts_with("daemon-") && name.ends_with(".json")) {
                    continue;
                }
                if let Some(pid) = std::fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .and_then(|v| v["pid"].as_u64())
                {
                    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the gateway as a client with its own `XDG_CONFIG_HOME` would: that client's
/// environment plus the entry's own.
fn start_as_client(
    home: &Path,
    xdg: &Path,
    entry_env: &[(String, String)],
    data: &Path,
) -> Gateway {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("TOOLPORT_") || key.starts_with("CONDUIT_") || key.starts_with("XDG_") {
            command.env_remove(key.as_ref());
        }
    }
    let mut child = command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("TOOLPORT_SECRET_KEY", "data-dir-pin-test-key")
        .env("TOOLPORT_DAEMON_IDLE_GRACE_MS", "5000")
        .envs(entry_env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the gateway");
    let stdin = child.stdin.take().expect("gateway stdin");
    let stdout = child.stdout.take().expect("gateway stdout");
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut gateway = Gateway {
        child,
        stdin,
        lines,
        // A release gateway would use `Toolport`, a debug one `Toolport-dev`.
        dirs: vec![
            data.to_path_buf(),
            xdg.join("Toolport"),
            xdg.join("Toolport-dev"),
        ],
        next_id: 1,
    };
    let init = gateway.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "data-dir-pin-test", "version": "1" }
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

#[test]
fn a_t3_code_home_with_its_own_xdg_config_home_still_finds_the_registry() {
    let data = scratch_dir("pin-data");
    let home = scratch_dir("pin-home");
    let xdg = home.join(".t3/hermes-providers/fixture/config");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(
        data.join("registry.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 2,
            "servers": [{
                "id": "pinned",
                "name": "pinned",
                "transport": "stdio",
                "command": env!("CARGO_BIN_EXE_mock-mcp-server"),
                "args": [],
                "env": [],
                "source": "manual",
                "disabledTools": []
            }],
            "profiles": [{ "id": "default", "name": "Default", "enabledServerIds": ["pinned"] }],
            "activeProfileId": "default",
            "lazyDiscovery": false
        }))
        .unwrap(),
    )
    .unwrap();

    // The environment Toolport writes into a T3 Code OpenCode home's entry.
    let entry_env = {
        let _lock = registry::data_dir_test_lock();
        let _data = registry::DataDirOverride::set(&data);
        clients::gateway_entry_env("opencode@t3-0123456789abcdef")
    };
    assert!(
        entry_env
            .iter()
            .any(|(key, value)| key == "TOOLPORT_DATA_DIR" && Path::new(value) == data),
        "the entry names the data folder: {entry_env:?}"
    );

    let mut gateway = start_as_client(&home, &xdg, &entry_env, &data);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !gateway.tool_names().contains(&"pinned__echo".to_string()) {
        assert!(
            Instant::now() < deadline,
            "the gateway never listed the registry's server"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let strays: Vec<_> = std::fs::read_dir(&xdg)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("Toolport") || name.starts_with("Conduit"))
        .collect();
    assert!(
        strays.is_empty(),
        "no second data folder under the client's XDG_CONFIG_HOME: {strays:?}"
    );
    drop(gateway);
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&home);
}
