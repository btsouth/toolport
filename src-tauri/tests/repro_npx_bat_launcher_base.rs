//! Repro: clients::launcher_base omits .bat, so npx.bat loses package identity.

#[test]
fn npx_bat_must_dedupe_by_package_like_npx_cmd() {
    use conduit_lib::clients::import_dedupe_key;

    let args_a = vec!["-y".into(), "@acme/mcp-weather".into()];
    let args_b = vec!["-y".into(), "@other/mcp-weather".into()];
    let cmd_bat = r"C:\Program Files\nodejs\npx.bat";
    let cmd_cmd = r"C:\Program Files\nodejs\npx.cmd";

    let k_cmd_a = import_dedupe_key("weather", Some(cmd_cmd), &args_a);
    let k_cmd_b = import_dedupe_key("weather", Some(cmd_cmd), &args_b);
    assert_ne!(
        k_cmd_a, k_cmd_b,
        "npx.cmd must distinguish packages (control); got {k_cmd_a} vs {k_cmd_b}"
    );

    let k_bat_a = import_dedupe_key("weather", Some(cmd_bat), &args_a);
    let k_bat_b = import_dedupe_key("weather", Some(cmd_bat), &args_b);
    assert_ne!(
        k_bat_a, k_bat_b,
        "npx.bat must distinguish packages the same way as npx.cmd; got {k_bat_a} vs {k_bat_b}"
    );
}

#[test]
fn parse_snippet_names_package_for_npx_bat() {
    use conduit_lib::clients::parse_snippet;

    let json = r#"{"command":"C:\\Program Files\\nodejs\\npx.bat","args":["-y","@acme/mcp-weather"]}"#;
    let servers = parse_snippet(json).expect("parse");
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers[0].name, "weather",
        "npx.bat must resolve the package name like npx.cmd; got {}",
        servers[0].name
    );
}

#[test]
fn a_plain_bat_program_is_still_its_own_identity() {
    use conduit_lib::clients::import_dedupe_key;

    // Stripping `.bat` must only affect the *runner* lookup. A normal batch program is
    // not a package runner, so its identity stays the command itself and two servers
    // with the same display name must not collapse on differing args.
    let key = import_dedupe_key(
        "weather",
        Some(r"C:\tools\my-server.bat"),
        &["--port".to_string(), "1".to_string()],
    );
    assert!(
        !key.contains("package:"),
        "a plain .bat program is not a package runner, got {key}"
    );
}
