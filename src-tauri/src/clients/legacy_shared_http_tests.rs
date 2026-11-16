//! F20: Disconnect of an entry v1.24.x wrote for its Shared HTTP connection. Each shape
//! below is what the v1.24.2 writer put in that client's config: the `npx mcp-remote`
//! bridge for clients without a native remote shape, a native remote entry for the rest.
//! Failure messages name the client and case but never print config text.
use super::*;

const URL: &str = "http://127.0.0.1:47821/mcp";
const TOKEN: &str = "tp-synthetic-legacy";

/// The JSON servers map key, or `None` for Codex's TOML.
fn servers_key(id: &str) -> Option<&'static str> {
    match id {
        "codex" => None,
        "vscode" => Some("servers"),
        "opencode" => Some("mcp"),
        _ => Some("mcpServers"),
    }
}

fn is_bridge(id: &str) -> bool {
    !matches!(
        id,
        "devin-cli" | "vscode" | "github-copilot-cli" | "opencode"
    )
}

/// v1's entry for `id` and the record v1 kept.
fn legacy_entry(id: &str) -> (serde_json::Value, crate::registry::ManagedEntry) {
    let bearer = format!("Bearer {TOKEN}");
    if is_bridge(id) {
        let record = serde_json::json!({"command": "npx",
            "args": ["-y", "mcp-remote", URL], "transport": "sharedHttp", "url": URL});
        let entry = serde_json::json!({"command": "npx",
            "args": ["-y", "mcp-remote", URL, "--header", format!("Authorization: {bearer}")]});
        return (entry, serde_json::from_value(record).unwrap());
    }
    let headers = serde_json::json!({"Authorization": bearer, "TOOLPORT_CLIENT_ID": id});
    let entry = match id {
        "github-copilot-cli" => serde_json::json!({
            "type": "http", "url": URL, "headers": headers, "tools": ["*"]}),
        "opencode" => serde_json::json!({
            "enabled": true, "type": "remote", "url": URL, "headers": headers}),
        _ => serde_json::json!({"url": URL, "type": "http", "headers": headers}),
    };
    let record = serde_json::json!({"command": "", "args": [],
        "env": {"TOOLPORT_CLIENT_ID": id}, "transport": "sharedHttp", "url": URL});
    (entry, serde_json::from_value(record).unwrap())
}

fn original_text(id: &str) -> String {
    match servers_key(id) {
        None => "model = \"o3\"\n\n[mcp_servers.notes]\ncommand = \"notes-mcp\"\n".into(),
        Some("mcp") => serde_json::json!({"theme": "dark",
            "mcp": {"notes": {"type": "local", "command": ["notes-mcp"]}}})
        .to_string(),
        Some(key) => serde_json::json!({"theme": "dark", key: {"notes": {"command": "notes-mcp"}}})
            .to_string(),
    }
}

/// An OpenCode file already on OpenCode 2.x's `mcp.servers` map.
fn opencode_mixed() -> String {
    serde_json::json!({"theme": "dark",
        "mcp": {"servers": {"notes": {"type": "local", "command": ["notes-mcp"]}}}})
    .to_string()
}

/// `text` with v1's entry added where v1 put it, beside the user's own servers.
fn with_legacy(id: &str, text: &str, entry: &serde_json::Value) -> String {
    match servers_key(id) {
        None => {
            let mut root: toml::Table = text.parse().unwrap();
            root["mcp_servers"]
                .as_table_mut()
                .unwrap()
                .insert("toolport".into(), toml::Value::try_from(entry).unwrap());
            toml::to_string(&root).unwrap()
        }
        Some(key) => {
            let mut root: serde_json::Value = serde_json::from_str(text).unwrap();
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

type Edit = (&'static str, fn(&mut serde_json::Value));

/// Hand edits of v1's entry for `id`: the URL, the token, a field no record keeps
/// (added), and every field v1 wrote for that shape.
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
        ("the token", |entry| match entry.get_mut("args") {
            Some(args) => args[4] = "Authorization: Bearer pasted-by-user".into(),
            None => entry["headers"]["Authorization"] = "Bearer pasted-by-user".into(),
        }),
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

/// One run. The defaults are v1's untouched entry, with its token row, where v1 put it.
#[derive(Clone, Copy)]
struct Case {
    before: Before,
    edit: Option<fn(&mut serde_json::Value)>,
    via: Via,
    /// v1's `client:<id>` token row is in the registry.
    token_row: bool,
    /// The app launched (and ran its launch repair) before Disconnect.
    launch: bool,
    /// v1's Move into gateway took the user's server out of the file into its record.
    moved: bool,
    /// OpenCode only: the entry sits in `mcp.servers`, where v1 never wrote it.
    in_opencode_servers: bool,
}

impl Case {
    fn new(before: Before) -> Self {
        Self {
            before,
            edit: None,
            via: Via::Disconnect,
            token_row: true,
            launch: false,
            moved: false,
            in_opencode_servers: false,
        }
    }
}

struct Run {
    after: String,
    /// The file as v1 left it, edit included.
    written: String,
    original: String,
    outcome: Result<Vec<String>, String>,
}

fn disconnect_legacy(id: &str, case: Case) -> Run {
    disconnect_legacy_from(id, &original_text(id), case)
}

fn disconnect_legacy_from(id: &str, original: &str, case: Case) -> Run {
    let data = crate::registry::DataDirTestEnv::new(&format!("f20-{id}"));
    let _env_lock = env_test_lock();
    let _client_env = isolated_client_env(&data.dir);
    // Disconnect revokes a Shared HTTP token through the secrets store; the file
    // backend keeps that inside the test data directory.
    let _secrets = EnvRestore::set("TOOLPORT_SECRET_KEY", Path::new("f20-test-key"));
    let home = data.dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = TestHome::set(&home);
    crate::registry::save(&crate::registry::Registry::default()).unwrap();
    let def = find_def(id).unwrap();
    let path = resolved_definition_path(&def).unwrap();
    assert!(
        path.starts_with(&data.dir),
        "{id} resolves outside the test data"
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();
    if case.before != Before::Nothing {
        install_gateway(id, None).unwrap();
    }
    if case.before == Before::Disconnected {
        let outcome = uninstall_gateway(id).unwrap();
        finish_uninstall(id, &outcome).unwrap();
        assert!(
            same_config(id, &read_config_file(&path).unwrap(), original),
            "{id}: the 2.x Disconnect did not restore the original"
        );
    }
    let (mut entry, record) = legacy_entry(id);
    if let Some(edit) = case.edit {
        edit(&mut entry);
    }
    let current = read_config_file(&path).unwrap();
    let mut v1_file = if case.in_opencode_servers {
        let mut root: serde_json::Value = serde_json::from_str(&current).unwrap();
        root["mcp"]["servers"]["toolport"] = entry;
        serde_json::to_string_pretty(&root).unwrap()
    } else {
        with_legacy(id, &current, &entry)
    };
    if case.moved {
        // v1's Move into gateway: the user's server leaves the file for v1's record.
        let key = servers_key(id).unwrap();
        let mut root: serde_json::Value = serde_json::from_str(&v1_file).unwrap();
        let notes = root[key].as_object_mut().unwrap().remove("notes").unwrap();
        v1_file = serde_json::to_string_pretty(&root).unwrap();
        let dir = backup_dir(id).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("moved-servers.json"),
            serde_json::json!({
                "configPath": path.to_string_lossy(),
                "entries": [{"name": "notes", "format": "json", "value": notes}],
            })
            .to_string(),
        )
        .unwrap();
    }
    std::fs::write(&path, &v1_file).unwrap();
    crate::registry::update(|registry| {
        registry.set_client_managed_entry(id, record);
        if case.token_row {
            registry.http_clients.push(crate::registry::HttpClient {
                id: format!("client:{id}"),
                label: format!("Client: {id}"),
                token_sha256: crate::registry::sha256_hex(TOKEN),
                profile: String::new(),
                unknown_fields: Default::default(),
            });
        }
        Ok(())
    })
    .unwrap();
    if case.launch {
        // What both shells run at launch for a Shared HTTP record: Codex's settings
        // repair, with the updated record stored.
        let managed = crate::registry::load().unwrap().client_managed_entries;
        let outcome = repoint_stale_gateways_in("", vec![read_client(&def)], &managed);
        assert!(outcome.failed.is_empty(), "{id}: the launch repair failed");
        crate::registry::update(|registry| {
            for (client, record) in &outcome.repointed {
                registry.set_client_managed_entry(client, record.clone());
            }
            Ok(())
        })
        .unwrap();
    }
    let written = read_config_file(&path).unwrap();
    let outcome = match case.via {
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
    Run {
        after: read_config_file(&path).unwrap(),
        written,
        original: original.to_owned(),
        outcome,
    }
}

fn kept_as_edited(warnings: &[String]) -> bool {
    warnings
        .iter()
        .any(|warning| warning.contains("kept the toolport entry you edited"))
}

/// The entry was removed, the original came back and nothing was reported.
fn assert_restored(id: &str, what: &str, run: &Run) {
    let warnings = run
        .outcome
        .as_ref()
        .unwrap_or_else(|error| panic!("{id}, {what}: {error}"));
    assert!(
        same_config(id, &run.after, &run.original),
        "{id}, {what}: the original was not restored"
    );
    assert!(
        !kept_as_edited(warnings),
        "{id}, {what}: reported as edited"
    );
}

/// The file is as v1 left it and the entry was reported as the user's, or (without a
/// record of its own) Disconnect refused and changed nothing.
fn assert_kept(id: &str, what: &str, run: &Run, may_refuse: bool) {
    assert!(
        same_config(id, &run.after, &run.written),
        "{id}, {what}: the file changed"
    );
    match &run.outcome {
        Ok(warnings) => assert!(kept_as_edited(warnings), "{id}, {what}: not reported"),
        Err(error) => assert!(
            may_refuse && error.contains("customized legacy gateway"),
            "{id}, {what}: {error}"
        ),
    }
}

#[test]
fn disconnect_removes_each_v1_shared_http_shape_and_restores_the_original() {
    for id in CLIENTS {
        for before in BEFORE {
            let run = disconnect_legacy(id, Case::new(before));
            assert_restored(id, &format!("{before:?}"), &run);
        }
    }
}

/// Greptile P1 and review F3 on #1370: the match covers the whole entry and the token.
/// Each field v1 wrote, the token and one field it never wrote, changed by hand, keep
/// the entry when Disconnect has a 2.x record to undo against (the adoption paths).
/// Without a record the older ownership check runs as in v2.3.5, so only the URL edit
/// is checked there.
#[test]
fn a_v1_shared_http_entry_edited_by_hand_is_kept_and_reported() {
    for id in CLIENTS {
        for (what, edit) in edits(id) {
            for before in BEFORE {
                if before == Before::Nothing && what != "url" {
                    continue;
                }
                let run = disconnect_legacy(
                    id,
                    Case {
                        edit: Some(edit),
                        ..Case::new(before)
                    },
                );
                assert_kept(
                    id,
                    &format!("{what} ({before:?})"),
                    &run,
                    before == Before::Nothing,
                );
            }
        }
    }
}

/// Review F3: without v1's token row nothing proves the entry is v1's.
#[test]
fn without_v1_token_row_the_entry_is_kept_and_reported() {
    for id in CLIENTS {
        for before in [Before::Connected, Before::Disconnected] {
            let run = disconnect_legacy(
                id,
                Case {
                    token_row: false,
                    ..Case::new(before)
                },
            );
            assert_kept(id, &format!("no token row ({before:?})"), &run, false);
        }
    }
}

/// Review F1: at launch Codex gets its Shared HTTP settings added to v1's entry in
/// place, which rebases v1's entry into a 2.x record's baseline. Disconnect still
/// removes it and restores the original.
#[test]
fn codex_v1_entry_is_removed_after_the_launch_repair() {
    for before in BEFORE {
        let run = disconnect_legacy(
            "codex",
            Case {
                launch: true,
                ..Case::new(before)
            },
        );
        assert!(
            run.written.contains("tool_timeout_sec"),
            "codex ({before:?}): the launch repair did not run"
        );
        assert_restored("codex", &format!("after launch ({before:?})"), &run);
    }
}

/// Review F2: servers v1's Move into gateway took out of the file come back with the
/// original when v1's entry is removed.
#[test]
fn servers_v1_moved_into_the_gateway_come_back() {
    for id in CLIENTS.into_iter().filter(|id| servers_key(id).is_some()) {
        for before in BEFORE {
            let run = disconnect_legacy(
                id,
                Case {
                    moved: true,
                    ..Case::new(before)
                },
            );
            assert_restored(id, &format!("moved servers ({before:?})"), &run);
        }
    }
}

/// CodeRabbit on #1370: v1.24.2 wrote OpenCode's entry at `mcp.toolport` only. In a
/// file that also has OpenCode 2.x's `mcp.servers` map it is still found there, removed,
/// and the 2.x map is left as it was. The same entry in `mcp.servers`, where v1 never
/// wrote it, is kept and reported.
#[test]
fn opencode_v1_entry_is_found_where_v1_wrote_it() {
    for before in BEFORE {
        let run = disconnect_legacy_from("opencode", &opencode_mixed(), Case::new(before));
        assert_restored(
            "opencode",
            &format!("beside mcp.servers ({before:?})"),
            &run,
        );
    }
    let run = disconnect_legacy_from(
        "opencode",
        &opencode_mixed(),
        Case {
            in_opencode_servers: true,
            ..Case::new(Before::Connected)
        },
    );
    assert_kept("opencode", "inside mcp.servers", &run, false);
}

/// Remove from all clients goes through the same restore: v1's entry is removed and the
/// original put back; an edited one is kept and reported.
#[test]
fn remove_from_all_clients_removes_v1_entries_and_keeps_edited_ones() {
    for id in CLIENTS {
        let run = disconnect_legacy(
            id,
            Case {
                via: Via::RemoveFromAll,
                ..Case::new(Before::Connected)
            },
        );
        assert_restored(id, "Remove from all clients", &run);
        let (_, edit) = edits(id)[1];
        let run = disconnect_legacy(
            id,
            Case {
                via: Via::RemoveFromAll,
                edit: Some(edit),
                ..Case::new(Before::Connected)
            },
        );
        assert_kept(id, "Remove from all clients, edited", &run, false);
    }
}
