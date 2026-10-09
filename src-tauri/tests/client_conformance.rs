//! Fixture-only client replay plus downstream OAuth expiry. Real CLI probes are opt-in.
#![cfg(feature = "test-support")]

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use conduit_lib::{registry, remote, secrets};
use serde_json::{json, Value};

#[test]
fn client_profiles_replay_against_the_real_adapter_and_daemon() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let output = Command::new("node")
        .arg(root.join("scripts/client-conformance.mjs"))
        .env(
            "TOOLPORT_GATEWAY_BIN",
            env!("CARGO_BIN_EXE_toolport-gateway"),
        )
        .env("TOOLPORT_MOCK_BIN", env!("CARGO_BIN_EXE_mock-mcp-server"))
        .current_dir(root)
        .output()
        .expect("Node is required for the client conformance harness");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "client replay failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct AuthFixture {
    dir: std::path::PathBuf,
    _data: registry::DataDirOverride,
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}
impl AuthFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-client-auth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let data = registry::DataDirOverride::set(&dir);
        let mut fixture = Self {
            dir,
            _data: data,
            saved: vec![],
        };
        let keys: Vec<_> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| key.starts_with("TOOLPORT_") || key.starts_with("CONDUIT_"))
            .collect();
        for key in keys {
            fixture.set(&key, None);
        }
        fixture.set(
            "TOOLPORT_DATA_DIR",
            Some(fixture.dir.clone().into_os_string()),
        );
        fixture.set(
            "TOOLPORT_SECRET_KEY",
            Some("disposable-client-auth-key".into()),
        );
        fixture
    }
    fn set(&mut self, key: &str, value: Option<std::ffi::OsString>) {
        self.saved.push((key.into(), std::env::var_os(key)));
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
}
impl Drop for AuthFixture {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Provider {
    origin: String,
    expired: Arc<AtomicBool>,
    revoked: Arc<AtomicBool>,
    refreshes: Arc<AtomicUsize>,
    successful_calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Provider {
    fn new() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let expired = Arc::new(AtomicBool::new(false));
        let revoked = Arc::new(AtomicBool::new(false));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let successful_calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (expired_worker, revoked_worker, count, calls, done) = (
            expired.clone(),
            revoked.clone(),
            refreshes.clone(),
            successful_calls.clone(),
            stop.clone(),
        );
        let worker = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(100)).unwrap()
                else {
                    continue;
                };
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let mut status = 200;
                let response = if request.url() == "/token" {
                    count.fetch_add(1, Ordering::SeqCst);
                    assert!(body.contains("grant_type=refresh_token"));
                    assert!(body.contains("refresh_token=fixture-refresh"));
                    if revoked_worker.load(Ordering::SeqCst) {
                        status = 400;
                        json!({"error":"invalid_grant"})
                    } else {
                        json!({"access_token":"fixture-new","token_type":"Bearer","expires_in":3600,"refresh_token":"fixture-refresh"})
                    }
                } else {
                    let auth = request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.as_str());
                    let accepted = !revoked_worker.load(Ordering::SeqCst)
                        && (auth == Some("Bearer fixture-new")
                            || (!expired_worker.load(Ordering::SeqCst)
                                && auth == Some("Bearer fixture-old")));
                    if !accepted {
                        status = 401;
                        json!({"error":"expired fixture"})
                    } else if request.method() != &tiny_http::Method::Post {
                        status = 405;
                        json!({})
                    } else {
                        let rpc: Value = serde_json::from_str(&body).unwrap();
                        if rpc.get("id").is_none() {
                            status = 202;
                            json!({})
                        } else {
                            let result = match rpc["method"].as_str().unwrap() {
                                "initialize" => {
                                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"auth-fixture","version":"1"},"capabilities":{"tools":{}}})
                                }
                                "tools/list" => {
                                    json!({"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{}}}]})
                                }
                                "tools/call" => {
                                    calls.fetch_add(1, Ordering::SeqCst);
                                    json!({"content":[{"type":"text","text":"fixture success"}]})
                                }
                                _ => json!({}),
                            };
                            json!({"jsonrpc":"2.0","id":rpc["id"],"result":result})
                        }
                    }
                };
                request
                    .respond(
                        tiny_http::Response::from_string(response.to_string())
                            .with_status_code(status)
                            .with_header(
                                tiny_http::Header::from_bytes("Content-Type", "application/json")
                                    .unwrap(),
                            ),
                    )
                    .unwrap();
            }
        });
        Self {
            origin,
            expired,
            revoked,
            refreshes,
            successful_calls,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn downstream_oauth_expiry_refreshes_once_and_revocation_fails_closed() {
    let _lock = registry::data_dir_test_lock();
    let _fixture = AuthFixture::new();
    let provider = Provider::new();
    let server: registry::ServerEntry = serde_json::from_value(json!({"id":"client-conformance-auth","name":"Auth fixture",
        "enabled":true,"transport":"http","url":format!("{}/mcp", provider.origin),"requestTimeoutMs":5000,"initializeTimeoutMs":5000})).unwrap();
    secrets::set_secret(&server.id, secrets::HTTP_AUTH_KEY, "fixture-old").unwrap();
    remote::store_oauth_state(
        &server.id,
        Some(provider.origin.clone()),
        &format!("{}/token", provider.origin),
        "fixture-client",
        Some("fixture-refresh".into()),
        server.url.clone(),
        None,
        1,
        None,
    )
    .unwrap();
    let mut downstream = remote::connect_remote(&server).unwrap();
    downstream.call("echo", json!({})).unwrap();
    provider.expired.store(true, Ordering::SeqCst);
    downstream.call("echo", json!({})).unwrap();
    assert_eq!(provider.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.successful_calls.load(Ordering::SeqCst), 2);
    provider.revoked.store(true, Ordering::SeqCst);
    let error = downstream.call("echo", json!({})).unwrap_err();
    assert!(remote::is_auth_error(&error.to_string()), "{error}");
    assert_eq!(provider.refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(
        provider.successful_calls.load(Ordering::SeqCst),
        2,
        "revoked call ran"
    );
}
