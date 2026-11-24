//! `toolport fix` against isolated fixtures: preview first, then the checked repairs.
mod cli_support;

use cli_support::{stderr, stdout, Fixture, SECRET};

#[test]
fn dry_run_lists_every_change_and_touches_nothing() {
    let f = Fixture::new("fix-dry");
    f.messy_machine();
    let claude = f.home.join(".claude.json");
    let before = f.read(&claude);
    let plan = f.json(&["fix", "--dry-run", "--json"]);
    let secrets = plan["secrets"].as_array().unwrap();
    let ids: Vec<&str> = secrets
        .iter()
        .map(|s| s["clientId"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"claude-code") && ids.contains(&"cursor"),
        "{plan:#}"
    );
    assert!(secrets
        .iter()
        .all(|s| s["servers"] == serde_json::json!(["github"])));
    #[cfg(unix)]
    assert!(plan["planned"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "broken-links"));
    assert_eq!(f.read(&claude), before);
    assert!(!f.data.join("registry.json").exists());
    #[cfg(unix)]
    assert!(std::fs::symlink_metadata(f.home.join(".claude/skills/broken")).is_ok());

    let text = stdout(&f.run(&["fix", "--dry-run"]));
    assert!(text.contains("dry run") && text.contains("toolport fix --yes"));
    assert!(!text.contains(SECRET));

    let out = f.run(&["fix"]);
    assert_eq!(out.status.code(), Some(1), "no terminal and no --yes");
    assert!(stderr(&out).contains("--yes"));
}

#[cfg(unix)]
#[test]
fn skills_fix_removes_a_broken_link() {
    let f = Fixture::new("fix-skills");
    f.messy_machine();
    let out = f.run(&["fix", "--only", "skills", "--yes"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(std::fs::symlink_metadata(f.home.join(".claude/skills/broken")).is_err());
    let scan = f.json(&["scan", "--json", "--all"]);
    assert!(scan["skills"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["id"] != "broken-links"));
    // Secrets were not part of this run.
    assert_eq!(scan["secrets"].as_array().unwrap().len(), 2);
}

#[test]
fn secrets_fix_vaults_values_and_says_the_agent_copy_remains() {
    let f = Fixture::new("fix-secrets");
    f.messy_machine();
    let claude = f.home.join(".claude.json");
    let before = f.read(&claude);
    let out = f.run(&["fix", "--only", "secrets", "--yes", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}{}", stdout(&out), stderr(&out)));
    assert!(
        report["failed"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
    let applied = report["applied"].to_string();
    assert!(applied.contains("1 server moved"), "{report:#}");
    let notes = report["notes"].to_string();
    assert!(notes.contains("--connect"), "{notes}");
    // The second agent's copy is the same server, now already in Toolport.
    assert!(notes.contains("already in Toolport"), "{notes}");

    let registry = f.read(&f.data.join("registry.json"));
    assert!(registry.contains("\"github\""));
    assert!(!registry.contains(SECRET));
    assert!(f.data.join("secrets.enc").is_file());
    // Without --connect the agent's own file is left exactly as it was.
    assert_eq!(f.read(&claude), before);
    assert!(!out
        .stdout
        .windows(SECRET.len())
        .any(|w| w == SECRET.as_bytes()));

    // Scan still reports the copies on disk, now as held by Toolport, and points at
    // connecting the agents instead of importing again.
    let scan = f.json(&["scan", "--json"]);
    let secrets = scan["secrets"].as_array().unwrap();
    assert_eq!(secrets.len(), 2);
    assert!(secrets.iter().all(|s| s["inToolport"] == true), "{scan:#}");
    let human = stdout(&f.run(&["scan"]));
    assert!(human.contains("--connect"), "{human}");
    assert!(
        !human.contains("moves them into your OS keychain"),
        "{human}"
    );
}
