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
            let mut root: toml::Table = original_text(id).parse().unwrap();
            root["mcp_servers"]
                .as_table_mut()
                .unwrap()
                .insert("toolport".into(), toml::Value::try_from(entry).unwrap());
            toml::to_string(&root).unwrap()
        }
        Some(key) => {
            let mut root: serde_json::Value = serde_json::from_str(&original_text(id)).unwrap();
            root[key]["toolport"] = entry.clone();
            serde_json::to_string_pretty(&root).unwrap()
        }
    }
}

fn is_bridge(id: &str) -> bool {
    !matches!(
        id,
        "devin-cli" | "vscode" | "github-copilot-cli" | "opencode"
    )
}

type Edit = (&'static str, fn(&mut serde_json::Value));

/// Hand edits of v1's entry for `id`: the URL, a field no record keeps (added or
/// changed), and every field v1 wrote for that shape.
fn edits(id: &str) -> Vec<Edit> {
    let mut edits: Vec<Edit> = vec![
        ("url", |entry| {
            let url = "http://127.0.0.1:9999/mcp";
            match entry.get_mut("args") {
                Some(args) => args[2] = url.into(),
                None => entry["url"] = url.into(),
            }
        }),
        ("an added field", |entry| entry["timeout"] = 30.into()),
    ];
    if is_bridge(id) {
        edits.push(("an added argument", |entry| {
            entry["args"].as_array_mut().unwrap().push("--debug".into())
        }));
        edits.push(("an added header argument", |entry| {
            let args = entry["args"].as_array_mut().unwrap();
            args.push("--header".into());
            args.push("X-Team: tools".into());
        }));
        edits.push(("added env", |entry| {
            entry["env"] = serde_json::json!({"DEBUG": "1"})
        }));
        edits.push(("the command", |entry| entry["command"] = "bunx".into()));
    } else {
        edits.push(("an added header", |entry| {
            entry["headers"]["X-Team"] = "tools".into()
        }));
        edits.push(("a removed header", |entry| {
            entry["headers"]
                .as_object_mut()
                .unwrap()
                .remove("TOOLPORT_CLIENT_ID");
        }));
        edits.push(("the type", |entry| entry["type"] = "sse".into()));
    }
    match id {
        "github-copilot-cli" => {
            edits.push(("tools", |entry| {
                entry["tools"] = serde_json::json!(["read_file"])
            }));
            edits.push(("tools removed", |entry| {
                entry.as_object_mut().unwrap().remove("tools");
            }));
        }
        "opencode" => edits.push(("enabled", |entry| entry["enabled"] = false.into())),
        _ => {}
    }
    edits
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

/// How the test disconnects: the client's own Disconnect, or Remove from all clients.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    Disconnect,
    RemoveFromAll,
}

/// Runs one client. `edit` changes v1's entry by hand before Disconnect.
fn disconnect_legacy(
    id: &str,
    before: Before,
    edit: Option<fn(&mut serde_json::Value)>,
    via: Via,
) -> (String, String, Result<Vec<String>, String>) {
    let data = crate::registry::DataDirTestEnv::new(&format!("f20-{id}"));
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    // Remove from all clients revokes a Shared HTTP token through the secrets store;
    // the file backend keeps that inside the test data directory.
    let _secrets = EnvRestore::set("TOOLPORT_SECRET_KEY", Path::new("f20-test-key"));
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
    let (mut entry, record) = legacy_entry(id, URL);
    if let Some(edit) = edit {
        edit(&mut entry);
    }
    let written = with_legacy(id, &entry);
    std::fs::write(&path, &written).unwrap();
    crate::registry::update(|registry| {
        registry.set_client_managed_entry(id, record);
        Ok(())
    })
    .unwrap();
    let outcome = match via {
        Via::Disconnect => uninstall_gateway(id).map(|outcome| outcome.warnings),
        Via::RemoveFromAll => {
            let rows = disconnect_all(false).unwrap();
            let row = rows
                .into_iter()
                .find(|row| row.client_id == id)
                .unwrap_or_else(|| panic!("{id}: Remove from all clients skipped it"));
            match row.error {
                Some(error) => Err(error),
                None => Ok(row.warnings),
            }
        }
    };
    (read_config_file(&path).unwrap(), written, outcome)
}

fn kept_as_edited(warnings: &[String]) -> bool {
    warnings
        .iter()
        .any(|warning| warning.contains("kept the toolport entry you edited"))
}

#[test]
fn disconnect_removes_each_v1_shared_http_shape_and_restores_the_original() {
    for id in CLIENTS {
        for before in BEFORE {
            let (after, _, outcome) = disconnect_legacy(id, before, None, Via::Disconnect);
            let warnings = outcome.unwrap_or_else(|error| panic!("{id}: {error}"));
            assert!(
                same_config(id, &after, &original_text(id)),
                "{id} ({before:?}) left:\n{after}"
            );
            assert!(!kept_as_edited(&warnings), "{id}: {warnings:?}");
        }
    }
}

/// Greptile P1 on #1370: the match must cover the whole entry. Each field v1 wrote, and
/// one it never wrote, changed by hand keeps the entry when Disconnect has a 2.x record
/// to undo against (the adoption paths). Without a record the old ownership check runs
/// unchanged, so only the URL edit is checked there.
#[test]
fn a_v1_shared_http_entry_edited_by_hand_is_kept_and_reported() {
    for id in CLIENTS {
        for (what, edit) in edits(id) {
            for before in BEFORE {
                if before == Before::Nothing && what != "url" {
                    continue;
                }
                let (after, written, outcome) =
                    disconnect_legacy(id, before, Some(edit), Via::Disconnect);
                assert!(
                    same_config(id, &after, &written),
                    "{id}, {what} ({before:?}) changed:\n{after}"
                );
                match outcome {
                    Ok(warnings) => {
                        assert!(kept_as_edited(&warnings), "{id}, {what}: {warnings:?}")
                    }
                    // Without a record of its own, Disconnect refuses and changes nothing.
                    Err(error) => assert!(
                        before == Before::Nothing && error.contains("customized legacy gateway"),
                        "{id}, {what} ({before:?}): {error}"
                    ),
                }
            }
        }
    }
}

/// Remove from all clients goes through the same restore: v1's entry is removed and the
/// original put back; an edited one is kept and reported.
#[test]
fn remove_from_all_clients_removes_v1_entries_and_keeps_edited_ones() {
    for id in CLIENTS {
        let (after, _, outcome) =
            disconnect_legacy(id, Before::Connected, None, Via::RemoveFromAll);
        let warnings = outcome.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert!(
            same_config(id, &after, &original_text(id)),
            "{id} left:\n{after}"
        );
        assert!(!kept_as_edited(&warnings), "{id}: {warnings:?}");
        let (_, edit) = edits(id)[1];
        let (after, written, outcome) =
            disconnect_legacy(id, Before::Connected, Some(edit), Via::RemoveFromAll);
        assert!(same_config(id, &after, &written), "{id} changed:\n{after}");
        let warnings = outcome.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert!(kept_as_edited(&warnings), "{id}: {warnings:?}");
    }
}
