//! F20: Disconnect of an entry v1.24.x wrote for its Shared HTTP connection. Each shape
//! below is what the v1.24.2 writer put in that client's config: the `npx mcp-remote`
//! bridge for clients without a native remote shape, a native remote entry for the rest.
use super::*;

const URL: &str = "http://127.0.0.1:47821/mcp";
const BEARER: &str = "Bearer tp-synthetic-legacy";

/// The JSON servers map key, or `None` for Codex's TOML.
fn servers_key(id: &str) -> Option<&'static str> {
    match id {
        "codex" => None,
        "vscode" => Some("servers"),
        "opencode" => Some("mcp"),
        _ => Some("mcpServers"),
    }
}

/// The bridge v1 wrote where the client had no native remote shape.
fn bridge(url: &str) -> serde_json::Value {
    serde_json::json!({
        "command": "npx",
        "args": ["-y", "mcp-remote", url, "--header", format!("Authorization: {BEARER}")],
    })
}

/// v1's entry for `id`, with `url` (a hand edit changes it) and the record v1 kept.
fn legacy_entry(id: &str, url: &str) -> (serde_json::Value, crate::registry::ManagedEntry) {
    let headers = serde_json::json!({"Authorization": BEARER, "TOOLPORT_CLIENT_ID": id});
    let native = |entry: serde_json::Value| {
        let record = serde_json::json!({"command": "", "args": [],
            "env": {"TOOLPORT_CLIENT_ID": id}, "transport": "sharedHttp", "url": URL});
        (entry, serde_json::from_value(record).unwrap())
    };
    match id {
        "devin-cli" | "vscode" => native(serde_json::json!({
            "url": url, "type": "http", "headers": headers})),
        "github-copilot-cli" => native(serde_json::json!({
            "type": "http", "url": url, "headers": headers, "tools": ["*"]})),
        "opencode" => native(serde_json::json!({
            "enabled": true, "type": "remote", "url": url, "headers": headers})),
        _ => {
            let record = serde_json::json!({"command": "npx",
                "args": ["-y", "mcp-remote", URL], "transport": "sharedHttp", "url": URL});
            (bridge(url), serde_json::from_value(record).unwrap())
        }
    }
}

fn original(id: &str) -> &'static str {
    if servers_key(id).is_none() {
        "model = \"o3\"\n\n[mcp_servers.notes]\ncommand = \"notes-mcp\"\n"
    } else if id == "opencode" {
        "{\"theme\": \"dark\", \"mcp\": {\"notes\": {\"type\": \"local\", \"command\": [\"notes-mcp\"]}}}"
    } else {
        "{\"theme\": \"dark\", \"SERVERS\": {\"notes\": {\"command\": \"notes-mcp\"}}}"
    }
}

fn original_text(id: &str) -> String {
    original(id).replace("SERVERS", servers_key(id).unwrap_or_default())
}

/// The original file with v1's entry added beside the user's own server.
fn with_legacy(id: &str, entry: &serde_json::Value) -> String {
    match servers_key(id) {
        None => {
            let args = entry["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| format!("{:?}", arg.as_str().unwrap()))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}\n[mcp_servers.toolport]\ncommand = \"npx\"\nargs = [{args}]\n",
                original_text(id)
            )
        }
        Some(key) => {
            let mut root: serde_json::Value = serde_json::from_str(&original_text(id)).unwrap();
            root[key]["toolport"] = entry.clone();
            serde_json::to_string_pretty(&root).unwrap()
        }
    }
}

fn same_config(id: &str, left: &str, right: &str) -> bool {
    match servers_key(id) {
        None => left.parse::<toml::Value>().unwrap() == right.parse::<toml::Value>().unwrap(),
        Some(_) => {
            serde_json::from_str::<serde_json::Value>(left).unwrap()
                == serde_json::from_str::<serde_json::Value>(right).unwrap()
        }
    }
}

const CLIENTS: [&str; 9] = [
    "claude-code",
    "codex",
    "cursor",
    "windsurf",
    "antigravity",
    "devin-cli",
    "github-copilot-cli",
    "vscode",
    "opencode",
];

/// What this build did with the client before v1 wrote its Shared HTTP entry.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Before {
    /// Nothing: a v1 user upgrading.
    Nothing,
    /// Connected, so Disconnect has its own recovery record whose last write is the
    /// stdio entry (a user who tried 2.x, went back to v1 and chose Shared HTTP there).
    Connected,
    /// Connected and disconnected again.
    Disconnected,
}

const BEFORE: [Before; 3] = [Before::Nothing, Before::Connected, Before::Disconnected];

/// Runs one client. `edited` changes v1's URL by hand.
fn disconnect_legacy(
    id: &str,
    before: Before,
    edited: bool,
) -> (String, Result<WriteOutcome, String>) {
    let data = crate::registry::DataDirTestEnv::new(&format!("f20-{id}"));
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    let home = data.dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = TestHome::set(&home);
    crate::registry::save(&crate::registry::Registry::default()).unwrap();
    let path = resolved_definition_path(&find_def(id).unwrap()).unwrap();
    assert!(
        path.starts_with(&data.dir),
        "{id} resolves outside the test data"
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original_text(id)).unwrap();
    if before != Before::Nothing {
        install_gateway(id, None).unwrap();
    }
    if before == Before::Disconnected {
        let outcome = uninstall_gateway(id).unwrap();
        finish_uninstall(id, &outcome).unwrap();
        assert!(same_config(
            id,
            &read_config_file(&path).unwrap(),
            &original_text(id)
        ));
    }
    let (entry, record) = legacy_entry(
        id,
        if edited {
            "http://127.0.0.1:9999/mcp"
        } else {
            URL
        },
    );
    std::fs::write(&path, with_legacy(id, &entry)).unwrap();
    crate::registry::update(|registry| {
        registry.set_client_managed_entry(id, record);
        Ok(())
    })
    .unwrap();
    let outcome = uninstall_gateway(id);
    (read_config_file(&path).unwrap(), outcome)
}

fn kept_as_edited(outcome: &WriteOutcome) -> bool {
    outcome
        .warnings
        .iter()
        .any(|warning| warning.contains("kept the toolport entry you edited"))
}

#[test]
fn disconnect_removes_each_v1_shared_http_shape_and_restores_the_original() {
    for id in CLIENTS {
        for before in BEFORE {
            let (after, outcome) = disconnect_legacy(id, before, false);
            let outcome = outcome.unwrap_or_else(|error| panic!("{id}: {error}"));
            assert!(
                same_config(id, &after, &original_text(id)),
                "{id} ({before:?}) left:\n{after}"
            );
            assert!(!kept_as_edited(&outcome), "{id}: {:?}", outcome.warnings);
        }
    }
}

#[test]
fn a_v1_shared_http_entry_edited_by_hand_is_kept_and_reported() {
    for id in CLIENTS {
        for before in BEFORE {
            let (entry, _) = legacy_entry(id, "http://127.0.0.1:9999/mcp");
            let expected = with_legacy(id, &entry);
            let (after, outcome) = disconnect_legacy(id, before, true);
            assert!(
                same_config(id, &after, &expected),
                "{id} ({before:?}) changed:\n{after}"
            );
            match outcome {
                Ok(outcome) => {
                    assert!(kept_as_edited(&outcome), "{id}: {:?}", outcome.warnings)
                }
                // Without a record of its own, Disconnect refuses and changes nothing.
                Err(error) => assert!(
                    before != Before::Connected && error.contains("customized legacy gateway"),
                    "{id}: {error}"
                ),
            }
        }
    }
}
