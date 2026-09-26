//! Real vault/controller/transport/gateway checks with disposable data and strict
//! fake providers. These establish Toolport's contract, not provider connectivity.
use conduit_lib::{
    catalog, launch_inputs, registry, registry_controller as controller, secrets, server_runtime,
    sharing_controller,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    dir: PathBuf,
    env: Vec<(String, Option<std::ffi::OsString>)>,
    _override: registry::DataDirOverride,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "toolport-catalog-review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut fixture = Self {
            _override: registry::DataDirOverride::set(&dir),
            dir,
            env: vec![],
        };
        // No reads of the installed registry or platform keychain, even when
        // the developer has TOOLPORT_* or legacy CONDUIT_* overrides configured.
        let overrides: Vec<_> = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("TOOLPORT_") || name.starts_with("CONDUIT_"))
            .collect();
        for name in overrides {
            fixture.set_env(&name, None);
        }
        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "QDRANT_URL",
            "QDRANT_API_KEY",
            "COLLECTION_NAME",
        ] {
            fixture.set_env(key, None);
        }
        fixture.set_env(
            "TOOLPORT_SECRET_KEY",
            Some("disposable-catalog-review-key".into()),
        );
        fixture.set_env(
            "TOOLPORT_REGISTRY",
            Some(fixture.dir.join("registry.json").into_os_string()),
        );
        fixture.set_env(
            "TOOLPORT_DATA_DIR",
            Some(fixture.dir.clone().into_os_string()),
        );
        fixture.set_env("TOOLPORT_ALLOW_BARE_SECRET_ENV", Some("0".into()));
        registry::save_to(
            &fixture.dir.join("registry.json"),
            &registry::Registry::default(),
        )
        .unwrap();
        fixture
    }
    fn set_env(&mut self, key: &str, value: Option<std::ffi::OsString>) {
        self.env.push((key.into(), std::env::var_os(key)));
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in self.env.iter().rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
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

fn fixture_entry(name: &str) -> catalog::CatalogEntry {
    let mut entry = catalog::curated()
        .into_iter()
        .find(|e| e.name == name)
        .unwrap();
    entry.command = Some("node".into());
    entry.args.insert(
        0,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/catalog-launch-server.mjs"
        )
        .into(),
    );
    for binding in &mut entry.launch.as_mut().unwrap().bindings {
        binding.index += 1;
    }
    entry
}

#[test]
fn catalog_values_survive_vault_reload_probe_sharing_and_gateway_launch() {
    let _lock = registry::data_dir_test_lock();
    let mut fixture = Fixture::new();
    fixture.set_env(
        "TOOLPORT_SECRET_TWILIO_API_SECRET",
        Some("ambient-value-must-not-be-an-argument".into()),
    );
    let cases = [
        (
            "Twilio",
            vec![
                ("TWILIO_ACCOUNT_SID", "ACreview", false),
                ("TWILIO_API_KEY", "SKreview", true),
                ("TWILIO_API_SECRET", "review secret:$literal & value", true),
            ],
        ),
        (
            "PostgreSQL",
            vec![(
                "POSTGRES_URL",
                "postgresql://user:review%40secret@localhost/test",
                true,
            )],
        ),
        (
            "Filesystem",
            vec![("ALLOWED_DIRECTORY", "/fixture/directory with spaces", false)],
        ),
        (
            "Redis",
            vec![(
                "REDIS_URL",
                "redis://user:review%40secret@localhost:6379/0",
                true,
            )],
        ),
        ("AWS", vec![]),
        ("Qdrant", vec![]),
    ];
    let mut ids = Vec::new();
    for (name, values) in cases {
        let added = controller::add_catalog_entry(fixture_entry(name)).unwrap();
        let server = added.servers.last().unwrap();
        let id = server.id.clone();
        if !values.is_empty() || name == "Qdrant" {
            assert!(controller::set_server_enabled("default", &id, true, false).is_err());
            assert!(!server_runtime::probe_one(server).ok);
        }
        for (key, value, secret) in values {
            if secret {
                controller::set_launch_secret(&id, key, value).unwrap();
            } else {
                controller::set_launch_input_value(&id, key, Some(value.into())).unwrap();
            }
        }
        if name == "Qdrant" {
            controller::set_server_secret(&id, "QDRANT_URL", "http://127.0.0.1:6333").unwrap();
        }
        let saved = controller::set_server_enabled("default", &id, true, false).unwrap();
        let server = saved.servers.iter().find(|s| s.id == id).unwrap();
        let probe = server_runtime::probe_one(server);
        assert!(probe.ok, "{name}: {probe:?}");
        assert_eq!(probe.tool_count, 1);
        // Exercise the exact prewarm resolver and a real child handshake too.
        let args = launch_inputs::resolve_args_for_prewarm(server).unwrap();
        if name == "Twilio" {
            let transport =
                conduit_lib::downstream::StdioTransport::spawn("node", &args.args, &[], None)
                    .unwrap();
            assert_eq!(
                conduit_lib::downstream::DownstreamServer::connect(id.clone(), Box::new(transport))
                    .unwrap()
                    .tools
                    .len(),
                1
            );
        }
        ids.push(id);
    }
    // Secrets must survive reload while remaining absent from registry, backups,
    // and shared setup. Share clears member-local values too.
    let reloaded = registry::load().unwrap();
    let exported = sharing_controller::export_json(None, None, None).unwrap();
    for entry in std::fs::read_dir(&fixture.dir).unwrap().flatten() {
        if entry
            .path()
            .extension()
            .is_some_and(|e| e == "json" || e == "bak")
        {
            let data = std::fs::read_to_string(entry.path()).unwrap();
            assert!(
                !data.contains("review%40secret")
                    && !data.contains("review secret:")
                    && !data.contains("SKreview")
            );
        }
    }
    assert!(
        !exported.contains("ACreview")
            && !exported.contains("review%40secret")
            && !exported.contains("directory with spaces")
    );
    assert!(fixture.dir.join("secrets.enc").exists());
    let encrypted = std::fs::read_to_string(fixture.dir.join("secrets.enc")).unwrap();
    assert!(!encrypted.contains("SKreview"));
    let twilio = reloaded
        .servers
        .iter()
        .find(|s| s.name == "Twilio")
        .unwrap();
    secrets::set_secret(&twilio.id, "TWILIO_API_SECRET", "wrong-secret-fragment").unwrap();
    let rejected = server_runtime::probe_one(twilio);
    assert!(!rejected.ok);
    assert!(!rejected.error.unwrap().contains("secret-fragment"));
    secrets::set_secret(
        &twilio.id,
        "TWILIO_API_SECRET",
        "review secret:$literal & value",
    )
    .unwrap();
    assert!(server_runtime::probe_registered(&twilio.id).unwrap().ok);
    gateway_lists_fixture_tools(&fixture, &ids);
    let vault_path = fixture.dir.join("secrets.enc");
    let vault = std::fs::read(&vault_path).unwrap();
    std::fs::write(&vault_path, "corrupt disposable vault").unwrap();
    assert!(launch_inputs::resolve_args_for_prewarm(twilio).is_err());
    assert!(!server_runtime::probe_one(twilio).ok);
    std::fs::write(vault_path, vault).unwrap();
    registry::save_to(
        &fixture.dir.join("registry.json"),
        &registry::Registry::default(),
    )
    .unwrap();
    let (imported, count) = sharing_controller::import_json(&exported).unwrap();
    assert_eq!(count, 6);
    let filesystem = imported
        .servers
        .iter()
        .find(|s| s.name == "Filesystem")
        .unwrap();
    assert!(controller::set_server_enabled("default", &filesystem.id, true, false).is_err());
}

#[test]
fn colliding_team_entries_keep_vault_values_and_consent_after_reorder() {
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let template = catalog::curated()
        .into_iter()
        .find(|e| e.name == "Twilio")
        .unwrap();
    let row = |id: &str| {
        json!({"id":id,"name":id,"transport":"stdio","command":template.command,
        "args":template.args,"launch":template.launch})
    };
    let mut config = json!({"servers":[row("a b"), row("a-b")]});
    let mut reg = registry::load().unwrap();
    assert_eq!(
        conduit_lib::teams::apply_team_config(&mut reg, "first", &config).review,
        2
    );
    registry::save_to(&fixture.dir.join("registry.json"), &reg).unwrap();
    let before: Vec<_> = reg
        .servers
        .iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    for (id, original) in &before {
        controller::set_launch_input_value(id, "TWILIO_ACCOUNT_SID", Some(original.clone()))
            .unwrap();
        controller::set_launch_secret(id, "TWILIO_API_KEY", "SKfixture").unwrap();
        controller::set_launch_secret(id, "TWILIO_API_SECRET", &format!("secret-for-{original}"))
            .unwrap();
    }
    controller::set_server_enabled("default", &before[0].0, true, true).unwrap();
    reg = registry::load().unwrap();
    config["servers"].as_array_mut().unwrap().reverse();
    assert_eq!(
        conduit_lib::teams::apply_team_config(&mut reg, "first", &config).review,
        1
    );
    for (id, original) in &before {
        let server = reg.servers.iter().find(|s| &s.id == id).unwrap();
        assert_eq!(&server.name, original);
        assert_eq!(
            launch_inputs::resolve_args(server).unwrap().args[2],
            format!("{original}/SKfixture:secret-for-{original}")
        );
    }
    assert!(reg.is_enabled("default", &before[0].0));
    assert!(!reg.is_enabled("default", &before[1].0));
    // A changed binding requires a new enable decision even with stored values.
    config["servers"][1]["launch"]["bindings"][0]["parts"][1]["value"] = json!("-");
    assert_eq!(
        conduit_lib::teams::apply_team_config(&mut reg, "first", &config).review,
        2
    );
    assert!(!reg.is_enabled("default", &before[0].0));
    conduit_lib::teams::remove_team(&mut reg, "first");
    conduit_lib::teams::apply_team_config(&mut reg, "second", &config);
    for server in &reg.servers {
        assert!(!before.iter().any(|(id, _)| id == &server.id));
        assert_eq!(
            secrets::get_vault_secret_result(&server.id, "TWILIO_API_SECRET").unwrap(),
            None
        );
    }
}

#[test]
fn legacy_twilio_migration_keeps_vault_keys_and_requires_missing_account_setup() {
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let path = fixture.dir.join("registry.json");
    let mut reg = registry::Registry::default();
    let old: registry::ServerEntry = serde_json::from_value(json!({
        "id":"twilio-work", "name":"Twilio", "source":"catalog:curated",
        "transport":"stdio", "command":"npx", "args":["-y","@twilio-alpha/mcp"],
        "env":[{"key":"TWILIO_API_KEY","secret":true},{"key":"TWILIO_API_SECRET","secret":true}]
    }))
    .unwrap();
    let mut edited = old.clone();
    edited.id = "twilio-custom".into();
    edited.args.push("--services".into());
    reg.servers = vec![old, edited.clone()];
    reg.profiles[0].enabled_server_ids = vec!["twilio-work".into(), "twilio-custom".into()];
    secrets::set_secret("twilio-work", "TWILIO_API_KEY", "SKreview").unwrap();
    secrets::set_secret(
        "twilio-work",
        "TWILIO_API_SECRET",
        "review secret:$literal & value",
    )
    .unwrap();
    registry::save_to(&path, &reg).unwrap();
    let legacy = std::fs::read(&path).unwrap();
    let migrated = registry::load().unwrap();
    assert_eq!(migrated.servers[1], edited);
    assert!(!migrated.is_enabled("default", "twilio-work"));
    assert!(migrated.is_enabled("default", "twilio-custom"));
    assert!(launch_inputs::resolve_args(&migrated.servers[0])
        .unwrap_err()
        .contains("Account SID"));
    assert_eq!(
        std::fs::read(path.with_extension("json.bak")).unwrap(),
        legacy
    );
    let saved = std::fs::read(&path).unwrap();
    registry::load().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert_eq!(
        std::fs::read(path.with_extension("json.bak")).unwrap(),
        legacy
    );
    let configured = controller::set_launch_input_value(
        "twilio-work",
        "TWILIO_ACCOUNT_SID",
        Some("ACreview".into()),
    )
    .unwrap();
    assert_eq!(
        launch_inputs::resolve_args(&configured.servers[0])
            .unwrap()
            .args[2],
        "ACreview/SKreview:review secret:$literal & value"
    );
    controller::set_server_enabled("default", "twilio-work", true, false).unwrap();
}

#[test]
fn changed_team_remote_url_cannot_send_a_stored_token_without_new_consent() {
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let config = |url: &str| {
        json!({"servers":[{
            "id":"remote", "name":"Remote", "transport":"http", "url":url
        }]})
    };
    let mut reg = registry::load().unwrap();
    conduit_lib::teams::apply_team_config(&mut reg, "first", &config("https://1.2.3.4/mcp"));
    let id = reg.servers[0].id.clone();
    assert!(reg.is_enabled("default", &id));
    secrets::set_secret(&id, secrets::HTTP_AUTH_KEY, "first-origin-only-token").unwrap();
    for _ in 0..2 {
        let outcome = conduit_lib::teams::apply_team_config(
            &mut reg,
            "first",
            &config("https://1.2.3.5/mcp"),
        );
        assert_eq!(reg.servers[0].id, id);
        assert!(
            !reg.is_enabled("default", &id),
            "changed destinations must stay off across repeated syncs"
        );
        assert_eq!(outcome.review, 1);
        assert!(reg.servers[0].needs_team_enable_review());
        reg.set_all_enabled("default", true).unwrap();
        assert!(
            !reg.is_enabled("default", &id),
            "Enable all must not bypass review"
        );
        assert!(controller::apply_server_enabled(&mut reg, "default", &id, true, false).is_err());
        registry::save_to(&fixture.dir.join("registry.json"), &reg).unwrap();
        reg = registry::load().unwrap();
        assert!(conduit_lib::playground::list_tools(&id)
            .unwrap_err()
            .contains("review"));
        assert_eq!(
            secrets::get_vault_secret_result(&id, secrets::HTTP_AUTH_KEY)
                .unwrap()
                .as_deref(),
            Some("first-origin-only-token")
        );
    }
    registry::save_to(&fixture.dir.join("registry.json"), &reg).unwrap();
    reg = controller::set_server_enabled("default", &id, true, true).unwrap();
    conduit_lib::teams::apply_team_config(&mut reg, "first", &config("https://1.2.3.5/mcp"));
    assert!(
        reg.is_enabled("default", &id),
        "new consent survives an unchanged sync"
    );
    conduit_lib::teams::apply_team_config(&mut reg, "first", &json!({"servers":[]}));
    conduit_lib::teams::apply_team_config(&mut reg, "first", &config("https://1.2.3.6/mcp"));
    assert_ne!(
        reg.servers[0].id, id,
        "remove/re-add cannot bypass destination review"
    );
    assert_eq!(
        secrets::get_vault_secret_result(&reg.servers[0].id, secrets::HTTP_AUTH_KEY).unwrap(),
        None
    );
}

fn gateway_lists_fixture_tools(fixture: &Fixture, ids: &[String]) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let stderr_path = fixture.dir.join("gateway.stderr");
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
            .args(["--http", &port.to_string()])
            .env("TOOLPORT_HTTP_HOST", "127.0.0.1")
            .env("TOOLPORT_HTTP_TOKEN", "fixture-gateway-token")
            .env("TOOLPORT_DISCOVERY", "full")
            .env("TOOLPORT_CODE_MODE", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap(),
    );
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    let post = |body: Value, session: Option<&str>| {
        let mut request = ureq::post(&endpoint)
            .set("Authorization", "Bearer fixture-gateway-token")
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .timeout(Duration::from_secs(10));
        if let Some(session) = session {
            request = request.set("Mcp-Session-Id", session);
        }
        request.send_json(body)
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let response = loop {
        if let Ok(response) = post(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"catalog-review","version":"1"}}}),
            None,
        ) {
            break response;
        }
        assert!(child.0.try_wait().unwrap().is_none(), "gateway exited");
        assert!(Instant::now() < deadline, "gateway startup deadline");
        std::thread::sleep(Duration::from_millis(50));
    };
    let session = response.header("Mcp-Session-Id").map(str::to_string);
    let initialized: Value = response.into_json().unwrap();
    assert!(initialized.get("error").is_none(), "{initialized}");
    let listed: Value = post(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        session.as_deref(),
    )
    .unwrap()
    .into_json()
    .unwrap();
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for id in ids {
        assert!(
            names.contains(&format!("{}__ready", id.replace('-', "_")).as_str()),
            "missing {id}: {names:?}"
        );
    }
    for id in ids {
        let called: Value = post(
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":format!("{}__ready", id.replace('-', "_")),"arguments":{}}}),
            session.as_deref(),
        )
        .unwrap()
        .into_json()
        .unwrap();
        assert!(
            called.get("error").is_none() && called["result"]["isError"] != true,
            "{called}"
        );
        assert!(called["result"]["content"].is_array(), "{called}");
    }
    drop(child);
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(
        !stderr.contains("review%40secret")
            && !stderr.contains("review secret:")
            && !stderr.contains("SKreview")
    );
}

struct AuthFixture {
    origin: String,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    refreshes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl AuthFixture {
    fn new() -> Self {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let stopped = Arc::new(AtomicBool::new(false));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let (base, stop, count) = (origin.clone(), stopped.clone(), refreshes.clone());
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(50)).unwrap()
                else {
                    continue;
                };
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let path = request.url();
                let auth = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str());
                let mut status = 200;
                let response = if path.starts_with("/.well-known/oauth-protected-resource") {
                    json!({"resource":format!("{base}/mcp"),"authorization_servers":[base]})
                } else if path == "/.well-known/oauth-authorization-server" {
                    json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token")})
                } else if path == "/token" {
                    let form: std::collections::HashMap<_, _> =
                        url::form_urlencoded::parse(body.as_bytes())
                            .into_owned()
                            .collect();
                    assert_eq!(
                        form.get("grant_type").map(String::as_str),
                        Some("refresh_token")
                    );
                    assert_eq!(
                        form.get("refresh_token").map(String::as_str),
                        Some("fixture-refresh")
                    );
                    assert_eq!(
                        form.get("client_id").map(String::as_str),
                        Some("fixture-client")
                    );
                    assert_eq!(form.get("resource"), Some(&format!("{base}/mcp")));
                    count.fetch_add(1, Ordering::SeqCst);
                    json!({"access_token":"fixture-refreshed","token_type":"Bearer","expires_in":3600,"refresh_token":"fixture-rotated"})
                } else if (path == "/mcp" && auth == Some("Bearer fixture-refreshed"))
                    || (path == "/api/public/mcp"
                        && auth == Some("Basic cGstbGYtZml4dHVyZTpzay1sZi1maXh0dXJl"))
                {
                    if request.method() != &tiny_http::Method::Post {
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
                                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"local-auth-fixture","version":"1"},"capabilities":{"tools":{}}})
                                }
                                "tools/list" => {
                                    json!({"tools":[{"name":"ready","description":"Read fixture readiness","inputSchema":{"type":"object","properties":{}}}]})
                                }
                                "tools/call" => {
                                    json!({"content":[{"type":"text","text":"local auth fixture ready"}]})
                                }
                                _ => json!({}),
                            };
                            json!({"jsonrpc":"2.0","id":rpc["id"],"result":result})
                        }
                    }
                } else {
                    status = 401;
                    json!({"error":"authentication required"})
                };
                let response = tiny_http::Response::from_string(response.to_string())
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    );
                let _ = request.respond(response);
            }
        });
        Self {
            origin,
            stopped,
            refreshes,
            worker: Some(worker),
        }
    }
}
impl Drop for AuthFixture {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn remote_basic_and_oauth_refresh_use_the_disposable_vault() {
    let _lock = registry::data_dir_test_lock();
    let fixture = Fixture::new();
    let auth = AuthFixture::new();
    let mut ids = Vec::new();
    for (name, path) in [("Langfuse", "/api/public/mcp"), ("Microsoft Learn", "/mcp")] {
        let mut entry = catalog::curated()
            .into_iter()
            .find(|e| e.name == name)
            .unwrap();
        entry.url = Some(format!("{}{path}", auth.origin));
        let added = controller::add_catalog_entry(entry).unwrap();
        let server = added.servers.last().unwrap();
        let unauthenticated = server_runtime::probe_one(server);
        assert!(
            !unauthenticated.ok && unauthenticated.auth_required,
            "{unauthenticated:?}"
        );
        if name == "Langfuse" {
            controller::set_auth_token(&server.id, "Basic cGstbGYtZml4dHVyZTpzay1sZi1maXh0dXJl")
                .unwrap();
        } else {
            controller::set_auth_token(&server.id, "fixture-expired").unwrap();
            conduit_lib::remote::store_oauth_state(
                &server.id,
                Some(auth.origin.clone()),
                &format!("{}/token", auth.origin),
                "fixture-client",
                Some("fixture-refresh".into()),
                server.url.clone(),
                None,
                1,
                Some(2),
            )
            .unwrap();
        }
        let probe = server_runtime::probe_one(server);
        assert!(probe.ok && probe.tool_count == 1, "{name}: {probe:?}");
        controller::set_server_enabled("default", &server.id, true, false).unwrap();
        ids.push(server.id.clone());
    }
    assert_eq!(auth.refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        secrets::get_vault_secret_result(&ids[1], secrets::HTTP_AUTH_KEY)
            .unwrap()
            .as_deref(),
        Some("fixture-refreshed")
    );
    gateway_lists_fixture_tools(&fixture, &ids);
    assert_eq!(
        auth.refreshes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the gateway should reuse the refreshed token"
    );
}

#[test]
#[ignore = "live publisher endpoints; opt in explicitly, no credentials required"]
fn live_public_catalog_connections() {
    let _lock = registry::data_dir_test_lock();
    let _fixture = Fixture::new();
    for name in [
        "Microsoft Learn",
        "DeepWiki",
        "Cloudflare Docs",
        "Parallel Search",
        "Context7",
    ] {
        let added = controller::add_catalog_entry(
            catalog::curated()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap(),
        )
        .unwrap();
        let probe = server_runtime::probe_one(added.servers.last().unwrap());
        println!("{name}: {probe:?}");
        assert!(probe.ok && probe.tool_count > 0, "{name}: {probe:?}");
    }
    let results = catalog::search_registry("github").unwrap();
    println!(
        "Registry latest-version github query: {} mapped entries",
        results.len()
    );
    assert!(!results.is_empty());
}

#[test]
#[ignore = "downloads and runs the real publisher filesystem package; opt in explicitly"]
fn live_filesystem_package_uses_only_the_disposable_allowed_directory() {
    let _lock = registry::data_dir_test_lock();
    let mut fixture = Fixture::new();
    fixture.set_env(
        "npm_config_cache",
        Some(fixture.dir.join("npm-cache").into_os_string()),
    );
    let npmrc = fixture.dir.join("empty-npmrc");
    std::fs::write(&npmrc, "").unwrap();
    fixture.set_env("npm_config_userconfig", Some(npmrc.into_os_string()));
    let global_npmrc = fixture.dir.join("empty-global-npmrc");
    std::fs::write(&global_npmrc, "").unwrap();
    fixture.set_env(
        "npm_config_globalconfig",
        Some(global_npmrc.into_os_string()),
    );
    let allowed = fixture.dir.join("allowed directory");
    std::fs::create_dir(&allowed).unwrap();
    std::fs::write(allowed.join("example.txt"), "disposable example").unwrap();
    let added = controller::add_catalog_entry(
        catalog::curated()
            .into_iter()
            .find(|e| e.name == "Filesystem")
            .unwrap(),
    )
    .unwrap();
    let id = &added.servers[0].id;
    let configured = controller::set_launch_input_value(
        id,
        "ALLOWED_DIRECTORY",
        Some(allowed.to_str().unwrap().into()),
    )
    .unwrap();
    let probe = server_runtime::probe_one(&configured.servers[0]);
    println!("Real filesystem package: {probe:?}");
    assert!(probe.ok && probe.tool_count > 0, "{probe:?}");
}
