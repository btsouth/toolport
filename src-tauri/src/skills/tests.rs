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
    trashed: RefCell<Vec<PathBuf>>,
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
    fn trash(&self, path: &Path) -> Result<(), String> {
        self.trashed.borrow_mut().push(path.into());
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    }
    fn link(&self, target: &Path, path: &Path) -> Result<(), String> {
        DiskWriter.link(target, path)
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
    home.write(".grok/config.toml", "[skills]\ndisabled = [\"orca\"]\n");
    home.write(
        ".reasonix/config.toml",
        "[skills]\ndisabled_skills = [\"orca\"]\n",
    );
    home.write(".codewhale/skills_state.toml", "disabled = [\"orca\"]\n");
    home.write(
        ".openclaw/openclaw.json",
        "{ skills: { entries: { orca: { enabled: false } } } }\n",
    );
    home.write(
        ".config/crush/crush.json",
        r#"{"options": {"disabled_skills": ["orca"]}}"#,
    );
    home.write(
        ".qwen/settings.json",
        r#"{"skills": {"disabled": ["orca"]}}"#,
    );
    home.write(
        ".omp/agent/config.yml",
        "skills:\n  ignoredSkills: [orca]\n",
    );

    let inv = scan(&home.env(), &all, None);
    for agent in [
        "claude-code",
        "codex",
        "github-copilot-cli",
        "gemini-cli",
        "opencode",
        "hermes",
        "grok",
        "crush",
        "qwen-code",
        "omp",
        "reasonix",
        "codewhale",
        "openclaw",
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
    home.write(".grok/config.toml", "# mine\nmodel = \"grok-5\"\n");
    home.write(".omp/agent/config.yml", "# Oh My Pi\ntheme: dark\n");
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
        "grok",
        "crush",
        "qwen-code",
        "omp",
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
        "grok",
        "crush",
        "qwen-code",
        "omp",
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
    let grok = home.read(".grok/config.toml");
    assert!(
        grok.starts_with("# mine\nmodel = \"grok-5\"\n") && grok.contains("disabled = [\"orca\"]"),
        "{grok}"
    );
    assert!(home
        .read(".omp/agent/config.yml")
        .starts_with("# Oh My Pi\ntheme: dark\nskills:\n  ignoredSkills: [\"orca\"]"));
    assert!(home
        .read(".config/crush/crush.json")
        .contains("disabled_skills"));
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
        "grok",
        "crush",
        "qwen-code",
        "omp",
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
        "grok",
        "crush",
        "qwen-code",
        "omp",
    ] {
        assert_eq!(state(&back, "orca", agent), Some(State::On), "{agent}");
    }
    assert!(!home.read(".codex/config.toml").contains("orca"));
    assert!(!home.read(".grok/config.toml").contains("orca"));
    assert!(!home.read(".omp/agent/config.yml").contains("orca"));
    // Existing files were backed up before each first change; new files were not.
    let backups = writer.backups.borrow();
    assert!(backups.iter().any(|(c, _)| c == "claude-code"));
    assert!(backups.iter().any(|(c, _)| c == "hermes"));
    assert!(backups.iter().any(|(c, _)| c == "grok"));
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

fn set_yaml_skills_list_disabled(text: &str, names: &[String]) -> Result<String, String> {
    set_yaml_skills_list(text, "disabled", names)
}

#[test]
fn yaml_list_edits_touch_only_their_own_key() {
    let names = vec!["a".to_string()];
    assert_eq!(
        set_yaml_skills_list("skills:\n  disabled: [x]\n", "ignoredSkills", &names).unwrap(),
        "skills:\n  ignoredSkills: [\"a\"]\n  disabled: [x]\n"
    );
}

#[test]
fn yaml_disabled_list_edits_keep_other_lines() {
    let names = vec!["a".to_string(), "b".to_string()];
    assert_eq!(
        set_yaml_skills_list_disabled("", &names).unwrap(),
        "skills:\n  disabled: [\"a\", \"b\"]\n"
    );
    assert_eq!(
        set_yaml_skills_list_disabled("model: x\n", &names).unwrap(),
        "model: x\nskills:\n  disabled: [\"a\", \"b\"]\n"
    );
    assert_eq!(
        set_yaml_skills_list_disabled(
            "skills:\n    disabled:\n    - old\n    - older\n    other: 1\nnext: 2\n",
            &names
        )
        .unwrap(),
        "skills:\n    disabled: [\"a\", \"b\"]\n    other: 1\nnext: 2\n"
    );
    assert_eq!(
        set_yaml_skills_list_disabled("skills:\n  disabled: [x]\n", &[]).unwrap(),
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

fn missing_rules(inv: &Inventory) -> Option<&Issue> {
    inv.issues.iter().find(|i| i.id == "rules-missing")
}

#[test]
fn missing_rules_fix_links_the_plain_file_most_clients_read() {
    let home = Home::new();
    let agreements = home.write(".agents/working-agreements.md", "# Working agreements\n");
    let instructions = home.write(
        ".agents/claude-instructions.md",
        &format!("@{}\n\n# Delegation\n", agreements.display()),
    );
    home.link(".claude/CLAUDE.md", &instructions);
    home.link(".codex/AGENTS.md", &agreements);
    home.write(".hermes/SOUL.md", "Be kind.\n");
    let env = home.env();
    let present = only(&[
        "claude-code",
        "codex",
        "grok",
        "hermes",
        "github-copilot-cli",
        "gemini-cli",
        "antigravity",
    ]);
    let inv = scan(&env, &present, None);

    let issue = missing_rules(&inv).unwrap();
    assert_eq!(
        issue.agents,
        vec![
            "github-copilot-cli".to_string(),
            "gemini-cli".to_string(),
            "antigravity".to_string()
        ]
    );
    // Antigravity reads Gemini CLI's GEMINI.md, so that one link covers both.
    // claude-instructions.md reaches Grok too, but it is mostly an import Grok skips.
    assert_eq!(
        issue.fix_label.as_deref(),
        Some("Link to working-agreements.md")
    );
    assert!(issue
        .fix_note
        .as_deref()
        .unwrap()
        .contains("~/.agents/working-agreements.md"));
    assert_eq!(
        issue.fix_paths,
        vec![
            "~/.copilot/copilot-instructions.md".to_string(),
            "~/.gemini/GEMINI.md".to_string()
        ]
    );

    assert_eq!(
        apply_fix(&env, &inv, "rules-missing", &TestWriter::default()),
        Ok(2)
    );
    for rel in [".copilot/copilot-instructions.md", ".gemini/GEMINI.md"] {
        assert_eq!(std::fs::read_link(home.path(rel)).unwrap(), agreements);
    }
    let after = scan(&env, &present, None);
    assert!(missing_rules(&after).is_none());
    let wa = after
        .rules
        .iter()
        .find(|r| r.name == "working-agreements.md")
        .unwrap();
    assert_eq!(
        wa.agents["github-copilot-cli"]
            .iter()
            .map(|c| c.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["file", "link"]
    );
}

#[test]
fn missing_rules_fix_never_replaces_a_file_or_feeds_a_client_twice() {
    let home = Home::new();
    home.write(".codex/AGENTS.md", "# Rules\n");
    // OpenCode has its own rules, so its CLAUDE.md fallback stays unread.
    home.write(".config/opencode/AGENTS.md", "# OpenCode\n");
    std::fs::create_dir_all(home.path(".copilot")).unwrap();
    home.link(".copilot/copilot-instructions.md", &home.path("gone.md"));
    let env = home.env();
    let present = only(&[
        "claude-code",
        "codex",
        "grok",
        "opencode",
        "github-copilot-cli",
    ]);
    let inv = scan(&env, &present, None);
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(
        issue.agents,
        vec![
            "claude-code".to_string(),
            "github-copilot-cli".to_string(),
            "grok".to_string()
        ]
    );
    // Grok reads ~/.claude/CLAUDE.md, so Claude Code's link covers it. The dangling link at
    // Copilot's path stays as it is.
    assert_eq!(issue.fix_paths, vec!["~/.claude/CLAUDE.md".to_string()]);

    // A file appears at the path after the scan: the fix plans again, finds a plan that
    // differs from the one shown, and makes nothing.
    home.write(".claude/CLAUDE.md", "mine\n");
    assert!(
        apply_fix(&env, &inv, "rules-missing", &TestWriter::default())
            .unwrap_err()
            .contains("changed since you looked")
    );
    assert_eq!(home.read(".claude/CLAUDE.md"), "mine\n");
    assert!(home.path(".grok/AGENTS.md").symlink_metadata().is_err());
    assert!(DiskWriter
        .link(
            &home.path(".codex/AGENTS.md"),
            &home.path(".claude/CLAUDE.md")
        )
        .unwrap_err()
        .contains("already exists"));
    assert_eq!(home.read(".claude/CLAUDE.md"), "mine\n");
}

#[test]
fn missing_rules_fix_never_shares_a_persona_file() {
    let home = Home::new();
    home.write("notes.md", "# Notes\n");
    home.write(".codex/AGENTS.md", "@~/notes.md\n");
    home.write(".hermes/SOUL.md", "Be kind.\n");
    let env = home.env();
    let inv = scan(
        &env,
        &only(&["codex", "hermes", "github-copilot-cli"]),
        None,
    );
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(issue.agents, vec!["github-copilot-cli".to_string()]);
    // SOUL.md is the only plain file, but it is Hermes's persona.
    assert_eq!(issue.fix_label, None);
    assert!(issue.fix_paths.is_empty());
}

#[test]
fn a_persona_file_alone_does_not_blame_imports() {
    let home = Home::new();
    home.write(".hermes/SOUL.md", "Be kind.\n");
    let env = home.env();
    let inv = scan(&env, &only(&["codex", "hermes"]), None);
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(issue.agents, vec!["codex".to_string()]);
    assert_eq!(issue.fix_label, None);
    assert!(!issue.detail.contains("@import"));
}

#[test]
fn missing_rules_fix_plans_each_claude_code_profile_in_its_own_folder() {
    let home = Home::new();
    home.write(".claude/CLAUDE.md", "# Work\n");
    home.write(".claude2/settings.json", "{}\n");
    let mut env = home.env();
    env.claude_profiles = claude_profile_dirs(&home.root, &env.claude);
    let inv = scan(&env, &only(&["claude-code"]), None);
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(issue.agents, vec!["claude-code@claude2".to_string()]);
    assert_eq!(issue.fix_paths, vec!["~/.claude2/CLAUDE.md".to_string()]);
    assert_eq!(
        apply_fix(&env, &inv, "rules-missing", &TestWriter::default()),
        Ok(1)
    );
    assert_eq!(
        std::fs::read_link(home.path(".claude2/CLAUDE.md")).unwrap(),
        home.path(".claude/CLAUDE.md")
    );
    assert!(missing_rules(&scan(&env, &only(&["claude-code"]), None)).is_none());
}

#[test]
fn missing_rules_fix_fingerprint_covers_the_plan() {
    let home = Home::new();
    home.write(".codex/AGENTS.md", "# Rules\n");
    std::fs::create_dir_all(home.path(".copilot")).unwrap();
    home.link(".copilot/copilot-instructions.md", &home.path("gone.md"));
    let env = home.env();
    let present = only(&["codex", "github-copilot-cli", "gemini-cli"]);
    let before = missing_rules(&scan(&env, &present, None)).unwrap().clone();
    assert_eq!(before.fix_paths, vec!["~/.gemini/GEMINI.md".to_string()]);
    // Removing the dangling link adds Copilot to the plan, so the fix shown is stale.
    std::fs::remove_file(home.path(".copilot/copilot-instructions.md")).unwrap();
    let after = missing_rules(&scan(&env, &present, None)).unwrap().clone();
    assert_eq!(after.agents, before.agents);
    assert_ne!(after.fingerprint, before.fingerprint);
}

#[test]
fn missing_rules_fix_skips_a_path_a_client_with_rules_already_reads() {
    let home = Home::new();
    // Grok has rules and also reads ~/.claude/CLAUDE.md, so linking it would change Grok.
    home.write(".grok/AGENTS.md", "# Grok\n");
    let env = home.env();
    let inv = scan(&env, &only(&["claude-code", "grok"]), None);
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(issue.agents, vec!["claude-code".to_string()]);
    assert_eq!(issue.fix_label, None);
    assert!(issue.fix_paths.is_empty());
}

#[test]
fn missing_rules_fix_is_not_offered_when_every_rules_file_imports() {
    let home = Home::new();
    home.write("notes.md", "# Notes\n");
    home.write(".codex/AGENTS.md", "@~/notes.md\n");
    let env = home.env();
    let inv = scan(&env, &only(&["codex", "github-copilot-cli"]), None);
    let issue = missing_rules(&inv).unwrap();
    assert_eq!(issue.fix_label, None);
    assert!(issue.detail.contains("@import"));
    assert!(apply_fix(&env, &inv, "rules-missing", &TestWriter::default()).is_err());
    assert!(home
        .path(".copilot/copilot-instructions.md")
        .symlink_metadata()
        .is_err());
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
fn opencode_permission_shorthand_is_spelled_out_not_refused() {
    // `"permission": "allow"` is what OpenCode writes for "allow everything".
    let mut config = serde_json::json!({"permission": "allow", "mcp": {}});
    opencode_toggle(&mut config, "orca-cli", true).unwrap();
    assert_eq!(
        config,
        serde_json::json!({
            "permission": {"*": "allow", "skill": {"orca-cli": "deny"}},
            "mcp": {}
        })
    );
    assert_eq!(
        opencode_denied(&config),
        BTreeSet::from(["orca-cli".to_string()])
    );
    // A blanket deny turns every skill off; turning one back on adds an explicit allow.
    let mut deny_all = serde_json::json!({"permission": "deny"});
    assert!(rules_deny(&opencode_rules(&deny_all), "inbox"));
    opencode_toggle(&mut deny_all, "inbox", false).unwrap();
    assert!(!rules_deny(&opencode_rules(&deny_all), "inbox"));
    assert!(rules_deny(&opencode_rules(&deny_all), "other"));
    // v2 all-actions rules reach skills only when they cover every resource.
    let v2 = serde_json::json!({"permissions": [
        {"action": "*", "resource": "*", "effect": "deny"},
        {"action": "*", "resource": "inbox", "effect": "allow"}
    ]});
    assert!(rules_deny(&opencode_rules(&v2), "inbox"));
}

#[test]
fn opencode_permission_shorthand_scans_and_toggles_end_to_end() {
    let home = Home::new();
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.write(
        ".config/opencode/opencode.json",
        r#"{"permission": "allow", "mcp": {"toolport": {"type": "local"}}}"#,
    );
    let inv = scan(&home.env(), &only(&["opencode"]), None);
    assert_eq!(state(&inv, "inbox", "opencode"), Some(State::On));
    let key = skill(&inv, "inbox").key.clone();
    set_state(
        &home.env(),
        &inv,
        "opencode",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    let inv = scan(&home.env(), &only(&["opencode"]), None);
    assert_eq!(state(&inv, "inbox", "opencode"), Some(State::Off));
    let saved: serde_json::Value =
        serde_json::from_str(&home.read(".config/opencode/opencode.json")).unwrap();
    assert_eq!(saved["permission"]["*"], "allow");
    assert_eq!(saved["mcp"]["toolport"]["type"], "local");
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
        set_yaml_skills_list_disabled("skills: # mine\n  other: 1\n", &names).unwrap(),
        "skills: # mine\n  disabled: [\"a\"]\n  other: 1\n"
    );
    // A flow mapping on the key's line is refused.
    assert!(set_yaml_skills_list_disabled("skills: {disabled: [b]}\n", &names).is_err());

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

#[test]
fn removing_a_skill_trashes_its_folder_and_every_link_to_it() {
    let home = Home::new();
    let real = home.skill(".agents/skills/orca-cli", "orca-cli", "Orca");
    let folder = real.parent().unwrap().to_path_buf();
    home.link(".claude/skills/orca-cli", &folder);
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "orca-cli").key.clone();
    let writer = TestWriter::default();

    let (trashed, links) = remove_skill_files(&env, &inv, &key, &writer).unwrap();

    assert_eq!(trashed.as_deref(), Some(folder.as_path()));
    assert_eq!(links, 1);
    assert_eq!(*writer.trashed.borrow(), vec![folder.clone()]);
    assert!(!is_link(&home.path(".claude/skills/orca-cli")));
    let after = scan(&env, &all, None);
    assert!(after.skills.iter().all(|s| s.name != "orca-cli"));
    // Nothing is left behind as a broken link, and other skills stay.
    assert!(after.issues.iter().all(|i| i.id != "broken-links"));
    assert!(after.skills.iter().any(|s| s.name == "inbox"));
}

#[test]
fn removing_a_linked_skill_keeps_the_folder_it_points_at() {
    let home = Home::new();
    let real = home.skill("Projects/omabox/skill", "omabox", "Boxes");
    let folder = real.parent().unwrap().to_path_buf();
    home.link(".agents/skills/omabox", &folder);
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "omabox").key.clone();
    let writer = TestWriter::default();

    let (trashed, links) = remove_skill_files(&env, &inv, &key, &writer).unwrap();

    assert_eq!(trashed, None);
    assert_eq!(links, 1);
    assert!(writer.trashed.borrow().is_empty());
    assert!(real.is_file(), "the repo's folder is untouched");
    assert!(!is_link(&home.path(".agents/skills/omabox")));
}

#[test]
fn removing_a_skill_inside_a_linked_folder_touches_nothing() {
    let home = Home::new();
    // A skills folder entry that links to a projects folder with skills further down.
    let real = home.skill("work/project-a", "project-a", "A repo");
    home.write("work/project-a/src/main.rs", "fn main() {}\n");
    home.link(".claude/skills/work", &home.path("work"));
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "project-a").key.clone();
    let writer = TestWriter::default();

    let error = remove_skill_files(&env, &inv, &key, &writer).unwrap_err();

    assert!(error.contains("linked folder"), "{error}");
    assert!(writer.trashed.borrow().is_empty());
    assert!(real.is_file());
    assert!(is_link(&home.path(".claude/skills/work")));
}

#[test]
fn a_skills_folder_that_is_itself_a_link_still_counts_as_in_place() {
    let home = Home::new();
    let real = home.skill("dotfiles/skills/notes", "notes", "Notes");
    home.link(".agents/skills", &home.path("dotfiles/skills"));
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "notes").key.clone();
    let writer = TestWriter::default();

    let (trashed, _) = remove_skill_files(&env, &inv, &key, &writer).unwrap();

    assert_eq!(trashed.as_deref(), real.parent());
}

#[test]
fn each_claude_code_profile_gets_its_own_column_and_settings() {
    let home = Home::new();
    home.skill(".claude/skills/work-notes", "work-notes", "Work");
    home.write(".claude2/settings.json", "{}\n");
    home.skill(".claude2/skills/home-notes", "home-notes", "Home");
    home.write(".claude2/CLAUDE.md", "# Personal\n");
    // Not profiles: a backup, and another tool's folder with no Claude Code files.
    home.write(".claude-backup/settings.json", "{}\n");
    home.skill(".claude-router/skills/x", "x", "X");
    let mut env = home.env();
    env.claude_profiles = claude_profile_dirs(&home.root, &env.claude);
    assert_eq!(
        env.claude_profiles,
        vec![home.path(".claude"), home.path(".claude2")]
    );

    let inv = scan(&env, &only(&["claude-code", "cursor"]), None);
    let names: Vec<(&str, &str)> = inv
        .agents
        .iter()
        .map(|a| (a.id.as_str(), a.name.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("claude-code", "Claude Code (claude)"),
            ("claude-code@claude2", "Claude Code (claude2)"),
            ("cursor", "Cursor"),
        ]
    );
    assert!(inv.agents.iter().filter(|a| a.name_only).count() == 2);
    let home_notes = skill(&inv, "home-notes");
    assert_eq!(
        home_notes.states.keys().collect::<Vec<_>>(),
        vec!["claude-code@claude2"]
    );
    // Cursor reads only ~/.claude, never the other profiles.
    assert!(skill(&inv, "work-notes").states.contains_key("cursor"));
    let personal = inv
        .rules
        .iter()
        .find(|r| r.path == "~/.claude2/CLAUDE.md")
        .unwrap();
    assert_eq!(
        personal.agents.keys().collect::<Vec<_>>(),
        vec!["claude-code@claude2"]
    );

    let key = home_notes.key.clone();
    set_state(
        &env,
        &inv,
        "claude-code@claude2",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    assert!(home.read(".claude2/settings.json").contains("home-notes"));
    assert!(!home.path(".claude/settings.json").exists());
    let after = scan(&env, &only(&["claude-code", "cursor"]), None);
    assert_eq!(
        state(&after, "home-notes", "claude-code@claude2"),
        Some(State::Off)
    );
}

#[test]
fn profiles_whose_folders_share_a_name_get_different_columns() {
    let home = Home::new();
    home.write(".claude/settings.json", "{}\n");
    home.write(".claude2/settings.json", "{}\n");
    home.write("data/.claude2/settings.json", "{}\n");
    let mut env = home.env();
    env.claude = home.path("data/.claude2");
    env.claude_profiles = claude_profile_dirs(&home.root, &env.claude);
    assert_eq!(env.claude_profiles.len(), 3);

    let inv = scan(&env, &only(&["claude-code"]), None);
    let names: Vec<&str> = inv.agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Claude Code (claude)",
            "Claude Code (~/data/.claude2)",
            "Claude Code (~/.claude2)",
        ]
    );
    let ids: BTreeSet<&str> = inv.agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids.len(), 3);
    // Ids name a backup folder, so they hold no path separators.
    assert!(
        ids.iter().all(|id| !id.contains(['/', '\\', '~'])),
        "{ids:?}"
    );
}

#[test]
fn one_claude_code_profile_keeps_the_plain_column() {
    let home = Home::new();
    home.skill(".claude/skills/notes", "notes", "Notes");
    let mut env = home.env();
    env.claude_profiles = claude_profile_dirs(&home.root, &env.claude);
    assert!(env.claude_profiles.is_empty());
    let inv = scan(&env, &only(&["claude-code"]), None);
    assert_eq!(inv.agents[0].id, "claude-code");
    assert_eq!(inv.agents[0].name, "Claude Code");
}

#[test]
fn a_blanket_opencode_deny_is_not_a_choice_to_spread() {
    let home = Home::new();
    home.skill(".agents/skills/inbox", "inbox", "Inbox");
    home.skill(".agents/skills/notes", "notes", "Notes");
    home.link(".claude/skills/inbox", &home.path(".agents/skills/inbox"));
    home.link(".claude/skills/notes", &home.path(".agents/skills/notes"));
    home.write(
        ".config/opencode/opencode.json",
        r#"{"permission": {"skill": {"*": "deny", "notes": "deny"}}}"#,
    );
    let inv = scan(&home.env(), &only(&["claude-code", "opencode"]), None);
    assert_eq!(state(&inv, "inbox", "opencode"), Some(State::Off));
    assert!(skill(&inv, "inbox").states["opencode"].by_pattern);
    assert!(!skill(&inv, "notes").states["opencode"].by_pattern);
    // Only the skill turned off by name is offered for turning off elsewhere.
    let issue = inv.issues.iter().find(|i| i.id == "off-elsewhere").unwrap();
    assert_eq!(issue.skill_names, vec!["notes".to_string()]);
}

#[test]
fn only_your_own_skills_can_be_removed() {
    let home = Home::new();
    home.skill(".codex/skills/.system/imagegen", "imagegen", "Images");
    home.skill(".claude/skills/synced/acct/notes", "notes", "Notes");
    let env = home.env();
    let inv = scan(&env, &all, None);
    let writer = TestWriter::default();
    for name in ["imagegen", "notes"] {
        let found = skill(&inv, name);
        assert!(!removable(found), "{name}");
        assert!(remove_skill_files(&env, &inv, &found.key, &writer).is_err());
    }
    assert!(writer.trashed.borrow().is_empty());
    assert!(home
        .path(".codex/skills/.system/imagegen/SKILL.md")
        .is_file());
}

#[test]
fn issue_fingerprints_change_only_when_the_problem_does() {
    let home = Home::new();
    let real = home.skill(".agents/skills/pdf", "pdf", "One");
    home.skill(".claude/skills/pdf", "pdf", "Two");
    let _ = real;
    let env = home.env();
    let first = scan(&env, &all, None);
    let again = scan(&env, &all, None);
    let print = |inv: &Inventory| {
        inv.issues
            .iter()
            .find(|i| i.id == "clashes")
            .map(|i| i.fingerprint.clone())
            .unwrap()
    };
    assert!(print(&first).starts_with("clashes:"));
    assert_eq!(print(&first), print(&again));
    home.skill(".claude/skills/docx", "docx", "Three");
    home.skill(".agents/skills/docx", "docx", "Four");
    assert_ne!(print(&first), print(&scan(&env, &all, None)));
}

#[test]
fn folder_issue_fingerprints_follow_the_folders_not_the_wording() {
    let home = Home::new();
    let synced = ".claude/skills/synced";
    home.skill(&format!("{synced}/a/one"), "one", "One");
    home.skill(&format!("{synced}/b/two"), "two", "Two");
    let env = home.env();
    let print = |inv: &Inventory| {
        inv.issues
            .iter()
            .find(|i| i.id == "stale-sync")
            .map(|i| i.fingerprint.clone())
            .unwrap()
    };
    let first = scan(&env, &all, None);
    let stale = first.issues.iter().find(|i| i.id == "stale-sync").unwrap();
    assert_eq!(stale.subject, vec!["~/.claude/skills/synced/a".to_string()]);
    assert_eq!(print(&first), print(&scan(&env, &all, None)));
    home.skill(&format!("{synced}/c/three"), "three", "Three");
    assert_ne!(print(&first), print(&scan(&env, &all, None)));
}

#[test]
fn a_clash_lists_only_the_copies_that_meet_in_one_client() {
    let home = Home::new();
    home.skill(".claude/skills/pdf", "pdf", "Mine");
    home.skill(".claude/skills/synced/team/pdf", "pdf", "Team");
    // Hermes has its own lone pdf; it isn't part of Claude Code's clash, even though Hermes
    // has a clash of its own on another name.
    home.skill(".hermes/skills/productivity/pdf", "pdf", "Hermes");
    home.skill(".hermes/skills/a/notes", "notes", "One");
    home.skill(".hermes/skills/b/notes", "notes", "Two");
    let inv = scan(&home.env(), &only(&["claude-code", "hermes"]), None);
    let clash = inv.issues.iter().find(|i| i.id == "clashes").unwrap();
    assert_eq!(
        clash.agents,
        vec!["claude-code".to_string(), "hermes".to_string()]
    );
    assert_eq!(clash.skills.len(), 4, "{:?}", clash.skills);
    assert!(!clash.skills.iter().any(|k| k.contains("productivity/pdf")));
}

#[test]
fn a_parked_skill_can_be_removed() {
    let home = Home::new();
    let real = home.skill(".agents/disabled-skills/old", "old", "Parked");
    let env = home.env();
    let inv = scan(&env, &all, None);
    let parked = skill(&inv, "old");
    assert!(parked.removable && parked.trash_on_remove);
    let writer = TestWriter::default();

    let (trashed, links) = remove_skill_files(&env, &inv, &parked.key, &writer).unwrap();

    assert_eq!(trashed.as_deref(), real.parent());
    assert_eq!(links, 0);
}

#[test]
fn a_skill_in_place_is_removable_even_when_a_linked_folder_also_reaches_it() {
    let home = Home::new();
    let real = home.skill(".agents/skills/foo", "foo", "Foo");
    // Codex walks its folder deeply, so this link reaches foo a second way.
    home.link(".codex/skills/shared", &home.path(".agents/skills"));
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "foo").key.clone();
    let writer = TestWriter::default();

    let (trashed, links) = remove_skill_files(&env, &inv, &key, &writer).unwrap();

    assert_eq!(trashed.as_deref(), real.parent());
    assert_eq!(links, 0);
    // The linked folder leads to other things too, so it stays.
    assert!(is_link(&home.path(".codex/skills/shared")));
}

#[test]
fn a_fix_waits_for_the_problem_the_user_saw() {
    let home = Home::new();
    home.link(".agents/skills/gone", &home.path("Projects/old/skill"));
    let inv = scan(&home.env(), &all, None);
    let seen = inv
        .issues
        .iter()
        .find(|i| i.id == "broken-links")
        .unwrap()
        .fingerprint
        .clone();
    assert_eq!(still_as_seen(&inv, "broken-links", &seen), Ok(()));
    // A second broken link turned up before the fix ran.
    home.link(
        ".agents/skills/gone-too",
        &home.path("Projects/older/skill"),
    );
    let newer = scan(&home.env(), &all, None);
    let error = still_as_seen(&newer, "broken-links", &seen).unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert!(still_as_seen(&newer, "clashes", "clashes:0").is_err());
}

#[test]
fn a_link_into_an_unmounted_drive_is_not_offered_for_removal() {
    let home = Home::new();
    home.link(
        ".agents/skills/nas-skill",
        Path::new("/toolport-test-unmounted/skills/nas-skill"),
    );
    home.link(".agents/skills/gone", &home.path("Projects/old/skill"));
    let env = home.env();
    let inv = scan(&env, &all, None);
    assert_eq!(state(&inv, "nas-skill", "codex"), Some(State::Broken));
    let issue = inv.issues.iter().find(|i| i.id == "broken-links").unwrap();
    assert_eq!(issue.skill_names, vec!["gone".to_string()]);
    assert_eq!(
        apply_fix(&env, &inv, "broken-links", &TestWriter::default()),
        Ok(1)
    );
    assert!(is_link(&home.path(".agents/skills/nas-skill")));
    // Open and Reveal on a broken link show the folder it sits in.
    assert!(dangling(&home.path(".agents/skills/nas-skill")));
}

#[test]
fn a_skill_on_a_second_drive_is_yours_not_a_package() {
    assert!(system_path(Path::new("/usr/share/skills/x/SKILL.md")));
    assert!(system_path(Path::new("/opt/tool/skills/x/SKILL.md")));
    assert!(!system_path(Path::new("/data/skills/x/SKILL.md")));
    assert!(!system_path(Path::new("/mnt/work/skills/x/SKILL.md")));
}

#[test]
fn imports_inside_a_sentence_count_but_code_does_not() {
    let base = Path::new("/p");
    let home = Path::new("/h");
    let text = "See @README.md and @docs/style.md, then ask @someone.\n\
                Run `@not/this.md` first.\n\
                ```\n@also/not.md\n```\n\
                @~/rules/a.md\n\
                Also (see @notes.md).\n";
    assert_eq!(
        imports(text, base, home),
        vec![
            PathBuf::from("/p/README.md"),
            PathBuf::from("/p/docs/style.md"),
            PathBuf::from("/p/someone"),
            PathBuf::from("/h/rules/a.md"),
            PathBuf::from("/p/notes.md"),
        ]
    );
}

#[test]
fn opencode_v2_turn_on_counts_a_v1_blanket_deny_beside_it() {
    let home = Home::new();
    home.skill(".agents/skills/x", "x", "X");
    home.write(
        ".config/opencode/opencode.json",
        r#"{"permission": "deny", "permissions": [{"action": "skill", "resource": "x", "effect": "deny"}]}"#,
    );
    let env = home.env();
    let inv = scan(&env, &all, None);
    let key = skill(&inv, "x").key.clone();
    set_state(
        &env,
        &inv,
        "opencode",
        &key,
        Wanted::On,
        &TestWriter::default(),
    )
    .unwrap();
    let after = scan(&env, &all, None);
    assert_eq!(state(&after, "x", "opencode"), Some(State::On));
}

#[test]
fn each_client_walks_its_folders_the_way_it_really_does() {
    let home = Home::new();
    // Claude Code reads one level, plus claude.ai's synced buckets.
    home.skill(".claude/skills/group/nested", "nested", "Nested");
    home.skill(".claude/skills/synced/bucket/account", "account", "Account");
    // Codex goes deep but skips a SKILL.md that is a link.
    home.skill(".codex/skills/group/deep", "deep", "Deep");
    let elsewhere = home.skill("store/filelink", "filelink", "File link");
    std::fs::create_dir_all(home.path(".codex/skills/filelink")).unwrap();
    home.link(".codex/skills/filelink/SKILL.md", &elsewhere);
    // Continue skips a skill folder that is a link.
    let shared = home.skill("store/dirlink", "dirlink", "Dir link");
    home.link(".continue/skills/dirlink", shared.parent().unwrap());
    home.skill(".continue/skills/own", "own", "Own");

    let inv = scan(&home.env(), &all, None);
    assert_eq!(state(&inv, "nested", "claude-code"), None);
    assert_eq!(state(&inv, "account", "claude-code"), Some(State::On));
    assert_eq!(state(&inv, "deep", "codex"), Some(State::On));
    assert_eq!(state(&inv, "filelink", "codex"), None);
    // Cursor reads Codex's folder and follows the link.
    assert_eq!(state(&inv, "filelink", "cursor"), Some(State::On));
    // Nobody else reads that folder, so the linked skill isn't listed at all.
    assert!(!inv.skills.iter().any(|s| s.name == "dirlink"));
    assert_eq!(state(&inv, "own", "continue"), Some(State::On));
}

#[test]
fn rules_files_match_what_each_client_reads() {
    let home = Home::new();
    home.write(".claude/CLAUDE.md", "# Claude\n");
    // OpenCode 2 has no CLAUDE.md fallback.
    home.write(".config/opencode/.keep", "");
    // Pi: the first name in its chain wins.
    home.write(".pi/agent/AGENTS.md", "# Pi\n");
    home.write(".pi/agent/CLAUDE.md", "# skipped\n");
    // Gemini CLI follows context.fileName.
    home.write(
        ".gemini/settings.json",
        r#"{"context": {"fileName": ["AGENTS.md", "GEMINI.md"]}}"#,
    );
    home.write(".gemini/AGENTS.md", "# Gemini\n");
    // Antigravity loads a rules file only with trigger: always_on.
    home.write(
        ".gemini/config/rules/always.md",
        "---\ntrigger: always_on\n---\nYes\n",
    );
    home.write(
        ".gemini/config/rules/manual.md",
        "---\ntrigger: manual\n---\nNo\n",
    );
    // Copilot's instructions folder.
    home.write(".copilot/instructions/style.instructions.md", "Style\n");
    home.write(
        ".copilot/instructions/notes.md",
        "Not an instructions file\n",
    );
    // Grok adds up its own files and the Cursor ones.
    home.write(".grok/rules/extra.md", "Extra\n");
    home.write(".cursor/AGENTS.md", "Cursor\n");

    let inv = scan(&home.env(), &all, None);
    let readers = |path: &str| -> Vec<String> {
        inv.rules
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.agents.keys().cloned().collect())
            .unwrap_or_default()
    };
    assert!(!readers("~/.claude/CLAUDE.md").contains(&"opencode".to_string()));
    assert!(readers("~/.claude/CLAUDE.md").contains(&"grok".to_string()));
    assert_eq!(readers("~/.pi/agent/AGENTS.md"), vec!["pi".to_string()]);
    assert!(readers("~/.pi/agent/CLAUDE.md").is_empty());
    assert!(readers("~/.gemini/AGENTS.md").contains(&"gemini-cli".to_string()));
    assert!(readers("~/.gemini/config/rules/always.md").contains(&"antigravity".to_string()));
    assert!(readers("~/.gemini/config/rules/manual.md").is_empty());
    assert!(readers("~/.copilot/instructions/style.instructions.md")
        .contains(&"github-copilot-cli".to_string()));
    assert!(readers("~/.copilot/instructions/notes.md").is_empty());
    assert!(readers("~/.grok/rules/extra.md").contains(&"grok".to_string()));
    assert!(readers("~/.cursor/AGENTS.md").contains(&"grok".to_string()));
}

#[test]
fn hermes_reads_the_extra_folders_its_config_lists() {
    let home = Home::new();
    home.skill(".agents/skills/shared", "shared", "Shared");
    let env = home.env();
    assert_eq!(state(&scan(&env, &all, None), "shared", "hermes"), None);
    home.write(
        ".hermes/config.yaml",
        "skills:\n  external_dirs:\n    - ~/.agents/skills\n",
    );
    assert_eq!(
        state(&scan(&env, &all, None), "shared", "hermes"),
        Some(State::On)
    );
}

#[test]
fn auggie_command_code_and_tabnine_have_columns() {
    let home = Home::new();
    home.skill(".augment/skills/a", "a", "A");
    home.skill(".commandcode/skills/c", "c", "C");
    home.skill(".tabnine/agent/skills/t", "t", "T");
    let inv = scan(
        &home.env(),
        &only(&["auggie", "command-code", "tabnine"]),
        None,
    );
    assert_eq!(state(&inv, "a", "auggie"), Some(State::On));
    assert_eq!(state(&inv, "c", "command-code"), Some(State::On));
    assert_eq!(state(&inv, "t", "tabnine"), Some(State::On));
}

#[test]
fn deepseek_harness_reads_both_homes_flat_and_both_user_rules() {
    let home = Home::new();
    home.skill(".dsh/skills/own", "own", "Own");
    home.skill(".agents/skills/shared", "shared", "Shared");
    // dsh deliberately skips nested SKILL.md files and the bundled .system folder.
    home.skill(".dsh/skills/group/nested", "nested", "Nested");
    home.skill(".dsh/skills/.system/bundled", "bundled", "Bundled");
    home.write(".dsh/AGENTS.md", "# dsh\n");
    home.write(".agents/AGENTS.md", "# shared\n");
    let inv = scan(&home.env(), &only(&["deepseek-harness"]), None);
    assert_eq!(state(&inv, "own", "deepseek-harness"), Some(State::On));
    assert_eq!(state(&inv, "shared", "deepseek-harness"), Some(State::On));
    assert!(!inv
        .skills
        .iter()
        .any(|s| s.name == "nested" || s.name == "bundled"));
    for path in ["~/.dsh/AGENTS.md", "~/.agents/AGENTS.md"] {
        let rule = inv.rules.iter().find(|r| r.path == path).expect(path);
        assert!(rule.agents.contains_key("deepseek-harness"), "{path}");
    }

    // A project skill beats a personal one with the same name, as in dsh.
    let project = home.path("work/app");
    home.skill(".dsh/skills/style", "style", "Personal");
    home.skill("work/app/.dsh/skills/style", "style", "Project");
    let inv = scan(&home.env(), &only(&["deepseek-harness"]), Some(&project));
    let winner = inv
        .skills
        .iter()
        .find(|s| {
            s.name == "style"
                && s.states
                    .get("deepseek-harness")
                    .is_some_and(|st| st.state == State::On)
        })
        .map(|s| s.description.clone());
    assert_eq!(winner.as_deref(), Some("Project"));
}

#[test]
fn reasonix_reads_its_folders_in_order_and_switches_through_disabled_skills() {
    let home = Home::new();
    home.skill(".reasonix/skills/own", "own", "Own");
    home.skill(".agents/skills/shared", "shared", "Shared");
    home.skill(".claude/skills/claude", "claude", "Claude");
    home.skill("more/extra", "extra", "Extra");
    // `[skills] excluded_paths` drops a folder; nested skills are supported.
    home.skill(".agent/skills/skipped", "skipped", "Skipped");
    home.skill(".reasonix/skills/group/nested", "nested", "Nested");
    // `[skills] paths` wins a name clash over the home folder.
    home.skill(".reasonix/skills/extra", "extra", "Home copy");
    home.write(
        ".reasonix/config.toml",
        "# mine\ndefault_model = \"deepseek-flash\"\n\n[skills]\npaths = [\"~/more\"]\nexcluded_paths = [\"~/.agent/skills\"]\ndisabled_skills = [\"claude\"]\n",
    );
    home.write(".reasonix/REASONIX.md", "# reasonix\n");
    home.write(".reasonix/AGENTS.md", "# agents\n");
    let env = home.env();
    let inv = scan(&env, &only(&["reasonix"]), None);
    for name in ["own", "shared", "nested"] {
        assert_eq!(state(&inv, name, "reasonix"), Some(State::On), "{name}");
    }
    assert_eq!(state(&inv, "claude", "reasonix"), Some(State::Off));
    assert!(!inv.skills.iter().any(|s| s.name == "skipped"));
    let extra: Vec<_> = inv.skills.iter().filter(|s| s.name == "extra").collect();
    let winner = extra
        .iter()
        .find(|s| s.states["reasonix"].state == State::On)
        .expect("one extra loads");
    assert!(winner.path.contains("more"), "{}", winner.path);
    for path in ["~/.reasonix/REASONIX.md", "~/.reasonix/AGENTS.md"] {
        let rule = inv.rules.iter().find(|r| r.path == path).expect(path);
        assert!(rule.agents.contains_key("reasonix"), "{path}");
    }
    // Project rule files follow `@` imports too.
    home.write("work/app/REASONIX.md", "@notes.md\n");
    home.write("work/app/notes.md", "# notes\n");
    let project = home.path("work/app");
    let inv = scan(&env, &only(&["reasonix"]), Some(&project));
    let notes = inv.rules.iter().find(|r| r.name == "notes.md").unwrap();
    assert!(notes.agents["reasonix"].iter().any(|c| c.kind == "import"));
    let inv = scan(&env, &only(&["reasonix"]), None);

    let writer = TestWriter::default();
    let key = skill(&inv, "own").key.clone();
    set_state(&env, &inv, "reasonix", &key, Wanted::Off, &writer).unwrap();
    let text = home.read(".reasonix/config.toml");
    assert!(text.starts_with("# mine\ndefault_model"), "{text}");
    assert!(
        text.contains("disabled_skills = [\"claude\", \"own\"]"),
        "{text}"
    );
    let after = scan(&env, &only(&["reasonix"]), None);
    assert_eq!(state(&after, "own", "reasonix"), Some(State::Off));

    // With REASONIX_HOME set Reasonix reads only its own home.
    let mut portable = env.clone();
    portable.reasonix_portable = true;
    let inv = scan(&portable, &only(&["reasonix"]), None);
    assert!(inv
        .skills
        .iter()
        .all(|s| s.name != "shared" && s.name != "claude"));
}

#[test]
fn codewhale_reads_its_folders_and_switches_through_skills_state() {
    let home = Home::new();
    home.skill(".codewhale/skills/own", "own", "Own");
    home.skill(".codewhale/skills/vendor/nested", "nested", "Nested");
    home.skill(".agents/skills/shared", "shared", "Shared");
    home.skill(".claude/skills/claude", "claude", "Claude");
    home.skill(".deepseek/skills/legacy", "legacy", "Legacy");
    // The owned folder wins a name clash over the shared ones.
    home.skill(".agents/skills/own", "own", "Shared copy");
    home.write(
        ".codewhale/skills_state.toml",
        "disabled = [\"claude\", \"!codewhale-skill-state:1:history:x\"]\n",
    );
    home.write(".agents/AGENTS.md", "# shared\n");
    home.write(".codewhale/instructions.md", "# later\n");
    let env = home.env();
    let inv = scan(&env, &only(&["codewhale"]), None);
    for name in ["nested", "shared", "legacy"] {
        assert_eq!(state(&inv, name, "codewhale"), Some(State::On), "{name}");
    }
    assert_eq!(state(&inv, "claude", "codewhale"), Some(State::Off));
    let own: Vec<_> = inv.skills.iter().filter(|s| s.name == "own").collect();
    let winner = own
        .iter()
        .find(|s| s.states["codewhale"].state == State::On)
        .expect("one copy loads");
    assert_eq!(winner.description, "Own");
    // Only the first global instruction file that exists loads.
    let rule = inv
        .rules
        .iter()
        .find(|r| r.path == "~/.agents/AGENTS.md")
        .expect("global AGENTS.md");
    assert!(rule.agents.contains_key("codewhale"));

    let writer = TestWriter::default();
    let key = skill(&inv, "shared").key.clone();
    set_state(&env, &inv, "codewhale", &key, Wanted::Off, &writer).unwrap();
    let text = home.read(".codewhale/skills_state.toml");
    for retained in [
        "claude",
        "!codewhale-skill-state:1:history:x",
        "shared",
        "!codewhale-skill-state:1:history:claude",
        "!codewhale-skill-state:1:history:shared",
    ] {
        assert!(text.contains(retained), "{text}");
    }
    let key = skill(&inv, "claude").key.clone();
    set_state(&env, &inv, "codewhale", &key, Wanted::On, &writer).unwrap();
    let after = scan(&env, &only(&["codewhale"]), None);
    assert_eq!(state(&after, "shared", "codewhale"), Some(State::Off));
    assert_eq!(state(&after, "claude", "codewhale"), Some(State::On));

    // `[skills] scan_codewhale_only` drops the shared folders.
    home.write(
        ".codewhale/config.toml",
        "[skills]\nscan_codewhale_only = true\n",
    );
    let project = home.path("work/app");
    home.skill("work/app/.codewhale/skills/mine", "mine", "Mine");
    home.skill("work/app/.claude/skills/theirs", "theirs", "Theirs");
    let on = |inv: &Inventory, name: &str| {
        inv.skills.iter().any(|s| {
            s.name == name
                && s.states
                    .get("codewhale")
                    .is_some_and(|st| st.state == State::On)
        })
    };
    let inv = scan(&home.env(), &only(&["codewhale"]), Some(&project));
    assert!(on(&inv, "mine") && !on(&inv, "theirs"));
    assert!(!on(&inv, "shared") && !on(&inv, "claude"));

    // The flag is read from the legacy config when Codewhale's home has none.
    std::fs::rename(
        home.path(".codewhale/config.toml"),
        home.path(".deepseek/config.toml"),
    )
    .unwrap();
    let inv = scan(&home.env(), &only(&["codewhale"]), Some(&project));
    assert!(on(&inv, "mine") && !on(&inv, "theirs"));
    home.write(".codewhale/config.toml", "");
    let inv = scan(&home.env(), &only(&["codewhale"]), Some(&project));
    assert!(on(&inv, "theirs") && on(&inv, "claude"));
}

#[test]
fn openclaw_reads_its_workspace_first_and_switches_through_skill_entries() {
    let home = Home::new();
    let writer = TestWriter::default();
    home.skill(".openclaw/workspace/skills/style", "style", "Workspace");
    home.skill(".openclaw/skills/style", "style", "Managed");
    home.skill(".openclaw/skills/team/deploy", "deploy", "Deploy");
    home.skill(".agents/skills/shared", "shared", "Shared");
    home.skill("extra/tools/lint", "lint", "Lint");
    let config = "{\n  // mine\n  gateway: { port: 18789 },\n  skills: {\n    load: { extraDirs: ['~/extra'] },\n    entries: { shared: { enabled: false, env: { A: '1' } } },\n  },\n}\n";
    home.write(".openclaw/openclaw.json", config);
    home.write(".openclaw/workspace/AGENTS.md", "# agent\n");

    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    for name in ["deploy", "lint"] {
        assert_eq!(state(&inv, name, "openclaw"), Some(State::On), "{name}");
    }
    assert_eq!(state(&inv, "shared", "openclaw"), Some(State::Off));
    let winner = inv
        .skills
        .iter()
        .find(|s| s.name == "style" && s.states["openclaw"].state == State::On)
        .unwrap();
    assert!(winner.key.contains("workspace"), "{}", winner.key);
    let rule = inv
        .rules
        .iter()
        .find(|r| r.path == "~/.openclaw/workspace/AGENTS.md")
        .unwrap();
    assert!(rule.agents.contains_key("openclaw"));

    // On removes only `enabled`; off adds it. The rest of the file stays.
    let shared = skill(&inv, "shared").key.clone();
    set_state(&home.env(), &inv, "openclaw", &shared, Wanted::On, &writer).unwrap();
    let text = home.read(".openclaw/openclaw.json");
    assert!(
        text.contains("// mine") && text.contains("extraDirs: ['~/extra']"),
        "{text}"
    );
    assert!(!text.contains("enabled"), "{text}");
    assert!(text.contains("A: '1'"), "{text}");
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    let deploy = skill(&inv, "deploy").key.clone();
    set_state(&home.env(), &inv, "openclaw", &deploy, Wanted::Off, &writer).unwrap();
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    assert_eq!(state(&inv, "deploy", "openclaw"), Some(State::Off));
    assert_eq!(state(&inv, "shared", "openclaw"), Some(State::On));
    set_state(&home.env(), &inv, "openclaw", &deploy, Wanted::On, &writer).unwrap();
    assert!(!home.read(".openclaw/openclaw.json").contains("deploy"));

    // A skill with a skillKey is filed under that key.
    home.write(
        ".openclaw/skills/keyed/SKILL.md",
        "---\nname: keyed\ndescription: Keyed\nmetadata: { \"openclaw\": { \"skillKey\": \"keyed-tool\" } }\n---\n",
    );
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    let keyed = skill(&inv, "keyed").key.clone();
    set_state(&home.env(), &inv, "openclaw", &keyed, Wanted::Off, &writer).unwrap();
    let text = home.read(".openclaw/openclaw.json");
    assert!(
        text.contains("keyed-tool") && !text.contains("\"keyed\""),
        "{text}"
    );
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    assert_eq!(state(&inv, "keyed", "openclaw"), Some(State::Off));

    // A root $include is refused: a skills key next to it would override it.
    home.write(".openclaw/openclaw.json", "{ $include: './base.json5' }\n");
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    assert!(set_state(&home.env(), &inv, "openclaw", &deploy, Wanted::Off, &writer).is_err());
    assert_eq!(
        home.read(".openclaw/openclaw.json"),
        "{ $include: './base.json5' }\n"
    );
    home.write(".openclaw/openclaw.json", config);

    // A state dir of its own drops the personal ~/.agents/skills.
    let mut env = home.env();
    env.openclaw = crate::clients::openclaw::dirs(&home.path(""), &|key| {
        (key == "OPENCLAW_STATE_DIR").then(|| "~/state".to_string())
    });
    let inv = scan(&env, &only(&["openclaw"]), None);
    assert!(!inv
        .skills
        .iter()
        .any(|s| s.name == "shared" && s.states.contains_key("openclaw")));
}

#[test]
fn grok_edits_keep_the_skills_table_and_refuse_a_list_they_cannot_read() {
    let home = Home::new();
    let writer = TestWriter::default();
    let path = home.write(
        ".grok/config.toml",
        "[skills]\npaths = [\"~/more\"]\ndisabled = [\"old\"]\n",
    );
    edit_toml_skills_list("grok", &path, "skills.disabled", &writer, "new", true).unwrap();
    let text = home.read(".grok/config.toml");
    assert!(text.contains("paths = [\"~/more\"]"), "{text}");
    assert!(text.contains("disabled = [\"old\", \"new\"]"), "{text}");
    edit_toml_skills_list("grok", &path, "skills.disabled", &writer, "old", false).unwrap();
    assert!(home
        .read(".grok/config.toml")
        .contains("disabled = [\"new\"]"));

    let inline = home.write(".grok/inline.toml", "skills = { paths = [\"~/more\"] }\n");
    edit_toml_skills_list("grok", &inline, "skills.disabled", &writer, "x", true).unwrap();
    let text = home.read(".grok/inline.toml");
    assert!(
        text.contains("paths") && text.contains("disabled = [\"x\"]"),
        "{text}"
    );

    let odd = home.write(".grok/odd.toml", "[skills]\ndisabled = \"x\"\n");
    assert!(edit_toml_skills_list("grok", &odd, "skills.disabled", &writer, "y", true).is_err());
    assert_eq!(home.read(".grok/odd.toml"), "[skills]\ndisabled = \"x\"\n");
}

#[test]
fn project_folders_are_walked_the_way_each_client_walks_them() {
    let home = Home::new();
    let project = home.path("work/app");
    home.skill("work/app/.claude/skills/group/nested", "nested", "Nested");
    let elsewhere = home.skill("store/linked", "linked", "Linked");
    std::fs::create_dir_all(project.join(".agents/skills/linked")).unwrap();
    home.link("work/app/.agents/skills/linked/SKILL.md", &elsewhere);
    // The same name personally and in the project: Gemini CLI lets the project win.
    home.skill(".gemini/skills/style", "style", "Personal");
    home.skill("work/app/.gemini/skills/style", "style", "Project");
    let inv = scan(&home.env(), &all, Some(&project));

    assert_eq!(state(&inv, "nested", "claude-code"), None);
    assert_eq!(state(&inv, "nested", "cursor"), Some(State::On));
    assert_eq!(state(&inv, "linked", "codex"), None);
    let winner = |agent: &str| {
        inv.skills
            .iter()
            .find(|s| {
                s.name == "style" && s.states.get(agent).is_some_and(|st| st.state == State::On)
            })
            .map(|s| s.description.clone())
    };
    assert_eq!(winner("gemini-cli").as_deref(), Some("Project"));
}

#[test]
fn a_rules_folder_that_links_back_on_itself_is_read_once() {
    let home = Home::new();
    home.write(".continue/rules/style.md", "Style\n");
    home.link(".continue/rules/again", &home.path(".continue/rules"));
    home.link(".continue/rules/home", &home.root);
    let inv = scan(&home.env(), &only(&["continue"]), None);
    let continue_rules: Vec<&str> = inv
        .rules
        .iter()
        .filter(|r| r.agents.contains_key("continue"))
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(continue_rules, vec!["~/.continue/rules/style.md"]);
}

#[test]
fn imports_resolve_next_to_the_link_or_its_target_per_client() {
    let home = Home::new();
    home.write(".agents/AGENTS.md", "@x.md\n");
    home.write(".agents/x.md", "Next to the target\n");
    home.write(".qwen/x.md", "Next to the link\n");
    home.link(".qwen/QWEN.md", &home.path(".agents/AGENTS.md"));
    home.link(".claude/CLAUDE.md", &home.path(".agents/AGENTS.md"));
    let inv = scan(&home.env(), &only(&["claude-code", "qwen-code"]), None);
    let readers = |path: &str| -> Vec<String> {
        inv.rules
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.agents.keys().cloned().collect())
            .unwrap_or_default()
    };
    assert_eq!(readers("~/.agents/x.md"), vec!["claude-code".to_string()]);
    assert_eq!(readers("~/.qwen/x.md"), vec!["qwen-code".to_string()]);
}

#[test]
fn goose_skips_a_hints_file_that_links_outside_its_folder() {
    let home = Home::new();
    home.write("store/rules.md", "Shared\n");
    home.link(".config/goose/AGENTS.md", &home.path("store/rules.md"));
    home.write(".config/goose/.goosehints", "Own\n");
    let inv = scan(&home.env(), &only(&["goose"]), None);
    assert!(inv
        .rules
        .iter()
        .any(|r| r.path == "~/.config/goose/.goosehints"));
    assert!(!inv
        .rules
        .iter()
        .any(|r| r.agents.contains_key("goose") && r.path.contains("store/rules.md")));
}

#[test]
fn gemini_reads_its_configured_file_names_in_a_project_too() {
    let home = Home::new();
    let project = home.path("work/app");
    home.write(
        ".gemini/settings.json",
        r#"{"context": {"fileName": "AGENTS.md"}}"#,
    );
    home.write("work/app/AGENTS.md", "Project\n");
    home.write("work/app/GEMINI.md", "Not read\n");
    let inv = scan(&home.env(), &only(&["gemini-cli"]), Some(&project));
    let gemini = |name: &str| {
        inv.rules
            .iter()
            .any(|r| r.name == name && r.agents.contains_key("gemini-cli"))
    };
    assert!(gemini("AGENTS.md"));
    assert!(!gemini("GEMINI.md"));
}

#[test]
fn clients_without_a_rules_file_yet_still_offer_where_one_goes() {
    let home = Home::new();
    // Another client has rules, so the ones without stand out.
    home.write(".codex/AGENTS.md", "# Codex\n");
    let inv = scan(
        &home.env(),
        &only(&["codex", "grok", "pi", "antigravity"]),
        None,
    );
    let locations = |id: &str| {
        inv.agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.rule_locations.clone())
            .unwrap_or_default()
    };
    assert!(locations("grok").contains(&"~/.grok/AGENTS.md".to_string()));
    assert!(locations("pi").contains(&"~/.pi/agent/AGENTS.md".to_string()));
    assert!(locations("antigravity").contains(&"~/.gemini/config/AGENTS.md".to_string()));
    let missing = inv.issues.iter().find(|i| i.id == "rules-missing").unwrap();
    assert!(missing.agents.contains(&"grok".to_string()));
    assert!(missing.agents.contains(&"pi".to_string()));
}

#[test]
fn openclaw_symlink_boundaries_apply_before_skill_precedence() {
    let home = Home::new();
    let escaped = home.skill("outside/style", "style", "Escaped workspace skill");
    let managed = home.skill(".openclaw/skills/style", "style", "Managed winner");
    home.link(
        ".openclaw/workspace/skills/style",
        escaped.parent().unwrap(),
    );
    let personal = home.skill("other/personal", "personal", "Personal folder link");
    home.link(".agents/skills/personal", personal.parent().unwrap());
    let foreign = home.write(
        "outside/file.md",
        "---\nname: foreign\ndescription: External file\n---\n",
    );
    home.link(".openclaw/skills/foreign/SKILL.md", &foreign);
    let hidden = home.skill(
        ".openclaw/workspace/skills/.hidden/leaf",
        "hidden",
        "Only reachable through an untrusted group",
    );
    home.link("group-outside/leaf", hidden.parent().unwrap());
    home.link(
        ".openclaw/workspace/skills/group",
        &home.path("group-outside"),
    );
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    assert!(inv.skills.iter().all(|skill| skill.name != "hidden"));
    assert_eq!(skill(&inv, "style").key, managed.display().to_string());
    assert_eq!(state(&inv, "style", "openclaw"), Some(State::On));
    assert_eq!(state(&inv, "personal", "openclaw"), Some(State::On));
    assert!(inv.skills.iter().all(|skill| skill.name != "foreign"));
    home.write(
        ".openclaw/openclaw.json",
        "{ skills: { load: { allowSymlinkTargets: [' ~/outside '] } } }\n",
    );
    let inv = scan(&home.env(), &only(&["openclaw"]), None);
    let winner = inv
        .skills
        .iter()
        .find(|skill| skill.name == "style" && skill.states["openclaw"].state == State::On)
        .unwrap();
    assert_eq!(winner.key, escaped.display().to_string());
    assert!(inv.skills.iter().all(|skill| skill.name != "foreign"));
}

#[test]
fn openclaw_relative_workspace_does_not_report_fallback_skills_or_rules() {
    let home = Home::new();
    home.skill(
        ".openclaw/workspace/skills/demo",
        "demo",
        "Not the selected workspace",
    );
    home.write(
        ".openclaw/workspace/AGENTS.md",
        "Not the selected workspace\n",
    );
    let mut env = home.env();
    env.openclaw = crate::clients::openclaw::dirs(&home.root, &|key| {
        (key == "OPENCLAW_WORKSPACE_DIR").then(|| "relative".into())
    });
    let inv = scan(&env, &only(&["openclaw"]), None);
    assert!(inv.skills.is_empty());
    assert!(inv.rules.is_empty());
}

#[test]
fn openhands_matches_released_skill_formats_and_keeps_plain_files_safe() {
    let home = Home::new();
    let direct = home.skill(".openhands/skills/direct", "direct", "Immediate AgentSkill");
    std::fs::rename(&direct, direct.with_file_name("skill.md")).unwrap();
    home.skill(
        ".openhands/skills/group/nested",
        "nested",
        "Not an immediate child",
    );
    home.write(
        ".openhands/skills/topic/review.md",
        "---\ntriggers: [review]\n---\nReview changes.\n",
    );
    home.write(
        ".openhands/skills/task.md",
        "---\nname: task\ninputs: []\n---\nDo task.\n",
    );
    let rules = home.write(
        ".openhands/microagents/toolport-team-rules.md",
        "Always follow team rules.\n",
    );
    home.write(
        ".openhands/skills/direct/details.md",
        "---\nname: excluded\ntriggers: [details]\n---\nSupporting file.\n",
    );
    home.write(".openhands/skills/README.md", "Not a skill.\n");
    let inv = scan(&home.env(), &only(&["openhands"]), None);
    assert_eq!(state(&inv, "direct", "openhands"), Some(State::On));
    assert_eq!(state(&inv, "topic/review", "openhands"), Some(State::On));
    assert_eq!(state(&inv, "task", "openhands"), Some(State::On));
    assert_eq!(inv.skills.len(), 3);
    assert!(!skill(&inv, "topic/review").editable);
    assert!(!skill(&inv, "topic/review").removable);
    assert!(inv
        .rules
        .iter()
        .any(|rule| rule.path == home.env().display(&rules)));
    assert!(
        !inv.agents
            .iter()
            .find(|agent| agent.id == "openhands")
            .unwrap()
            .can_toggle
    );
}

#[test]
fn openhands_installed_metadata_and_lower_priority_are_respected() {
    let home = Home::new();
    home.skill(
        ".openhands/skills/installed/active-skill",
        "active-skill",
        "Installed",
    );
    home.skill(
        ".openhands/skills/installed/disabled-skill",
        "disabled-skill",
        "Installed off",
    );
    let duplicate = home.skill(
        ".openhands/skills/installed/duplicate",
        "duplicate",
        "Installed lower priority",
    );
    home.skill(".openhands/microagents/duplicate", "duplicate", "User wins");
    // SDK auto-discovers valid copied directories without a metadata record.
    home.skill(
        ".openhands/skills/installed/copied-skill",
        "copied-skill",
        "Copied locally",
    );
    // Ordinary Markdown loads independently even inside a disabled installed skill.
    home.write(
        ".openhands/skills/installed/disabled-skill/notes.md",
        "---\nname: plain-notes\ntriggers: [notes]\n---\nNotes.\n",
    );
    home.write(
        ".openhands/skills/installed/.installed.json",
        r#"{
        "skills":{"active-skill":{"enabled":false},"disabled-skill":{"enabled":false}},
        "extensions":{"active-skill":{"enabled":true}}
    }"#,
    );
    let inv = scan(&home.env(), &only(&["openhands"]), None);
    assert_eq!(state(&inv, "active-skill", "openhands"), Some(State::On));
    assert_eq!(state(&inv, "disabled-skill", "openhands"), Some(State::Off));
    assert_eq!(state(&inv, "copied-skill", "openhands"), Some(State::On));
    assert_eq!(state(&inv, "plain-notes", "openhands"), Some(State::On));
    assert!(skill(&inv, "disabled-skill").states["openhands"]
        .via
        .contains(".installed.json"));
    let installed = inv
        .skills
        .iter()
        .find(|skill| skill.key == duplicate.display().to_string())
        .unwrap();
    assert_eq!(installed.states["openhands"].state, State::Shadowed);
    assert!(!inv.agents[0].can_toggle);
}

#[test]
fn openhands_markdown_walk_bounds_depth_and_follows_cycles_once() {
    let home = Home::new();
    let root = home.path(".openhands/skills");
    home.write(
        ".openhands/skills/topic/visible.md",
        "---\ntriggers: [visible]\n---\nVisible.\n",
    );
    home.link(".openhands/skills/topic/cycle", &root);
    let deep = format!(
        ".openhands/skills/{}/hidden.md",
        ["deep"; MAX_DEPTH + 1].join("/")
    );
    home.write(&deep, "---\ntriggers: [hidden]\n---\nToo deep.\n");
    let found = walk_openhands_root(&Root::flat(root, "OpenHands skills"));
    assert_eq!(found.len(), 1);
    assert!(found[0].link.ends_with("visible.md"));
}

#[test]
fn openhands_markdown_walk_skips_linked_folders_python_rglob_skips() {
    let home = Home::new();
    let root = home.path(".openhands/skills");
    home.write(
        ".openhands/skills/real/sub/real.md",
        "---\ntriggers: [real]\n---\nReal.\n",
    );
    home.write(
        "shared/plain/linked.md",
        "---\ntriggers: [linked]\n---\nLinked.\n",
    );
    home.write("shared/agent/SKILL.md", "---\nname: agent\n---\nAgent.\n");
    home.link(".openhands/skills/plain", &home.path("shared/plain"));
    home.link(".openhands/skills/real/nested", &home.path("shared/plain"));
    home.link(".openhands/skills/agent", &home.path("shared/agent"));
    let found = walk_openhands_root(&Root::flat(root, "OpenHands skills"));
    let mut names: Vec<_> = found
        .iter()
        .map(|found| {
            found
                .link
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    // A top-level linked AgentSkills folder still loads; linked Markdown doesn't.
    assert_eq!(names, ["SKILL.md", "real.md"]);
}

#[test]
fn openhands_markdown_walk_still_reads_a_real_folder_after_a_link_to_it() {
    let home = Home::new();
    let root = home.path(".openhands/skills");
    home.write(
        ".openhands/skills/z/sub/nested.md",
        "---\ntriggers: [nested]\n---\nNested.\n",
    );
    home.link(".openhands/skills/a", &home.path(".openhands/skills/z"));
    let found = walk_openhands_root(&Root::flat(root, "OpenHands skills"));
    assert_eq!(found.len(), 1);
    assert!(found[0].link.ends_with("z/sub/nested.md"));
}

#[test]
fn openhands_markdown_walk_respects_its_entry_budget() {
    let home = Home::new();
    for name in ["a", "b", "c"] {
        home.write(
            &format!(".openhands/skills/{name}.md"),
            "---\ntriggers: [test]\n---\nTest.\n",
        );
    }
    let root = Root::flat(home.path(".openhands/skills"), "OpenHands skills");
    assert_eq!(walk_openhands_root_budget(&root, 2).len(), 2);
    assert!(walk_openhands_root_budget(&root, 0).is_empty());
}

#[test]
fn openhands_project_search_uses_local_then_nearest_git_root() {
    let home = Home::new();
    // A .git file, as used by Git worktrees, also identifies a root.
    home.write("repo/.git", "gitdir: /unused/worktree\n");
    let root_only = home.skill("repo/.agents/skills/root-only", "root-only", "Root skill");
    let root_duplicate = home.skill("repo/.agents/skills/shared", "shared", "Root loses");
    let local_duplicate = home.skill(
        "repo/sub/work/.openhands/skills/shared",
        "shared",
        "Local wins",
    );
    let root_agents = home.write("repo/AGENTS.md", "Root agents\n");
    let local_agents = home.write("repo/sub/work/aGeNt.md", "Local agents\n");
    let claude = home.write("repo/ClAuDe.Md", "Root Claude\n");
    let gemini = home.write("repo/sub/work/gEmInI.mD", "Local Gemini\n");
    let cursor = home.write("repo/.CuRsOrRuLeS", "Root Cursor\n");
    // Canonical aliases never create a second rules row.
    home.link("repo/sub/work/CLAUDE.md", &local_agents);
    home.write(
        "repo/sub/AGENTS.md",
        "Intermediate directory is not searched\n",
    );
    home.skill(
        "repo/sub/.agents/skills/intermediate",
        "intermediate",
        "Not searched",
    );
    let project = home.path("repo/sub/work");
    let inv = scan(&home.env(), &only(&["openhands"]), Some(&project));
    assert_eq!(state(&inv, "root-only", "openhands"), Some(State::On));
    let by_path = |path: &Path| {
        inv.skills
            .iter()
            .find(|skill| skill.key == path.display().to_string())
            .unwrap()
            .states["openhands"]
            .state
    };
    assert_eq!(by_path(&root_only), State::On);
    assert_eq!(by_path(&root_duplicate), State::Shadowed);
    assert_eq!(by_path(&local_duplicate), State::On);
    assert!(!inv.skills.iter().any(|skill| skill.name == "intermediate"));
    let paths: BTreeSet<_> = inv.rules.iter().map(|rule| rule.path.clone()).collect();
    for path in [&local_agents, &claude, &gemini, &cursor] {
        assert!(
            paths.contains(&home.env().display(path)),
            "{}",
            path.display()
        );
    }
    assert!(!paths.contains(&home.env().display(&root_agents)));
    assert_eq!(paths.len(), 4);
    assert_eq!(
        openhands_project_dirs(&project),
        vec![project, home.path("repo")]
    );
}

#[test]
fn openhands_project_search_stops_at_nearest_git_root_and_deduplicates_aliases() {
    let home = Home::new();
    home.write("repo/.git", "outer\n");
    home.write("repo/nested/.git", "inner\n");
    home.write("repo/nested/work/AGENTS.md", "Local\n");
    let project = home.path("repo/nested/work");
    assert_eq!(
        openhands_project_dirs(&project),
        vec![project.clone(), home.path("repo/nested")]
    );
    home.link("alias", &project);
    assert_eq!(
        openhands_project_dirs(&home.path("alias")),
        openhands_project_dirs(&project)
    );
    assert_eq!(
        openhands_project_dirs(&home.path("repo/nested")),
        vec![home.path("repo/nested")]
    );
    assert_eq!(openhands_project_dirs(&home.root), vec![home.root.clone()]);
}

#[test]
fn openhands_global_rules_follow_name_precedence_and_canonical_dedup() {
    let home = Home::new();
    let shared = home.write(".agents/skills/team.md", "Shared rules win.\n");
    let explicit = home.write(
        ".openhands/skills/renamed.md",
        "---\nname: explicit\n---\nExplicit name wins.\n",
    );
    home.write(
        ".openhands/microagents/team.md",
        "Legacy team rules lose.\n",
    );
    home.write(
        ".openhands/microagents/other.md",
        "---\nname: explicit\n---\nDuplicate explicit name loses.\n",
    );
    home.write(
        ".agents/skills/triggered.md",
        "---\ntriggers: [review]\n---\nHigher-priority skill.\n",
    );
    home.write(
        ".openhands/microagents/triggered.md",
        "Lower-priority rule loses to skill.\n",
    );
    home.link(".openhands/microagents/alias.md", &shared);
    let inv = scan(&home.env(), &only(&["openhands"]), None);
    let paths: BTreeSet<_> = inv.rules.iter().map(|rule| rule.path.clone()).collect();
    assert_eq!(
        paths,
        BTreeSet::from([home.env().display(&shared), home.env().display(&explicit)])
    );
    assert_eq!(state(&inv, "triggered", "openhands"), Some(State::On));
}

#[test]
fn nanobot_uses_workspace_folder_names_rules_and_native_toggle() {
    let home = Home::new();
    home.write(".nanobot/config.json", r#"{"agents":{"defaults":{"workspace":"~/robot","disabledSkills":["orca"]}},"tools":{"mcpServers":{"native":{"command":"node"}}}}"#);
    home.skill("robot/skills/orca", "different-frontmatter-name", "Orca");
    home.skill("robot/skills/nested/deeper", "hidden", "Not loaded");
    home.write("robot/AGENTS.md", "Team rules");
    home.write("robot/SOUL.md", "Personality");
    home.skill(
        ".agents/skills/unrelated",
        "unrelated",
        "Not loaded by nanobot",
    );
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(
        state(&inv, "different-frontmatter-name", "nanobot"),
        Some(State::Off)
    );
    assert!(inv
        .skills
        .iter()
        .all(|s| s.name != "hidden" && s.name != "unrelated"));
    let key = skill(&inv, "different-frontmatter-name").key.clone();
    let writer = TestWriter::default();
    set_state(&env, &inv, "nanobot", &key, Wanted::On, &writer).unwrap();
    let after = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(
        state(&after, "different-frontmatter-name", "nanobot"),
        Some(State::On)
    );
    set_state(&env, &after, "nanobot", &key, Wanted::Off, &writer).unwrap();
    let root: serde_json::Value = serde_json::from_str(&home.read(".nanobot/config.json")).unwrap();
    assert_eq!(
        root["agents"]["defaults"]["disabledSkills"],
        serde_json::json!(["orca"])
    );
    assert_eq!(root["tools"]["mcpServers"]["native"]["command"], "node");
}

#[test]
fn nanobot_reads_snake_case_off_list_and_refuses_to_shadow_it() {
    let home = Home::new();
    let original =
        r#"{"tools":{"mcp_servers":{}},"agents":{"defaults":{"disabled_skills":["orca"]}}}"#;
    home.write(".nanobot/config.json", original);
    home.skill(".nanobot/workspace/skills/orca", "orca", "Orca");
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state(&inv, "orca", "nanobot"), Some(State::Off));
    let key = skill(&inv, "orca").key.clone();
    assert!(set_state(
        &env,
        &inv,
        "nanobot",
        &key,
        Wanted::On,
        &TestWriter::default()
    )
    .is_err());
    assert_eq!(home.read(".nanobot/config.json"), original);
}

#[test]
fn nanobot_skill_writes_refuse_non_json_or_duplicate_keys() {
    let home = Home::new();
    let path = home.path(".nanobot/config.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for original in [
        "{agents:{defaults:{}}}",
        "[]",
        r#"{"agents":{},"agents":{}}"#,
        r#"{"tools":{"mcp_servers":{},"mcp_servers":{}}}"#,
    ] {
        std::fs::write(&path, original).unwrap();
        assert!(edit_json("nanobot", &path, &TestWriter::default(), |root| {
            toggle_list(
                root,
                &["agents", "defaults", "disabledSkills"],
                "orca",
                true,
            )?;
            Ok("agents")
        })
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}

#[test]
fn nanobot_skill_toggle_preserves_snake_case_mcp_text_and_workspace_rules() {
    let home = Home::new();
    let mcp = r#""tools" : { "mcp_servers" : { "native": { "command" : "node", "args" : [] } } }"#;
    home.write(
        ".nanobot/config.json",
        &format!(
            "{{\n  {mcp},\n  \"agents\": {{\"defaults\": {{\"workspace\": \"~/robot\"}}}}\n}}\n"
        ),
    );
    home.skill("robot/skills/orca", "orca", "Orca");
    let rules = home.write("robot/AGENTS.md", "Team rules.\n");
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state(&inv, "orca", "nanobot"), Some(State::On));
    assert!(inv
        .rules
        .iter()
        .any(|rule| rule.path == env.display(&rules)));
    let key = skill(&inv, "orca").key.clone();
    let writer = TestWriter::default();
    set_state(&env, &inv, "nanobot", &key, Wanted::Off, &writer).unwrap();
    let text = home.read(".nanobot/config.json");
    assert!(text.contains(mcp), "{text}");
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        root["agents"]["defaults"]["disabledSkills"],
        serde_json::json!(["orca"])
    );
    let after = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state(&after, "orca", "nanobot"), Some(State::Off));
    set_state(&env, &after, "nanobot", &key, Wanted::On, &writer).unwrap();
    assert!(home.read(".nanobot/config.json").contains(mcp));
}

#[test]
fn nanobot_distinct_folders_with_the_same_frontmatter_name_load_and_toggle_separately() {
    let home = Home::new();
    let a = home.skill(".nanobot/workspace/skills/a", "shared", "First");
    let b = home.skill(".nanobot/workspace/skills/b", "shared", "Second");
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    let state_at = |inv: &Inventory, path: &Path| {
        inv.skills
            .iter()
            .find(|skill| skill.key == path.display().to_string())
            .unwrap()
            .states["nanobot"]
            .state
    };
    assert_eq!(state_at(&inv, &a), State::On);
    assert_eq!(state_at(&inv, &b), State::On);
    let key = a.display().to_string();
    set_state(
        &env,
        &inv,
        "nanobot",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    let after = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state_at(&after, &a), State::Off);
    assert_eq!(state_at(&after, &b), State::On);
    let root: serde_json::Value = serde_json::from_str(&home.read(".nanobot/config.json")).unwrap();
    assert_eq!(
        root["agents"]["defaults"]["disabledSkills"],
        serde_json::json!(["a"])
    );
}

fn nanobot_package(
    home: &Home,
    folder: &str,
    identity: &str,
    names: &[&str],
    enabled: bool,
) -> (PathBuf, PathBuf) {
    home.write(".nanobot/config.json", "{}");
    let rel = format!(".nanobot/workspace/plugins/{folder}");
    home.write(&format!("{rel}/plugin.json"), &serde_json::json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":identity}).to_string());
    for name in names {
        home.skill(&format!("{rel}/skills/{name}"), name, "Package skill");
    }
    let root = home.path(&rel);
    let workspace = home.path(".nanobot/workspace");
    let hash = crate::registry::sha256_hex(workspace.to_str().unwrap());
    let marker_rel = format!(".nanobot/plugin-data/{}/{identity}/enabled", &hash[..12]);
    if enabled {
        home.write(&marker_rel, &nanobot_plugins::activation(&root).unwrap());
    }
    (root, home.path(&marker_rel))
}

#[test]
fn nanobot_enabled_packages_follow_precedence_plain_toggles_and_protection() {
    let home = Home::new();
    let (first, first_marker) =
        nanobot_package(&home, "a-first", "first", &["same", "package-only"], true);
    let (second, _) = nanobot_package(&home, "b-second", "second", &["same"], true);
    let workspace = home.skill(".nanobot/workspace/skills/same", "same", "Workspace wins");
    let before = std::fs::read(&first_marker).unwrap();
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    let state_at = |inv: &Inventory, path: &Path| {
        inv.skills
            .iter()
            .find(|s| s.key == path.display().to_string())
            .unwrap()
            .states["nanobot"]
            .state
    };
    assert_eq!(state_at(&inv, &workspace), State::On);
    assert_eq!(
        state_at(&inv, &first.join("skills/same/SKILL.md")),
        State::Shadowed
    );
    assert_eq!(
        state_at(&inv, &second.join("skills/same/SKILL.md")),
        State::Shadowed
    );
    let package = skill(&inv, "package-only");
    assert_eq!(package.source, Source::BuiltIn);
    assert!(!package.editable && !package.removable && !removable(package));
    let key = package.key.clone();
    let writer = TestWriter::default();
    assert!(remove_skill_files(&env, &inv, &key, &writer).is_err());
    set_state(&env, &inv, "nanobot", &key, Wanted::Off, &writer).unwrap();
    let after = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state(&after, "package-only", "nanobot"), Some(State::Off));
    let root: serde_json::Value = serde_json::from_str(&home.read(".nanobot/config.json")).unwrap();
    assert_eq!(
        root["agents"]["defaults"]["disabledSkills"],
        serde_json::json!(["package-only"])
    );
    assert_eq!(std::fs::read(&first_marker).unwrap(), before);
    std::fs::remove_file(workspace).unwrap();
    let next = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(
        state_at(&next, &first.join("skills/same/SKILL.md")),
        State::On
    );
    assert_eq!(
        state_at(&next, &second.join("skills/same/SKILL.md")),
        State::Shadowed
    );
}

#[test]
fn nanobot_package_activation_rejects_stale_disabled_and_duplicate_identities_without_writes() {
    let home = Home::new();
    let (stale, stale_marker) = nanobot_package(&home, "stale", "stale", &["stale-skill"], true);
    let stale_bytes = std::fs::read(&stale_marker).unwrap();
    std::fs::write(stale.join(".hidden-change"), "changed").unwrap();
    nanobot_package(&home, "disabled", "disabled", &["disabled-skill"], false);
    let (_, duplicate_marker) =
        nanobot_package(&home, "duplicate-a", "duplicate", &["dupe-a"], true);
    nanobot_package(&home, "duplicate-b", "duplicate", &["dupe-b"], false);
    let duplicate_bytes = std::fs::read(&duplicate_marker).unwrap();
    let (legacy, legacy_marker) =
        nanobot_package(&home, "legacy", "legacy", &["legacy-skill"], true);
    std::fs::write(&legacy_marker, legacy.to_str().unwrap()).unwrap();
    let inv = scan(&home.env(), &only(&["nanobot"]), None);
    assert_eq!(inv.skills.len(), 1);
    assert_eq!(state(&inv, "legacy-skill", "nanobot"), Some(State::On));
    assert_eq!(std::fs::read(stale_marker).unwrap(), stale_bytes);
    assert_eq!(std::fs::read(duplicate_marker).unwrap(), duplicate_bytes);
    assert_eq!(
        std::fs::read_to_string(legacy_marker).unwrap(),
        legacy.to_str().unwrap()
    );
}

#[test]
fn nanobot_packages_reject_escaping_paths_invalid_metadata_and_wrong_activation_bytes() {
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "valid", "valid", &["valid-skill"], true);
    let activation = std::fs::read_to_string(&marker).unwrap();
    std::fs::write(&marker, format!("{activation}\n")).unwrap();
    assert!(nanobot_plugins::discover(&home.root).is_empty());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("{activation}\n")
    );
    let wrong_root = serde_json::json!({"fingerprint":nanobot_plugins::fingerprint(&root).unwrap(),"root":home.path("elsewhere")}).to_string();
    std::fs::write(&marker, &wrong_root).unwrap();
    assert!(nanobot_plugins::discover(&home.root).is_empty());
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), wrong_root);
    home.skill(
        ".nanobot/workspace/plugins/valid/skills/mismatch",
        "other",
        "Wrong identity",
    );
    home.skill(
        ".nanobot/workspace/plugins/valid/skills/bad--name",
        "bad--name",
        "Invalid name",
    );
    let outside = home.skill("outside/escape", "escape", "Outside package");
    home.link(
        ".nanobot/workspace/plugins/valid/skills/escape",
        outside.parent().unwrap(),
    );
    std::fs::write(&marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
    let inv = scan(&home.env(), &only(&["nanobot"]), None);
    assert_eq!(inv.skills.len(), 1);
    assert_eq!(state(&inv, "valid-skill", "nanobot"), Some(State::On));
    std::fs::remove_dir_all(home.path(".nanobot/plugin-data")).unwrap();
    let workspace = home.path(".nanobot/workspace");
    let hash = crate::registry::sha256_hex(workspace.to_str().unwrap());
    home.write(
        &format!("outside-marker/{}/valid/enabled", &hash[..12]),
        &nanobot_plugins::activation(&root).unwrap(),
    );
    home.link(".nanobot/plugin-data", &home.path("outside-marker"));
    assert!(nanobot_plugins::discover(&home.root).is_empty());
}

#[test]
fn nanobot_package_manifests_require_valid_schema_and_identity_even_when_activated() {
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "package", "valid", &["valid-skill"], true);
    for manifest in ["[]".to_string(), "not JSON".to_string(), serde_json::json!({"$schema":"https://invalid.example/schema","name":"valid"}).to_string(), serde_json::json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"bad..identity"}).to_string()] {
        std::fs::write(root.join("plugin.json"), manifest).unwrap();
        let activation = nanobot_plugins::activation(&root).unwrap();
        std::fs::write(&marker, &activation).unwrap();
        assert!(nanobot_plugins::discover(&home.root).is_empty());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), activation);
    }
}

#[test]
fn nanobot_package_discovery_applies_pyyaml_string_metadata_semantics() {
    let home = Home::new();
    let (root, marker) = nanobot_package(
        &home,
        "typed",
        "typed",
        &[
            "yes",
            "bad-description",
            "quoted",
            "tagged",
            "block",
            "alias",
            "merge",
        ],
        true,
    );
    for (name, metadata) in [
        (
            "bad-description",
            "name: bad-description\ndescription: yes\n",
        ),
        ("quoted", "name: 'quoted'\ndescription: 'yes'\n"),
        ("tagged", "name: !!str tagged\ndescription: !!str 012\n"),
        ("block", "name: block\ndescription: |-\n  2026-10-18\n"),
        (
            "alias",
            "text: &text 'yes'\nname: alias\ndescription: *text\n",
        ),
        (
            "merge",
            "base: &base {name: merge, description: 'yes'}\n<<: *base\n",
        ),
    ] {
        std::fs::write(
            root.join(format!("skills/{name}/SKILL.md")),
            format!("---\n{metadata}---\nBody\n"),
        )
        .unwrap();
    }
    std::fs::write(marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
    let inv = scan(&home.env(), &only(&["nanobot"]), None);
    let names: BTreeSet<_> = inv.skills.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["quoted", "tagged", "block", "alias", "merge"])
    );
    for name in ["quoted", "alias", "merge"] {
        assert_eq!(skill(&inv, name).description, "yes", "{name}");
    }
    assert_eq!(skill(&inv, "tagged").description, "012");
    assert_eq!(skill(&inv, "block").description, "2026-10-18");
}

#[test]
fn openclaw_personal_skills_use_os_home_with_relocated_openclaw_home() {
    let home = Home::new();
    let personal = home.skill("library/personal", "personal", "OS home");
    home.link(".agents/skills/personal", personal.parent().unwrap());
    home.skill(
        "relocated/.agents/skills/unrelated",
        "unrelated",
        "Wrong home",
    );
    let mut env = home.env();
    env.openclaw = crate::clients::openclaw::dirs(&home.root, &|key| {
        (key == "OPENCLAW_HOME").then(|| home.path("relocated").to_string_lossy().into_owned())
    });
    let inv = scan(&env, &only(&["openclaw"]), None);
    assert_eq!(state(&inv, "personal", "openclaw"), Some(State::On));
    assert!(!inv
        .skills
        .iter()
        .any(|s| s.name == "unrelated" && s.states.contains_key("openclaw")));
}

#[test]
fn openclaw_later_extra_directory_wins_and_toggles_its_key() {
    let home = Home::new();
    for (dir, key) in [("extra-z", "copy-a"), ("extra-a", "copy-b")] {
        home.write(&format!("{dir}/demo/SKILL.md"), &format!("---\nname: demo\ndescription: {key}\nmetadata:\n  openclaw:\n    skillKey: {key}\n---\n"));
    }
    home.write(".openclaw/openclaw.json", &serde_json::json!({"skills":{"load":{"extraDirs":[home.path("extra-z"), home.path("extra-a")]}}}).to_string());
    let env = home.env();
    let inv = scan(&env, &only(&["openclaw"]), None);
    let winner = inv
        .skills
        .iter()
        .find(|s| s.name == "demo" && s.states["openclaw"].state == State::On)
        .unwrap();
    assert_eq!(winner.description, "copy-b");
    set_state(
        &env,
        &inv,
        "openclaw",
        &winner.key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    let config = read_json(&home.path(".openclaw/openclaw.json")).unwrap();
    assert_eq!(config["skills"]["entries"]["copy-b"]["enabled"], false);
    assert!(config["skills"]["entries"].get("copy-a").is_none());
}

#[test]
fn openclaw_installed_skill_origin_key_controls_inventory_and_toggle() {
    let home = Home::new();
    home.skill(".openclaw/skills/installed", "Display Demo", "Installed");
    home.write(
        ".openclaw/skills/installed/.openclaw/source-origin.json",
        r#"{"slug":" catalog-demo "}"#,
    );
    home.write(
        ".openclaw/openclaw.json",
        r#"{"skills":{"entries":{"catalog-demo":{"enabled":false}}}}"#,
    );
    let env = home.env();
    let inv = scan(&env, &only(&["openclaw"]), None);
    assert_eq!(state(&inv, "Display Demo", "openclaw"), Some(State::Off));
    let key = skill(&inv, "Display Demo").key.clone();
    set_state(
        &env,
        &inv,
        "openclaw",
        &key,
        Wanted::On,
        &TestWriter::default(),
    )
    .unwrap();
    let inv = scan(&env, &only(&["openclaw"]), None);
    set_state(
        &env,
        &inv,
        "openclaw",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    let config = read_json(&home.path(".openclaw/openclaw.json")).unwrap();
    assert_eq!(
        config["skills"]["entries"]["catalog-demo"]["enabled"],
        false
    );
    assert!(config["skills"]["entries"].get("Display Demo").is_none());
}

#[test]
fn openclaw_origin_key_falls_back_safely_and_manifest_wins() {
    let home = Home::new();
    let file = home.skill(".openclaw/skills/demo", "Demo", "Demo");
    let origin = ".openclaw/skills/demo/.openclaw/source-origin.json";
    for content in [
        r#"{"slug":""}"#,
        r#"{"slug":42}"#,
        "broken",
        &" ".repeat(16_385),
    ] {
        home.write(origin, content);
        assert_eq!(openclaw_skill_key(&file), None);
    }
    home.write(origin, r#"{"slug":"catalog-demo"}"#);
    assert_eq!(openclaw_skill_key(&file).as_deref(), Some("catalog-demo"));
    home.write(
        ".openclaw/skills/demo/SKILL.md",
        "---\nname: Demo\nmetadata: {openclaw: {skillKey: explicit}}\n---\n",
    );
    assert_eq!(openclaw_skill_key(&file).as_deref(), Some("explicit"));
}

#[cfg(unix)]
#[test]
fn openclaw_origin_metadata_rejects_directory_and_file_escapes() {
    let home = Home::new();
    let file = home.skill(".openclaw/skills/demo", "Demo", "Demo");
    home.write("outside/source-origin.json", r#"{"slug":"escaped"}"#);
    home.link(".openclaw/skills/demo/.openclaw", &home.path("outside"));
    assert_eq!(openclaw_skill_key(&file), None);
    // An escaped metadata directory is still refused when its file links back in.
    home.write(
        ".openclaw/skills/demo/origin.json",
        r#"{"slug":"back-inside"}"#,
    );
    std::fs::remove_file(home.path("outside/source-origin.json")).unwrap();
    home.link(
        "outside/source-origin.json",
        &home.path(".openclaw/skills/demo/origin.json"),
    );
    assert_eq!(openclaw_skill_key(&file), None);
    std::fs::remove_file(home.path(".openclaw/skills/demo/.openclaw")).unwrap();
    std::fs::remove_file(home.path("outside/source-origin.json")).unwrap();
    home.write("outside/source-origin.json", r#"{"slug":"escaped"}"#);
    home.link(
        ".openclaw/skills/demo/.openclaw/source-origin.json",
        &home.path("outside/source-origin.json"),
    );
    assert_eq!(openclaw_skill_key(&file), None);
}

#[test]
fn nanobot_lists_and_toggles_hidden_workspace_folders() {
    let home = Home::new();
    home.skill(
        ".nanobot/workspace/skills/.hidden",
        ".hidden",
        "Hidden skill",
    );
    let env = home.env();
    let inv = scan(&env, &only(&["nanobot"]), None);
    assert_eq!(state(&inv, ".hidden", "nanobot"), Some(State::On));
    let key = inv
        .skills
        .iter()
        .find(|s| s.name == ".hidden")
        .unwrap()
        .key
        .clone();
    set_state(
        &env,
        &inv,
        "nanobot",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    assert_eq!(
        state(&scan(&env, &only(&["nanobot"]), None), ".hidden", "nanobot"),
        Some(State::Off)
    );
}

#[test]
fn nanobot_package_frontmatter_matches_newlines_closers_and_python_strip() {
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "valid", "valid", &["valid-skill"], true);
    let skill_path = root.join("skills/valid-skill/SKILL.md");
    for newline in ["\n", "\r\n", "\r"] {
        for closer in ["--- # end", "----", "---more"] {
            std::fs::write(&skill_path, format!("---{newline}name: valid-skill{newline}description: ordinary{newline}{closer}{newline}body")).unwrap();
            std::fs::write(&marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
            assert_eq!(
                nanobot_plugins::discover(&home.root).len(),
                1,
                "{newline:?} {closer}"
            );
            let inv = scan(&home.env(), &only(&["nanobot"]), None);
            assert_eq!(skill(&inv, "valid-skill").description, "ordinary");
        }
    }
    for description in [r#""\x1c""#, r#""\x1d\x1e\x1f""#] {
        std::fs::write(
            &skill_path,
            format!("---\nname: valid-skill\ndescription: {description}\n---\n"),
        )
        .unwrap();
        std::fs::write(&marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
        assert!(nanobot_plugins::discover(&home.root).is_empty());
    }
}

#[test]
fn nanobot_package_frontmatter_preserves_final_block_scalar_chomping() {
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "valid", "valid", &["valid-skill"], true);
    for yaml in [
        "name: valid-skill\ndescription: ordinary\nx: ! |\n  yes",
        "description: ordinary\nname: |\n  valid-skill",
    ] {
        home.write(
            ".nanobot/workspace/plugins/valid/skills/valid-skill/SKILL.md",
            &format!("---\n{yaml}\n---\n"),
        );
        std::fs::write(&marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
        let inv = scan(&home.env(), &only(&["nanobot"]), None);
        assert_eq!(
            state(&inv, "valid-skill", "nanobot"),
            Some(State::On),
            "{yaml}"
        );
        assert_eq!(skill(&inv, "valid-skill").description, "ordinary");
    }
}

#[test]
fn nanobot_package_frontmatter_keeps_a_leading_yaml_document_marker() {
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "valid", "valid", &["valid-skill"], true);
    for prefix in [
        "---\n---",
        "---\n\n---",
        "---\n--- # start",
        "---\u{1c}\n---",
    ] {
        home.write(
            ".nanobot/workspace/plugins/valid/skills/valid-skill/SKILL.md",
            &format!("{prefix}\nname: valid-skill\ndescription: ordinary\n---\n"),
        );
        std::fs::write(&marker, nanobot_plugins::activation(&root).unwrap()).unwrap();
        let inv = scan(&home.env(), &only(&["nanobot"]), None);
        assert_eq!(
            state(&inv, "valid-skill", "nanobot"),
            Some(State::On),
            "{prefix:?}"
        );
        assert_eq!(skill(&inv, "valid-skill").description, "ordinary");
    }
}

#[test]
#[cfg(unix)]
fn nanobot_unreadable_subdirectory_preserves_package_discovery() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let (root, marker) = nanobot_package(&home, "valid", "valid", &["valid-skill"], true);
    let locked = root.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o0)).unwrap();
    let unreadable = std::fs::read_dir(&locked).is_err();
    let fingerprint = nanobot_plugins::activation(&root);
    let count = fingerprint.as_ref().map(|activation| {
        std::fs::write(&marker, activation).unwrap();
        nanobot_plugins::discover(&home.root).len()
    });
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    if unreadable {
        assert!(fingerprint.is_some());
        assert_eq!(count, Some(1));
    }
}

#[test]
fn nanobot_hidden_folder_policy_is_part_of_the_shared_walk_cache_key() {
    let home = Home::new();
    home.skill(".claude/skills/.hidden", ".hidden", "Hidden skill");
    home.write(
        ".nanobot/config.json",
        r#"{"agents":{"defaults":{"workspace":"~/.claude"}}}"#,
    );
    let inv = scan(&home.env(), &only(&["claude-code", "nanobot"]), None);
    assert_eq!(state(&inv, ".hidden", "nanobot"), Some(State::On));
    assert_eq!(state(&inv, ".hidden", "claude-code"), None);
}

#[test]
fn codewhale_native_name_identity_table_and_toggle_round_trip() {
    // Generated by Codewhale v0.10.1's unmodified frontmatter reader and
    // extracted native normalization functions, not Toolport's display parser.
    // Keep the original SKILL.md input so the entire extraction path is tested.
    let cases = [
        (
            "---\nname: demo\ndescription: Demo\n---\nBody\n",
            "demo",
            "demo",
            None,
        ),
        (
            "---\nname: 'My Skill!'\ndescription: Demo\n---\nBody\n",
            "My Skill!",
            "my-skill",
            None,
        ),
        (
            "---\nname: '  My  Skill!  '\ndescription: Demo\n---\nBody\n",
            "  My  Skill!  ",
            "my-skill",
            None,
        ),
        (
            "---\nname: 'A\tB'\ndescription: Demo\n---\nBody\n",
            "A\tB",
            "a-b",
            None,
        ),
        (
            "---\nname: 'café'\ndescription: Demo\n---\nBody\n",
            "café",
            "caf-850f7dc43910ff890f8879c0ed26fe69",
            Some("caf"),
        ),
        (
            "---\nname: 'cafè'\ndescription: Demo\n---\nBody\n",
            "cafè",
            "caf-08dcdafde9046804985b9047747bb587",
            Some("caf"),
        ),
        (
            "---\nname: 'café  tools'\ndescription: Demo\n---\nBody\n",
            "café  tools",
            "caf-tools-db13c9f6d1c094b25679bc53d82cc06f",
            Some("caf-tools"),
        ),
        (
            "---\nname: 'café tools'\ndescription: Demo\n---\nBody\n",
            "café tools",
            "caf-tools-1cf98c492b54aba7156ff9c76cd5e80c",
            Some("caf-tools"),
        ),
        (
            "---\nname: 'café\u{a0}tools'\ndescription: Demo\n---\nBody\n",
            "café\u{a0}tools",
            "caf-tools-7462ce7ce2883bf7e27fa44f79385527",
            Some("caf-tools"),
        ),
        (
            "---\nname: '  café  tools  '\ndescription: Demo\n---\nBody\n",
            "  café  tools  ",
            "caf-tools-db13c9f6d1c094b25679bc53d82cc06f",
            Some("caf-tools"),
        ),
        (
            "---\nname: '\tcafé\t tools\t'\ndescription: Demo\n---\nBody\n",
            "\tcafé\t tools\t",
            "caf-tools-5139fc84f8aa395494623ed1863e7077",
            Some("caf-tools"),
        ),
        (
            "---\nname: 'CAFé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "CAFé  TOOLS",
            "caf-tools-db13c9f6d1c094b25679bc53d82cc06f",
            Some("caf-tools"),
        ),
        (
            "---\nname: 'CAFÉ  TOOLS'\ndescription: Demo\n---\nBody\n",
            "CAFÉ  TOOLS",
            "caf-tools-128b84b9bfcf209ab87aaf7eae7dd925",
            Some("caf-tools"),
        ),
        (
            "---\nname: 'ÉCOLE'\ndescription: Demo\n---\nBody\n",
            "ÉCOLE",
            "cole-8bbd461ececbeb7c4d1a7bf723d2328a",
            Some("cole"),
        ),
        (
            "---\nname: 'école'\ndescription: Demo\n---\nBody\n",
            "école",
            "cole-f0f772e182a4941e6fdef31116540b17",
            Some("cole"),
        ),
        (
            "---\nname: 'cafe\u{301}'\ndescription: Demo\n---\nBody\n",
            "cafe\u{301}",
            "cafe-81ef060bcd98adc7824eb5c1ada83c32",
            Some("cafe"),
        ),
        (
            "---\nname: '日本語'\ndescription: Demo\n---\nBody\n",
            "日本語",
            "skill-77710aedc74ecfa33685e33a6c7df5cc",
            Some("skill"),
        ),
        (
            "---\nname: 'My Plugin:café  tools'\ndescription: Demo\n---\nBody\n",
            "My Plugin:café  tools",
            "my-plugin:caf-tools-db13c9f6d1c094b25679bc53d82cc06f",
            Some("my-plugin:caf-tools"),
        ),
        (
            "---\nname: 'one:two:three'\ndescription: Demo\n---\nBody\n",
            "one:two:three",
            "one-two-three",
            None,
        ),
        (
            "---\nname: \"café\\ttools\"\ndescription: Demo\n---\nBody\n",
            "café\\ttools",
            "caf-ttools-aac35a426ff42f938876f495237ae1af",
            Some("caf-ttools"),
        ),
        (
            "---\nname: 'café''tools'\ndescription: Demo\n---\nBody\n",
            "café''tools",
            "caf-tools-1a02a2587f82cf64624a6e70b792320d",
            Some("caf-tools"),
        ),
        (
            "---\nname: >-\n  café  tools\n  Next\ndescription: Demo\n---\nBody\n",
            "café  tools Next",
            "caf-tools-next-691d70cc1bd3b27db720693661602a85",
            Some("caf-tools-next"),
        ),
        (
            "---\nname: |-\n  café  tools\n  Next\ndescription: Demo\n---\nBody\n",
            "café  tools\nNext",
            "caf-tools-next-29421529a5f7a183978b4339df87d1f9",
            Some("caf-tools-next"),
        ),
        (
            "---\nname: \n  - café  tools\n  - Next\ndescription: Demo\n---\nBody\n",
            "café  tools, Next",
            "caf-tools-next-454de398ee51e007627be3e3dfe76331",
            Some("caf-tools-next"),
        ),
        (
            "---\nname: ['café  tools', Next]\ndescription: Demo\n---\nBody\n",
            "café  tools, Next",
            "caf-tools-next-454de398ee51e007627be3e3dfe76331",
            Some("caf-tools-next"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-c5d4490e4995785a54174d57592ee36a",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-tools"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-5e32ce14434cb5eacdf56458077ec65c",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-tools"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-d090ea3816c959e57389d065d9433a47",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-tools"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-669d4519a3681a20948878d8f0f45c26",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-198b13cccfcc5cc21e03306cf8ff2517",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a57e4190d310ea5debc2362918889034",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé  TOOLS",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-b81840e1f161a05ba3567e11f3c918cf",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "---\nname: 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\ndescription: Demo\n---\nBody\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        ),
    ];
    for (content, native_name, current, legacy) in cases {
        assert_eq!(
            codewhale_metadata::name(content).as_deref(),
            Some(native_name),
            "{content:?}"
        );
        assert_eq!(
            codewhale_skill_identity(native_name),
            (current.into(), legacy.map(str::to_owned)),
            "{content:?}"
        );
        let home = Home::new();
        home.write(".codewhale/skills/case/SKILL.md", content);
        home.write(".codewhale/skills_state.toml", "disabled = []\n");
        let env = home.env();
        let writer = TestWriter::default();
        let inv = scan(&env, &only(&["codewhale"]), None);
        let target = inv
            .skills
            .iter()
            .find(|skill| Path::new(&skill.key).ends_with(Path::new("case").join("SKILL.md")))
            .unwrap();
        assert_eq!(
            target.states["codewhale"].switch_name.as_deref(),
            Some(current),
            "{content:?}"
        );
        let key = target.key.clone();
        for wanted in [Wanted::Off, Wanted::On, Wanted::Off] {
            let inv = scan(&env, &only(&["codewhale"]), None);
            set_state(&env, &inv, "codewhale", &key, wanted, &writer).unwrap();
            let doc: toml::Value = home.read(".codewhale/skills_state.toml").parse().unwrap();
            let names = doc["disabled"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                codewhale_native_enabled(&names, current, legacy),
                wanted == Wanted::On,
                "{content:?}"
            );
            let inv = scan(&env, &only(&["codewhale"]), None);
            assert_eq!(
                inv.skills
                    .iter()
                    .find(|skill| skill.key == key)
                    .unwrap()
                    .states["codewhale"]
                    .state,
                if wanted == Wanted::On {
                    State::On
                } else {
                    State::Off
                },
                "{content:?}"
            );
        }
    }
}

#[test]
fn codewhale_does_not_deduplicate_distinct_native_whitespace_names() {
    let home = Home::new();
    for (folder, name) in [
        ("one", "café tools"),
        ("two", "café  tools"),
        ("nbsp", "café\u{a0}tools"),
    ] {
        home.skill(&format!(".codewhale/skills/{folder}"), name, "Demo");
    }
    let inv = scan(&home.env(), &only(&["codewhale"]), None);
    let identities: BTreeSet<_> = inv
        .skills
        .iter()
        .map(|skill| {
            assert_eq!(skill.states["codewhale"].state, State::On);
            assert_eq!(skill.name, "café tools"); // Shared display formatting is unchanged.
            skill.states["codewhale"].switch_name.clone().unwrap()
        })
        .collect();
    assert_eq!(identities.len(), 3);
}

#[test]
fn codewhale_activation_identity_matches_native_segments() {
    for (name, current, legacy) in [
        (" My Skill! ", "my-skill", None),
        ("---", "skill", None),
        ("café", "caf-850f7dc43910ff890f8879c0ed26fe69", Some("caf")),
        (
            "日本語",
            "skill-77710aedc74ecfa33685e33a6c7df5cc",
            Some("skill"),
        ),
        (
            "My Plugin:café",
            "my-plugin:caf-850f7dc43910ff890f8879c0ed26fe69",
            Some("my-plugin:caf"),
        ),
        ("one:two:three", "one-two-three", None),
    ] {
        assert_eq!(
            codewhale_skill_identity(name),
            (current.into(), legacy.map(str::to_owned)),
            "{name}"
        );
    }
    assert_eq!(
        codewhale_skill_identity(&"A".repeat(65)),
        ("a".repeat(64), None)
    );
}

fn codewhale_native_enabled(disabled: &BTreeSet<String>, name: &str, legacy: Option<&str>) -> bool {
    if disabled.contains(name) {
        return false;
    }
    if disabled.contains(&format!("!codewhale-skill-state:1:enabled:{name}")) {
        return true;
    }
    !disabled.contains(&format!("!codewhale-skill-state:1:history:{name}"))
        && !legacy.is_some_and(|legacy| {
            disabled.contains(legacy)
                || disabled.contains(&format!("!codewhale-skill-state:1:history:{legacy}"))
        })
}

#[test]
fn codewhale_native_activation_identities_round_trip() {
    // Identities and predicate from Codewhale v0.10.1 skills/mod.rs and
    // runtime/skill_state.rs, independent of Toolport's identity resolver.
    for (display, current, legacy) in [
        ("café", "caf-850f7dc43910ff890f8879c0ed26fe69", Some("caf")),
        (" My Skill! ", "my-skill", None),
    ] {
        for veto in [None, Some(legacy.unwrap_or(current))] {
            let home = Home::new();
            home.skill(".codewhale/skills/demo", display, "Demo");
            if legacy.is_some() {
                home.skill(".codewhale/skills/sibling", "cafè", "Sibling");
            }
            let path = ".codewhale/skills_state.toml";
            let disabled = veto.map_or_else(Vec::new, |name| {
                vec![format!("!codewhale-skill-state:1:history:{name}")]
            });
            home.write(path, &format!("disabled = {disabled:?}\nother = true\n"));
            let env = home.env();
            let writer = TestWriter::default();
            let inv = scan(&env, &only(&["codewhale"]), None);
            let target = inv
                .skills
                .iter()
                .find(|s| s.name.trim() == display.trim())
                .unwrap();
            assert_eq!(
                target.states["codewhale"].state,
                if veto.is_some() {
                    State::Off
                } else {
                    State::On
                }
            );
            let key = target.key.clone();
            for wanted in [Wanted::On, Wanted::Off, Wanted::On] {
                let inv = scan(&env, &only(&["codewhale"]), None);
                set_state(&env, &inv, "codewhale", &key, wanted, &writer).unwrap();
                let doc: toml::Value = home.read(path).parse().unwrap();
                let names = doc["disabled"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(
                    codewhale_native_enabled(&names, current, legacy),
                    wanted == Wanted::On
                );
                if let Some(veto) = veto {
                    assert!(names.contains(&format!("!codewhale-skill-state:1:history:{veto}")));
                    if legacy.is_some() {
                        // Another Unicode skill in the legacy collision cohort
                        // keeps its inherited veto after toggling this skill.
                        assert!(!codewhale_native_enabled(
                            &names,
                            "caf-08dcdafde9046804985b9047747bb587",
                            Some(veto)
                        ));
                    }
                }
                let inv = scan(&env, &only(&["codewhale"]), None);
                if legacy.is_some() && veto.is_some() {
                    assert_eq!(state(&inv, "cafè", "codewhale"), Some(State::Off));
                }
                assert_eq!(
                    inv.skills.iter().find(|s| s.key == key).unwrap().states["codewhale"].state,
                    if wanted == Wanted::On {
                        State::On
                    } else {
                        State::Off
                    }
                );
                assert!(home.read(path).contains("other = true"));
            }
        }
    }
}

#[test]
fn codewhale_deduplicates_native_activation_identities() {
    let home = Home::new();
    home.skill(".codewhale/skills/first", "My Skill", "First");
    home.skill(".agents/skills/second", "my-skill", "Second");
    home.skill(".agents/skills/unicode", "café", "Unicode");
    home.skill(".agents/skills/unicode-other", "cafè", "Other Unicode");
    let inv = scan(&home.env(), &only(&["codewhale"]), None);
    assert_eq!(state(&inv, "My Skill", "codewhale"), Some(State::On));
    assert_eq!(state(&inv, "my-skill", "codewhale"), Some(State::Shadowed));
    assert_eq!(state(&inv, "café", "codewhale"), Some(State::On));
    assert_eq!(state(&inv, "cafè", "codewhale"), Some(State::On));
}

#[test]
fn codewhale_native_history_veto_and_exact_enable_round_trip() {
    let home = Home::new();
    home.skill(".codewhale/skills/demo", "demo", "Demo");
    let path = ".codewhale/skills_state.toml";
    let writer = TestWriter::default();
    for disabled in [
        r#"["demo", "!codewhale-skill-state:1:history:demo"]"#,
        r#"["!codewhale-skill-state:1:history:demo"]"#,
    ] {
        home.write(
            path,
            &format!("# native state\ndisabled = {disabled}\nother = true\n"),
        );
        let env = home.env();
        let inv = scan(&env, &only(&["codewhale"]), None);
        assert_eq!(state(&inv, "demo", "codewhale"), Some(State::Off));
        let key = skill(&inv, "demo").key.clone();
        set_state(&env, &inv, "codewhale", &key, Wanted::On, &writer).unwrap();
        let text = home.read(path);
        assert!(
            text.contains("!codewhale-skill-state:1:enabled:demo"),
            "{text}"
        );
        assert!(text.contains("!codewhale-skill-state:1:history:demo"));
        assert!(text.contains("# native state") && text.contains("other = true"));
        let inv = scan(&env, &only(&["codewhale"]), None);
        assert_eq!(state(&inv, "demo", "codewhale"), Some(State::On));
        set_state(&env, &inv, "codewhale", &key, Wanted::Off, &writer).unwrap();
        assert!(!home
            .read(path)
            .contains("!codewhale-skill-state:1:enabled:demo"));
        assert_eq!(
            state(
                &scan(&env, &only(&["codewhale"]), None),
                "demo",
                "codewhale"
            ),
            Some(State::Off)
        );
    }
}

#[test]
fn reasonix_discovers_bounded_nested_and_flat_skills_and_switches_them() {
    let home = Home::new();
    home.write(
        ".reasonix/skills/flat.md",
        "---\ndescription: Flat\n---\nDo it\n",
    );
    home.skill(".reasonix/skills/a/b/c", "third", "Depth three");
    home.skill(".reasonix/skills/a/b/d/e", "fourth", "Depth four");
    home.write(".reasonix/skills/a/no-description.md", "No description\n");
    home.write(".claude/skills/readme.md", "# ordinary docs\n");
    home.write(
        ".claude/skills/marked.md",
        "---\nuser-invocable: false\n---\nMarked\n",
    );
    home.write(
        "work/app/.reasonix/skills/pack/project.md",
        "---\ndescription: Project flat\n---\nDo it\n",
    );
    home.skill(
        "work/app/.reasonix/skills/pack/nested",
        "project-nested",
        "Project nested",
    );
    let project = home.path("work/app");
    let env = home.env();
    let inv = scan(&env, &only(&["reasonix"]), Some(&project));
    for name in ["flat", "third", "marked", "project", "project-nested"] {
        assert_eq!(state(&inv, name, "reasonix"), Some(State::On), "{name}");
    }
    for name in ["fourth", "no-description", "readme"] {
        assert!(inv.skills.iter().all(|skill| skill.name != name), "{name}");
    }
    assert!(!skill(&inv, "flat").editable);
    assert!(!skill(&inv, "flat").removable);
    let key = skill(&inv, "flat").key.clone();
    set_state(
        &env,
        &inv,
        "reasonix",
        &key,
        Wanted::Off,
        &TestWriter::default(),
    )
    .unwrap();
    assert_eq!(
        state(&scan(&env, &only(&["reasonix"]), None), "flat", "reasonix"),
        Some(State::Off)
    );
    home.write(".reasonix/config.toml", "[skills]\nmax_depth = 4\n");
    assert_eq!(
        state(
            &scan(&env, &only(&["reasonix"]), None),
            "fourth",
            "reasonix"
        ),
        Some(State::On)
    );
    home.write(".reasonix/config.toml", "[skills]\nmax_depth = 1\n");
    let shallow = scan(&env, &only(&["reasonix"]), None);
    assert!(shallow.skills.iter().all(|skill| skill.name != "third"));
}

#[test]
fn reasonix_inventory_reads_rules_from_configured_state() {
    let home = Home::new();
    home.write(
        ".reasonix/config.toml",
        "[storage]\nstate = '~/relocated'\n",
    );
    home.write("relocated/AGENTS.md", "# Actual rules\n");
    let inv = scan(&home.env(), &only(&["reasonix"]), None);
    assert!(inv
        .rules
        .iter()
        .any(|r| r.path == "~/relocated/AGENTS.md" && r.agents.contains_key("reasonix")));
}

#[test]
fn reasonix_switches_by_its_resolved_identity_without_renaming_other_clients() {
    let home = Home::new();
    for (file, declared, expected) in [
        ("review", "レビュー", "review"),
        ("invalid", "review changes", "invalid"),
        ("renamed", "inspect", "inspect"),
        ("cafe\u{301}", "", "café"),
    ] {
        home.write(
            &format!(".reasonix/skills/{file}.md"),
            &format!("---\nname: '{declared}'\ndescription: Review changes\n---\nReview\n"),
        );
        let env = home.env();
        let inv = scan(&env, &only(&["reasonix"]), None);
        let record = inv
            .skills
            .iter()
            .find(|s| s.key.ends_with(&format!("/{file}.md")))
            .unwrap();
        assert_eq!(
            record.states["reasonix"].switch_name.as_deref(),
            Some(expected)
        );
        assert!(record.states["reasonix"].read_only_reason.is_some());
        assert!(set_state(
            &env,
            &inv,
            "reasonix",
            &record.key,
            Wanted::Off,
            &TestWriter::default()
        )
        .is_err());
    }
    home.skill(".claude/skills/shared", "レビュー", "Shared review");
    let env = home.env();
    let inv = scan(&env, &only(&["claude-code", "reasonix"]), None);
    let record = inv
        .skills
        .iter()
        .find(|s| s.key.ends_with("/shared/SKILL.md"))
        .unwrap();
    assert_eq!(record.name, "レビュー");
    assert_eq!(
        record.states["reasonix"].switch_name.as_deref(),
        Some("shared")
    );
    assert!(record.states["reasonix"].read_only_reason.is_some());
    assert!(set_state(
        &env,
        &inv,
        "reasonix",
        &record.key,
        Wanted::Off,
        &TestWriter::default()
    )
    .is_err());
    assert_eq!(record.states["claude-code"].state, State::On);
    assert!(record.states["claude-code"].read_only_reason.is_none());
}

#[test]
fn reasonix_deduplicates_but_keeps_unicode_switches_read_only() {
    let home = Home::new();
    home.write(
        ".reasonix/skills/review.md",
        "---\nname: レビュー\ndescription: Review\n---\nReview\n",
    );
    home.write(
        ".reasonix/skills/second.md",
        "---\nname: review\ndescription: Second\n---\nSecond\n",
    );
    home.write(
        ".reasonix/skills/café.md",
        "---\ndescription: Cafe\n---\nCafe\n",
    );
    home.write(
        ".reasonix/config.toml",
        "[skills]\ndisabled_skills = ['cafe\u{301}']\n",
    );
    let env = home.env();
    let inv = scan(&env, &only(&["reasonix"]), None);
    let first = inv
        .skills
        .iter()
        .find(|s| s.key.ends_with("/review.md"))
        .unwrap();
    let second = inv
        .skills
        .iter()
        .find(|s| s.key.ends_with("/second.md"))
        .unwrap();
    assert_eq!(first.states["reasonix"].state, State::On);
    assert_eq!(second.states["reasonix"].state, State::Shadowed);
    let cafe = inv
        .skills
        .iter()
        .find(|s| s.key.ends_with("/café.md"))
        .unwrap();
    assert_eq!(cafe.states["reasonix"].state, State::Off);
    assert!(cafe.states["reasonix"].read_only_reason.is_some());
    let before = std::fs::read(home.path(".reasonix/config.toml")).unwrap();
    assert!(set_state(
        &env,
        &inv,
        "reasonix",
        &cafe.key,
        Wanted::On,
        &TestWriter::default()
    )
    .is_err());
    assert_eq!(
        std::fs::read(home.path(".reasonix/config.toml")).unwrap(),
        before
    );
}

#[test]
fn reasonix_scalar_spelling_matches_upstream_when_toggled() {
    for (header, expected) in [
        ("name: inspect\n", "inspect"),
        ("name: 123\n", "123"),
        ("name: Inspect\n", "Inspect"),
        ("name: 00123\n", "00123"),
    ] {
        let home = Home::new();
        home.write(
            ".reasonix/skills/review.md",
            &format!("---\n{header}description: Review\n---\nReview\n"),
        );
        let env = home.env();
        let inv = scan(&env, &only(&["reasonix"]), None);
        let record = inv.skills.first().unwrap();
        set_state(
            &env,
            &inv,
            "reasonix",
            &record.key,
            Wanted::Off,
            &TestWriter::default(),
        )
        .unwrap();
        let config = read_toml(&home.path(".reasonix/config.toml")).unwrap();
        assert_eq!(
            config["skills"]["disabled_skills"][0].as_str(),
            Some(expected),
            "{header}"
        );
    }
}

#[test]
fn reasonix_switch_certification_uses_upstream_name_table() {
    // Generated by running the unmodified frontmatter/{document,frontmatter}.go
    // and config/skill_name.go from Reasonix 31f230cf490ffee9db98e6734860dc0b92fb5bc5.
    // Uncertified rows deliberately refuse both directions without writing config.
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("reasonix-name-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let home = Home::new();
        let stem = case["stem"].as_str().unwrap();
        let header = case["header"].as_str().unwrap();
        let expected = case["expected"].as_str().unwrap();
        let safe = case["safe"].as_bool().unwrap();
        home.write(
            &format!(".reasonix/skills/{stem}.md"),
            &format!("---\n{header}description: Review\n---\nReview\n"),
        );
        let env = home.env();
        let inv = scan(&env, &only(&["reasonix"]), None);
        let record = inv.skills.first().unwrap();
        assert_eq!(
            record.states["reasonix"].read_only_reason.is_none(),
            safe,
            "{case}"
        );
        for wanted in [Wanted::Off, Wanted::On] {
            let result = set_state(
                &env,
                &inv,
                "reasonix",
                &record.key,
                wanted,
                &TestWriter::default(),
            );
            if safe {
                result.unwrap();
                assert_eq!(
                    record.states["reasonix"].switch_name.as_deref(),
                    Some(expected),
                    "{case}"
                );
                let config = read_toml(&home.path(".reasonix/config.toml")).unwrap();
                let list = config["skills"]["disabled_skills"].as_array().unwrap();
                assert_eq!(
                    list.iter().any(|v| v.as_str() == Some(expected)),
                    wanted == Wanted::Off,
                    "{case}"
                );
                let after = scan(&env, &only(&["reasonix"]), None);
                assert_eq!(
                    after.skills.first().unwrap().states["reasonix"].state,
                    if wanted == Wanted::Off {
                        State::Off
                    } else {
                        State::On
                    },
                    "{case}"
                );
            } else {
                assert!(result.unwrap_err().contains("cannot verify"), "{case}");
                assert!(!home.path(".reasonix/config.toml").exists(), "{case}");
            }
        }
    }
}

#[test]
fn reasonix_malformed_frontmatter_never_certifies_a_switch() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("reasonix-refused-name-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let home = Home::new();
        let text = case["text"].as_str().map(str::to_owned).unwrap_or_else(|| {
            format!(
                "---\n{}description: Review\n---\nReview\n",
                case["header"].as_str().unwrap()
            )
        });
        home.write(".reasonix/skills/review.md", &text);
        for initial in [None, Some("[skills]\ndisabled_skills = ['review']\n")] {
            if let Some(initial) = initial {
                home.write(".reasonix/config.toml", initial);
            }
            let before = std::fs::read(home.path(".reasonix/config.toml")).ok();
            let env = home.env();
            let inv = scan(&env, &only(&["reasonix"]), None);
            let record = inv.skills.first().unwrap();
            assert!(
                record.states["reasonix"].read_only_reason.is_some(),
                "{case}"
            );
            for wanted in [Wanted::Off, Wanted::On] {
                assert!(
                    set_state(
                        &env,
                        &inv,
                        "reasonix",
                        &record.key,
                        wanted,
                        &TestWriter::default()
                    )
                    .unwrap_err()
                    .contains("cannot verify"),
                    "{case}"
                );
                assert_eq!(
                    std::fs::read(home.path(".reasonix/config.toml")).ok(),
                    before,
                    "{case}"
                );
                let after = scan(&env, &only(&["reasonix"]), None);
                assert!(after.skills.first().unwrap().states["reasonix"]
                    .read_only_reason
                    .is_some());
            }
        }
    }
}

#[test]
fn reasonix_certification_rejects_header_controls_before_trimming() {
    let home = Home::new();
    let path = home.path(".reasonix/skills/review.md");
    let found = Found {
        link: path.clone(),
        real: Some(path),
        root_label: "test",
        root_path: home.path(".reasonix/skills"),
    };
    for byte in (0..=31).chain([127]).filter(|byte| *byte != b'\n') {
        home.write(
            ".reasonix/skills/review.md",
            &format!(
                "---\nname: inspect\ndescription: Rev{}iew\n---\nReview\n",
                char::from(byte)
            ),
        );
        assert_eq!(reasonix_proven_skill_name(&found), None, "byte {byte:#x}");
    }
    for newline in ["\n", "\r\n"] {
        home.write(".reasonix/skills/review.md", &format!("---{newline}name: inspect{newline}description: Review{newline}---{newline}Body\x0c"));
        assert_eq!(
            reasonix_proven_skill_name(&found).as_deref(),
            Some("inspect")
        );
    }
}

#[test]
fn reasonix_certification_respects_the_skill_file_size_cap() {
    let home = Home::new();
    let path = home.write(
        ".reasonix/skills/review.md",
        "---\nname: inspect\ndescription: Review\n---\nBody\n",
    );
    let found = Found {
        link: path.clone(),
        real: Some(path.clone()),
        root_label: "test",
        root_path: home.path(".reasonix/skills"),
    };
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(MAX_FILE_BYTES).unwrap();
    assert_eq!(
        reasonix_proven_skill_name(&found).as_deref(),
        Some("inspect")
    );
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert_eq!(reasonix_proven_skill_name(&found), None);
}

#[cfg(unix)]
#[test]
fn reasonix_read_only_identity_keeps_each_state_explanation() {
    let home = Home::new();
    for file in ["a", "b"] {
        home.write(
            &format!(".reasonix/skills/{file}.md"),
            "---\nname: 'shared'\ndescription: Review\n---\nReview\n",
        );
    }
    home.link(".reasonix/skills/broken", &home.path("missing"));
    home.write(
        ".reasonix/config.toml",
        "[skills]\ndisabled_skills = ['shared']\n",
    );
    let inv = scan(&home.env(), &only(&["reasonix"]), None);
    for (suffix, expected, via) in [
        ("/a.md", State::Off, "Off in"),
        ("/b.md", State::Shadowed, "wins in"),
        ("/broken", State::Broken, "missing folder"),
    ] {
        let record = inv.skills.iter().find(|s| s.key.ends_with(suffix)).unwrap();
        let state = &record.states["reasonix"];
        assert_eq!(state.state, expected);
        assert!(state.read_only_reason.is_some());
        assert!(state.via.contains(via), "{suffix}: {}", state.via);
        assert_ne!(Some(&state.via), state.read_only_reason.as_ref());
    }
}

fn detected_profile(
    home: &Home,
    id: &str,
    file: &str,
    connected: bool,
) -> crate::clients::DetectedClient {
    crate::clients::DetectedClient {
        id: id.into(),
        name: id.into(),
        uses_connectors: false,
        location_removable: false,
        discovery: crate::clients::DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: None,
            tools_list_changed: None,
            cold_full_list_wait_ms: 0,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "test",
        },
        config_path: home.path(file).display().to_string(),
        config_exists: true,
        app_present: true,
        servers: vec![],
        plugin_servers: vec![],
        gateway_installed: connected,
        entry_state: crate::clients::GatewayEntryState::Absent,
        error: None,
        claude_profile: None,
    }
}

#[test]
fn client_profiles_keep_exact_skill_rule_and_write_targets() {
    let home = Home::new();
    home.write(".claude/settings.json", "{}");
    home.write(".claude2/settings.json", "{}");
    home.write(".claude3/settings.json", "{}");
    home.skill(".agents/skills/shared", "shared", "Shared skill");
    home.skill("Codex-Work/.codex/skills/private", "private", "Work only");
    home.write(".codex/config.toml", "# main\n");
    home.write("Codex-Work/.codex/config.toml", "# work\n");
    home.write("Codex-Work/.codex/AGENTS.md", "Work rules\n");
    let clients = vec![
        detected_profile(&home, "claude-code", ".claude.json", true),
        detected_profile(&home, "claude-code@claude2", ".claude2/.claude.json", true),
        detected_profile(&home, "claude-code@claude3", ".claude3/.claude.json", false),
        detected_profile(&home, "codex", ".codex/config.toml", true),
        detected_profile(
            &home,
            "codex@Codex-Work%2F.codex",
            "Codex-Work/.codex/config.toml",
            false,
        ),
    ];
    let mut env = home.env();
    env.use_clients(&clients);
    let inv = scan(&env, &only(&["claude-code", "codex"]), None);
    assert_eq!(inv.agents.len(), 5);
    assert_eq!(inv.agents.iter().filter(|a| a.connected).count(), 3);
    let work = "codex@Codex-Work%2F.codex";
    assert_eq!(
        skill(&inv, "private")
            .states
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![work]
    );
    assert_eq!(
        inv.rules
            .iter()
            .find(|r| r.preview.contains("Work rules"))
            .unwrap()
            .agents
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![work]
    );
    let key = skill(&inv, "shared").key.clone();
    set_state(&env, &inv, work, &key, Wanted::Off, &TestWriter::default()).unwrap();
    assert_eq!(home.read(".codex/config.toml"), "# main\n");
    assert!(home
        .read("Codex-Work/.codex/config.toml")
        .contains("enabled = false"));
    let after = scan(&env, &only(&["claude-code", "codex"]), None);
    assert_eq!(state(&after, "shared", work), Some(State::Off));
    assert_eq!(state(&after, "shared", "codex"), Some(State::On));
    assert_eq!(home.read(".claude2/settings.json"), "{}");
}

#[test]
fn stale_codex_entries_are_reported_and_fixed_per_home() {
    let home = Home::new();
    let missing = home.path("missing/SKILL.md");
    let config = format!(
        "[[skills.config]]\npath = {:?}\nenabled = false\n",
        missing.display().to_string()
    );
    home.write(".codex/config.toml", &config);
    home.write(".codex2/config.toml", &config);
    let mut env = home.env();
    env.use_clients(&[
        detected_profile(&home, "codex", ".codex/config.toml", true),
        detected_profile(&home, "codex@.codex2", ".codex2/config.toml", false),
    ]);
    let inv = scan(&env, &only(&["codex"]), None);
    assert!(inv.issues.iter().any(|i| i.id == "codex-stale"));
    assert_eq!(
        inv.issues
            .iter()
            .find(|i| i.id == "codex-stale@.codex2")
            .unwrap()
            .agents,
        vec!["codex@.codex2"]
    );
    let writer = TestWriter::default();
    assert_eq!(
        apply_fix(&env, &inv, "codex-stale@.codex2", &writer).unwrap(),
        1
    );
    assert_eq!(home.read(".codex/config.toml"), config);
    assert!(!home.read(".codex2/config.toml").contains("enabled = false"));
    assert_eq!(apply_fix(&env, &inv, "codex-stale", &writer).unwrap(), 1);
    assert_eq!(
        *writer.backups.borrow(),
        vec![
            ("codex@.codex2".into(), home.path(".codex2/config.toml")),
            ("codex".into(), home.path(".codex/config.toml")),
        ],
        "each profile keeps its own recovery history"
    );
}

#[test]
fn default_claude_profile_maps_to_skills_folder_without_alias_columns() {
    let home = Home::new();
    home.skill(
        ".claude/skills/default",
        "default-profile",
        "Default profile skill",
    );
    home.write(".claude/CLAUDE.md", "Default rules");
    let main = detected_profile(&home, "claude-code", ".claude.json", true);
    let mut alias = detected_profile(&home, "claude-code@claude", ".claude/.claude.json", false);
    alias.claude_profile = Some(crate::clients::ClaudeProfile {
        same_as: Some("claude-code".into()),
        ..Default::default()
    });
    let mut env = home.env();
    env.use_clients(&[main, alias]);
    let inv = scan(&env, &only(&["claude-code"]), None);
    assert_eq!(inv.agents.len(), 1);
    assert_eq!(inv.agents[0].id, "claude-code");
    assert!(inv.agents[0].connected);
    assert_eq!(
        state(&inv, "default-profile", "claude-code"),
        Some(State::On)
    );
    assert_eq!(inv.rules[0].path, "~/.claude/CLAUDE.md");
    assert!(inv.rules[0].agents.contains_key("claude-code"));
}

#[test]
fn skills_discovers_unsaved_profiles_before_clients_without_adopting_configs() {
    let home = Home::new();
    let _data = crate::registry::DataDirTestEnv::new("skills-unsaved-profiles");
    let _home = crate::clients::TestHome::set(&home.root);
    let claude = r#"{"mcpServers":{"toolport":{"command":"/old/toolport-gateway","args":[]}}}"#;
    home.write(".claude.json", claude);
    home.write(".claude2/.claude.json", claude);
    home.write(".codex/config.toml", "# main\n");
    home.write(".codex2/config.toml", "# second\n");
    home.skill(
        ".codex2/skills/second",
        "second-home",
        "Only the second Codex home",
    );
    let data_dir = crate::registry::conduit_dir().unwrap();
    let locations = data_dir.join("client-locations.json");
    assert!(!locations.exists());
    let clients = crate::clients::detect_clients_for_skills();
    let mut env = home.env();
    env.use_clients(&clients);
    let inv = scan(&env, &|id| env.detected_agents.contains(id), None);
    assert_eq!(
        inv.agents
            .iter()
            .filter(|a| a.id.starts_with("claude-code") && a.connected)
            .count(),
        2
    );
    assert_eq!(
        inv.agents
            .iter()
            .filter(|a| a.id.starts_with("codex"))
            .count(),
        2
    );
    assert!(skill(&inv, "second-home")
        .states
        .keys()
        .any(|id| id.starts_with("codex@")));
    assert_eq!(home.read(".claude.json"), claude);
    assert_eq!(home.read(".claude2/.claude.json"), claude);
    assert_eq!(home.read(".codex/config.toml"), "# main\n");
    assert_eq!(home.read(".codex2/config.toml"), "# second\n");
    assert!(
        !locations.exists(),
        "Skills must not remember or adopt client locations"
    );
}

#[test]
fn claude_configs_sharing_one_skills_folder_have_one_connected_column() {
    for (main_connected, copy_connected, expected) in [
        (false, true, "claude-code@claude"),
        (true, false, "claude-code"),
        (true, true, "claude-code"),
        (false, false, "claude-code"),
    ] {
        let home = Home::new();
        home.skill(
            ".claude/skills/shared",
            "same-folder",
            "Shared Claude settings",
        );
        let mut env = home.env();
        env.use_clients(&[
            detected_profile(&home, "claude-code", ".claude.json", main_connected),
            detected_profile(
                &home,
                "claude-code@claude",
                ".claude/.claude.json",
                copy_connected,
            ),
        ]);
        let inv = scan(&env, &only(&["claude-code"]), None);
        assert_eq!(
            inv.agents.len(),
            1,
            "two MCP configs read the same skill settings"
        );
        assert_eq!(inv.agents[0].id, expected);
        assert_eq!(inv.agents[0].connected, main_connected || copy_connected);
        assert_eq!(
            skill(&inv, "same-folder")
                .states
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![expected]
        );
    }
}
