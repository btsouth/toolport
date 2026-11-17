//! Repro: clients::launcher_base omits .bat, so npx.bat loses package identity.

#[test]
fn parse_snippet_names_package_for_npx_bat() {
    use conduit_lib::clients::parse_snippet;

    let json =
        r#"{"command":"C:\\Program Files\\nodejs\\npx.bat","args":["-y","@acme/mcp-weather"]}"#;
    let servers = parse_snippet(json).expect("parse");
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers[0].name, "weather",
        "npx.bat must resolve the package name like npx.cmd; got {}",
        servers[0].name
    );
}
