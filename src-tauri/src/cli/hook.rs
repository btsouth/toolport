//! `toolport hook`: an opt-in SessionStart check for Claude Code and Codex.
//!
//! `check` prints nothing when the machine is healthy and one line when something broke,
//! so an agent session only hears about Toolport when it matters. It always exits 0: a
//! session start hook must never block or fail the agent.
//!
//! Toolport's entry is recognized by the words `toolport` and `hook check` in its command,
//! never by the retired `--toolport-hook` marker (`crate::hooks`), so the two can't be
//! confused. Install and uninstall edit the settings file through the JSONC syntax tree, so
//! comments, key order and the user's own hooks stay exactly as they were, and the previous
//! file is copied to Toolport's backups first.

use std::path::{Path, PathBuf};

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use serde::Serialize;

use super::format::{self, Severity};
use super::scan::Snapshot;
use super::{Args, CliError, Ctx, EXIT_OK};

const EVENT: &str = "SessionStart";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// Settings files that hold the hook: one per Claude Code profile, and Codex's hooks.json.
pub fn targets(agent: Option<Agent>) -> Vec<(Agent, PathBuf)> {
    let mut out = Vec::new();
    let Some(env) = crate::skills::Env::current() else {
        return out;
    };
    if agent.is_none_or(|a| a == Agent::ClaudeCode) {
        let mut dirs = env.claude_profiles.clone();
        if dirs.is_empty() {
            dirs.push(env.claude.clone());
        }
        for dir in dirs.into_iter().filter(|d| d.is_dir()) {
            out.push((Agent::ClaudeCode, dir.join("settings.json")));
        }
    }
    if agent.is_none_or(|a| a == Agent::Codex) && env.codex.is_dir() {
        out.push((Agent::Codex, env.codex.join("hooks.json")));
    }
    out
}

/// True for a hook command Toolport's `hook install` wrote.
pub fn is_ours(command: &str) -> bool {
    command.contains("hook check") && command.to_ascii_lowercase().contains("toolport")
}

/// Whether `path` runs this CLI: the binary itself (the standalone one, or the Linux
/// desktop shell that `/usr/bin/toolport` points at, which runs `hook check` without
/// opening a window), or the `@toolport/cli` npm launcher. Any other `toolport`, such as
/// an older desktop package's, may open the app, and a session start hook must never
/// launch that.
fn runs_this_cli(path: &Path) -> bool {
    let real = std::fs::canonicalize(path).ok();
    if real.is_some()
        && std::env::current_exe()
            .ok()
            .and_then(|e| std::fs::canonicalize(e).ok())
            == real
    {
        return true;
    }
    let Some(real) = real else { return false };
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(&real)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]);
    head.contains("@toolport/cli") || head.contains("@toolport\\cli")
}

/// The command a hook runs, always by absolute path: an agent can start with a different
/// PATH than this shell, where a bare `toolport` could be the desktop app. Prefers the
/// `toolport` on PATH when it runs this CLI (an npm global install keeps that path across
/// upgrades), else this binary. A copy npx unpacked into its cache can vanish, so that
/// case goes through npx.
pub fn hook_command() -> (String, Option<String>) {
    if let Some(p) = std::env::var_os("PATH").and_then(|path| cli_on_path(&path)) {
        return (format!("\"{}\" hook check", p.display()), None);
    }
    if let Ok(exe) = std::env::current_exe() {
        if !is_ephemeral(&exe) {
            return (format!("\"{}\" hook check", exe.display()), None);
        }
    }
    (
        "npx --yes @toolport/cli hook check".into(),
        Some("The hook runs through npx, which adds a moment to each session start. Install it once with 'npm install -g @toolport/cli' to make it instant.".into()),
    )
}

/// A copy unpacked into an ephemeral package-runner cache (npx, pnpm dlx, bunx, yarn dlx),
/// which may be cleared at any time. Those runners also put their cache's `.bin` on PATH
/// while running, so both the path and its symlink target are checked.
fn is_ephemeral(path: &Path) -> bool {
    let cached = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        text.contains("_npx")
            || text.contains("npm-cache")
            || text.contains("/dlx/")
            || text.contains("pnpm-store")
            || text.contains("/bunx-")
            || text.contains("/.bun/install/cache/")
    };
    cached(path) || std::fs::canonicalize(path).is_ok_and(|real| cached(&real))
}

/// The first `toolport` on `path` that runs this CLI and won't vanish with npx's cache.
fn cli_on_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec!["toolport.exe".into(), "toolport.cmd".into()]
    } else {
        vec!["toolport".into()]
    };
    std::env::split_paths(path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .filter(|p| p.is_file())
        .find(|p| !is_ephemeral(p) && runs_this_cli(p))
}

fn parse(text: &str) -> Result<CstRootNode, String> {
    let text = if text.trim().is_empty() { "{}" } else { text };
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| format!("isn't valid JSON: {e}"))
}

fn string_of(node: Option<jsonc_parser::cst::CstNode>) -> Option<String> {
    node?.as_string_lit()?.decoded_value().ok()
}

/// Whether the settings text already runs Toolport's check at session start.
pub fn installed_in(text: &str) -> bool {
    let Ok(root) = parse(text) else { return false };
    let Some(groups) = root
        .object_value()
        .and_then(|o| o.object_value("hooks"))
        .and_then(|h| h.array_value(EVENT))
    else {
        return false;
    };
    groups.elements().iter().any(|g| {
        g.as_object()
            .and_then(|g| g.array_value("hooks"))
            .is_some_and(|entries| {
                entries.elements().iter().any(|e| {
                    e.as_object()
                        .and_then(|e| e.get("command"))
                        .and_then(|p| string_of(p.value()))
                        .is_some_and(|c| is_ours(&c))
                })
            })
    })
}

/// Add Toolport's entry. Leaves every other key, hook and comment as it was.
pub fn add_hook(text: &str, agent: Agent, command: &str) -> Result<String, String> {
    let root = parse(text)?;
    let obj = root
        .object_value()
        .ok_or("isn't a JSON object at the top level")?;
    let hooks = obj.object_value_or_set("hooks");
    let groups = hooks.array_value_or_set(EVENT);
    let mut group = vec![(
        "hooks".to_string(),
        CstInputValue::Array(vec![CstInputValue::Object(vec![
            ("type".into(), CstInputValue::String("command".into())),
            ("command".into(), CstInputValue::String(command.into())),
            ("timeout".into(), CstInputValue::Number("10".into())),
        ])]),
    )];
    if agent == Agent::ClaudeCode {
        group.insert(
            0,
            ("matcher".into(), CstInputValue::String("startup".into())),
        );
    }
    groups.append(CstInputValue::Object(group));
    Ok(root.to_string())
}

/// Remove only Toolport's entries, then any group, event list or `hooks` object that this
/// removal emptied. Returns the new text and how many entries went.
pub fn remove_hook(text: &str) -> Result<(String, usize), String> {
    let root = parse(text)?;
    let Some(obj) = root.object_value() else {
        return Ok((text.to_string(), 0));
    };
    let Some(hooks) = obj.object_value("hooks") else {
        return Ok((text.to_string(), 0));
    };
    let mut removed = 0;
    for prop in hooks.properties() {
        let Some(groups) = prop.value().and_then(|v| v.as_array()) else {
            continue;
        };
        let before_groups = groups.elements().len();
        let mut emptied_groups = 0;
        for group in groups.elements() {
            let Some(entries) = group.as_object().and_then(|g| g.array_value("hooks")) else {
                continue;
            };
            let before = entries.elements().len();
            for entry in entries.elements() {
                let ours = entry
                    .as_object()
                    .and_then(|e| e.get("command"))
                    .and_then(|p| string_of(p.value()))
                    .is_some_and(|c| is_ours(&c));
                if ours {
                    entry.remove();
                    removed += 1;
                }
            }
            if before > 0 && entries.elements().is_empty() {
                group.remove();
                emptied_groups += 1;
            }
        }
        if emptied_groups > 0 && emptied_groups == before_groups {
            prop.remove();
        }
    }
    if removed > 0 && hooks.properties().is_empty() {
        if let Some(prop) = obj.get("hooks") {
            prop.remove();
        }
    }
    Ok((root.to_string(), removed))
}

fn backup(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let dir = super::data_dir()
        .map_err(|e| e.message)?
        .join("backups")
        .join("hooks");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".into());
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = dir.join(format!(
        "{}-{}-{}",
        parent.trim_start_matches('.'),
        super::now_ms(),
        name
    ));
    std::fs::copy(path, &dest).map_err(|e| e.to_string())?;
    Ok(Some(dest))
}

/// A settings file's text: empty when it doesn't exist yet, an error when it exists but
/// can't be read, so an unreadable file is never replaced.
fn read_settings(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("couldn't be read ({e}); left unchanged")),
    }
}

fn read_codex_config(config: &Path) -> Result<toml_edit::DocumentMut, String> {
    let text = read_settings(config).map_err(|e| format!("{} {e}", config.display()))?;
    text.parse()
        .map_err(|e| format!("{} isn't valid TOML: {e}", config.display()))
}

fn codex_hooks_on(doc: &toml_edit::DocumentMut) -> bool {
    doc.get("features")
        .and_then(|f| f.get("hooks"))
        .and_then(|h| h.as_bool())
        == Some(true)
}

/// Codex only runs hooks with `[features] hooks = true`. Set it, keeping the file's layout.
fn enable_codex_hooks(config: &Path) -> Result<bool, String> {
    let mut doc = read_codex_config(config)?;
    if codex_hooks_on(&doc) {
        return Ok(false);
    }
    backup(config)?;
    if !doc.contains_key("features") {
        doc["features"] = toml_edit::table();
    }
    doc["features"]["hooks"] = toml_edit::value(true);
    crate::registry::atomic_write(config, &doc.to_string())?;
    Ok(true)
}

/// One settings file's planned change, worked out before anything is written.
struct Planned {
    agent: Agent,
    path: PathBuf,
    shown: String,
    /// The settings file's new text, `None` when it needs no change.
    change: Result<Option<String>, String>,
    /// Codex's config.toml, when installing and it doesn't turn hooks on yet. Checked even
    /// when hooks.json already has the entry, so a retry finishes an earlier partial install.
    codex_config: Option<PathBuf>,
}

impl Planned {
    fn pending(&self) -> bool {
        matches!(self.change, Ok(Some(_))) || (self.change.is_ok() && self.codex_config.is_some())
    }
}

fn plan_target(agent: Agent, path: PathBuf, install: bool, cmd: &str) -> Planned {
    let shown = format::home_relative(&path);
    let change = read_settings(&path).and_then(|text| {
        if install {
            if installed_in(&text) {
                Ok(None)
            } else {
                add_hook(&text, agent, cmd).map(Some)
            }
        } else {
            remove_hook(&text).map(|(new, n)| (n > 0).then_some(new))
        }
    });
    let codex_config = (install && agent == Agent::Codex)
        .then(|| path.parent().map(|p| p.join("config.toml")))
        .flatten()
        // An unreadable or invalid config is planned too, so applying reports why.
        .filter(|config| !read_codex_config(config).is_ok_and(|doc| codex_hooks_on(&doc)));
    Planned {
        agent,
        path,
        shown,
        change,
        codex_config,
    }
}

/// Apply (or, on a dry run, describe) one planned change. Returns what happened.
fn apply_target(
    p: Planned,
    install: bool,
    dry_run: bool,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let Planned {
        agent: a,
        path,
        shown,
        change,
        codex_config,
    } = p;
    let config_shown = codex_config.as_deref().map(format::home_relative);
    let outcome = match change {
        Err(e) => Err(e),
        Ok(None) if !install => Ok(format!("{}: not installed in {shown}", a.label())),
        // The entry is there, but an earlier install may have stopped before Codex's
        // config.toml turned hooks on: finish that now.
        Ok(None) => {
            let enabled = match &codex_config {
                Some(_) if dry_run => Ok(true),
                Some(config) => enable_codex_hooks(config),
                None => Ok(false),
            };
            enabled.map(|enabled| match &config_shown {
                Some(c) if enabled => format!(
                    "{}: already installed in {shown}; {} [features] hooks in {c}",
                    a.label(),
                    if dry_run {
                        "would turn on"
                    } else {
                        "turned on"
                    }
                ),
                _ => format!("{}: already installed in {shown}", a.label()),
            })
        }
        Ok(Some(_)) if dry_run => Ok(format!(
            "{}: would {} {shown}{}",
            a.label(),
            if install { "install in" } else { "remove from" },
            match &config_shown {
                Some(c) => format!(", and turn on [features] hooks in {c}"),
                None => String::new(),
            }
        )),
        Ok(Some(new)) => (|| {
            backup(&path)?;
            crate::registry::atomic_write(&path, &new)?;
            if let Some(config) = &codex_config {
                if enable_codex_hooks(config)? {
                    notes.push(format!(
                        "Turned on [features] hooks in {}.",
                        format::home_relative(config)
                    ));
                }
            }
            if install && a == Agent::Codex {
                notes.push("Codex asks you to trust the new hook the next time it starts.".into());
            }
            Ok(format!(
                "{}: {} {shown}",
                a.label(),
                if install {
                    "installed in"
                } else {
                    "removed from"
                }
            ))
        })(),
    };
    outcome.map_err(|e: String| format!("{}: {shown} {e}", a.label()))
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub plaintext_secrets: usize,
    pub broken_skill_links: usize,
    pub unreadable_configs: usize,
    pub conflicting_servers: usize,
}

impl Health {
    pub fn line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.plaintext_secrets > 0 {
            parts.push(format!(
                "{} on disk",
                format::plural(
                    self.plaintext_secrets,
                    "plaintext secret",
                    "plaintext secrets"
                )
            ));
        }
        if self.broken_skill_links > 0 {
            parts.push(format::plural(
                self.broken_skill_links,
                "broken skill link",
                "broken skill links",
            ));
        }
        if self.unreadable_configs > 0 {
            parts.push(format::plural(
                self.unreadable_configs,
                "unreadable agent config",
                "unreadable agent configs",
            ));
        }
        if self.conflicting_servers > 0 {
            parts.push(format::plural(
                self.conflicting_servers,
                "conflicting MCP server",
                "conflicting MCP servers",
            ));
        }
        (!parts.is_empty()).then(|| format!("toolport: {} (run 'toolport scan')", parts.join(", ")))
    }
}

pub fn health(snapshot: &Snapshot) -> Health {
    Health {
        plaintext_secrets: super::scan::secret_findings(snapshot).len(),
        broken_skill_links: snapshot
            .inventory
            .as_ref()
            .map(|inv| {
                inv.issues
                    .iter()
                    .filter(|i| i.id == "broken-links" && !i.dismissed)
                    .map(|i| i.skills.len().max(1))
                    .sum()
            })
            .unwrap_or(0),
        unreadable_configs: snapshot.present().filter(|c| c.error.is_some()).count(),
        conflicting_servers: super::scan::drift(snapshot)
            .iter()
            .filter(|d| d.kind == "conflict")
            .count(),
    }
}

fn parse_agent(value: Option<&str>) -> Result<Option<Agent>, CliError> {
    match value {
        None => Ok(None),
        Some("claude-code" | "claude") => Ok(Some(Agent::ClaudeCode)),
        Some("codex") => Ok(Some(Agent::Codex)),
        Some(other) => {
            Err(CliError::new(format!("unknown agent '{other}'")).hint("use claude-code or codex"))
        }
    }
}

pub fn command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let action = args.positionals[0].as_str();
    let agent = parse_agent(args.value("agent"))?;
    match action {
        "check" => {
            // Never fail or block the session that runs this.
            let project = ctx.project_str();
            let snapshot = Snapshot::read(project.as_deref());
            let h = health(&snapshot);
            if ctx.json {
                let _ = ctx.emit_json(&h);
            } else if let Some(line) = h.line() {
                super::out(&line);
            }
            Ok(EXIT_OK)
        }
        "install" | "uninstall" => {
            let targets = targets(agent);
            if targets.is_empty() {
                return Err(CliError::new(match agent {
                    Some(a) => format!("{} isn't set up on this machine", a.label()),
                    None => "neither Claude Code nor Codex is set up on this machine".into(),
                }));
            }
            let install = action == "install";
            let (cmd, cmd_note) = hook_command();
            let mut notes: Vec<String> = cmd_note.into_iter().collect();

            // Plan every file first, so nothing is written before the whole change is shown.
            let planned: Vec<Planned> = targets
                .into_iter()
                .map(|(a, path)| plan_target(a, path, install, &cmd))
                .collect();
            let pending = planned.iter().filter(|p| p.pending()).count();
            let dry_run = args.has("dry-run");
            if pending > 0 && !dry_run && !args.has("yes") {
                let s = &ctx.style;
                if !ctx.interactive || ctx.json {
                    return Err(CliError::new(format!(
                        "this {} {}",
                        if install {
                            "adds a session start hook to"
                        } else {
                            "removes the hook from"
                        },
                        format::plural(pending, "settings file", "settings files")
                    ))
                    .hint("run with --yes to apply, or --dry-run to preview"));
                }
                let mut text = String::new();
                for p in planned.iter().filter(|p| p.pending()) {
                    let hooks_on = s.dim(", and [features] hooks = true in config.toml");
                    text.push_str(&format!(
                        "  {} {} {}{}\n",
                        s.cyan(if install { "+" } else { "-" }),
                        p.agent.label(),
                        s.dim(&p.shown),
                        match (&p.change, &p.codex_config) {
                            (Ok(None), Some(_)) => s.dim(
                                " already has the hook; turn on [features] hooks = true in config.toml"
                            ),
                            (_, Some(_)) => hooks_on,
                            _ => String::new(),
                        }
                    ));
                }
                if install {
                    text.push_str(&format!(
                        "  {}\n",
                        s.dim(&format!("runs '{cmd}' at session start"))
                    ));
                }
                eprint!("{text}");
                let question = if install {
                    "Install the hook?"
                } else {
                    "Remove the hook?"
                };
                if !ctx.confirm(question) {
                    super::out("Nothing changed.\n");
                    return Ok(EXIT_OK);
                }
            }

            let mut results = Vec::new();
            for p in planned {
                let a = p.agent;
                results.push((a, apply_target(p, install, dry_run, &mut notes)));
            }
            let failed = results.iter().any(|(_, r)| r.is_err());
            if ctx.json {
                ctx.emit_json(&serde_json::json!({
                    "action": action,
                    "dryRun": dry_run,
                    "command": cmd,
                    "results": results.iter().map(|(a, r)| serde_json::json!({
                        "agent": a,
                        "ok": r.is_ok(),
                        "message": match r { Ok(m) | Err(m) => m },
                    })).collect::<Vec<_>>(),
                    "notes": notes,
                }))?;
            } else {
                let s = &ctx.style;
                let mut text = String::new();
                for (_, r) in &results {
                    match r {
                        Ok(m) => text.push_str(&format!(
                            "{} {m}\n",
                            if dry_run {
                                s.mark(Severity::Info)
                            } else {
                                s.mark(Severity::Ok)
                            }
                        )),
                        Err(m) => text.push_str(&format!("{} {m}\n", s.mark(Severity::Error))),
                    }
                }
                for n in &notes {
                    text.push_str(&format!("  {}\n", s.dim(n)));
                }
                if install && !failed {
                    text.push_str(&format!(
                        "  {}\n",
                        s.dim(&format!(
                            "{} '{cmd}' at session start: silent when healthy, one line when not.",
                            if dry_run { "Would run" } else { "Runs" }
                        ))
                    ));
                }
                if dry_run && pending > 0 {
                    text.push_str(&format!(
                        "  {}\n",
                        s.hint(&format!(
                            "run {} to apply",
                            s.cmd(&format!("toolport hook {action} --yes"))
                        ))
                    ));
                }
                super::out(&text);
            }
            Ok(if failed { super::EXIT_ERROR } else { EXIT_OK })
        }
        other => Err(CliError::new(format!("unknown hook action '{other}'"))
            .hint("use install, uninstall or check")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_preserves_comments_and_other_hooks_and_uninstall_restores() {
        let original = r#"{
  // my settings
  "model": "opus",
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "echo mine" } ] }
    ],
    "PostToolUse": [
      { "hooks": [ { "type": "command", "command": "conduit --toolport-hook post" } ] }
    ]
  }
}"#;
        assert!(!installed_in(original));
        let added = add_hook(original, Agent::ClaudeCode, "toolport hook check").unwrap();
        assert!(added.contains("// my settings"));
        assert!(added.contains("echo mine"));
        assert!(installed_in(&added));
        let v: serde_json::Value = json5::from_str(&added).unwrap();
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
        assert_eq!(v["hooks"]["SessionStart"][1]["matcher"], "startup");
        let (removed, n) = remove_hook(&added).unwrap();
        assert_eq!(n, 1);
        assert!(!installed_in(&removed));
        assert!(removed.contains("echo mine"), "{removed}");
        // The retired marker is not ours: it is left for crate::hooks to clean up.
        assert!(removed.contains("--toolport-hook"));
        let v: serde_json::Value = json5::from_str(&removed).unwrap();
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn uninstall_prunes_only_what_it_emptied() {
        let added = add_hook("{}", Agent::Codex, "\"/opt/toolport\" hook check").unwrap();
        let v: serde_json::Value = json5::from_str(&added).unwrap();
        assert!(v["hooks"]["SessionStart"][0].get("matcher").is_none());
        let (removed, n) = remove_hook(&added).unwrap();
        assert_eq!(n, 1);
        let v: serde_json::Value = json5::from_str(&removed).unwrap();
        assert!(v.get("hooks").is_none(), "{removed}");
        let user_empty = r#"{"hooks": {"SessionStart": []}}"#;
        let (same, n) = remove_hook(user_empty).unwrap();
        assert_eq!(n, 0);
        assert_eq!(same, user_empty);
    }

    fn scratch(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "toolport-hook-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_launcher_in_the_npx_cache_on_path_is_never_saved() {
        let root = scratch("npx-path");
        let launcher = |dir: PathBuf| {
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join(if cfg!(windows) {
                "toolport.cmd"
            } else {
                "toolport"
            });
            std::fs::write(&file, "#!/usr/bin/env node\n// @toolport/cli launcher\n").unwrap();
            file
        };
        let cached = launcher(root.join(".npm/_npx/4f1c/node_modules/.bin"));
        let cache_dir = launcher(root.join("npm-cache/bin"));
        let global = launcher(root.join("global/bin"));
        assert!(is_ephemeral(&cached) && is_ephemeral(&cache_dir));
        assert!(!is_ephemeral(&global));
        let path = |dirs: &[&PathBuf]| {
            std::env::join_paths(dirs.iter().map(|f| f.parent().unwrap())).unwrap()
        };
        // npx puts its cache first on PATH while it runs; the global install behind it wins.
        assert_eq!(
            cli_on_path(&path(&[&cached, &cache_dir, &global])),
            Some(global.clone())
        );
        assert_eq!(cli_on_path(&path(&[&cached, &cache_dir])), None);
        // A PATH entry that only links into the cache is the cache too.
        #[cfg(unix)]
        {
            let linked = root.join("linked/bin");
            std::fs::create_dir_all(&linked).unwrap();
            std::os::unix::fs::symlink(&cached, linked.join("toolport")).unwrap();
            assert!(is_ephemeral(&linked.join("toolport")));
            assert_eq!(cli_on_path(linked.as_os_str()), None);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_retry_turns_hooks_on_when_the_entry_is_already_there() {
        let _data = crate::registry::DataDirTestEnv::new("hook-codex-retry");
        let codex = scratch("codex-home");
        let hooks = codex.join("hooks.json");
        let config = codex.join("config.toml");
        let cmd = "\"/opt/toolport\" hook check";
        // An earlier install wrote hooks.json, then stopped before config.toml.
        let entry = add_hook("{}", Agent::Codex, cmd).unwrap();
        std::fs::write(&hooks, &entry).unwrap();
        std::fs::write(
            &config,
            "# mine\nmodel = \"o3\"\n\n[features]\nhooks = false\n",
        )
        .unwrap();

        let plan = plan_target(Agent::Codex, hooks.clone(), true, cmd);
        assert!(matches!(plan.change, Ok(None)));
        assert!(plan.pending(), "hooks are still off, so there is work left");
        let mut notes = Vec::new();
        let dry = apply_target(plan, true, true, &mut notes).unwrap();
        assert!(dry.contains("would turn on [features] hooks"), "{dry}");
        assert!(std::fs::read_to_string(&config)
            .unwrap()
            .contains("hooks = false"));

        let plan = plan_target(Agent::Codex, hooks.clone(), true, cmd);
        let done = apply_target(plan, true, false, &mut notes).unwrap();
        assert!(
            done.contains("already installed") && done.contains("turned on [features] hooks"),
            "{done}"
        );
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(
            text.contains("# mine") && text.contains("model = \"o3\""),
            "{text}"
        );
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert!(codex_hooks_on(&doc), "{text}");
        assert_eq!(
            std::fs::read_to_string(&hooks).unwrap(),
            entry,
            "hooks.json untouched"
        );

        // Now everything is in place: nothing left to do, and it says so.
        let plan = plan_target(Agent::Codex, hooks.clone(), true, cmd);
        assert!(!plan.pending());
        let again = apply_target(plan, true, false, &mut notes).unwrap();
        assert!(
            again.contains("already installed") && !again.contains("turned on"),
            "{again}"
        );
        let _ = std::fs::remove_dir_all(&codex);
    }

    #[test]
    fn markers_never_collide() {
        assert!(is_ours("toolport hook check"));
        assert!(is_ours("\"C:\\Tools\\toolport.exe\" hook check"));
        assert!(is_ours("npx --yes @toolport/cli hook check"));
        assert!(!is_ours("conduit --toolport-hook session-start"));
        assert!(!is_ours("my-tool hook check"));
        assert_eq!(
            Health::default().line(),
            None,
            "a healthy machine prints nothing"
        );
        let h = Health {
            plaintext_secrets: 2,
            broken_skill_links: 1,
            ..Default::default()
        };
        assert_eq!(
            h.line().unwrap(),
            "toolport: 2 plaintext secrets on disk, 1 broken skill link (run 'toolport scan')"
        );
    }
}
