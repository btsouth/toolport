//! Fields a client format requires on Toolport's entry, and their repair.
use super::paths_tests::{read, write, Fixture};
use super::*;
use serde_json::json;

#[test]
fn vscode_cursor_and_copilot_entries_carry_type_and_old_entries_gain_only_that_field() {
    let fixture = Fixture::new("typed-entries");
    for (client, key, kind) in [
        ("vscode", "servers", "stdio"),
        ("cursor", "mcpServers", "stdio"),
        ("github-copilot-cli", "mcpServers", "local"),
    ] {
        let path = client_config_path(client).unwrap();
        // Toolport's old entry, with a user's extra env var and a comment.
        let text = format!(
            "{{\n  // keep this comment\n  \"{key}\": {{\n    \"toolport\": {{\n      \"command\": {gw},\n      \"args\": [],\n      \"env\": {{\"TOOLPORT_CLIENT_ID\": \"{client}\", \"TOOLPORT_ATTRIBUTION_ID\": \"{client}\", \"MINE\": \"1\"}}\n    }},\n    \"native\": {{\"command\": \"native\"}}\n  }}\n}}\n",
            // JSON-escaped, so a Windows path keeps its backslashes.
            gw = serde_json::to_string(fixture.gateway()).unwrap(),
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        if client != "vscode" {
            // Cursor and Copilot need strict JSON; drop the comment for them.
            std::fs::write(&path, text.replace("  // keep this comment\n", "")).unwrap();
        }
        let before = read(&path);
        let outcome = repoint_stale_gateways_in(
            fixture.gateway(),
            vec![read_client(&find_def(client).unwrap())],
            &HashMap::new(),
        );
        assert!(outcome.failed.is_empty(), "{client}: {:?}", outcome.failed);
        let after = read(&path);
        let mut expected = before.clone();
        expected[key]["toolport"]["type"] = json!(kind);
        assert_eq!(after, expected, "{client}: only `type` is added");
        if client == "vscode" {
            assert!(std::fs::read_to_string(&path)
                .unwrap()
                .contains("// keep this comment"));
        }
        // Once present it is left alone.
        let settled = std::fs::read_to_string(&path).unwrap();
        repoint_stale_gateways_in(
            fixture.gateway(),
            vec![read_client(&find_def(client).unwrap())],
            &HashMap::new(),
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), settled);

        // A customized entry under our name is never touched.
        let custom = json!({key: {"toolport": {"command": "npx", "args": ["bridge"]}}});
        write(&path, &custom);
        repoint_stale_gateways_in(
            fixture.gateway(),
            vec![read_client(&find_def(client).unwrap())],
            &HashMap::new(),
        );
        assert_eq!(read(&path), custom, "{client}");

        // New connections are written with `type`.
        std::fs::remove_file(&path).unwrap();
        install_gateway(client, None).unwrap();
        let written = read(&path);
        assert_eq!(written[key]["toolport"]["type"], kind, "{client}");
    }
    // A remote entry keeps its own transport type, and other formats get none.
    let remote: ServerEntry = serde_json::from_value(
        json!({"id": "r", "name": "r", "transport": "sse", "url": "https://x"}),
    )
    .unwrap();
    assert_eq!(entry_to_typed_json(&remote)["type"], "sse");
    let entry = tests::sample_gateway(None, "claude-desktop");
    assert!(entry_to_json(&entry).get("type").is_none());
    assert!(missing_required_fields(Format::JsonMcpServers, &json!({"command": "x"})).is_empty());
    assert!(missing_required_fields(
        Format::JsonServers,
        &json!({"command": "x", "type": "custom"})
    )
    .is_empty());
}
