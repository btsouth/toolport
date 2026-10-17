//! Skills and rules inventory: what each installed AI client actually loads besides MCP tools.
//!
//! Toolport does not own any skill or rule content. This module reads the folders each client
//! scans for `SKILL.md` files, applies that client's own off switch (Claude Code `skillOverrides`,
//! Codex `[[skills.config]]`, Copilot CLI `disabledSkills`, OpenCode skill permissions, Hermes and
//! Gemini CLI `skills.disabled`), and reports the effective state per client. Writes only ever go
//! through those same native settings, with a backup first.
//!
//! The per-client table below was verified on 2026-10-17 against the clients' own listing commands
//! (`codex debug prompt-input`, `copilot skill list`, `grok inspect --json`, `hermes skills list`)
//! or their shipped code (Cursor agent, Gemini CLI, OpenCode). Rows marked `verified: false` come
//! from published docs and are labelled that way in the UI.

use crate::i18n::{msg, tr, trf};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Deepest folder level a recursive client walks below a skill root. Cursor stops at 10; the
/// claude.ai sync layout (`synced/<bucket>/<skill>/SKILL.md`) only needs 3.
const MAX_DEPTH: usize = 6;
/// Claude Code truncates skill descriptions past this many characters.
pub const DESCRIPTION_CAP: usize = 1536;
/// Largest SKILL.md or rules file read into memory.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const PREVIEW_CHARS: usize = 4000;

/// Where clients keep their files. Built from the real environment in production and by hand in
/// tests, so scans never depend on process-global env vars.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    /// `home` with symlinks resolved, so canonical skill paths still count as inside it.
    pub home_real: PathBuf,
    pub config: PathBuf,
    /// Claude Code's own folder (`CLAUDE_CONFIG_DIR`, default `~/.claude`).
    pub claude: PathBuf,
    /// The `~/.claude` folder other clients read for compatibility. They don't follow
    /// `CLAUDE_CONFIG_DIR`.
    pub claude_compat: PathBuf,
    pub codex: PathBuf,
    pub copilot: PathBuf,
    pub gemini: PathBuf,
    pub grok: PathBuf,
    pub hermes: PathBuf,
    pub opencode: PathBuf,
}

impl Env {
    pub fn from_home(home: &Path) -> Self {
        let config = home.join(".config");
        Env {
            home: home.to_path_buf(),
            home_real: canon(home).unwrap_or_else(|_| home.to_path_buf()),
            claude: home.join(".claude"),
            claude_compat: home.join(".claude"),
            codex: home.join(".codex"),
            copilot: home.join(".copilot"),
            gemini: home.join(".gemini"),
            grok: home.join(".grok"),
            hermes: home.join(".hermes"),
            opencode: config.join("opencode"),
            config,
        }
    }

    pub fn current() -> Option<Self> {
        let home = dirs::home_dir()?;
        let mut env = Env::from_home(&home);
        let abs = |key: &str| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(dir) = abs("XDG_CONFIG_HOME") {
            env.opencode = dir.join("opencode");
            env.config = dir;
        }
        if let Some(dir) = abs("CLAUDE_CONFIG_DIR") {
            env.claude = dir;
        }
        if let Some(dir) = abs("CODEX_HOME") {
            env.codex = dir;
        }
        if let Some(dir) = abs("COPILOT_HOME") {
            env.copilot = dir;
        }
        if let Some(dir) = abs("GEMINI_CLI_HOME") {
            env.gemini = dir.join(".gemini");
        }
        if let Some(dir) = abs("GROK_HOME") {
            env.grok = dir;
        }
        if let Some(dir) = abs("HERMES_HOME") {
            env.hermes = dir;
        }
        Some(env)
    }

    fn agents_skills(&self) -> PathBuf {
        self.home.join(".agents").join("skills")
    }

    fn parked(&self) -> PathBuf {
        self.home.join(".agents").join("disabled-skills")
    }

    /// `~`-relative display form of a path under home.
    pub fn display(&self, path: &Path) -> String {
        for base in [&self.home, &self.home_real] {
            if let Ok(rest) = path.strip_prefix(base) {
                return if rest.as_os_str().is_empty() {
                    "~".into()
                } else {
                    format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
                };
            }
        }
        path.display().to_string()
    }
}

// ---------------------------------------------------------------------------------------------
// Client table
// ---------------------------------------------------------------------------------------------

/// How a client resolves two skill folders that reach the same skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dedup {
    /// One entry per real SKILL.md; a later root wins a name clash (Cursor).
    RealPathLaterWins,
    /// One entry per name; the first root in order wins (most clients).
    NameFirstWins,
}

/// The client's own per-skill off switch, if Toolport knows how to read and write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Switch {
    ClaudeOverrides,
    CodexConfig,
    CopilotSettings,
    OpencodePermissions,
    HermesConfig,
    GeminiSettings,
    None(&'static str),
}

#[derive(Debug, Clone)]
struct Root {
    path: PathBuf,
    recursive: bool,
    label: &'static str,
}

impl Root {
    fn flat(path: PathBuf, label: &'static str) -> Self {
        Root {
            path,
            recursive: false,
            label,
        }
    }
    fn deep(path: PathBuf, label: &'static str) -> Self {
        Root {
            path,
            recursive: true,
            label,
        }
    }
}

#[derive(Debug, Clone)]
struct RuleSource {
    path: PathBuf,
    /// Whether the client expands `@path` import lines.
    imports: bool,
    /// Only used when no earlier source in the list exists.
    fallback: bool,
}

impl RuleSource {
    fn file(path: PathBuf, imports: bool) -> Self {
        RuleSource {
            path,
            imports,
            fallback: false,
        }
    }
}

struct AgentDef {
    id: &'static str,
    name: &'static str,
    verified: bool,
    dedup: Dedup,
    switch: Switch,
    roots: fn(&Env) -> Vec<Root>,
    project_roots: &'static [&'static str],
    rules: fn(&Env) -> Vec<RuleSource>,
    project_rules: &'static [&'static str],
    /// Shown when the client keeps rules somewhere Toolport cannot read (app settings, cloud).
    rules_note: Option<&'static str>,
}

fn no_rules(_: &Env) -> Vec<RuleSource> {
    Vec::new()
}

/// Enabled plugin skill folders under a plugin cache laid out as
/// `<cache>/<marketplace>/<plugin>/<version>/skills`. The newest version folder wins.
fn plugin_skill_roots(cache: &Path, enabled: &[String], label: &'static str) -> Vec<Root> {
    let mut roots = Vec::new();
    for key in enabled {
        let Some((plugin, market)) = key.split_once('@') else {
            continue;
        };
        let dir = cache.join(market).join(plugin);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let newest = entries
            .flatten()
            .filter(|e| e.path().join("skills").is_dir())
            .max_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            });
        if let Some(entry) = newest {
            roots.push(Root::flat(entry.path().join("skills"), label));
        }
    }
    roots
}

fn claude_enabled_plugins(env: &Env) -> Vec<String> {
    read_json(&env.claude.join("settings.json"))
        .and_then(|v| v.get("enabledPlugins").cloned())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter(|(_, on)| on.as_bool() == Some(true))
                .map(|(k, _)| k)
                .collect()
        })
        .unwrap_or_default()
}

fn codex_enabled_plugins(env: &Env) -> Vec<String> {
    read_toml(&env.codex.join("config.toml"))
        .and_then(|v| v.get("plugins").cloned())
        .and_then(|v| v.as_table().cloned())
        .map(|t| {
            t.into_iter()
                .filter(|(_, v)| v.get("enabled").and_then(|e| e.as_bool()) != Some(false))
                .map(|(k, _)| k)
                .collect()
        })
        .unwrap_or_default()
}

/// Newest Copilot CLI package cache folder with bundled skills.
fn copilot_builtin_root(env: &Env) -> Option<PathBuf> {
    let base = env.home.join(".cache").join("copilot").join("pkg");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for platform in std::fs::read_dir(&base).ok()?.flatten() {
        for version in std::fs::read_dir(platform.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let dir = version.path().join("builtin-skills");
            if !dir.is_dir() {
                continue;
            }
            let modified = version
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                best = Some((modified, dir));
            }
        }
    }
    best.map(|(_, dir)| dir)
}

const AGENTS_MD: &[&str] = &["AGENTS.md"];

fn agent_defs() -> Vec<AgentDef> {
    vec![
        AgentDef {
            id: "claude-code",
            name: "Claude Code",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::ClaudeOverrides,
            roots: |env| {
                let mut roots = vec![Root::deep(env.claude.join("skills"), msg("Claude Code skills"))];
                roots.extend(plugin_skill_roots(
                    &env.claude.join("plugins").join("cache"),
                    &claude_enabled_plugins(env),
                    msg("Claude Code plugin"),
                ));
                roots
            },
            project_roots: &[".claude/skills"],
            rules: |env| {
                let mut rules = vec![RuleSource::file(env.claude.join("CLAUDE.md"), true)];
                rules.extend(md_files(&env.claude.join("rules")).into_iter().map(|p| RuleSource::file(p, true)));
                rules
            },
            project_rules: &["CLAUDE.md", "CLAUDE.local.md"],
            rules_note: None,
        },
        AgentDef {
            id: "codex",
            name: "Codex",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::CodexConfig,
            roots: |env| {
                let mut roots = vec![
                    Root::flat(env.codex.join("skills"), msg("Codex skills")),
                    Root::flat(env.agents_skills(), msg("shared ~/.agents/skills")),
                    Root::flat(env.codex.join("skills").join(".system"), msg("Codex built-in")),
                ];
                roots.extend(plugin_skill_roots(
                    &env.codex.join("plugins").join("cache"),
                    &codex_enabled_plugins(env),
                    msg("Codex plugin"),
                ));
                roots
            },
            project_roots: &[".agents/skills", ".codex/skills"],
            rules: |env| {
                vec![
                    RuleSource::file(env.codex.join("AGENTS.override.md"), false),
                    RuleSource { fallback: true, ..RuleSource::file(env.codex.join("AGENTS.md"), false) },
                ]
            },
            project_rules: AGENTS_MD,
            rules_note: None,
        },
        AgentDef {
            id: "cursor",
            name: "Cursor",
            verified: true,
            dedup: Dedup::RealPathLaterWins,
            switch: Switch::None(msg("Cursor has no per-skill switch. Its \"Include third-party plugins, skills, and other configs\" setting turns off the Claude Code, Codex and Grok folders all at once.")),
            roots: |env| {
                vec![
                    Root::flat(env.home.join(".cursor").join("skills-cursor"), msg("Cursor built-in")),
                    Root::deep(env.home.join(".cursor").join("skills"), msg("Cursor skills")),
                    Root::deep(env.claude_compat.join("skills"), msg("Claude Code folder")),
                    Root::deep(env.codex.join("skills"), msg("Codex folder")),
                    Root::deep(env.grok.join("skills"), msg("Grok folder")),
                    Root::deep(env.agents_skills(), msg("shared ~/.agents/skills")),
                ]
            },
            project_roots: &[".cursor/skills", ".claude/skills", ".codex/skills", ".grok/skills", ".agents/skills"],
            rules: no_rules,
            project_rules: &["AGENTS.md", "CLAUDE.md", ".cursorrules"],
            rules_note: Some(msg("Cursor keeps your user rules in its app settings, not in a file.")),
        },
        AgentDef {
            id: "github-copilot-cli",
            name: "Copilot CLI",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::CopilotSettings,
            roots: |env| {
                let mut roots = vec![
                    Root::flat(env.copilot.join("skills"), msg("Copilot skills")),
                    Root::flat(env.agents_skills(), msg("shared ~/.agents/skills")),
                ];
                if let Some(dir) = copilot_builtin_root(env) {
                    roots.push(Root::flat(dir, msg("Copilot built-in")));
                }
                roots
            },
            project_roots: &[".github/skills", ".agents/skills", ".claude/skills"],
            rules: |env| vec![RuleSource::file(env.copilot.join("copilot-instructions.md"), false)],
            project_rules: &["AGENTS.md", "CLAUDE.md", ".github/copilot-instructions.md"],
            rules_note: None,
        },
        AgentDef {
            id: "opencode",
            name: "OpenCode",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::OpencodePermissions,
            roots: |env| {
                vec![
                    Root::deep(env.opencode.join("skills"), msg("OpenCode skills")),
                    Root::deep(env.opencode.join("skill"), msg("OpenCode skills")),
                    Root::deep(env.claude_compat.join("skills"), msg("Claude Code folder")),
                    Root::deep(env.agents_skills(), msg("shared ~/.agents/skills")),
                ]
            },
            project_roots: &[".opencode/skills", ".claude/skills", ".agents/skills"],
            rules: |env| {
                vec![
                    RuleSource::file(env.opencode.join("AGENTS.md"), false),
                    RuleSource { fallback: true, ..RuleSource::file(env.claude_compat.join("CLAUDE.md"), false) },
                ]
            },
            project_rules: &["AGENTS.md", "CLAUDE.md"],
            rules_note: None,
        },
        AgentDef {
            id: "grok",
            name: "Grok",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::None(msg("Turn skills off with /skills inside Grok. Toolport can't change that setting yet.")),
            roots: |env| {
                vec![
                    Root::deep(env.grok.join("skills"), msg("Grok skills")),
                    Root::deep(env.agents_skills(), msg("shared ~/.agents/skills")),
                    // `grok inspect` shows bundled skills beating same-named Claude ones.
                    Root::flat(env.grok.join("bundled").join("skills"), msg("Grok built-in")),
                    Root::deep(env.claude_compat.join("skills"), msg("Claude Code folder")),
                ]
            },
            project_roots: &[".grok/skills", ".claude/skills", ".cursor/skills"],
            rules: |env| {
                vec![
                    RuleSource::file(env.grok.join("AGENTS.md"), false),
                    RuleSource::file(env.claude_compat.join("CLAUDE.md"), false),
                ]
            },
            project_rules: &["AGENTS.md", "CLAUDE.md"],
            rules_note: None,
        },
        AgentDef {
            id: "hermes",
            name: "Hermes",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::HermesConfig,
            roots: |env| vec![Root::deep(env.hermes.join("skills"), msg("Hermes skills"))],
            project_roots: &[".hermes/skills", ".agents/skills"],
            rules: |env| vec![RuleSource::file(env.hermes.join("SOUL.md"), false)],
            project_rules: &[],
            rules_note: None,
        },
        AgentDef {
            id: "gemini-cli",
            name: "Gemini CLI",
            verified: true,
            dedup: Dedup::NameFirstWins,
            switch: Switch::GeminiSettings,
            roots: |env| {
                vec![
                    Root::flat(env.agents_skills(), msg("shared ~/.agents/skills")),
                    Root::flat(env.gemini.join("skills"), msg("Gemini skills")),
                ]
            },
            project_roots: &[".agents/skills", ".gemini/skills"],
            rules: |env| vec![RuleSource::file(env.gemini.join("GEMINI.md"), true)],
            project_rules: &["GEMINI.md"],
            rules_note: None,
        },
        docs_agent("antigravity", "Antigravity", |env| vec![Root::flat(env.gemini.join("antigravity").join("skills"), msg("Antigravity skills"))], |env| vec![RuleSource::file(env.gemini.join("GEMINI.md"), true)], &["GEMINI.md", "AGENTS.md"]),
        docs_agent("vscode", "VS Code", |env| vec![Root::flat(env.copilot.join("skills"), msg("Copilot skills")), Root::flat(env.agents_skills(), msg("shared ~/.agents/skills")), Root::flat(env.claude_compat.join("skills"), msg("Claude Code folder"))], |env| md_files(&env.claude_compat.join("rules")).into_iter().map(|p| RuleSource::file(p, false)).collect(), &["AGENTS.md", ".github/copilot-instructions.md"]),
        docs_agent("amp", "Amp", |env| vec![Root::flat(env.config.join("agents").join("skills"), msg("Amp skills"))], |env| vec![RuleSource::file(env.config.join("amp").join("AGENTS.md"), false)], AGENTS_MD),
        docs_agent("cline", "Cline", |env| vec![Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], |env| md_files(&env.home.join("Documents").join("Cline").join("Rules")).into_iter().map(|p| RuleSource::file(p, false)).collect(), &[".clinerules", "AGENTS.md"]),
        docs_agent("continue", "Continue", |env| vec![Root::flat(env.home.join(".continue").join("skills"), msg("Continue skills"))], no_rules, AGENTS_MD),
        docs_agent("crush", "Crush", |env| vec![Root::flat(env.config.join("crush").join("skills"), msg("Crush skills"))], no_rules, &["AGENTS.md", "CRUSH.md"]),
        docs_agent("devin-cli", "Devin CLI", |env| vec![Root::flat(env.config.join("devin").join("skills"), msg("Devin skills"))], |env| vec![RuleSource::file(env.config.join("devin").join("AGENTS.md"), false)], AGENTS_MD),
        docs_agent("droid", "Factory Droid", |env| vec![Root::flat(env.home.join(".factory").join("skills"), msg("Droid skills"))], |env| vec![RuleSource::file(env.home.join(".factory").join("AGENTS.md"), false)], AGENTS_MD),
        docs_agent("goose", "Goose", |env| vec![Root::flat(env.config.join("goose").join("skills"), msg("Goose skills"))], |env| vec![RuleSource::file(env.config.join("goose").join(".goosehints"), false)], &[".goosehints", "AGENTS.md"]),
        docs_agent("junie", "Junie", |env| vec![Root::flat(env.home.join(".junie").join("skills"), msg("Junie skills"))], no_rules, &[".junie/guidelines.md", "AGENTS.md"]),
        docs_agent("kilo-code", "Kilo Code", |env| vec![Root::flat(env.home.join(".kilo").join("skills"), msg("Kilo skills"))], no_rules, AGENTS_MD),
        docs_agent("kimi-code", "Kimi Code", |env| vec![Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], no_rules, AGENTS_MD),
        docs_agent("kiro", "Kiro", |env| vec![Root::flat(env.home.join(".kiro").join("skills"), msg("Kiro skills"))], |env| md_files(&env.home.join(".kiro").join("steering")).into_iter().map(|p| RuleSource::file(p, false)).collect(), AGENTS_MD),
        docs_agent("pi", "Pi", |env| vec![Root::flat(env.home.join(".pi").join("agent").join("skills"), msg("Pi skills")), Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], |env| vec![RuleSource::file(env.home.join(".pi").join("agent").join("AGENTS.md"), false)], AGENTS_MD),
        docs_agent("omp", "Oh My Pi", |env| vec![Root::flat(env.home.join(".omp").join("agent").join("skills"), msg("Oh My Pi skills")), Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], |env| vec![RuleSource::file(env.home.join(".omp").join("agent").join("AGENTS.md"), false)], AGENTS_MD),
        docs_agent("qwen-code", "Qwen Code", |env| vec![Root::flat(env.home.join(".qwen").join("skills"), msg("Qwen skills"))], |env| vec![RuleSource::file(env.home.join(".qwen").join("QWEN.md"), false)], &["QWEN.md", "AGENTS.md"]),
        docs_agent("roo-code", "Roo Code", |env| vec![Root::flat(env.home.join(".roo").join("skills"), msg("Roo skills"))], |env| md_files(&env.home.join(".roo").join("rules")).into_iter().map(|p| RuleSource::file(p, false)).collect(), AGENTS_MD),
        docs_agent("warp", "Warp", |env| vec![Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], no_rules, &["AGENTS.md", "WARP.md"]),
        docs_agent("windsurf", "Devin Desktop", |env| vec![Root::flat(env.home.join(".codeium").join("windsurf").join("skills"), msg("Windsurf skills"))], |env| vec![RuleSource::file(env.home.join(".codeium").join("windsurf").join("memories").join("global_rules.md"), false)], AGENTS_MD),
        docs_agent("zcode", "ZCode", |env| vec![Root::flat(env.home.join(".zcode").join("skills"), msg("ZCode skills"))], no_rules, AGENTS_MD),
        docs_agent("zed", "Zed", |env| vec![Root::flat(env.agents_skills(), msg("shared ~/.agents/skills"))], |env| vec![RuleSource::file(env.config.join("zed").join("AGENTS.md"), false)], AGENTS_MD),
    ]
}

/// A client whose folders come from published docs, not a check against a real install.
fn docs_agent(
    id: &'static str,
    name: &'static str,
    roots: fn(&Env) -> Vec<Root>,
    rules: fn(&Env) -> Vec<RuleSource>,
    project_rules: &'static [&'static str],
) -> AgentDef {
    AgentDef {
        id,
        name,
        verified: false,
        dedup: Dedup::NameFirstWins,
        switch: Switch::None(msg("Toolport doesn't know this client's off switch yet.")),
        roots,
        project_roots: &[".agents/skills"],
        rules,
        project_rules,
        rules_note: None,
    }
}

/// Clients Toolport detects that have no file-based skills.
const NO_SKILLS: &[(&str, &str)] = &[
    ("claude-desktop", "Claude Desktop"),
    ("lm-studio", "LM Studio"),
    ("jan", "Jan"),
    ("boltai", "BoltAI"),
    ("anythingllm", "AnythingLLM"),
    ("witsy", "Witsy"),
    ("amazon-q", "Amazon Q"),
];

fn md_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == "md" || x == "mdc")
                && p.is_file()
        })
        .collect();
    files.sort();
    files
}

// ---------------------------------------------------------------------------------------------
// Public model
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub agents: Vec<Agent>,
    pub skills: Vec<Skill>,
    pub rules: Vec<RuleFile>,
    pub issues: Vec<Issue>,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// False for clients without file-based skills.
    pub supports_skills: bool,
    /// False when the folders come from docs rather than a check against a real install.
    pub verified: bool,
    /// True when Toolport can write the client's own per-skill switch.
    pub can_toggle: bool,
    pub switch_note: String,
    pub roots: Vec<AgentRoot>,
    pub rules_note: Option<String>,
    /// Global rules files this client would read, whether or not they exist yet.
    pub rule_locations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRoot {
    pub path: String,
    pub label: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Yours,
    Package,
    Account,
    BuiltIn,
    Library,
    Parked,
    Broken,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    On,
    NameOnly,
    Off,
    /// Another file with the same name wins in this client.
    Shadowed,
    Broken,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// Stable id: the canonical SKILL.md path, or the link path for a broken link.
    pub key: String,
    pub name: String,
    pub description: String,
    pub description_chars: usize,
    /// Display path of the real SKILL.md (or the dangling link).
    pub path: String,
    /// Absolute folder holding the skill, for Reveal.
    pub folder: String,
    pub source: Source,
    pub source_note: String,
    pub editable: bool,
    pub size_bytes: u64,
    pub lines: usize,
    pub modified_secs: Option<u64>,
    pub git_repo: Option<String>,
    /// Every path that reaches this skill (links and the real folder).
    pub links: Vec<String>,
    /// Effective state per client id. Clients that don't load it are absent.
    pub states: BTreeMap<String, SkillState>,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillState {
    pub state: State,
    pub via: String,
    /// The name this client's own setting uses, when it differs from the skill name
    /// (Claude Code plugin skills are `plugin:skill`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuleFile {
    pub key: String,
    pub name: String,
    pub path: String,
    pub scope: String,
    pub size_bytes: u64,
    pub preview: String,
    /// Load chain per client id: the file the client opens first, then links and imports.
    pub agents: BTreeMap<String, Vec<ChainStep>>,
}

/// One hop in how a client reaches a rules file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChainStep {
    /// `file` (opened directly), `link` (symlink target), `import` (followed `@path`) or
    /// `skipped-import` (an `@path` line this client doesn't follow).
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub id: String,
    pub level: String,
    pub title: String,
    pub detail: String,
    pub agents: Vec<String>,
    /// Skill keys the issue covers; fixes act on these.
    pub skills: Vec<String>,
    /// Display names for `skills`, without repeats.
    pub skill_names: Vec<String>,
    pub fix_label: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// `canonicalize` without Windows' `\\?\` prefix, so results compare with ordinary paths.
fn canon(path: &Path) -> std::io::Result<PathBuf> {
    let real = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let text = real.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(real)
}

fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = read_text(path)?;
    json5::from_str(&text).ok()
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    toml::from_str(&read_text(path)?).ok()
}

fn read_yaml(path: &Path) -> Option<serde_yaml::Value> {
    serde_yaml::from_str(&read_text(path)?).ok()
}

/// `name` and `description` from a SKILL.md YAML front matter block.
fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let body = text.trim_start_matches('\u{feff}');
    let Some(rest) = body.strip_prefix("---") else {
        return (None, None);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None);
    };
    let yaml = &rest[..end];
    let parsed: Option<serde_yaml::Value> = serde_yaml::from_str(yaml).ok();
    let get = |key: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| loose_field(yaml, key))
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
    };
    (get("name"), get("description"))
}

/// Read `key: value` from front matter that isn't valid YAML, such as an unquoted
/// description containing `: `. Agents read these leniently, so Toolport does too.
fn loose_field(yaml: &str, key: &str) -> Option<String> {
    let mut lines = yaml.lines();
    while let Some(line) = lines.next() {
        let Some(value) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        let value = value.trim();
        if value.starts_with('|') || value.starts_with('>') || value.is_empty() {
            let block: Vec<&str> = lines
                .take_while(|l| l.is_empty() || l.starts_with([' ', '\t']))
                .map(str::trim)
                .collect();
            return Some(block.join(" "));
        }
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        return Some(unquoted.to_string());
    }
    None
}

fn preview(text: &str) -> String {
    text.chars().take(PREVIEW_CHARS).collect()
}

/// One SKILL.md reached through one root.
#[derive(Debug, Clone)]
struct Found {
    /// The path as the client sees it (may go through links).
    link: PathBuf,
    /// Canonical SKILL.md path, or None for a dangling link.
    real: Option<PathBuf>,
    root_label: &'static str,
    root_path: PathBuf,
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn walk_root(root: &Root) -> Vec<Found> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    walk_dir(root, &root.path, 0, &mut seen, &mut out);
    out
}

fn walk_dir(
    root: &Root,
    dir: &Path,
    depth: usize,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<Found>,
) {
    let Ok(real_dir) = canon(dir) else {
        return;
    };
    if !seen.insert(real_dir) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || matches!(name, "node_modules" | "__pycache__") {
            continue;
        }
        if is_link(&path) && !path.exists() {
            if depth == 0 {
                out.push(Found {
                    link: path.clone(),
                    real: None,
                    root_label: root.label,
                    root_path: root.path.clone(),
                });
            }
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if skill_md.is_file() {
            out.push(Found {
                real: canon(&skill_md).ok(),
                link: skill_md,
                root_label: root.label,
                root_path: root.path.clone(),
            });
        } else if root.recursive && depth + 1 < MAX_DEPTH {
            walk_dir(root, &path, depth + 1, seen, out);
        }
    }
}

// --- client switches ---------------------------------------------------------------------------

/// Per-name and per-path disables read from one client's settings.
#[derive(Debug, Default, Clone)]
struct Disabled {
    off: BTreeSet<String>,
    /// OpenCode's ordered skill rules (pattern, deny); the last match wins. When present they
    /// decide instead of `off`.
    rules: Vec<(String, bool)>,
    name_only: BTreeSet<String>,
    /// Absolute paths (SKILL.md or its folder) turned off by path.
    off_paths: BTreeSet<PathBuf>,
    /// Paths in the client's config that no longer exist.
    stale_paths: Vec<String>,
}

fn claude_settings_path(env: &Env) -> PathBuf {
    env.claude.join("settings.json")
}
fn codex_config_path(env: &Env) -> PathBuf {
    env.codex.join("config.toml")
}
fn copilot_settings_path(env: &Env) -> PathBuf {
    env.copilot.join("settings.json")
}
fn gemini_settings_path(env: &Env) -> PathBuf {
    env.gemini.join("settings.json")
}
fn hermes_config_path(env: &Env) -> PathBuf {
    env.hermes.join("config.yaml")
}
fn opencode_config_path(env: &Env) -> PathBuf {
    let jsonc = env.opencode.join("opencode.jsonc");
    if jsonc.is_file() && !env.opencode.join("opencode.json").is_file() {
        jsonc
    } else {
        env.opencode.join("opencode.json")
    }
}

fn string_list(value: Option<&serde_json::Value>) -> BTreeSet<String> {
    value
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn read_disabled(switch: Switch, env: &Env) -> Disabled {
    let mut d = Disabled::default();
    match switch {
        Switch::ClaudeOverrides => {
            if let Some(map) = read_json(&claude_settings_path(env))
                .and_then(|v| v.get("skillOverrides").cloned())
                .and_then(|v| v.as_object().cloned())
            {
                for (name, value) in map {
                    match value.as_str() {
                        Some("off") => {
                            d.off.insert(name);
                        }
                        Some("name-only") => {
                            d.name_only.insert(name);
                        }
                        _ => {}
                    }
                }
            }
        }
        Switch::CodexConfig => {
            let entries = read_toml(&codex_config_path(env))
                .and_then(|v| v.get("skills").and_then(|s| s.get("config")).cloned())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            for entry in entries {
                if entry.get("enabled").and_then(|e| e.as_bool()) != Some(false) {
                    continue;
                }
                let Some(path) = entry.get("path").and_then(|p| p.as_str()) else {
                    continue;
                };
                let path = PathBuf::from(path);
                if path.exists() {
                    d.off_paths.insert(canon(&path).unwrap_or(path.clone()));
                    d.off_paths.insert(path);
                } else {
                    d.stale_paths.push(path.display().to_string());
                }
            }
        }
        Switch::CopilotSettings => {
            let v = read_json(&copilot_settings_path(env));
            d.off = string_list(v.as_ref().and_then(|v| v.get("disabledSkills")));
        }
        Switch::GeminiSettings => {
            let v = read_json(&gemini_settings_path(env));
            d.off = string_list(
                v.as_ref()
                    .and_then(|v| v.get("skills"))
                    .and_then(|s| s.get("disabled")),
            );
        }
        Switch::HermesConfig => {
            if let Some(list) = read_yaml(&hermes_config_path(env))
                .as_ref()
                .and_then(|v| v.get("skills"))
                .and_then(|s| s.get("disabled"))
                .and_then(|l| l.as_sequence())
            {
                d.off = list
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
            }
        }
        Switch::OpencodePermissions => {
            let v = read_json(&opencode_config_path(env)).unwrap_or_default();
            d.off = opencode_denied(&v);
            d.rules = opencode_rules(&v);
        }
        Switch::None(_) => {}
    }
    d
}

/// OpenCode's skill rules in the order it applies them; the last matching rule wins. v2 uses
/// an ordered `permissions` list; v1 used a `permission.skill` map where an exact name beats a
/// pattern.
fn opencode_rules(config: &serde_json::Value) -> Vec<(String, bool)> {
    let mut rules = Vec::new();
    if let Some(map) = config
        .get("permission")
        .and_then(|p| p.get("skill"))
        .and_then(|s| s.as_object())
    {
        let entry = |(name, effect): (&String, &serde_json::Value)| {
            (name.clone(), effect.as_str() == Some("deny"))
        };
        rules.extend(map.iter().filter(|(n, _)| n.contains('*')).map(entry));
        rules.extend(map.iter().filter(|(n, _)| !n.contains('*')).map(entry));
    }
    if let Some(list) = config.get("permissions").and_then(|p| p.as_array()) {
        for rule in list {
            if rule.get("action").and_then(|a| a.as_str()) != Some("skill") {
                continue;
            }
            if let Some(resource) = rule.get("resource").and_then(|r| r.as_str()) {
                let deny = rule.get("effect").and_then(|e| e.as_str()) == Some("deny");
                rules.push((resource.to_string(), deny));
            }
        }
    }
    rules
}

/// Whether the last rule matching `name` denies it.
fn rules_deny(rules: &[(String, bool)], name: &str) -> bool {
    rules
        .iter()
        .rev()
        .find(|(pattern, _)| glob_match(pattern, name))
        .is_some_and(|(_, deny)| *deny)
}

/// `*` matches any run of characters, `?` one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(at) = star {
            pi = at + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Exact skill names an OpenCode config ends up denying.
fn opencode_denied(config: &serde_json::Value) -> BTreeSet<String> {
    let rules = opencode_rules(config);
    rules
        .iter()
        .filter(|(pattern, _)| !pattern.contains(['*', '?']))
        .filter(|(pattern, _)| rules_deny(&rules, pattern))
        .map(|(pattern, _)| pattern.clone())
        .collect()
}

// --- skill records -----------------------------------------------------------------------------

struct SkillFile {
    name: String,
    description: String,
    size: u64,
    lines: usize,
    modified: Option<u64>,
    preview: String,
}

fn read_skill_file(path: &Path) -> SkillFile {
    let text = read_text(path).unwrap_or_default();
    let (name, description) = front_matter(&text);
    let meta = std::fs::metadata(path).ok();
    SkillFile {
        name: name.unwrap_or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("skill")
                .to_string()
        }),
        description: description.unwrap_or_default(),
        size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        lines: text.lines().count(),
        modified: meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        preview: preview(&text),
    }
}

fn git_repo(dir: &Path, home: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d == home || d.parent().is_none() {
            return None;
        }
        if d.join(".git").exists() {
            return d.file_name().and_then(|n| n.to_str()).map(str::to_string);
        }
        cur = d.parent();
    }
    None
}

fn classify(real: &Path, env: &Env, project: Option<&Path>) -> (Source, String) {
    let under = |base: &Path| {
        canon(base).map(|b| real.starts_with(&b)).unwrap_or(false) || real.starts_with(base)
    };
    let text = real.to_string_lossy();
    if under(&env.parked()) {
        return (Source::Parked, tr("Parked in ~/.agents/disabled-skills"));
    }
    let inside =
        |base: &Path| real.starts_with(base) || canon(base).is_ok_and(|b| real.starts_with(b));
    if !inside(&env.home) && !project.is_some_and(inside) {
        return (
            Source::Package,
            tr("Installed by a system package; updates replace edits"),
        );
    }
    if under(&env.claude.join("skills").join("synced")) || text.contains("/skills/synced/") {
        return (Source::Account, tr("Synced from your claude.ai account"));
    }
    let builtin = [
        (
            env.codex.join("skills").join(".system"),
            msg("Bundled with Codex"),
        ),
        (
            env.codex.join("plugins").join("cache"),
            msg("From a Codex plugin"),
        ),
        (
            env.claude.join("plugins").join("cache"),
            msg("From a Claude Code plugin"),
        ),
        (
            env.home.join(".cursor").join("skills-cursor"),
            msg("Bundled with Cursor"),
        ),
        (env.grok.join("bundled"), msg("Bundled with Grok")),
        (
            env.home.join(".cache").join("copilot"),
            msg("Bundled with Copilot CLI"),
        ),
    ];
    for (base, note) in builtin {
        if under(&base) {
            return (Source::BuiltIn, crate::i18n::lookup(note));
        }
    }
    if under(&env.hermes.join("skills")) {
        return (Source::Library, tr("Hermes's own skill library"));
    }
    (Source::Yours, tr("Yours"))
}

// ---------------------------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------------------------

/// Build the inventory for the given clients (`present` is called with each client id).
pub fn scan(env: &Env, present: &dyn Fn(&str) -> bool, project: Option<&Path>) -> Inventory {
    let defs: Vec<AgentDef> = agent_defs().into_iter().filter(|d| present(d.id)).collect();

    let mut agents = Vec::new();
    // skill key -> record
    let mut skills: BTreeMap<String, Skill> = BTreeMap::new();
    let mut stale_codex: Vec<String> = Vec::new();

    for def in &defs {
        let mut roots = (def.roots)(env);
        // Project folders keep their documented order and come after the agent's own folders:
        // Claude Code ranks personal skills above project ones when names collide.
        if let Some(project) = project {
            roots.extend(
                def.project_roots
                    .iter()
                    .map(|rel| Root::deep(project.join(rel), "project")),
            );
        }
        agents.push(Agent {
            id: def.id.into(),
            name: def.name.into(),
            supports_skills: true,
            verified: def.verified,
            can_toggle: !matches!(def.switch, Switch::None(_)),
            switch_note: switch_note(def.switch),
            roots: roots
                .iter()
                .map(|r| AgentRoot {
                    path: env.display(&r.path),
                    label: crate::i18n::lookup(r.label),
                    exists: r.path.is_dir(),
                })
                .collect(),
            rules_note: def.rules_note.map(crate::i18n::lookup),
            rule_locations: (def.rules)(env)
                .iter()
                .map(|r| env.display(&r.path))
                .collect(),
        });

        let disabled = read_disabled(def.switch, env);
        if def.switch == Switch::CodexConfig {
            stale_codex = disabled.stale_paths.clone();
        }

        let found: Vec<Found> = roots.iter().flat_map(walk_root).collect();
        // name -> winning key, for name-based dedup.
        let mut by_name: HashMap<String, String> = HashMap::new();
        let mut reached: Vec<(String, Found, String)> = Vec::new();
        let mut seen_real: BTreeSet<String> = BTreeSet::new();
        for f in found {
            let key = f.real.as_ref().unwrap_or(&f.link).display().to_string();
            let record = skills
                .entry(key.clone())
                .or_insert_with(|| new_skill(&f, env, project));
            let link_display = env.display(f.link.parent().unwrap_or(&f.link));
            let link_display = if f.real.is_none() {
                env.display(&f.link)
            } else {
                link_display
            };
            if !record.links.contains(&link_display) {
                record.links.push(link_display);
            }
            if !seen_real.insert(key.clone()) {
                // Same file through another root: note the extra path on the existing entry.
                if let Some((_, _, via)) = reached.iter_mut().find(|(k, _, _)| *k == key) {
                    let extra = env.display(&f.root_path);
                    if !via.contains(&extra) {
                        *via = trf(
                            "{first} and {second}",
                            &[("first", via.as_str()), ("second", &extra)],
                        );
                    }
                }
                continue;
            }
            let via = env.display(&f.root_path);
            reached.push((key, f, via));
        }

        // Plugin skills are namespaced (`plugin:skill`), so they never collide with your own.
        let dedup_name = |key: &str, f: &Found| {
            let name = skills[key].name.clone();
            f.root_label
                .ends_with("plugin")
                .then(|| plugin_of(&f.root_path).map(|p| format!("{p}:{name}")))
                .flatten()
                .unwrap_or(name)
        };
        let names: Vec<String> = reached.iter().map(|(k, f, _)| dedup_name(k, f)).collect();
        for (index, (key, f, via)) in reached.iter().enumerate() {
            let name = skills[key].name.clone();
            let unique = names[index].clone();
            let broken = f.real.is_none();
            let shadowed = !broken
                && match def.dedup {
                    Dedup::NameFirstWins => match by_name.get(&unique) {
                        Some(winner) if winner != key => true,
                        _ => {
                            by_name.insert(unique.clone(), key.clone());
                            false
                        }
                    },
                    Dedup::RealPathLaterWins => {
                        let later = reached
                            .iter()
                            .zip(&names)
                            .rev()
                            .find(|((_, f, _), n)| f.real.is_some() && **n == unique)
                            .map(|((k, _, _), _)| k.clone());
                        later.as_deref() != Some(key.as_str())
                    }
                };
            let real = f.real.clone();
            let folder_off = real
                .as_ref()
                .and_then(|r| r.parent())
                .is_some_and(|p| disabled.off_paths.contains(p));
            let link_off = disabled.off_paths.contains(&f.link)
                || f.link
                    .parent()
                    .is_some_and(|p| disabled.off_paths.contains(p));
            let path_off = real
                .as_ref()
                .is_some_and(|r| disabled.off_paths.contains(r))
                || folder_off
                || link_off;
            let plugin_name = f
                .root_label
                .ends_with("plugin")
                .then(|| plugin_of(&f.root_path).map(|p| format!("{p}:{name}")))
                .flatten();
            let named = |set: &BTreeSet<String>| {
                set.contains(&name) || plugin_name.as_ref().is_some_and(|p| set.contains(p))
            };
            let switched_off = if disabled.rules.is_empty() {
                named(&disabled.off)
            } else {
                rules_deny(&disabled.rules, &name)
            };
            let (state, via) = if broken {
                (
                    State::Broken,
                    trf(
                        "{path} points at a missing folder",
                        &[("path", &env.display(&f.link))],
                    ),
                )
            } else if shadowed {
                (
                    State::Shadowed,
                    trf(
                        r#"Another "{name}" wins in {client}"#,
                        &[("name", &name), ("client", def.name)],
                    ),
                )
            } else if path_off || switched_off {
                (
                    State::Off,
                    trf("Off in {place}", &[("place", switch_place(def.switch))]),
                )
            } else if named(&disabled.name_only) {
                (State::NameOnly, tr("Listed by name; loads when asked"))
            } else {
                (State::On, via.clone())
            };
            if let Some(record) = skills.get_mut(key) {
                record.states.insert(
                    def.id.into(),
                    SkillState {
                        state,
                        via,
                        switch_name: plugin_name.clone(),
                    },
                );
            }
        }
    }

    for (id, name) in NO_SKILLS {
        if present(id) {
            agents.push(Agent {
                id: (*id).into(),
                name: (*name).into(),
                supports_skills: false,
                verified: true,
                can_toggle: false,
                switch_note: tr("This client has no file-based skills."),
                roots: Vec::new(),
                rules_note: None,
                rule_locations: Vec::new(),
            });
        }
    }

    // Parked skills nobody loads.
    for f in walk_root(&Root::flat(env.parked(), "parked")) {
        let key = f.real.as_ref().unwrap_or(&f.link).display().to_string();
        skills
            .entry(key)
            .or_insert_with(|| new_skill(&f, env, project));
    }

    let rules = scan_rules(env, &defs, project);
    let mut skills: Vec<Skill> = skills.into_values().collect();
    skills.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.name.cmp(&b.name)));
    let issues = find_issues(env, &agents, &skills, &rules, &stale_codex);
    Inventory {
        agents,
        skills,
        rules,
        issues,
        project: project.map(|p| p.display().to_string()),
    }
}

fn plugin_of(skills_root: &Path) -> Option<String> {
    // <cache>/<market>/<plugin>/<version>/skills
    skills_root
        .parent()?
        .parent()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

fn switch_note(switch: Switch) -> String {
    match switch {
        Switch::ClaudeOverrides => {
            tr("Uses Claude Code's skillOverrides in settings.json (on, name only, off).")
        }
        Switch::CodexConfig => tr("Uses [[skills.config]] entries in Codex's config.toml."),
        Switch::CopilotSettings => tr("Uses disabledSkills in Copilot CLI's settings.json."),
        Switch::OpencodePermissions => tr("Uses a skill deny rule in OpenCode's permissions."),
        Switch::HermesConfig => tr("Uses skills.disabled in Hermes's config.yaml."),
        Switch::GeminiSettings => tr("Uses skills.disabled in Gemini CLI's settings.json."),
        Switch::None(note) => crate::i18n::lookup(note),
    }
}

fn switch_place(switch: Switch) -> &'static str {
    match switch {
        Switch::ClaudeOverrides => "skillOverrides",
        Switch::CodexConfig => "config.toml",
        Switch::CopilotSettings => "disabledSkills",
        Switch::OpencodePermissions => "permissions",
        Switch::HermesConfig => "config.yaml",
        Switch::GeminiSettings => "settings.json",
        Switch::None(_) => "settings",
    }
}

fn new_skill(f: &Found, env: &Env, project: Option<&Path>) -> Skill {
    match &f.real {
        None => {
            let name = f
                .link
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("skill")
                .to_string();
            let target = std::fs::read_link(&f.link)
                .map(|t| env.display(&f.link.parent().unwrap_or(&f.link).join(t)))
                .unwrap_or_default();
            Skill {
                key: f.link.display().to_string(),
                name,
                description: trf(
                    "Link points at {path}, which doesn't exist.",
                    &[("path", &target)],
                ),
                description_chars: 0,
                path: env.display(&f.link),
                folder: f.link.parent().unwrap_or(&f.link).display().to_string(),
                source: Source::Broken,
                source_note: tr("Broken link"),
                editable: false,
                size_bytes: 0,
                lines: 0,
                modified_secs: None,
                git_repo: None,
                links: Vec::new(),
                states: BTreeMap::new(),
                preview: String::new(),
            }
        }
        Some(real) => {
            let file = read_skill_file(real);
            let folder = real.parent().unwrap_or(real).to_path_buf();
            let (source, source_note) = classify(real, env, project);
            Skill {
                key: real.display().to_string(),
                description_chars: file.description.chars().count(),
                name: file.name,
                description: file.description,
                path: env.display(real),
                git_repo: git_repo(&folder, &env.home),
                folder: folder.display().to_string(),
                editable: matches!(source, Source::Yours | Source::Library | Source::Parked),
                source,
                source_note,
                size_bytes: file.size,
                lines: file.lines,
                modified_secs: file.modified,
                links: Vec::new(),
                states: BTreeMap::new(),
                preview: file.preview,
            }
        }
    }
}

// --- rules ---------------------------------------------------------------------------------

/// `@path` import targets on their own line, resolved against the importing file's folder.
fn imports(text: &str, base: &Path, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let Some(target) = line.strip_prefix('@') else {
            continue;
        };
        let target = target.split_whitespace().next().unwrap_or("");
        if target.is_empty() {
            continue;
        }
        let path = if let Some(rest) = target.strip_prefix("~/") {
            home.join(rest)
        } else if Path::new(target).is_absolute() {
            PathBuf::from(target)
        } else {
            base.join(target)
        };
        out.push(path);
    }
    out
}

fn scan_rules(env: &Env, defs: &[AgentDef], project: Option<&Path>) -> Vec<RuleFile> {
    let mut files: BTreeMap<String, RuleFile> = BTreeMap::new();
    for def in defs {
        let mut sources: Vec<(RuleSource, &'static str)> = (def.rules)(env)
            .into_iter()
            .map(|s| (s, "global"))
            .collect();
        if let Some(project) = project {
            for rel in def.project_rules {
                let imports = def.id == "claude-code" || def.id == "gemini-cli";
                sources.push((RuleSource::file(project.join(rel), imports), "project"));
            }
        }
        let mut loaded_any = false;
        for (source, scope) in sources {
            if source.fallback && loaded_any {
                continue;
            }
            if !source.path.is_file() {
                continue;
            }
            loaded_any = true;
            let mut chain = vec![step("file", env.display(&source.path))];
            if is_link(&source.path) {
                if let Ok(real) = canon(&source.path) {
                    chain.push(step("link", env.display(&real)));
                }
            }
            add_rule(&mut files, env, &source.path, scope, def.id, chain.clone());
            if source.imports {
                follow_imports(&mut files, env, &source.path, scope, def.id, &chain, 0);
            } else if let Some(text) =
                read_text(&canon(&source.path).unwrap_or(source.path.clone()))
            {
                let skipped: Vec<PathBuf> = imports(
                    &text,
                    source.path.parent().unwrap_or(Path::new("/")),
                    &env.home,
                )
                .into_iter()
                .filter(|p| p.is_file())
                .collect();
                if let Some(file) = files.get_mut(&rule_key(&source.path)) {
                    if let Some(c) = file.agents.get_mut(def.id) {
                        c.extend(
                            skipped
                                .iter()
                                .map(|p| step("skipped-import", env.display(p))),
                        );
                    }
                }
            }
        }
    }
    files.into_values().collect()
}

fn step(kind: &str, path: String) -> ChainStep {
    ChainStep {
        kind: kind.into(),
        path,
    }
}

fn rule_key(path: &Path) -> String {
    canon(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn add_rule(
    files: &mut BTreeMap<String, RuleFile>,
    env: &Env,
    path: &Path,
    scope: &str,
    agent: &str,
    chain: Vec<ChainStep>,
) {
    let key = rule_key(path);
    let real = PathBuf::from(&key);
    let file = files.entry(key.clone()).or_insert_with(|| {
        let text = read_text(&real).unwrap_or_default();
        RuleFile {
            name: real
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("rules")
                .to_string(),
            path: env.display(&real),
            scope: scope.into(),
            size_bytes: std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0),
            preview: preview(&text),
            agents: BTreeMap::new(),
            key,
        }
    });
    if scope == "global" {
        file.scope = "global".into();
    }
    file.agents.entry(agent.into()).or_insert(chain);
}

fn follow_imports(
    files: &mut BTreeMap<String, RuleFile>,
    env: &Env,
    path: &Path,
    scope: &str,
    agent: &str,
    chain: &[ChainStep],
    depth: usize,
) {
    // Claude Code follows imports up to five hops.
    if depth >= 5 {
        return;
    }
    let real = canon(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(text) = read_text(&real) else {
        return;
    };
    for target in imports(&text, real.parent().unwrap_or(Path::new("/")), &env.home) {
        if !target.is_file() {
            continue;
        }
        let mut next = chain.to_vec();
        next.push(step("import", env.display(&target)));
        add_rule(files, env, &target, scope, agent, next.clone());
        follow_imports(files, env, &target, scope, agent, &next, depth + 1);
    }
}

// --- issues ----------------------------------------------------------------------------------

fn agent_name<'a>(agents: &'a [Agent], id: &'a str) -> &'a str {
    agents
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.name.as_str())
        .unwrap_or(id)
}

fn agent_names<'a>(agents: &[Agent], ids: impl IntoIterator<Item = &'a String>) -> String {
    list_names(
        &ids.into_iter()
            .map(|id| agent_name(agents, id).to_string())
            .collect::<Vec<_>>(),
    )
}

fn list_names(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        2 => trf(
            "{first} and {second}",
            &[("first", &names[0]), ("second", &names[1])],
        ),
        n => {
            let head = names[1..n - 1].iter().fold(names[0].clone(), |acc, name| {
                trf("{first}, {second}", &[("first", &acc), ("second", name)])
            });
            trf(
                "{first} and {second}",
                &[("first", &head), ("second", &names[n - 1])],
            )
        }
    }
}

/// The skills that are off in at least one client but on in another, with the clients that
/// still load them.
fn off_elsewhere(skills: &[Skill]) -> Vec<(&Skill, Vec<String>, Vec<String>)> {
    let mut out = Vec::new();
    for skill in skills {
        let off: Vec<String> = skill
            .states
            .iter()
            .filter(|(_, s)| s.state == State::Off)
            .map(|(id, _)| id.clone())
            .collect();
        let on: Vec<String> = skill
            .states
            .iter()
            .filter(|(_, s)| s.state == State::On)
            .map(|(id, _)| id.clone())
            .collect();
        if !off.is_empty() && !on.is_empty() {
            out.push((skill, off, on));
        }
    }
    out
}

fn issue(
    id: &str,
    level: &str,
    title: String,
    detail: String,
    agents: Vec<String>,
    skills: Vec<String>,
    fix_label: Option<String>,
) -> Issue {
    Issue {
        id: id.into(),
        level: level.into(),
        title,
        detail,
        agents,
        skills,
        skill_names: Vec::new(),
        fix_label,
    }
}

fn find_issues(
    env: &Env,
    agents: &[Agent],
    skills: &[Skill],
    rules: &[RuleFile],
    stale_codex: &[String],
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let toggles: BTreeSet<&str> = agents
        .iter()
        .filter(|a| a.can_toggle)
        .map(|a| a.id.as_str())
        .collect();

    let leaks = off_elsewhere(skills);
    if !leaks.is_empty() {
        let names: Vec<String> = leaks.iter().map(|(s, _, _)| s.name.clone()).collect();
        let still: BTreeSet<String> = leaks.iter().flat_map(|(_, _, on)| on.clone()).collect();
        let fixable = leaks
            .iter()
            .any(|(_, _, on)| on.iter().any(|id| toggles.contains(id.as_str())));
        let count = names.len().to_string();
        let (skills_text, clients) = (list_names(&names), agent_names(agents, &still));
        let values = [
            ("count", count.as_str()),
            ("skills", &skills_text),
            ("clients", &clients),
        ];
        issues.push(issue(
            "off-elsewhere",
            "warn",
            if names.len() == 1 {
                trf(
                    "{count} skill you turned off still loads in other clients",
                    &values,
                )
            } else {
                trf(
                    "{count} skills you turned off still load in other clients",
                    &values,
                )
            },
            if names.len() == 1 {
                trf(
                    "{skills} is off in at least one client but still on in {clients}.",
                    &values,
                )
            } else {
                trf(
                    "{skills} are off in at least one client but still on in {clients}.",
                    &values,
                )
            },
            still.into_iter().collect(),
            leaks.iter().map(|(s, _, _)| s.key.clone()).collect(),
            fixable.then(|| tr("Turn off where possible")),
        ));
    }

    let broken: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.source == Source::Broken)
        .collect();
    if !broken.is_empty() {
        let who: BTreeSet<String> = broken
            .iter()
            .flat_map(|s| s.states.keys().cloned())
            .collect();
        let count = broken.len().to_string();
        let links = list_names(&broken.iter().map(|s| s.path.clone()).collect::<Vec<_>>());
        let values = [("count", count.as_str()), ("links", &links)];
        let one = broken.len() == 1;
        issues.push(issue(
            "broken-links",
            "error",
            if one {
                trf("{count} link to a missing folder", &values)
            } else {
                trf("{count} links to a missing folder", &values)
            },
            if one {
                trf(
                    "{links} points at nothing. Clients skip it without saying so.",
                    &values,
                )
            } else {
                trf(
                    "{links} point at nothing. Clients skip them without saying so.",
                    &values,
                )
            },
            who.into_iter().collect(),
            broken.iter().map(|s| s.key.clone()).collect(),
            Some(if one {
                tr("Remove link")
            } else {
                trf("Remove {count} links", &values)
            }),
        ));
    }

    // Same name, different files, both reaching one client.
    let shadowed: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.states.values().any(|st| st.state == State::Shadowed))
        .collect();
    if !shadowed.is_empty() {
        let names: Vec<String> = shadowed
            .iter()
            .map(|s| s.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let who: BTreeSet<String> = shadowed
            .iter()
            .flat_map(|s| {
                s.states
                    .iter()
                    .filter(|(_, st)| st.state == State::Shadowed)
                    .map(|(id, _)| id.clone())
            })
            .collect();
        let count = names.len().to_string();
        let (skills_text, clients) = (list_names(&names), agent_names(agents, &who));
        let values = [
            ("count", count.as_str()),
            ("skills", &skills_text),
            ("clients", &clients),
        ];
        issues.push(issue(
            "clashes",
            "warn",
            if names.len() == 1 {
                trf("{count} skill name with more than one copy", &values)
            } else {
                trf("{count} skill names with more than one copy", &values)
            },
            if who.len() == 1 {
                trf("{clients} finds two different files named {skills} and uses only one. Remove the copy you don't use so it's clear which one runs.", &values)
            } else {
                trf("{clients} find two different files named {skills} and use only one. Remove the copy you don't use so it's clear which one runs.", &values)
            },
            who.into_iter().collect(),
            skills
                .iter()
                .filter(|s| names.contains(&s.name) && s.source != Source::Broken)
                .map(|s| s.key.clone())
                .collect(),
            None,
        ));
    }

    if !stale_codex.is_empty() {
        let paths = list_names(
            &stale_codex
                .iter()
                .map(|p| env.display(Path::new(p)))
                .collect::<Vec<_>>(),
        );
        let file = env.display(&codex_config_path(env));
        issues.push(issue(
            "codex-stale",
            "info",
            tr("Codex turns off a skill that isn't there"),
            trf(
                "{paths} in {file} points at nothing.",
                &[("paths", &paths), ("file", &file)],
            ),
            vec!["codex".into()],
            Vec::new(),
            Some(tr("Remove entry")),
        ));
    }

    let long: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.description_chars > DESCRIPTION_CAP && s.states.contains_key("claude-code"))
        .collect();
    if !long.is_empty() {
        let count = long.len().to_string();
        let cap = DESCRIPTION_CAP.to_string();
        let names = list_names(&long.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        let values = [("count", count.as_str()), ("cap", &cap), ("skills", &names)];
        let one = long.len() == 1;
        issues.push(issue(
            "long-descriptions",
            "warn",
            if one {
                trf("{count} description cut off in Claude Code", &values)
            } else {
                trf("{count} descriptions cut off in Claude Code", &values)
            },
            if one {
                trf("Claude Code keeps the first {cap} characters of a description. {skills} is longer.", &values)
            } else {
                trf("Claude Code keeps the first {cap} characters of a description. {skills} are longer.", &values)
            },
            vec!["claude-code".into()],
            long.iter().map(|s| s.key.clone()).collect(),
            None,
        ));
    }

    // claude.ai sync keeps one folder per account; older ones linger after account changes.
    let synced = env.claude.join("skills").join("synced");
    let mut buckets: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&synced)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            (
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
                e.path(),
            )
        })
        .collect();
    if buckets.len() > 1 {
        buckets.sort();
        let old: Vec<String> = buckets[..buckets.len() - 1]
            .iter()
            .map(|(_, p)| env.display(p))
            .collect();
        let folders = list_names(&old);
        let values = [("folders", folders.as_str())];
        issues.push(issue(
            "stale-sync",
            "info",
            tr("Old claude.ai sync folder"),
            if old.len() == 1 {
                trf("{folders} is left from an earlier sync. Clients that read Claude's folder load it too.", &values)
            } else {
                trf("{folders} are left from earlier syncs. Clients that read Claude's folder load them too.", &values)
            },
            vec!["claude-code".into()],
            Vec::new(),
            None,
        ));
    }

    let packaged: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.source == Source::Package && !s.states.is_empty())
        .collect();
    if !packaged.is_empty() {
        let count = packaged.len().to_string();
        let names = list_names(&packaged.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        let values = [("count", count.as_str()), ("skills", &names)];
        issues.push(issue(
            "packaged",
            "info",
            if packaged.len() == 1 {
                trf("{count} skill from a system package", &values)
            } else {
                trf("{count} skills from a system package", &values)
            },
            trf(
                "Edits to {skills} are replaced when the package updates.",
                &values,
            ),
            Vec::new(),
            packaged.iter().map(|s| s.key.clone()).collect(),
            None,
        ));
    }

    // Rules: imports a client skips, and clients with no global rules at all.
    let skipped: BTreeSet<String> = rules
        .iter()
        .flat_map(|rule| {
            rule.agents
                .iter()
                .filter(|(_, chain)| chain.iter().any(|c| c.kind == "skipped-import"))
                .map(|(agent, _)| agent.clone())
        })
        .collect();
    if !skipped.is_empty() {
        let clients = agent_names(agents, &skipped);
        let values = [("clients", clients.as_str())];
        issues.push(issue(
            "rules-imports",
            "info",
            if skipped.len() == 1 {
                trf("{clients} reads your rules file but not what it imports", &values)
            } else {
                trf("{clients} read your rules files but not what they import", &values)
            },
            tr("These clients load the file itself but skip its @import lines, so the imported rules never reach them."),
            skipped.into_iter().collect(),
            Vec::new(),
            None,
        ));
    }
    let with_rules: BTreeSet<&str> = rules
        .iter()
        .filter(|r| r.scope == "global")
        .flat_map(|r| r.agents.keys().map(String::as_str))
        .collect();
    let without: Vec<String> = agents
        .iter()
        .filter(|a| {
            a.supports_skills
                && a.verified
                && a.rules_note.is_none()
                && !a.rule_locations.is_empty()
                && !with_rules.contains(a.id.as_str())
        })
        .map(|a| a.id.clone())
        .collect();
    if !without.is_empty() && !with_rules.is_empty() {
        let clients = agent_names(agents, &without);
        let values = [("clients", clients.as_str())];
        issues.push(issue(
            "rules-missing",
            "info",
            if without.len() == 1 {
                trf("{clients} gets no global rules", &values)
            } else {
                trf("{clients} get no global rules", &values)
            },
            tr("Your global rules files reach other clients but not these. Project files such as AGENTS.md still apply inside a project."),
            without,
            Vec::new(),
            None,
        ));
    }

    // Keys stay the stable ids fixes act on; names are what people read.
    for found in &mut issues {
        let mut names: Vec<String> = Vec::new();
        for key in &found.skills {
            let name = skills
                .iter()
                .find(|s| &s.key == key)
                .map_or_else(|| key.clone(), |s| s.name.clone());
            if !names.contains(&name) {
                names.push(name);
            }
        }
        found.skill_names = names;
    }
    issues
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

/// Desired state when changing a skill in one client.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Wanted {
    On,
    NameOnly,
    Off,
}

/// Write hooks so tests can capture writes without touching backups in the data dir.
pub trait Writer {
    fn backup(&self, client: &str, path: &Path) -> Result<(), String>;
    fn write(&self, path: &Path, contents: &str) -> Result<(), String>;
    fn remove_link(&self, path: &Path) -> Result<(), String>;
}

pub struct DiskWriter;

impl Writer for DiskWriter {
    fn backup(&self, client: &str, path: &Path) -> Result<(), String> {
        crate::clients::backup_config(client, path).map(|_| ())
    }
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        crate::registry::atomic_write(path, contents)
    }
    fn remove_link(&self, path: &Path) -> Result<(), String> {
        if !is_link(path) {
            return Err(tr("Toolport only removes links, not folders or files."));
        }
        // Windows stores a link to a folder as a directory symlink, removed with remove_dir.
        std::fs::remove_file(path)
            .or_else(|e| {
                if cfg!(windows) {
                    std::fs::remove_dir(path)
                } else {
                    Err(e)
                }
            })
            .map_err(|e| e.to_string())
    }
}

/// Change one skill's state in one client through that client's own setting.
pub fn set_state(
    env: &Env,
    inventory: &Inventory,
    agent: &str,
    key: &str,
    wanted: Wanted,
    writer: &dyn Writer,
) -> Result<(), String> {
    let def = agent_defs()
        .into_iter()
        .find(|d| d.id == agent)
        .ok_or_else(|| tr("Unknown client"))?;
    let skill = inventory
        .skills
        .iter()
        .find(|s| s.key == key)
        .ok_or_else(|| tr("That skill is no longer on disk. Refresh and try again."))?;
    if !skill.states.contains_key(agent) {
        return Err(trf(
            "{client} doesn't load {skill}.",
            &[("client", def.name), ("skill", &skill.name)],
        ));
    }
    if wanted == Wanted::NameOnly && def.switch != Switch::ClaudeOverrides {
        return Err(tr("Only Claude Code can list a skill by name only."));
    }
    let off = wanted == Wanted::Off;
    match def.switch {
        Switch::ClaudeOverrides => {
            let path = claude_settings_path(env);
            // Plugin skills are switched by their `plugin:skill` name, so turning one off never
            // touches your own skill with the same short name.
            let name = skill.states[agent]
                .switch_name
                .clone()
                .unwrap_or_else(|| skill.name.clone());
            // A bare-name entry also covers a plugin skill. Clear it on turn-on only when no
            // other skill Claude Code loads uses that short name.
            let bare_is_only_this = name != skill.name
                && !inventory.skills.iter().any(|s| {
                    s.key != skill.key
                        && s.name == skill.name
                        && s.states
                            .get(agent)
                            .is_some_and(|st| st.switch_name.is_none())
                });
            edit_json(agent, &path, writer, |root| {
                let map = object_at(root, &["skillOverrides"])?;
                match wanted {
                    Wanted::On => {
                        map.remove(&name);
                        if bare_is_only_this {
                            map.remove(&skill.name);
                        }
                    }
                    Wanted::NameOnly => {
                        map.insert(name.clone(), "name-only".into());
                    }
                    Wanted::Off => {
                        map.insert(name.clone(), "off".into());
                    }
                }
                Ok("skillOverrides")
            })
        }
        Switch::CopilotSettings => {
            let path = copilot_settings_path(env);
            edit_json(agent, &path, writer, |root| {
                toggle_list(root, &["disabledSkills"], &skill.name, off)?;
                Ok("disabledSkills")
            })
        }
        Switch::GeminiSettings => {
            let path = gemini_settings_path(env);
            edit_json(agent, &path, writer, |root| {
                toggle_list(root, &["skills", "disabled"], &skill.name, off)?;
                Ok("skills")
            })
        }
        Switch::OpencodePermissions => {
            let path = opencode_config_path(env);
            edit_json(agent, &path, writer, |root| {
                opencode_toggle(root, &skill.name, off)
            })
        }
        Switch::CodexConfig => {
            let path = codex_config_path(env);
            // Codex matches entries by the path it loaded, which is the link path when the skill
            // came through a symlinked folder. Write each path Codex can see for this skill.
            let codex_roots = [env.codex.join("skills"), env.agents_skills()];
            let mut paths: Vec<PathBuf> = skill
                .links
                .iter()
                .map(|link| expand(env, link))
                .filter(|dir| codex_roots.iter().any(|r| dir.starts_with(r)))
                .map(|dir| dir.join("SKILL.md"))
                .filter(|p| p.is_file())
                .collect();
            if paths.is_empty() {
                paths.push(PathBuf::from(&skill.key));
            }
            edit_codex(agent, &path, writer, &paths, Path::new(&skill.key), off)
        }
        Switch::HermesConfig => {
            let path = hermes_config_path(env);
            let text = read_existing(&path)?;
            // Start from what the file says, not a lenient read: a list Toolport couldn't parse
            // must not be rewritten as empty.
            let parsed: Option<serde_yaml::Value> = if text.trim().is_empty() {
                None
            } else {
                Some(serde_yaml::from_str(&text).map_err(|e| not_valid(&path, &e.to_string()))?)
            };
            let mut names: BTreeSet<String> = BTreeSet::new();
            if let Some(list) = parsed
                .as_ref()
                .and_then(|v| v.get("skills"))
                .and_then(|s| s.get("disabled"))
            {
                let items = list
                    .as_sequence()
                    .ok_or_else(|| wrong_shape("skills.disabled"))?;
                names.extend(items.iter().filter_map(|x| x.as_str().map(str::to_string)));
            }
            if off {
                names.insert(skill.name.clone());
            } else {
                names.remove(&skill.name);
            }
            let next = set_yaml_skills_disabled(&text, &names.into_iter().collect::<Vec<_>>())?;
            if next != text {
                writer.backup(agent, &path)?;
                writer.write(&path, &next)?;
            }
            Ok(())
        }
        Switch::None(note) => Err(crate::i18n::lookup(note)),
    }
}

fn expand(env: &Env, display: &str) -> PathBuf {
    match display
        .strip_prefix("~/")
        .or_else(|| display.strip_prefix(&format!("~{}", std::path::MAIN_SEPARATOR)))
    {
        Some(rest) => env.home.join(rest),
        None if display == "~" => env.home.clone(),
        None => PathBuf::from(display),
    }
}

/// Backs a file up the first time it is changed and passes later backups through as done, so
/// a fix that edits one file many times keeps the copy from before the fix.
struct BackupOnce<'a> {
    inner: &'a dyn Writer,
    done: std::cell::RefCell<BTreeSet<PathBuf>>,
}

impl Writer for BackupOnce<'_> {
    fn backup(&self, client: &str, path: &Path) -> Result<(), String> {
        if self.done.borrow().contains(path) {
            return Ok(());
        }
        self.inner.backup(client, path)?;
        self.done.borrow_mut().insert(path.to_path_buf());
        Ok(())
    }
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        self.inner.write(path, contents)
    }
    fn remove_link(&self, path: &Path) -> Result<(), String> {
        self.inner.remove_link(path)
    }
}

/// True only when `path` is a symlink whose target is missing. An unreadable target or an
/// unmounted volume is not proof the link is broken.
fn dangling(path: &Path) -> bool {
    is_link(path)
        && matches!(std::fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Run every fix the issue offers. Returns how many changes were made.
pub fn apply_fix(
    env: &Env,
    inventory: &Inventory,
    issue_id: &str,
    writer: &dyn Writer,
) -> Result<usize, String> {
    let issue = inventory
        .issues
        .iter()
        .find(|i| i.id == issue_id)
        .ok_or_else(|| tr("That problem is already gone. Refresh to see the latest."))?;
    let writer = &BackupOnce {
        inner: writer,
        done: Default::default(),
    };
    match issue.id.as_str() {
        "off-elsewhere" => {
            // Keep going past a client that refuses, so one odd config doesn't block the rest.
            let mut done = 0;
            let mut first_error = None;
            for (skill, _, on) in off_elsewhere(&inventory.skills) {
                for agent in on {
                    let can = inventory
                        .agents
                        .iter()
                        .any(|a| a.id == agent && a.can_toggle);
                    if !can {
                        continue;
                    }
                    match set_state(env, inventory, &agent, &skill.key, Wanted::Off, writer) {
                        Ok(()) => done += 1,
                        Err(e) => {
                            first_error.get_or_insert(e);
                        }
                    }
                }
            }
            first_error.map_or(Ok(done), Err)
        }
        "broken-links" => {
            let mut done = 0;
            for key in &issue.skills {
                let path = Path::new(key);
                // Re-check at apply time: only remove a link that still dangles.
                if dangling(path) {
                    writer.remove_link(path)?;
                    done += 1;
                }
            }
            Ok(done)
        }
        "codex-stale" => {
            let path = codex_config_path(env);
            let text = read_existing(&path)?;
            let mut doc: toml_edit::DocumentMut = text
                .parse()
                .map_err(|e| not_valid(&path, &format!("{e}")))?;
            let mut removed = 0;
            if let Some(arr) = doc
                .get_mut("skills")
                .and_then(|s| s.get_mut("config"))
                .and_then(|c| c.as_array_of_tables_mut())
            {
                let before = arr.len();
                arr.retain(|t| {
                    let disabled = t.get("enabled").and_then(|e| e.as_bool()) == Some(false);
                    let missing = t
                        .get("path")
                        .and_then(|p| p.as_str())
                        .is_some_and(|p| !Path::new(p).exists());
                    !(disabled && missing)
                });
                removed = before - arr.len();
            }
            if removed > 0 {
                writer.backup("codex", &path)?;
                writer.write(&path, &doc.to_string())?;
            }
            Ok(removed)
        }
        _ => Err(tr("Toolport has no automatic fix for this one.")),
    }
}

/// The file's text, or an empty string when it doesn't exist yet. A file that exists but can't be
/// read (too big, not UTF-8, no permission) is an error, never treated as empty.
fn read_existing(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    read_text(path).ok_or_else(|| {
        trf(
            "Couldn't read {file}",
            &[("file", &path.display().to_string())],
        )
    })
}

fn not_valid(path: &Path, error: &str) -> String {
    trf(
        "{file} isn't valid: {error}",
        &[("file", &path.display().to_string()), ("error", error)],
    )
}

fn edit_json(
    agent: &str,
    path: &Path,
    writer: &dyn Writer,
    change: impl FnOnce(&mut serde_json::Value) -> Result<&'static str, String>,
) -> Result<(), String> {
    let original = read_existing(path)?;
    let mut value: serde_json::Value = if original.trim().is_empty() {
        serde_json::json!({})
    } else {
        json5::from_str(&original).map_err(|e| not_valid(path, &e.to_string()))?
    };
    if !value.is_object() {
        return Err(not_valid(path, &tr("not a JSON object")));
    }
    let before = value.clone();
    let key = change(&mut value)?;
    if value == before {
        return Ok(());
    }
    let new_value = value.get(key).cloned().unwrap_or(serde_json::Value::Null);
    let next = if original.trim().is_empty() {
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())? + "\n"
    } else {
        crate::clients::rewrite_json_key(&original, key, &new_value)?
    };
    if path.exists() {
        writer.backup(agent, path)?;
    }
    writer.write(path, &next)
}

/// A setting whose current value isn't the shape Toolport writes. Replacing it would change
/// what the user set, so refuse instead.
fn wrong_shape(key: &str) -> String {
    trf(
        "{key} isn't in a shape Toolport can change. Edit it by hand.",
        &[("key", key)],
    )
}

fn object_at<'a>(
    root: &'a mut serde_json::Value,
    keys: &[&str],
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, String> {
    let mut cur = root;
    for (depth, key) in keys.iter().enumerate() {
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| wrong_shape(&keys[..depth].join(".")))?;
        let next = obj
            .entry(key.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !next.is_object() {
            return Err(wrong_shape(&keys[..=depth].join(".")));
        }
        cur = next;
    }
    cur.as_object_mut()
        .ok_or_else(|| wrong_shape(&keys.join(".")))
}

fn toggle_list(
    root: &mut serde_json::Value,
    keys: &[&str],
    name: &str,
    off: bool,
) -> Result<(), String> {
    let (last, parents) = keys.split_last().ok_or_else(|| wrong_shape(""))?;
    let parent = object_at(root, parents)?;
    let list = parent
        .entry(last.to_string())
        .or_insert_with(|| serde_json::json!([]));
    let arr = list
        .as_array_mut()
        .ok_or_else(|| wrong_shape(&keys.join(".")))?;
    arr.retain(|v| v.as_str() != Some(name));
    if off {
        arr.push(serde_json::Value::String(name.into()));
    }
    Ok(())
}

fn opencode_toggle(
    root: &mut serde_json::Value,
    name: &str,
    off: bool,
) -> Result<&'static str, String> {
    // v1 configs keep a `permission` object; follow the shape already in the file. A single
    // string such as `"skill": "deny"` covers every skill, so it is refused, not replaced.
    let v1 = root.get("permission").is_some() && root.get("permissions").is_none();
    if v1 {
        let map = object_at(root, &["permission", "skill"])?;
        map.remove(name);
        let still_denied = rules_deny(
            &opencode_rules(&serde_json::json!({"permission": {"skill": map.clone()}})),
            name,
        );
        if off {
            map.insert(name.into(), "deny".into());
        } else if still_denied {
            map.insert(name.into(), "allow".into());
        }
        return Ok("permission");
    }
    let obj = root.as_object_mut().ok_or_else(|| wrong_shape(""))?;
    let rules = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!([]));
    let arr = rules
        .as_array_mut()
        .ok_or_else(|| wrong_shape("permissions"))?;
    arr.retain(|r| {
        !(r.get("action").and_then(|a| a.as_str()) == Some("skill")
            && r.get("resource").and_then(|x| x.as_str()) == Some(name))
    });
    // A wildcard rule can still deny it; an explicit allow after it wins.
    let still_denied = rules_deny(
        &opencode_rules(&serde_json::json!({"permissions": arr.clone()})),
        name,
    );
    if off {
        arr.push(serde_json::json!({"action": "skill", "resource": name, "effect": "deny"}));
    } else if still_denied {
        arr.push(serde_json::json!({"action": "skill", "resource": name, "effect": "allow"}));
    }
    Ok("permissions")
}

fn edit_codex(
    agent: &str,
    path: &Path,
    writer: &dyn Writer,
    paths: &[PathBuf],
    skill_real: &Path,
    off: bool,
) -> Result<(), String> {
    let text = read_existing(path)?;
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| not_valid(path, &format!("{e}")))?;
    let wanted: BTreeSet<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let skills = doc.entry("skills").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    let skills = skills
        .as_table_like_mut()
        .ok_or_else(|| wrong_shape("skills"))?;
    if skills.get("config").is_none() {
        skills.insert(
            "config",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let arr = skills
        .get_mut("config")
        .and_then(|c| c.as_array_of_tables_mut())
        .ok_or_else(|| wrong_shape("skills.config"))?;
    // Codex also matches an entry written as the folder or the resolved path, so clear every
    // entry that reaches this skill, not only the exact paths Toolport writes.
    let reaches = |p: &str| {
        let entry = Path::new(p);
        wanted.contains(p)
            || canon(entry).is_ok_and(|r| r == skill_real)
            || canon(&entry.join("SKILL.md")).is_ok_and(|r| r == skill_real)
    };
    arr.retain(|t| !t.get("path").and_then(|p| p.as_str()).is_some_and(reaches));
    if off {
        for p in paths {
            let mut t = toml_edit::Table::new();
            t.insert("path", toml_edit::value(p.display().to_string()));
            t.insert("enabled", toml_edit::value(false));
            arr.push(t);
        }
    }
    let next = doc.to_string();
    if next == text {
        return Ok(());
    }
    if path.exists() {
        writer.backup(agent, path)?;
    }
    writer.write(path, &next)
}

/// Replace (or add) the `disabled` list under the top-level `skills:` key of a YAML file while
/// leaving every other line, including comments, as it was.
fn set_yaml_skills_disabled(text: &str, names: &[String]) -> Result<String, String> {
    let flow = if names.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            names
                .iter()
                .map(|n| serde_json::to_string(n).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let lines: Vec<&str> = text.lines().collect();
    // The top-level `skills:` key, with or without a trailing comment. A value on the same line
    // (`skills: {disabled: [a]}`) is a flow mapping this editor doesn't rewrite.
    fn key_rest(line: &str) -> Option<&str> {
        line.strip_prefix("skills:").map(str::trim)
    }
    let Some(skills_at) = lines.iter().position(|l| key_rest(l).is_some()) else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("skills:\n  disabled: {flow}\n"));
        return Ok(out);
    };
    let rest = key_rest(lines[skills_at]).unwrap_or_default();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(wrong_shape("skills"));
    }
    // Child block: following lines that are blank or indented.
    let mut end = skills_at + 1;
    while end < lines.len() && (lines[end].trim().is_empty() || lines[end].starts_with([' ', '\t']))
    {
        end += 1;
    }
    let indent = lines[skills_at + 1..end]
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(2);
    let pad = " ".repeat(indent);
    let mut out: Vec<String> = lines[..=skills_at].iter().map(|s| s.to_string()).collect();
    let mut replaced = false;
    let mut i = skills_at + 1;
    while i < end {
        let line = lines[i];
        let own = line.len() - line.trim_start().len();
        if own == indent && line.trim_start().starts_with("disabled:") {
            out.push(format!("{pad}disabled: {flow}"));
            replaced = true;
            i += 1;
            // Drop a block list that followed the key.
            while i < end {
                let l = lines[i];
                let ind = l.len() - l.trim_start().len();
                if !l.trim().is_empty()
                    && (ind > indent || (ind == indent && l.trim_start().starts_with("- ")))
                {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    if !replaced {
        out.insert(skills_at + 1, format!("{pad}disabled: {flow}"));
    }
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    if text.ends_with('\n') || text.is_empty() {
        joined.push('\n');
    }
    Ok(joined)
}

/// Paths the UI may open or reveal: skill folders and files, and rules files from a fresh scan.
pub fn allowed_path(inventory: &Inventory, path: &str) -> bool {
    inventory
        .skills
        .iter()
        .any(|s| s.key == path || s.folder == path)
        || inventory.rules.iter().any(|r| r.key == path)
}

// ---------------------------------------------------------------------------------------------
// Production entry points
// ---------------------------------------------------------------------------------------------

fn present_clients() -> impl Fn(&str) -> bool {
    let present: BTreeSet<String> = crate::clients::detect_clients()
        .into_iter()
        .filter(|c| c.app_present || c.config_exists)
        .map(|c| c.id)
        .collect();
    move |id: &str| present.contains(id)
}

pub fn inventory(project: Option<&str>) -> Result<Inventory, String> {
    let env = Env::current().ok_or_else(|| tr("Couldn't find your home folder"))?;
    let project = project.map(PathBuf::from).filter(|p| p.is_dir());
    Ok(scan(&env, &present_clients(), project.as_deref()))
}

/// Toolport's own skill writes run one at a time, so two quick toggles of the same file can't
/// both read the old contents.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn set_skill_state(
    agent: &str,
    key: &str,
    wanted: Wanted,
    project: Option<&str>,
) -> Result<Inventory, String> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::current().ok_or_else(|| tr("Couldn't find your home folder"))?;
    let inv = inventory(project)?;
    set_state(&env, &inv, agent, key, wanted, &DiskWriter)?;
    inventory(project)
}

pub fn fix_issue(issue_id: &str, project: Option<&str>) -> Result<Inventory, String> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::current().ok_or_else(|| tr("Couldn't find your home folder"))?;
    let inv = inventory(project)?;
    apply_fix(&env, &inv, issue_id, &DiskWriter)?;
    inventory(project)
}

/// Files Toolport hands to the default app. Anything else is revealed, never opened.
fn opens_as_text(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "md" | "mdc" | "markdown" | "txt" | "yaml" | "yml" | "json" | "toml"
        )
    })
}

/// Open an inventory file in its default app, or its folder when `reveal` is set.
pub fn open_path(path: &str, reveal: bool, project: Option<&str>) -> Result<(), String> {
    let inv = inventory(project)?;
    if !allowed_path(&inv, path) {
        return Err(tr("Toolport only opens skill and rules files it found."));
    }
    let target = PathBuf::from(path);
    // The default app for a script is often the shell. A rules file can @import any path, so
    // only text files are opened; anything else is shown in its folder instead.
    let reveal = reveal || (target.is_file() && !opens_as_text(&target));
    let target = if reveal && target.is_file() {
        target.parent().map(Path::to_path_buf).unwrap_or(target)
    } else {
        target
    };
    crate::diagnostics_controller::open_in_default_app(&target)
}

#[cfg(all(test, unix))]
mod tests;
