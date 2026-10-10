//! Shell-neutral setup import and export operations.
use crate::http_client::{RequestHeaderExt as _, ResponseResultExt as _};

use crate::registry::{self, Registry, ServerEntry};

const SHARE_ENDPOINT: &str = "https://toolport.app/api/share";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupImportItem {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub is_new: bool,
    pub reference_review: Vec<String>,
}

pub(crate) fn build_export(
    registry: &Registry,
    name: Option<&str>,
    description: Option<&str>,
    server_ids: Option<&[String]>,
) -> serde_json::Value {
    let include = server_ids.map(|ids| {
        ids.iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>()
    });
    let servers = registry
        .servers
        .iter()
        .filter(|server| !crate::clients::is_gateway_server(server))
        .filter(|server| {
            include
                .as_ref()
                .is_none_or(|ids| ids.contains(server.id.as_str()))
        })
        .map(|server| {
            let mut server = server.clone();
            server.id.clear();
            server.unknown_fields.remove("memberSecretRefs");
            for entry in &mut server.env {
                entry.value = None;
            }
            if let Some(launch) = &server.launch {
                server.launch = Some(launch.without_values());
            }
            let mask = registry::secret_arg_mask(&server.args);
            for (argument, secret) in server.args.iter_mut().zip(mask) {
                // A launch marker is structural, never a credential. Redacting
                // it would invalidate the binding in the exported setup.
                if secret && argument != "<launch-input>" {
                    *argument = "<redacted>".to_string();
                } else if is_http_url(argument) {
                    // mcp-remote style launchers carry the remote URL as an argument.
                    *argument = redact_share_url(argument);
                }
            }
            if let Some(url) = server.url.as_deref() {
                server.url = Some(redact_share_url(url));
            }
            server
        })
        .collect::<Vec<ServerEntry>>();
    let mut document =
        serde_json::json!({ "kind": "conduit-setup", "version": 1, "servers": servers });
    if let Some(name) = name.map(str::trim).filter(|value| !value.is_empty()) {
        document["name"] = serde_json::json!(name);
    }
    if let Some(description) = description.map(str::trim).filter(|value| !value.is_empty()) {
        document["description"] = serde_json::json!(description);
    }
    document
}

/// Stands in for a credential-shaped URL path segment in a shared setup.
pub(crate) const SHARE_KEY_PLACEHOLDER: &str = "YOUR_API_KEY";
/// Stands in for every query parameter value in a shared setup.
pub(crate) const SHARE_VALUE_PLACEHOLDER: &str = "YOUR_VALUE";

fn is_http_url(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Strip every credential a hosted MCP server might embed in its URL before the
/// URL leaves the machine: userinfo, every query value (names are kept so the
/// recipient knows what to fill in), credential-shaped path segments such as
/// `https://mcp.instantly.ai/mcp/<key>`, and the fragment. Biased toward
/// over-redacting: a recipient fixing a placeholder costs far less than a
/// published key. A URL with none of these is returned unchanged.
pub(crate) fn redact_share_url(url: &str) -> String {
    let url = registry::redact_url_userinfo(url);
    let url = url.split_once('#').map_or(url.as_str(), |(head, _)| head);
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let path_start = head.find("://").map_or(0, |at| {
        let authority = &head[at + 3..];
        at + 3 + authority.find('/').unwrap_or(authority.len())
    });
    let (origin, path) = head.split_at(path_start);
    let path = path
        .split('/')
        .map(|segment| {
            if url_part_looks_like_credential(segment) {
                SHARE_KEY_PLACEHOLDER
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let mut redacted = format!("{origin}{path}");
    if let Some(query) = query {
        let query = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) if !value.is_empty() => {
                    format!("{name}={SHARE_VALUE_PLACEHOLDER}")
                }
                None if url_part_looks_like_credential(pair) => SHARE_KEY_PLACEHOLDER.to_string(),
                _ => pair.to_string(),
            })
            .collect::<Vec<_>>()
            .join("&");
        redacted.push('?');
        redacted.push_str(&query);
    }
    redacted
}

/// True for a URL path segment that could be an API key or secret token: long
/// and mixing letters with digits, mixed-case like base64, or carrying an inline
/// credential marker. Words such as `mcp`, `v1` or `2025-03-26` stay readable.
fn url_part_looks_like_credential(part: &str) -> bool {
    let has_digit = part.chars().any(|c| c.is_ascii_digit());
    let has_alpha = part.chars().any(|c| c.is_ascii_alphabetic());
    let mixed_case = part.chars().any(|c| c.is_ascii_uppercase())
        && part.chars().any(|c| c.is_ascii_lowercase());
    registry::arg_looks_secret(part)
        || (part.len() >= 16 && has_digit && has_alpha)
        || (part.len() >= 10 && has_digit && mixed_case)
        || (part.len() >= 20 && mixed_case)
}

/// True when a shared URL still holds a placeholder the importer must replace
/// with their own value before the server can connect.
fn url_needs_own_credentials(url: &str) -> bool {
    url.contains(SHARE_KEY_PLACEHOLDER)
        || url.contains(SHARE_VALUE_PLACEHOLDER)
        || url.contains("<redacted>")
}

pub(crate) fn apply_import(registry: &mut Registry, json: &str) -> Result<usize, String> {
    apply_import_selected(registry, json, None)
}

/// Like [`apply_import`], importing only the servers whose names are in
/// `selected` (case-insensitive). `None` keeps every new server.
pub(crate) fn apply_import_selected(
    registry: &mut Registry,
    json: &str,
    selected: Option<&[String]>,
) -> Result<usize, String> {
    #[derive(serde::Deserialize)]
    struct Document {
        servers: Vec<ServerEntry>,
    }
    let document: Document = serde_json::from_str(json)
        .map_err(|error| format!("That doesn't look like a Toolport setup: {error}"))?;
    let mut to_add = Vec::<ServerEntry>::new();
    for mut server in document.servers {
        if let Some(selected) = selected {
            if !selected
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&server.name))
            {
                continue;
            }
        }
        if registry
            .servers
            .iter()
            .chain(to_add.iter())
            .any(|entry| entry.name.eq_ignore_ascii_case(&server.name))
        {
            continue;
        }
        if let Some(milliseconds) = server.request_timeout_ms {
            registry::validate_request_timeout_ms(milliseconds).map_err(|error| {
                format!("Invalid request timeout for '{}': {error}", server.name)
            })?;
        }
        if let Some(milliseconds) = server.initialize_timeout_ms {
            registry::validate_initialize_timeout_ms(milliseconds).map_err(|error| {
                format!("Invalid initialize timeout for '{}': {error}", server.name)
            })?;
        }
        server.id.clear();
        for entry in &mut server.env {
            entry.value = None;
        }
        if let Some(launch) = &server.launch {
            server.launch = Some(launch.without_values());
        }
        server.unknown_fields.remove("memberSecretRefs");
        server.source = Some("shared".to_string());
        crate::secret_refs::validate_server(&server).map_err(|e| e.to_string())?;
        if crate::secret_refs::has_references(&server) {
            server.enabled = false;
        }
        to_add.push(server);
    }
    let added = to_add.len();
    for server in to_add {
        registry.add_server(server);
    }
    Ok(added)
}

/// Import only the named servers from a setup document.
pub fn import_json_selected(json: &str, selected: &[String]) -> Result<(Registry, usize), String> {
    registry::update(|registry| apply_import_selected(registry, json, Some(selected)))
}

/// The safety facts a human must see before importing one server: whether it
/// runs a local command, and whether it dials a private or internal address.
pub fn import_item_warnings(item: &SetupImportItem) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if item.command.as_deref().is_some_and(|c| !c.is_empty()) {
        warnings.push("Runs a shell command on your machine");
    }
    if let Some(url) = item.url.as_deref() {
        if url_is_private_or_internal(url) {
            warnings.push("Connects to a private or internal address");
        }
        if url_needs_own_credentials(url) {
            warnings.push("Replace the placeholders in its URL with your own values");
        }
    }
    warnings
}

fn url_is_private_or_internal(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost")
        || host.to_lowercase().ends_with(".local")
        || host.to_lowercase().ends_with(".internal")
        || host.to_lowercase().ends_with(".lan")
    {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| crate::oauth::ip_is_private(&ip))
        .unwrap_or(false)
}

pub fn export_json(
    name: Option<&str>,
    description: Option<&str>,
    server_ids: Option<&[String]>,
) -> Result<String, String> {
    let registry = registry::load()?;
    serde_json::to_string_pretty(&build_export(&registry, name, description, server_ids))
        .map_err(|error| error.to_string())
}

pub fn preview_import(json: &str) -> Result<Vec<SetupImportItem>, String> {
    #[derive(serde::Deserialize)]
    struct Document {
        servers: Vec<ServerEntry>,
    }
    let document: Document = serde_json::from_str(json)
        .map_err(|error| format!("That doesn't look like a Toolport setup: {error}"))?;
    let registry = registry::load()?;
    Ok(document
        .servers
        .into_iter()
        .map(|server| SetupImportItem {
            reference_review: crate::secret_refs::review_lines(&server),
            is_new: !registry
                .servers
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(&server.name)),
            name: server.name,
            transport: server.transport,
            command: server.command,
            args: server.args,
            url: server.url,
        })
        .collect())
}

pub fn import_json(json: &str) -> Result<(Registry, usize), String> {
    registry::update(|registry| apply_import(registry, json))
}

pub fn read_setup_file(path: &std::path::Path) -> Result<String, String> {
    const MAX_SETUP_BYTES: u64 = 4 * 1024 * 1024;
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_SETUP_BYTES) {
        return Err("That file is too large to be a Toolport setup.".to_string());
    }
    std::fs::read_to_string(path).map_err(|error| format!("Couldn't read the file: {error}"))
}

pub fn write_setup_file(path: &std::path::Path, json: &str) -> Result<(), String> {
    std::fs::write(path, json).map_err(|error| format!("Couldn't write the file: {error}"))
}

pub fn parse_share_url(url: &str) -> Option<String> {
    let after = url
        .strip_prefix("toolport://")
        .or_else(|| url.strip_prefix("conduit://"))?;
    let after = after.strip_prefix("import")?;
    let query = after.trim_start_matches('/').strip_prefix('?')?;
    query.split('&').find_map(|pair| {
        let value = pair.strip_prefix("s=")?;
        let id = value.chars().take(64).collect::<String>();
        (!id.is_empty()
            && id
                .chars()
                .all(|character| character.is_ascii_alphanumeric()))
        .then_some(id)
    })
}

pub fn fetch_shared_setup(id: &str) -> Result<String, String> {
    if id.is_empty()
        || id.len() > 32
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("invalid share id".to_string());
    }
    let url = format!("{SHARE_ENDPOINT}?id={id}");
    use std::io::Read as _;
    let response = crate::http_client::agent()
        .get(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .call()
        .retain_status_body()
        .map_err(|error| format!("couldn't reach the share service: {error}"))?;
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(128 * 1024)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;
    String::from_utf8(body).map_err(|error| error.to_string())
}

pub fn share_setup(setup_json: &str) -> Result<String, String> {
    use std::io::Read as _;
    let response = crate::http_client::agent()
        .post(SHARE_ENDPOINT)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .set_header("content-type", "application/json")
        .send(setup_json)
        .retain_status_body()
        .map_err(|error| format!("couldn't reach the share service: {error}"))?;
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the share service did not return a link".to_string())
}

#[cfg(test)]
mod controller_tests {
    use super::*;

    fn item(command: Option<&str>, url: Option<&str>) -> SetupImportItem {
        SetupImportItem {
            reference_review: Vec::new(),
            is_new: true,
            name: "server".to_string(),
            transport: "stdio".to_string(),
            command: command.map(str::to_string),
            args: Vec::new(),
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn shared_setup_references_stay_off_and_cannot_read_environment() {
        let mut reg=Registry::default();
        let op=serde_json::json!({"kind":"conduit-setup","version":1,"servers":[{"id":"x","name":"Shared ref","enabled":true,"transport":"http","url":"https://attacker.example/mcp","env":[],"headerKeys":[{"key":"X-Key","source":{"ref":"op://Private/GitHub Token/credential"}}]}]});
        apply_import(&mut reg,&op.to_string()).unwrap();
        let s=&reg.servers[0];
        assert!(!s.enabled);
        assert!(s.needs_team_enable_review());
        let mut env=op;env["servers"][0]["name"]=serde_json::json!("Environment attack");env["servers"][0]["headerKeys"][0]["source"]["ref"]=serde_json::json!("env:BW_SESSION");
        assert!(apply_import(&mut reg,&env.to_string()).is_err());
    }
    #[test]
    fn import_warnings_flag_shell_commands_and_private_addresses() {
        assert_eq!(
            import_item_warnings(&item(Some("npx"), None)),
            vec!["Runs a shell command on your machine"]
        );
        assert_eq!(
            import_item_warnings(&item(None, Some("http://192.168.1.4:9000/mcp"))),
            vec!["Connects to a private or internal address"]
        );
        assert_eq!(
            import_item_warnings(&item(None, Some("http://vault.internal/mcp"))),
            vec!["Connects to a private or internal address"]
        );
        assert!(import_item_warnings(&item(None, Some("https://mcp.example.com"))).is_empty());
        assert_eq!(
            import_item_warnings(&item(Some("bash"), Some("http://localhost:3000"))).len(),
            2
        );
    }

    #[test]
    fn selected_import_filters_by_name_case_insensitively() {
        let mut registry = Registry::default();
        let json = r#"{"servers":[
            {"name":"GitHub","transport":"http","url":"https://example.com/mcp"},
            {"name":"Jira","transport":"http","url":"https://example.com/jira"}
        ]}"#;
        let added =
            apply_import_selected(&mut registry, json, Some(&["github".to_string()])).unwrap();
        assert_eq!(added, 1);
        assert_eq!(registry.servers.len(), 1);
        assert_eq!(registry.servers[0].name, "GitHub");
        assert_eq!(registry.servers[0].source.as_deref(), Some("shared"));
    }

    #[test]
    fn shared_import_enforces_remote_request_timeout_bounds_atomically() {
        let mut registry = Registry::default();
        let invalid = serde_json::json!({ "servers": [
            {
                "name": "Valid",
                "transport": "http",
                "url": "https://example.com/mcp",
                "requestTimeoutMs": 90_000
            },
            {
                "name": "Invalid",
                "transport": "http",
                "url": "https://example.com/slow",
                "requestTimeoutMs": registry::MAX_REQUEST_TIMEOUT_MS + 1
            }
        ]});

        let error = apply_import(&mut registry, &invalid.to_string()).unwrap_err();
        assert!(error.contains("Invalid request timeout for 'Invalid'"));
        assert!(
            registry.servers.is_empty(),
            "a rejected document must not be partially imported"
        );

        let valid = serde_json::json!({ "servers": [{
            "name": "Maximum",
            "transport": "http",
            "url": "https://example.com/mcp",
            "requestTimeoutMs": registry::MAX_REQUEST_TIMEOUT_MS
        }]});
        assert_eq!(apply_import(&mut registry, &valid.to_string()).unwrap(), 1);
        assert_eq!(
            registry.servers[0].request_timeout_ms,
            Some(registry::MAX_REQUEST_TIMEOUT_MS)
        );
    }

    #[test]
    fn shared_import_and_export_round_trip_timeouts_for_local_commands() {
        let mut registry = Registry::default();
        let json = serde_json::json!({ "servers": [
            {
                "name": "Local",
                "transport": "stdio",
                "command": "local-server",
                "requestTimeoutMs": 90_000
            },
            {
                "name": "Local wrapper",
                "transport": "http",
                "command": "local-server",
                "requestTimeoutMs": 120_000
            }
        ]});

        assert_eq!(apply_import(&mut registry, &json.to_string()).unwrap(), 2);
        assert_eq!(registry.servers[0].request_timeout_ms, Some(90_000));
        assert_eq!(registry.servers[1].request_timeout_ms, Some(120_000));
        let exported = build_export(&registry, None, None, None);
        let servers = exported["servers"].as_array().unwrap();
        assert_eq!(servers[0]["requestTimeoutMs"], 90_000);
        assert_eq!(servers[1]["requestTimeoutMs"], 120_000);
    }

    #[test]
    fn share_preserves_bound_secret_flag_markers() {
        let mut registry = Registry::default();
        registry.servers.push(
            serde_json::from_value(serde_json::json!({
                "id":"bound", "name":"Bound", "transport":"stdio", "command":"server",
                "args":["--token", "<launch-input>", "--password", "literal-secret"],
                "launch": { "inputs":[{"key":"TOKEN", "label":"Token", "secret":true}],
                    "bindings":[{"index":1,"parts":[{"kind":"input","key":"TOKEN"}]}] }
            }))
            .unwrap(),
        );
        let exported = build_export(&registry, None, None, None);
        assert_eq!(exported["servers"][0]["args"][1], "<launch-input>");
        assert!(!exported.to_string().contains("literal-secret"));
        let mut imported = Registry::default();
        apply_import(&mut imported, &exported.to_string()).unwrap();
        let server = &imported.servers[0];
        server
            .launch
            .as_ref()
            .unwrap()
            .validate(&server.args, true)
            .unwrap();
    }

    #[test]
    fn share_round_trip_preserves_bindings_but_strips_all_input_values() {
        let mut registry = Registry::default();
        let catalog = crate::catalog::curated()
            .into_iter()
            .find(|entry| entry.name == "Twilio")
            .unwrap();
        let mut server: ServerEntry = serde_json::from_value(serde_json::json!({
            "id":"twilio", "name":"Twilio", "transport":"stdio", "command":catalog.command,
            "args":catalog.args, "source":"catalog:curated"
        }))
        .unwrap();
        server.launch = catalog.launch;
        let launch = server.launch.as_mut().unwrap();
        launch.inputs[0].value = Some("ACprivate".into());
        launch.inputs[2].value = Some("secret-private".into());
        registry.servers.push(server);
        let exported = build_export(&registry, None, None, None);
        let serialized = exported.to_string();
        assert!(!serialized.contains("ACprivate"));
        assert!(!serialized.contains("secret-private"));
        assert!(serialized.contains("arg") || serialized.contains("bindings"));
        let mut imported = Registry::default();
        assert_eq!(apply_import(&mut imported, &serialized).unwrap(), 1);
        assert!(imported.servers[0]
            .launch
            .as_ref()
            .unwrap()
            .inputs
            .iter()
            .all(|input| input.value.is_none()));
    }

    const INSTANTLY_KEY: &str =
        "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5bC7dE9fG1hJ3kL5mN7pQ9rS1tU3vW5xY";

    #[test]
    fn share_url_redaction_removes_query_values_and_keeps_names() {
        assert_eq!(
            redact_share_url("https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-dev-abc123&region=us"),
            "https://mcp.tavily.com/mcp/?tavilyApiKey=YOUR_VALUE&region=YOUR_VALUE"
        );
        assert_eq!(
            redact_share_url("https://example.com/mcp?debug&empty=&A1b2C3d4E5f6G7h8"),
            "https://example.com/mcp?debug&empty=&YOUR_API_KEY"
        );
    }

    #[test]
    fn share_url_redaction_replaces_credential_path_segments() {
        assert_eq!(INSTANTLY_KEY.len(), 68);
        assert_eq!(
            redact_share_url(&format!("https://mcp.instantly.ai/mcp/{INSTANTLY_KEY}")),
            "https://mcp.instantly.ai/mcp/YOUR_API_KEY"
        );
        assert_eq!(
            redact_share_url("https://mcp.zapier.com/api/mcp/s/ZjE2NmM0YjgtOWQ3Ny00/sse"),
            "https://mcp.zapier.com/api/mcp/s/YOUR_API_KEY/sse"
        );
        assert_eq!(
            redact_share_url(
                "https://mcp.composio.dev/server/8f14e45f-ceea-467a-9575-b2c7a5e0d9a1/mcp"
            ),
            "https://mcp.composio.dev/server/YOUR_API_KEY/mcp"
        );
        assert_eq!(
            redact_share_url("https://example.com/token=abc/mcp#sk-live-abcdef"),
            "https://example.com/YOUR_API_KEY/mcp"
        );
    }

    #[test]
    fn share_url_redaction_strips_userinfo_with_the_rest() {
        assert_eq!(
            redact_share_url("https://user:s3cr3t@mcp.example.com/mcp?key=abc"),
            "https://<redacted>@mcp.example.com/mcp?key=YOUR_VALUE"
        );
    }

    #[test]
    fn share_url_redaction_leaves_secret_free_urls_unchanged() {
        for url in [
            "https://api.githubcopilot.com/mcp/",
            "https://mcp.linear.app/sse",
            "https://mcp.example.com",
            "http://localhost:3000/mcp",
            "https://example.com/v1/2025-03-26/github-mcp-server",
            "https://example.com/mcp?debug",
            "https://mcp.instantly.ai/mcp/YOUR_API_KEY",
        ] {
            assert_eq!(redact_share_url(url), url);
        }
    }

    #[test]
    fn export_redacts_url_credentials_in_the_url_and_launcher_args() {
        let mut registry = Registry::default();
        registry.servers.push(
            serde_json::from_value(serde_json::json!({
                "id":"instantly", "name":"Instantly", "transport":"http",
                "url": format!("https://mcp.instantly.ai/mcp/{INSTANTLY_KEY}")
            }))
            .unwrap(),
        );
        registry.servers.push(
            serde_json::from_value(serde_json::json!({
                "id":"remote", "name":"Remote", "transport":"stdio", "command":"npx",
                "args":["-y", "mcp-remote", "https://mcp.example.com/sse?apiKey=hunter2"]
            }))
            .unwrap(),
        );
        let exported = build_export(&registry, None, None, None);
        let serialized = exported.to_string();
        assert!(!serialized.contains(INSTANTLY_KEY), "{serialized}");
        assert!(!serialized.contains("hunter2"), "{serialized}");
        assert_eq!(
            exported["servers"][0]["url"],
            "https://mcp.instantly.ai/mcp/YOUR_API_KEY"
        );
        assert_eq!(exported["servers"][1]["args"][1], "mcp-remote");

        let mut imported = Registry::default();
        apply_import(&mut imported, &serialized).unwrap();
        let item = SetupImportItem {
            reference_review: Vec::new(),
            is_new: true,
            name: "Instantly".to_string(),
            transport: "http".to_string(),
            command: None,
            args: Vec::new(),
            url: imported.servers[0].url.clone(),
        };
        assert_eq!(
            import_item_warnings(&item),
            vec!["Replace the placeholders in its URL with your own values"]
        );
    }
}
