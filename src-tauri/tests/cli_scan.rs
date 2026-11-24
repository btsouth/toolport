//! `toolport scan`, `hook`, `report` and the command line itself, against isolated fixtures.
mod cli_support;

use cli_support::{stderr, stdout, Fixture, SECRET};

#[test]
fn help_version_and_usage_errors() {
    let f = Fixture::new("basics");
    let out = f.run(&["--version"]);
    assert!(out.status.success());
    assert!(stdout(&out).starts_with("toolport "));

    let out = f.run(&["--help"]);
    assert!(out.status.success());
    let help = stdout(&out);
    for cmd in [
        "scan", "usage", "fix", "init", "up", "export", "import", "hook", "report",
    ] {
        assert!(help.contains(cmd), "{cmd} missing from help");
    }
    assert!(!help.contains('\x1b'), "NO_COLOR output has escapes");

    let out = f.run(&["usage", "--help"]);
    assert!(stdout(&out).contains("--days"));

    let out = f.run(&["scna"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("did you mean 'toolport scan'"));
    let out = f.run(&["usage", "--days", "zero"]);
    assert_eq!(out.status.code(), Some(1));
    let out = f.run(&["--project", "/definitely/not/here"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn scan_finds_secrets_drift_and_skill_problems_without_printing_values() {
    let f = Fixture::new("scan-messy");
    f.messy_machine();
    let report = f.json(&["scan", "--json", "--all"]);
    let secrets = report["secrets"].as_array().unwrap();
    let clients: Vec<&str> = secrets
        .iter()
        .map(|s| s["clientId"].as_str().unwrap())
        .collect();
    assert!(clients.contains(&"claude-code"), "{report:#}");
    assert!(clients.contains(&"cursor"), "{report:#}");
    assert!(secrets
        .iter()
        .all(|s| s["setting"] == "GITHUB_TOKEN" && s["server"] == "github"));

    let drift = report["drift"].as_array().unwrap();
    let kind = |name: &str| {
        drift
            .iter()
            .find(|d| d["server"] == name)
            .map(|d| d["kind"].as_str().unwrap().to_string())
    };
    assert_eq!(kind("fs").as_deref(), Some("conflict"));
    assert_eq!(kind("github").as_deref(), Some("duplicate"));

    #[cfg(unix)]
    assert!(report["skills"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == "broken-links" && i["fixable"] == true));
    assert_eq!(report["skills"]["sharedFolder"], "~/.agents/skills");
    let claude = report["startup"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["agentId"] == "claude-code")
        .expect("claude code startup budget");
    assert!(claude["rulesTokens"].as_u64().unwrap() > 0);

    // Human output, JSON output and the report never carry the value.
    let human = f.run(&["scan", "--all"]);
    let text = stdout(&human);
    assert!(text.contains("Plaintext secrets on disk"));
    assert!(text.contains("Works with skills.sh"));
    assert!(text.contains("toolport fix --dry-run"));
    assert!(!text.contains('\x1b'));
    for out in [text, report.to_string()] {
        assert!(!out.contains(SECRET), "secret value leaked");
        assert!(!out.contains("ghp_"), "secret prefix leaked");
    }
}

#[test]
fn check_exits_two_on_findings_and_zero_when_clean() {
    let messy = Fixture::new("scan-check-messy");
    messy.messy_machine();
    assert_eq!(messy.run(&["scan", "--check"]).status.code(), Some(2));
    assert_eq!(messy.run(&["--check"]).status.code(), Some(2));
    assert_eq!(messy.run(&["scan"]).status.code(), Some(0));

    let clean = Fixture::new("scan-check-clean");
    clean.clean_machine();
    let out = clean.run(&["scan", "--check"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(stdout(&out).contains("All clear"));
}

#[test]
fn hook_check_is_silent_when_healthy_and_one_line_when_not() {
    let clean = Fixture::new("hook-clean");
    clean.clean_machine();
    let out = clean.run(&["hook", "check"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");

    let messy = Fixture::new("hook-messy");
    messy.messy_machine();
    let out = messy.run(&["hook", "check"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with("toolport: 2 plaintext secrets on disk"),
        "{text}"
    );
    assert!(text.contains("(run 'toolport scan')"));
    assert!(!text.contains(SECRET));
}

#[test]
fn hook_install_and_uninstall_touch_only_toolports_entry() {
    let f = Fixture::new("hook-install");
    f.clean_machine();
    let settings = f.write(
        ".claude/settings.json",
        "{\n  // keep me\n  \"hooks\": {\n    \"SessionStart\": [\n      { \"hooks\": [ { \"type\": \"command\", \"command\": \"echo mine\" } ] }\n    ]\n  }\n}\n",
    );
    let codex_hooks = f.home.join(".codex/hooks.json");
    std::fs::create_dir_all(f.home.join(".codex")).unwrap();
    std::fs::write(
        f.home.join(".codex/config.toml"),
        "# my codex\nmodel = \"x\"\n",
    )
    .unwrap();

    let out = f.run(&["hook", "install"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let text = f.read(&settings);
    assert!(text.contains("// keep me") && text.contains("echo mine"));
    assert!(text.contains("hook check"));
    let codex = f.read(&codex_hooks);
    assert!(codex.contains("hook check"), "{codex}");
    let config = f.read(&f.home.join(".codex/config.toml"));
    assert!(
        config.contains("# my codex") && config.contains("hooks = true"),
        "{config}"
    );

    // Installing twice changes nothing.
    let before = f.read(&settings);
    assert!(f
        .run(&["hook", "install", "--agent", "claude-code"])
        .status
        .success());
    assert_eq!(f.read(&settings), before);

    let out = f.run(&["hook", "uninstall"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = f.read(&settings);
    assert!(!text.contains("hook check"));
    assert!(text.contains("// keep me") && text.contains("echo mine"));
    let codex = f.read(&codex_hooks);
    assert!(!codex.contains("hook check"));
    assert!(f.data.join("backups/hooks").read_dir().unwrap().count() >= 2);
}

#[test]
fn report_is_one_static_file_without_secrets() {
    let f = Fixture::new("report");
    f.messy_machine();
    let out_path = f.root.join("card.html");
    let out = f.run(&["report", "--out", out_path.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let html = f.read(&out_path);
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("Plaintext secrets on disk"));
    assert!(html.contains("GITHUB_TOKEN"));
    assert!(!html.contains(SECRET) && !html.contains("ghp_"));
    assert!(!html.contains("<script") && !html.contains("<link") && !html.contains("src="));
}
