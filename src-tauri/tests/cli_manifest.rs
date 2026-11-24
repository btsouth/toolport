//! `toolport init`, `up`, `export` and `import` against isolated fixtures.
mod cli_support;

use std::io::Write as _;
use std::process::Stdio;

use cli_support::{stderr, stdout, Fixture, SECRET};

const MANIFEST: &str = r#"
version = 1
name = "demo"

[rules]
source = "AGENTS.md"

[[skills]]
path = ".agents/skills/release"

[[skills]]
source = "vercel-labs/agent-skills@frontend-design"

[[servers]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = ["GITHUB_TOKEN"]
[servers.values]
LOG_LEVEL = "debug"

[[servers]]
name = "docs"
url = "https://mcp.example.com/mcp"
"#;

fn project(f: &Fixture) {
    f.clean_machine();
    f.write_project(
        "AGENTS.md",
        "# Demo rules\n\nUse the release skill to ship.\n",
    );
    f.write_project(
        ".agents/skills/release/SKILL.md",
        "---\nname: release\ndescription: Cut a release of the demo project.\n---\nSteps.\n",
    );
    f.write_project("toolport.toml", MANIFEST);
}

#[test]
fn init_writes_a_secret_free_manifest_that_up_accepts() {
    let f = Fixture::new("init");
    f.clean_machine();
    f.write_project("AGENTS.md", "# rules\n");
    f.write_project(
        ".agents/skills/release/SKILL.md",
        "---\nname: release\n---\n",
    );
    f.write_project(
        ".mcp.json",
        &format!(
            r#"{{"mcpServers":{{"github":{{"command":"npx","args":["-y","server-github"],"env":{{"GITHUB_TOKEN":"{SECRET}","LOG_LEVEL":"info"}}}}}}}}"#
        ),
    );
    let out = f.run(&["init"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = f.read(&f.project.join("toolport.toml"));
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains("env = [\"GITHUB_TOKEN\"]"), "{text}");
    assert!(text.contains("LOG_LEVEL = \"info\""));
    assert!(text.contains("source = \"AGENTS.md\""));
    assert!(text.contains("path = \".agents/skills/release\""));

    let again = f.run(&["init"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(stderr(&again).contains("--force"));
    assert!(stdout(&f.run(&["init", "--out", "-"])).contains("version = 1"));

    let plan = f.json(&["up", "--dry-run", "--json"]);
    assert!(plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["kind"] == "serverAdd"));
}

#[test]
fn up_refuses_a_manifest_with_a_literal_secret_and_never_echoes_it() {
    let f = Fixture::new("up-secret");
    f.clean_machine();
    f.write_project(
        "toolport.toml",
        &format!(
            "version = 1\n[[servers]]\nname = \"github\"\ncommand = \"npx\"\n[servers.values]\nGITHUB_TOKEN = \"{SECRET}\"\n"
        ),
    );
    let out = f.run(&["up", "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("looks like a credential"), "{err}");
    assert!(err.contains("keychain"), "{err}");
    assert!(!err.contains(SECRET) && !stdout(&out).contains(SECRET));
    assert!(!f.data.join("registry.json").exists());
}

#[test]
fn up_plans_then_links_rules_skills_and_vaults_credentials() {
    let f = Fixture::new("up");
    project(&f);
    let plan = f.json(&["up", "--dry-run", "--json"]);
    let steps = plan["steps"].as_array().unwrap();
    let has = |kind: &str| steps.iter().any(|s| s["kind"] == kind);
    assert!(
        steps.iter().any(|s| s["kind"] == "rulesPointer"
            && s["path"] == "CLAUDE.md"
            && s["create"] == true),
        "{plan:#}"
    );
    assert!(has("serverAdd"));
    assert!(steps.iter().any(|s| s["kind"] == "skillInstall"
        && s["command"] == "npx skills add vercel-labs/agent-skills --skill frontend-design"));
    let github = steps.iter().find(|s| s["name"] == "github").unwrap();
    assert_eq!(github["needs"], serde_json::json!(["GITHUB_TOKEN"]));
    let docs = steps.iter().find(|s| s["name"] == "docs").unwrap();
    assert!(docs["warnings"].as_array().unwrap().is_empty());
    assert!(github["warnings"][0]
        .as_str()
        .unwrap()
        .contains("shell command"));
    // A dry run writes nothing.
    assert!(!f.project.join("CLAUDE.md").exists());
    assert!(!f.data.join("registry.json").exists());

    // Not on a terminal and no --yes: refuse.
    let out = f.run(&["up"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--yes"));

    let out = f.run_env(&["up", "--yes", "--json"], &[("GITHUB_TOKEN", SECRET)]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        report["failed"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
    assert_eq!(f.read(&f.project.join("CLAUDE.md")), "@AGENTS.md\n");
    let registry = f.read(&f.data.join("registry.json"));
    assert!(registry.contains("\"github\"") && registry.contains("\"docs\""));
    assert!(registry.contains("LOG_LEVEL"));
    assert!(
        !registry.contains(SECRET),
        "a credential reached registry.json"
    );
    let vault = std::fs::read(f.data.join("secrets.enc")).expect("value stored in the vault");
    assert!(
        !String::from_utf8_lossy(&vault).contains(SECRET),
        "vault is not plaintext"
    );
    let applied = report["applied"].to_string();
    assert!(applied.contains("added github (enabled)"), "{applied}");
    #[cfg(unix)]
    if steps.iter().any(|s| s["kind"] == "skillLink") {
        let link = f.project.join(".claude/skills/release");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(link.join("SKILL.md").is_file());
    }

    // Running it again finds everything in place.
    let again = f.json(&["up", "--dry-run", "--json"]);
    let steps = again["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .all(|s| s["kind"] != "serverAdd" && s["kind"] != "rulesPointer"),
        "{again:#}"
    );
    assert!(steps.iter().any(|s| s["kind"] == "serverSame"));
}

#[test]
fn export_strips_credentials_and_import_adds_servers_switched_off() {
    let a = Fixture::new("export");
    project(&a);
    let out = a.run_env(&["up", "--yes"], &[("GITHUB_TOKEN", SECRET)]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let file = a.root.join("setup.json");
    let out = a.run(&["export", "--out", file.to_str().unwrap(), "--name", "Demo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json = a.read(&file);
    assert!(json.contains("\"conduit-setup\"") && json.contains("\"github\""));
    assert!(!json.contains(SECRET));
    assert!(!stdout(&a.run(&["export"])).contains(SECRET));

    let b = Fixture::new("import");
    b.clean_machine();
    let preview = b.json(&["import", file.to_str().unwrap(), "--dry-run", "--json"]);
    let servers = preview["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);
    assert!(servers.iter().all(|s| s["selected"] == true));
    assert!(!b.data.join("registry.json").exists());

    let out = b.run(&[
        "import",
        file.to_str().unwrap(),
        "--only",
        "github",
        "--yes",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let registry: serde_json::Value =
        serde_json::from_str(&b.read(&b.data.join("registry.json"))).unwrap();
    let names: Vec<&str> = registry["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["github"]);
    assert_eq!(registry["servers"][0]["enabled"], false);

    // From stdin, the rest.
    let mut child = b
        .command()
        .args(["import", "-", "--yes"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Imported 1 server"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        b.run(&["import", "/no/such/file.json"]).status.code(),
        Some(1)
    );
    assert!(
        b.run(&["import", file.to_str().unwrap(), "--only", "nope"])
            .status
            .code()
            == Some(1)
    );
}
