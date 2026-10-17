use super::*;
use std::cell::RefCell;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A throwaway home folder, removed on drop.
struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "toolport-skills-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // Canonical so paths compare equal to what the scanner resolves.
        let root = std::fs::canonicalize(&root).unwrap();
        Home { root }
    }

    fn env(&self) -> Env {
        Env::from_home(&self.root)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn skill(&self, rel_dir: &str, name: &str, description: &str) -> PathBuf {
        self.write(
            &format!("{rel_dir}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
    }

    fn link(&self, rel: &str, target: &Path) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(target, &path).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct TestWriter {
    backups: RefCell<Vec<(String, PathBuf)>>,
}

impl Writer for TestWriter {
    fn backup(&self, client: &str, path: &Path) -> Result<(), String> {
        self.backups.borrow_mut().push((client.into(), path.into()));
        Ok(())
    }
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).map_err(|e| e.to_string())
    }
    fn remove_link(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn all(_: &str) -> bool {
    true
}

fn only<'a>(ids: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |id| ids.contains(&id)
}

fn skill<'a>(inv: &'a Inventory, name: &str) -> &'a Skill {
    inv.skills
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no skill {name}"))
}

fn state(inv: &Inventory, name: &str, agent: &str) -> Option<State> {
    skill(inv, name).states.get(agent).map(|s| s.state)
}

#[test]
fn shared_skill_reaches_each_client_through_its_own_folders() {
    let home = Home::new();
    let real = home.skill(".agents/skills/inbox", "inbox", "What needs me");
    home.link(".claude/skills/inbox", real.parent().unwrap());
    home.link(".codex/skills/inbox", real.parent().unwrap());

    let inv = scan(&home.env(), &all, None);
    let inbox = skill(&inv, "inbox");
    assert_eq!(inbox.source, Source::Yours);
    for agent in [
        "claude-code",
        "codex",
        "cursor",
        "github-copilot-cli",
        "opencode",
        "grok",
        "gemini-cli",
    ] {
        assert_eq!(state(&inv, "inbox", agent), Some(State::On), "{agent}");
    }
    assert_eq!(state(&inv, "inbox", "hermes"), None);
    // One record for the real file, with every path that reaches it.
    assert_eq!(inv.skills.iter().filter(|s| s.name == "inbox").count(), 1);
    assert!(inbox.links.contains(&"~/.claude/skills/inbox".to_string()));
    assert!(inbox.links.contains(&"~/.agents/skills/inbox".to_string()));
    // Cursor reaches the same file through three folders and loads it once.
    let via = &inbox.states["cursor"].via;
    assert!(
        via.contains("~/.claude/skills") && via.contains("~/.agents/skills"),
        "{via}"
    );
}

#[test]
fn native_switches_turn_skills_off_per_client() {
    let home = Home::new();
    let real = home.skill(".agents/skills/orca", "orca", "Orca");
    home.link(".claude/skills/orca", real.parent().unwrap());
    home.write(
        ".claude/settings.json",
        r#"{"skillOverrides": {"orca": "off"}}"#,
    );
    home.write(
        ".codex/config.toml",
        &format!(
            "[[skills.config]]\npath = \"{}\"\nenabled = false\n",
            home.path(".agents/skills/orca/SKILL.md").display()
        ),
    );
    home.write(".copilot/settings.json", r#"{"disabledSkills": ["orca"]}"#);
    home.write(
        ".gemini/settings.json",
        r#"{"skills": {"disabled": ["orca"]}}"#,
    );
    home.write(
        ".config/opencode/opencode.json",
        r#"{"permissions": [{"action": "*", "resource": "*", "effect": "allow"}, {"action": "skill", "resource": "orca", "effect": "deny"}]}"#,
    );
    home.write(".hermes/config.yaml", "skills:\n  disabled:\n    - orca\n");
    home.link(".hermes/skills/orca", real.parent().unwrap());

    let inv = scan(&home.env(), &all, None);
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
    ] {
        assert_eq!(state(&inv, "orca", agent), Some(State::Off), "{agent}");
    }
    // Cursor has no switch and still loads it.
    assert_eq!(state(&inv, "orca", "cursor"), Some(State::On));
    let issue = inv.issues.iter().find(|i| i.id == "off-elsewhere").unwrap();
    assert!(issue.agents.contains(&"cursor".to_string()));
    assert!(
        issue.fix_label.is_none(),
        "only Cursor still loads it, and it has no switch"
    );
}

#[test]
fn claude_name_only_and_synced_account_skills() {
    let home = Home::new();
    home.skill(".claude/skills/synced/bucket-a/pdf", "pdf", "PDF files");
    home.skill(
        ".claude/skills/synced/bucket-a/computer-use",
        "computer-use",
        "Desktop",
    );
    home.write(
        ".claude/settings.json",
        r#"{"skillOverrides": {"pdf": "name-only", "computer-use": "off"}}"#,
    );
    let inv = scan(&home.env(), &all, None);
    assert_eq!(skill(&inv, "pdf").source, Source::Account);
    assert_eq!(state(&inv, "pdf", "claude-code"), Some(State::NameOnly));
    assert_eq!(state(&inv, "computer-use", "claude-code"), Some(State::Off));
    // Clients that read Claude's folder recursively ignore Claude's setting.
    assert_eq!(state(&inv, "computer-use", "opencode"), Some(State::On));
    assert_eq!(state(&inv, "computer-use", "cursor"), Some(State::On));
    assert_eq!(state(&inv, "computer-use", "grok"), Some(State::On));
    // Copilot CLI doesn't read Claude's folder.
    assert_eq!(state(&inv, "computer-use", "github-copilot-cli"), None);
    let issue = inv.issues.iter().find(|i| i.id == "off-elsewhere").unwrap();
    assert_eq!(issue.fix_label.as_deref(), Some("Turn off where possible"));
}

#[test]
fn same_name_from_two_files_is_shadowed_by_client_precedence() {
    let home = Home::new();
    home.skill(".grok/skills/review", "review", "Grok copy");
    home.skill(".agents/skills/review", "review", "Shared copy");
    let inv = scan(&home.env(), &only(&["grok", "cursor"]), None);
    let grok_copy = inv
        .skills
        .iter()
        .find(|s| s.description == "Grok copy")
        .unwrap();
    let shared = inv
        .skills
        .iter()
        .find(|s| s.description == "Shared copy")
        .unwrap();
    // Grok: its own folder comes first.
    assert_eq!(grok_copy.states["grok"].state, State::On);
    assert_eq!(shared.states["grok"].state, State::Shadowed);
    // Cursor: the later folder (~/.agents/skills) overwrites.
    assert_eq!(shared.states["cursor"].state, State::On);
    assert_eq!(grok_copy.states["cursor"].state, State::Shadowed);
    let clash = inv.issues.iter().find(|i| i.id == "clashes").unwrap();
    // Two keys, one readable name.
    assert_eq!(clash.skills.len(), 2);
    assert_eq!(clash.skill_names.len(), 1);
    assert!(!clash.skill_names[0].contains('/'));
}

#[test]
fn broken_links_are_reported_and_only_dangling_links_are_removed() {
    let home = Home::new();
    home.link(
        ".grok/skills/computer-use",
        &home.path(".agents/skills/computer-use"),
    );
    let ok = home.skill(".agents/skills/ok", "ok", "fine");
    home.link(".grok/skills/ok", ok.parent().unwrap());

    let inv = scan(&home.env(), &only(&["grok"]), None);
    let broken = inv
        .skills
        .iter()
        .find(|s| s.source == Source::Broken)
        .unwrap();
    assert_eq!(broken.name, "computer-use");
    assert_eq!(broken.states["grok"].state, State::Broken);
    assert!(inv.issues.iter().any(|i| i.id == "broken-links"));

    let writer = TestWriter::default();
    let removed = apply_fix(&home.env(), &inv, "broken-links", &writer).unwrap();
    assert_eq!(removed, 1);
    assert!(std::fs::symlink_metadata(home.path(".grok/skills/computer-use")).is_err());
    assert!(home.path(".grok/skills/ok/SKILL.md").is_file());
}

#[test]
fn codex_stale_entry_is_reported_and_removed_keeping_the_rest() {
    let home = Home::new();
    let keep = home.skill(".agents/skills/keep", "keep", "Keep");
    home.write(
        ".codex/config.toml",
        &format!(
            "model = \"x\" # my model\n\n[[skills.config]]\npath = \"{}\"\nenabled = false\n\n[[skills.config]]\npath = \"{}\"\nenabled = false\n",
            home.path(".agents/skills/gone/SKILL.md").display(),
            keep.display()
        ),
    );
    let inv = scan(&home.env(), &only(&["codex"]), None);
    assert_eq!(state(&inv, "keep", "codex"), Some(State::Off));
    assert!(inv.issues.iter().any(|i| i.id == "codex-stale"));
    let writer = TestWriter::default();
    assert_eq!(
        apply_fix(&home.env(), &inv, "codex-stale", &writer).unwrap(),
        1
    );
    let text = home.read(".codex/config.toml");
    assert!(text.contains("# my model"));
    assert!(!text.contains("gone"));
    assert!(text.contains("keep/SKILL.md"));
    assert_eq!(writer.backups.borrow()[0].0, "codex");
}

#[test]
fn set_state_writes_each_clients_own_setting() {
    let home = Home::new();
    let real = home.skill(".agents/skills/orca", "orca", "Orca");
    home.link(".claude/skills/orca", real.parent().unwrap());
    home.link(".hermes/skills/orca", real.parent().unwrap());
    home.write(".claude/settings.json", "{\n  \"theme\": \"dark\"\n}\n");
    home.write(
        ".config/opencode/opencode.json",
        "{\n  // keep me\n  \"permissions\": [{\"action\": \"*\", \"resource\": \"*\", \"effect\": \"allow\"}]\n}\n",
    );
    home.write(
        ".hermes/config.yaml",
        "# Hermes\nskills:\n  creation_nudge_interval: 0\nmodel: x\n",
    );
    let env = home.env();
    let writer = TestWriter::default();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "orca").key.clone();
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
    ] {
        set_state(&env, &inv, agent, &key, Wanted::Off, &writer).unwrap();
    }
    let after = scan(&env, &all, None);
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
    ] {
        assert_eq!(state(&after, "orca", agent), Some(State::Off), "{agent}");
    }
    assert!(home
        .read(".claude/settings.json")
        .contains("\"theme\": \"dark\""));
    assert!(home
        .read(".config/opencode/opencode.json")
        .contains("// keep me"));
    let hermes = home.read(".hermes/config.yaml");
    assert!(
        hermes.starts_with(
            "# Hermes\nskills:\n  disabled: [\"orca\"]\n  creation_nudge_interval: 0\nmodel: x"
        ),
        "{hermes}"
    );
    // Codex gets the path it loads the skill through.
    assert!(home.read(".codex/config.toml").contains(
        &home
            .path(".agents/skills/orca/SKILL.md")
            .display()
            .to_string()
    ));

    // And back on.
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
    ] {
        set_state(&env, &after, agent, &key, Wanted::On, &writer).unwrap();
    }
    let back = scan(&env, &all, None);
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
    ] {
        assert_eq!(state(&back, "orca", agent), Some(State::On), "{agent}");
    }
    assert!(!home.read(".codex/config.toml").contains("orca"));
    // Existing files were backed up before each first change; new files were not.
    let backups = writer.backups.borrow();
    assert!(backups.iter().any(|(c, _)| c == "claude-code"));
    assert!(backups.iter().any(|(c, _)| c == "hermes"));
}

#[test]
fn set_state_refuses_clients_without_a_switch_and_name_only_outside_claude() {
    let home = Home::new();
    home.skill(".agents/skills/a", "a", "A");
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "a").key.clone();
    let writer = TestWriter::default();
    assert!(set_state(&env, &inv, "cursor", &key, Wanted::Off, &writer).is_err());
    assert!(set_state(&env, &inv, "codex", &key, Wanted::NameOnly, &writer).is_err());
    assert!(
        set_state(&env, &inv, "hermes", &key, Wanted::Off, &writer).is_err(),
        "Hermes doesn't load it"
    );
}

#[test]
fn opencode_v1_permission_map_is_read_and_written_in_place() {
    let mut config = serde_json::json!({"permission": {"skill": {"a": "deny"}}});
    assert!(opencode_denied(&config).contains("a"));
    opencode_toggle(&mut config, "a", false).unwrap();
    opencode_toggle(&mut config, "b", true).unwrap();
    assert!(config.get("permissions").is_none());
    assert_eq!(opencode_denied(&config), BTreeSet::from(["b".to_string()]));
    // v2: the last matching rule wins.
    let v2 = serde_json::json!({"permissions": [
        {"action": "skill", "resource": "a", "effect": "deny"},
        {"action": "skill", "resource": "a", "effect": "allow"}
    ]});
    assert!(opencode_denied(&v2).is_empty());
}

#[test]
fn yaml_disabled_list_edits_keep_other_lines() {
    let names = vec!["a".to_string(), "b".to_string()];
    assert_eq!(
        set_yaml_skills_disabled("", &names).unwrap(),
        "skills:\n  disabled: [\"a\", \"b\"]\n"
    );
    assert_eq!(
        set_yaml_skills_disabled("model: x\n", &names).unwrap(),
        "model: x\nskills:\n  disabled: [\"a\", \"b\"]\n"
    );
    assert_eq!(
        set_yaml_skills_disabled(
            "skills:\n    disabled:\n    - old\n    - older\n    other: 1\nnext: 2\n",
            &names
        )
        .unwrap(),
        "skills:\n    disabled: [\"a\", \"b\"]\n    other: 1\nnext: 2\n"
    );
    assert_eq!(
        set_yaml_skills_disabled("skills:\n  disabled: [x]\n", &[]).unwrap(),
        "skills:\n  disabled: []\n"
    );
}

#[test]
fn rules_follow_links_and_imports_and_note_skipped_imports() {
    let home = Home::new();
    let agreements = home.write(".agents/working-agreements.md", "# Working agreements\n");
    let instructions = home.write(
        ".agents/claude-instructions.md",
        &format!("@{}\n\n# Delegation\n", agreements.display()),
    );
    home.link(".claude/CLAUDE.md", &instructions);
    home.link(".codex/AGENTS.md", &agreements);
    std::fs::create_dir_all(home.path(".grok")).unwrap();

    let inv = scan(
        &home.env(),
        &only(&["claude-code", "codex", "grok", "github-copilot-cli"]),
        None,
    );
    let wa = inv
        .rules
        .iter()
        .find(|r| r.name == "working-agreements.md")
        .unwrap();
    let chain: Vec<(String, String)> = wa.agents["claude-code"]
        .iter()
        .map(|c| (c.kind.clone(), c.path.clone()))
        .collect();
    assert_eq!(
        chain,
        vec![
            ("file".to_string(), "~/.claude/CLAUDE.md".to_string()),
            (
                "link".to_string(),
                "~/.agents/claude-instructions.md".to_string()
            ),
            (
                "import".to_string(),
                "~/.agents/working-agreements.md".to_string()
            ),
        ]
    );
    assert!(wa.agents.contains_key("codex"));
    assert!(!wa.agents.contains_key("grok"));
    let ci = inv
        .rules
        .iter()
        .find(|r| r.name == "claude-instructions.md")
        .unwrap();
    assert!(ci.agents["grok"]
        .iter()
        .any(|c| c.kind == "skipped-import" && c.path == "~/.agents/working-agreements.md"));
    assert!(inv.issues.iter().any(|i| i.id == "rules-imports"));
    let missing = inv.issues.iter().find(|i| i.id == "rules-missing").unwrap();
    assert_eq!(missing.agents, vec!["github-copilot-cli".to_string()]);
}

#[test]
fn codex_override_file_hides_agents_md() {
    let home = Home::new();
    home.write(".codex/AGENTS.md", "base\n");
    home.write(".codex/AGENTS.override.md", "override\n");
    let inv = scan(&home.env(), &only(&["codex"]), None);
    assert!(inv.rules.iter().any(|r| r.name == "AGENTS.override.md"));
    assert!(!inv.rules.iter().any(|r| r.name == "AGENTS.md"));
}

#[test]
fn project_scope_adds_project_skills_and_rules() {
    let home = Home::new();
    home.skill("work/app/.claude/skills/deploy", "deploy", "Deploy the app");
    home.write("work/app/AGENTS.md", "# App\n");
    home.write("work/app/CLAUDE.md", "@AGENTS.md\n");
    let project = home.path("work/app");
    let inv = scan(
        &home.env(),
        &only(&["claude-code", "codex"]),
        Some(&project),
    );
    assert_eq!(state(&inv, "deploy", "claude-code"), Some(State::On));
    let agents_md = inv.rules.iter().find(|r| r.name == "AGENTS.md").unwrap();
    assert_eq!(agents_md.scope, "project");
    assert!(agents_md.agents["claude-code"]
        .iter()
        .any(|c| c.kind == "import"));
    assert!(agents_md.agents.contains_key("codex"));
}

#[test]
fn parked_package_and_builtin_sources() {
    let home = Home::new();
    home.skill(".agents/disabled-skills/ds", "ds", "Parked");
    home.skill(".codex/skills/.system/imagegen", "imagegen", "Images");
    home.skill(".grok/bundled/skills/review", "review", "Review");
    home.skill(".hermes/skills/devops/build-host", "build-host", "Tuning");
    let inv = scan(&home.env(), &all, None);
    assert_eq!(skill(&inv, "ds").source, Source::Parked);
    assert!(skill(&inv, "ds").states.is_empty());
    assert_eq!(skill(&inv, "imagegen").source, Source::BuiltIn);
    assert_eq!(state(&inv, "imagegen", "codex"), Some(State::On));
    // Cursor skips Codex's hidden .system folder.
    assert_eq!(state(&inv, "imagegen", "cursor"), None);
    assert_eq!(skill(&inv, "review").source, Source::BuiltIn);
    assert_eq!(skill(&inv, "build-host").source, Source::Library);
    assert_eq!(state(&inv, "build-host", "hermes"), Some(State::On));
}

#[test]
fn long_descriptions_and_front_matter() {
    let (name, description) =
        front_matter("---\nname: x\ndescription: >\n  folded\n  text\n---\nbody");
    assert_eq!(name.as_deref(), Some("x"));
    assert_eq!(description.as_deref(), Some("folded text"));
    assert_eq!(front_matter("no front matter"), (None, None));

    let home = Home::new();
    home.skill(
        ".claude/skills/big",
        "big",
        &"y".repeat(DESCRIPTION_CAP + 1),
    );
    let inv = scan(&home.env(), &only(&["claude-code"]), None);
    assert!(inv.issues.iter().any(|i| i.id == "long-descriptions"));
}

#[test]
fn clients_without_skills_are_listed_but_not_scanned() {
    let home = Home::new();
    let inv = scan(&home.env(), &only(&["claude-desktop"]), None);
    assert_eq!(inv.agents.len(), 1);
    assert!(!inv.agents[0].supports_skills);
    assert!(inv.skills.is_empty());
}

#[test]
fn allowed_paths_are_limited_to_the_inventory() {
    let home = Home::new();
    let real = home.skill(".agents/skills/a", "a", "A");
    let inv = scan(&home.env(), &all, None);
    assert!(allowed_path(&inv, &real.display().to_string()));
    assert!(allowed_path(
        &inv,
        &real.parent().unwrap().display().to_string()
    ));
    assert!(!allowed_path(&inv, "/etc/passwd"));
}

/// Manual check against a real machine: `cargo test --lib skills::tests::dump_real_inventory --
/// --ignored --nocapture`. Read-only.
#[test]
#[ignore]
fn dump_real_inventory() {
    let inv = scan(&Env::current().unwrap(), &|_| true, None);
    println!("{}", serde_json::to_string_pretty(&inv).unwrap());
}

#[test]
fn other_clients_read_the_default_claude_folder_not_claude_config_dir() {
    let home = Home::new();
    home.skill(".claude/skills/work", "work", "Default folder");
    home.skill(
        ".claude-personal/skills/personal",
        "personal",
        "Relocated folder",
    );
    let mut env = home.env();
    env.claude = home.path(".claude-personal");
    let inv = scan(&env, &only(&["claude-code", "cursor"]), None);
    assert_eq!(state(&inv, "personal", "claude-code"), Some(State::On));
    assert_eq!(state(&inv, "personal", "cursor"), None);
    assert_eq!(state(&inv, "work", "cursor"), Some(State::On));
    assert_eq!(state(&inv, "work", "claude-code"), None);
}

#[test]
fn broken_links_do_not_shadow_a_working_skill_with_the_same_name() {
    let home = Home::new();
    home.link(
        ".grok/skills/computer-use",
        &home.path(".agents/skills/computer-use"),
    );
    home.skill(
        ".claude/skills/synced/b/computer-use",
        "computer-use",
        "Desktop",
    );
    let inv = scan(&home.env(), &only(&["grok"]), None);
    let working = inv
        .skills
        .iter()
        .find(|s| s.source == Source::Account)
        .unwrap();
    assert_eq!(working.states["grok"].state, State::On);
    assert!(!inv.issues.iter().any(|i| i.id == "clashes"));
}

#[test]
fn front_matter_that_isnt_valid_yaml_still_gives_name_and_description() {
    let (name, description) = front_matter(
        "---\nname: deploy-check\ndescription: Check a deploy: health and errors\n---\n",
    );
    assert_eq!(name.as_deref(), Some("deploy-check"));
    assert_eq!(
        description.as_deref(),
        Some("Check a deploy: health and errors")
    );
    let (_, folded) =
        front_matter("---\nname: x\ndescription: >\n  First line: more\n  second\nother: 1\n---\n");
    assert_eq!(folded.as_deref(), Some("First line: more second"));
}

#[test]
fn opencode_wildcards_decide_state_and_writes_never_widen_a_blanket_rule() {
    // v2: a later wildcard deny turns everything off; an explicit allow after it wins.
    let v2 = serde_json::json!({"permissions": [
        {"action": "skill", "resource": "keep", "effect": "allow"},
        {"action": "skill", "resource": "*", "effect": "deny"}
    ]});
    let rules = opencode_rules(&v2);
    assert!(rules_deny(&rules, "keep"));
    assert!(rules_deny(&rules, "anything"));
    let mut config = v2.clone();
    opencode_toggle(&mut config, "keep", false).unwrap();
    assert!(!rules_deny(&opencode_rules(&config), "keep"));
    assert!(rules_deny(&opencode_rules(&config), "other"));
    // Patterns match by glob.
    assert!(glob_match("orca-*", "orca-cli"));
    assert!(!glob_match("orca-*", "inbox"));
    // v1: one string for every skill is refused, never replaced by a single-skill map.
    let mut blanket = serde_json::json!({"permission": {"skill": "deny"}});
    assert!(opencode_toggle(&mut blanket, "x", true).is_err());
    assert_eq!(
        blanket,
        serde_json::json!({"permission": {"skill": "deny"}})
    );
    // v1 map with a wildcard: turning one on adds an allow instead of only removing its entry.
    let mut map = serde_json::json!({"permission": {"skill": {"*": "deny", "x": "deny"}}});
    opencode_toggle(&mut map, "x", false).unwrap();
    assert_eq!(map["permission"]["skill"]["x"], "allow");
    assert!(rules_deny(&opencode_rules(&map), "y"));
}

#[test]
fn opencode_wildcard_deny_shows_skills_off() {
    let home = Home::new();
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.write(
        ".config/opencode/opencode.json",
        r#"{"permissions": [{"action": "skill", "resource": "*", "effect": "deny"}]}"#,
    );
    let inv = scan(&home.env(), &only(&["opencode"]), None);
    assert_eq!(state(&inv, "inbox", "opencode"), Some(State::Off));
}

#[test]
fn json_settings_in_an_unexpected_shape_are_refused_not_replaced() {
    let home = Home::new();
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.write(".copilot/settings.json", r#"{"disabledSkills": "inbox"}"#);
    let env = home.env();
    let inv = scan(&env, &only(&["github-copilot-cli"]), None);
    let key = skill(&inv, "inbox").key.clone();
    let writer = TestWriter::default();
    assert!(set_state(&env, &inv, "github-copilot-cli", &key, Wanted::Off, &writer).is_err());
    assert_eq!(
        home.read(".copilot/settings.json"),
        r#"{"disabledSkills": "inbox"}"#
    );
}

#[test]
fn hermes_edits_keep_comments_and_refuse_what_they_cannot_read() {
    let names = vec!["a".to_string()];
    // A comment after the key is still the same key, not a reason to add a second one.
    assert_eq!(
        set_yaml_skills_disabled("skills: # mine\n  other: 1\n", &names).unwrap(),
        "skills: # mine\n  disabled: [\"a\"]\n  other: 1\n"
    );
    // A flow mapping on the key's line is refused.
    assert!(set_yaml_skills_disabled("skills: {disabled: [b]}\n", &names).is_err());

    // A config serde_yaml can't parse keeps its list: the toggle fails instead.
    let home = Home::new();
    home.skill(".hermes/skills/notes", "notes", "Notes");
    let broken = "skills:\n  disabled: [x]\nmodel: a\nmodel: b\n";
    home.write(".hermes/config.yaml", broken);
    let env = home.env();
    let inv = scan(&env, &only(&["hermes"]), None);
    let key = skill(&inv, "notes").key.clone();
    let writer = TestWriter::default();
    assert!(set_state(&env, &inv, "hermes", &key, Wanted::Off, &writer).is_err());
    assert_eq!(home.read(".hermes/config.yaml"), broken);
}

#[test]
fn codex_turn_on_clears_entries_written_as_the_folder_or_real_path() {
    let home = Home::new();
    let real = home.skill(".agents/skills/inbox", "inbox", "Inbox");
    let folder = real.parent().unwrap().display().to_string();
    home.write(
        ".codex/config.toml",
        &format!("[[skills.config]]\npath = \"{folder}\"\nenabled = false\n"),
    );
    let env = home.env();
    let inv = scan(&env, &only(&["codex"]), None);
    assert_eq!(state(&inv, "inbox", "codex"), Some(State::Off));
    let key = skill(&inv, "inbox").key.clone();
    let writer = TestWriter::default();
    set_state(&env, &inv, "codex", &key, Wanted::On, &writer).unwrap();
    let inv = scan(&env, &only(&["codex"]), None);
    assert_eq!(state(&inv, "inbox", "codex"), Some(State::On));
}

#[test]
fn unreadable_codex_config_is_an_error_not_an_empty_file() {
    let home = Home::new();
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.write(".codex/config.toml", "model = \"x\"\n");
    let env = home.env();
    let inv = scan(&env, &only(&["codex"]), None);
    let key = skill(&inv, "inbox").key.clone();
    // Too big to read: the write must fail rather than replace the file.
    let big = format!(
        "model = \"x\"\n# {}\n",
        "x".repeat(MAX_FILE_BYTES as usize + 1)
    );
    home.write(".codex/config.toml", &big);
    let writer = TestWriter::default();
    assert!(set_state(&env, &inv, "codex", &key, Wanted::Off, &writer).is_err());
    assert_eq!(home.read(".codex/config.toml").len(), big.len());
}

#[test]
fn claude_plugin_skills_switch_by_plugin_name_and_dont_clash_with_yours() {
    let home = Home::new();
    home.write(
        ".claude/settings.json",
        r#"{"enabledPlugins": {"tools@market": true}}"#,
    );
    home.skill(
        ".claude/plugins/cache/market/tools/1.0.0/skills/review",
        "review",
        "Plugin review",
    );
    home.skill(".claude/skills/review", "review", "My review");
    let env = home.env();
    let inv = scan(&env, &only(&["claude-code"]), None);
    let mine = inv
        .skills
        .iter()
        .find(|s| s.description == "My review")
        .unwrap();
    let plugin = inv
        .skills
        .iter()
        .find(|s| s.description == "Plugin review")
        .unwrap();
    assert_eq!(mine.states["claude-code"].state, State::On);
    assert_eq!(plugin.states["claude-code"].state, State::On);
    assert!(!inv.issues.iter().any(|i| i.id == "clashes"));
    let writer = TestWriter::default();
    set_state(&env, &inv, "claude-code", &plugin.key, Wanted::Off, &writer).unwrap();
    let settings = home.read(".claude/settings.json");
    assert!(settings.contains(r#""tools:review": "off""#), "{settings}");
    let inv = scan(&env, &only(&["claude-code"]), None);
    let mine = inv
        .skills
        .iter()
        .find(|s| s.description == "My review")
        .unwrap();
    assert_eq!(mine.states["claude-code"].state, State::On);
}

#[test]
fn a_fix_that_edits_one_file_many_times_backs_it_up_once() {
    let home = Home::new();
    for name in ["a", "b", "c", "d", "e", "f", "g"] {
        home.skill(&format!(".agents/skills/{name}"), name, "Skill");
        home.link(
            &format!(".claude/skills/{name}"),
            &home.path(&format!(".agents/skills/{name}")),
        );
    }
    home.write(
        ".claude/settings.json",
        r#"{"skillOverrides": {"a": "off", "b": "off", "c": "off", "d": "off", "e": "off", "f": "off", "g": "off"}}"#,
    );
    home.write(".copilot/settings.json", "{}");
    let env = home.env();
    let inv = scan(&env, &only(&["claude-code", "github-copilot-cli"]), None);
    let writer = TestWriter::default();
    assert_eq!(apply_fix(&env, &inv, "off-elsewhere", &writer).unwrap(), 7);
    let backups = writer.backups.borrow();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let inv = scan(&env, &only(&["claude-code", "github-copilot-cli"]), None);
    assert_eq!(state(&inv, "g", "github-copilot-cli"), Some(State::Off));
}

#[test]
fn skills_count_as_yours_through_a_symlinked_home_or_inside_the_project() {
    let home = Home::new();
    home.skill("real-home/.agents/skills/inbox", "inbox", "Inbox");
    home.link("home", &home.path("real-home"));
    let env = Env::from_home(&home.path("home"));
    let inv = scan(&env, &only(&["codex"]), None);
    let inbox = skill(&inv, "inbox");
    assert_eq!(inbox.source, Source::Yours);
    assert!(inbox.path.starts_with("~/"), "{}", inbox.path);

    let project = home.path("work/repo");
    home.skill("work/repo/.agents/skills/build", "build", "Build");
    let other = Home::new();
    let inv = scan(&other.env(), &only(&["codex"]), Some(&project));
    assert_eq!(skill(&inv, "build").source, Source::Yours);
}

#[test]
fn only_dangling_links_count_as_broken() {
    let home = Home::new();
    let target = home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.link(".grok/skills/inbox", target.parent().unwrap());
    assert!(!dangling(&home.path(".grok/skills/inbox")));
    home.link(".grok/skills/gone", &home.path(".agents/skills/gone"));
    assert!(dangling(&home.path(".grok/skills/gone")));
    // A plain folder is never removed as a link.
    assert!(DiskWriter
        .remove_link(&home.path(".agents/skills/inbox"))
        .is_err());
    assert!(home.path(".agents/skills/inbox").is_dir());
}

#[test]
fn open_never_runs_a_non_text_file_it_reached_through_an_import() {
    assert!(opens_as_text(Path::new("/p/CLAUDE.md")));
    assert!(opens_as_text(Path::new("/p/rules.MDC")));
    assert!(!opens_as_text(Path::new("/p/run.sh")));
    assert!(!opens_as_text(Path::new("/p/setup.bat")));
    assert!(!opens_as_text(Path::new("/p/no-extension")));
}

#[test]
fn lists_join_with_localized_separators() {
    let names: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    assert_eq!(list_names(&names), "a, b and c");
    assert_eq!(list_names(&names[..2]), "a and b");
}
