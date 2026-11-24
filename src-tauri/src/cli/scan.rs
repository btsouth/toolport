//! `toolport scan`: one screen that says what every coding agent on this machine loads,
//! where its secrets sit in plain text, and what drifted.
//!
//! Discovery is the Clients page's own read-only scan (`clients::detect_clients_for_skills`,
//! which never adopts gateways or saves locations), credentials are judged by the same
//! `import_credentials` rules a reviewed import uses, and skills and rules come from
//! `skills::inventory`. Secret values never leave this module: findings carry the client,
//! file, server and setting name only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use super::format::{self, Align, Severity, Style};
use super::{Args, CliError, Ctx, EXIT_FINDINGS, EXIT_OK};
use crate::clients::{self, DetectedClient, GatewayEntryState, McpServer};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub version: &'static str,
    pub project: Option<String>,
    pub agents: Vec<AgentRow>,
    pub secrets: Vec<SecretFinding>,
    pub drift: Vec<DriftFinding>,
    pub skills: SkillsSummary,
    pub startup: Vec<BudgetRow>,
    pub counts: Counts,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub config_path: String,
    pub servers: usize,
    /// `connected`, `customized` (a hand-edited gateway entry) or `not-connected`.
    pub gateway: &'static str,
    pub error: Option<String>,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct SecretFinding {
    pub client_id: String,
    pub client_name: String,
    pub config_path: String,
    pub server: String,
    /// `env`, `header`, `url`, `arg` or `command`.
    pub kind: &'static str,
    /// The setting's name (an env var or header name, `argument 3`, `URL`). Never a value.
    pub setting: String,
    /// Toolport already holds this server (and its credentials), so only the agent's own
    /// copy is left: connecting the agent to the gateway removes it.
    pub in_toolport: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftFinding {
    pub server: String,
    /// `duplicate` (same definition in several agents) or `conflict` (same name, different
    /// definitions).
    pub kind: &'static str,
    pub clients: Vec<String>,
    pub detail: String,
    pub in_toolport: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillsSummary {
    pub total: usize,
    pub rules_files: usize,
    /// `~/.agents/skills` exists: the folder `npx skills add` installs into.
    pub shared_folder: Option<String>,
    /// Agents that read the shared folder.
    pub shared_readers: Vec<String>,
    pub issues: Vec<IssueRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRow {
    pub id: String,
    pub level: String,
    pub title: String,
    pub detail: String,
    pub agents: Vec<String>,
    pub fixable: bool,
    pub fix_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BudgetRow {
    pub agent_id: String,
    pub agent_name: String,
    pub rules_files: Vec<RulesTokens>,
    pub rules_tokens: u64,
    pub skills_loaded: usize,
    pub skills_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesTokens {
    pub path: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
}

// ---------------------------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------------------------

/// Everything a scan needs, read once. `usage`, `fix`, `hook check` and `report` share it.
pub struct Snapshot {
    pub clients: Vec<DetectedClient>,
    pub inventory: Option<crate::skills::Inventory>,
    pub inventory_error: Option<String>,
    /// Servers in Toolport's registry, as (lowercase name, what it runs or connects to,
    /// its arguments with credentials masked).
    pub toolport: Vec<(String, String, Vec<String>)>,
}

impl Snapshot {
    pub fn read(project: Option<&str>) -> Self {
        let clients = clients::detect_clients_for_skills();
        let (inventory, inventory_error) = match crate::skills::inventory(project) {
            Ok(inv) => (Some(inv), None),
            Err(e) => (None, Some(e)),
        };
        let toolport = crate::registry::load()
            .map(|r| {
                r.servers
                    .iter()
                    .filter(|s| !clients::is_gateway_server(s))
                    .map(|s| {
                        (
                            s.name.to_lowercase(),
                            target(s.command.as_deref(), s.url.as_deref()),
                            crate::import_credentials::shown_args(&s.args),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Snapshot {
            clients,
            inventory,
            inventory_error,
            toolport,
        }
    }

    /// Whether Toolport already has this agent server: the same name, running the same
    /// program with the same arguments, or reaching the same host.
    pub fn in_toolport(&self, server: &McpServer) -> bool {
        let name = server.name.to_lowercase();
        let want = target(server.command.as_deref(), server.url.as_deref());
        let args = crate::import_credentials::shown_args(&server.args);
        // An argument that holds a credential on either side matches any value, since an
        // import moves it out.
        let open = |a: &str| a == "<launch-input>" || without_references(a).1;
        self.toolport.iter().any(|(n, t, theirs)| {
            *n == name
                && *t == want
                && theirs.len() == args.len()
                && theirs
                    .iter()
                    .zip(&args)
                    .all(|(a, b)| a == b || open(a) || open(b))
        })
    }

    /// Clients that are installed or configured, without the duplicate rows a background
    /// tool's profile copy produces.
    pub fn present(&self) -> impl Iterator<Item = &DetectedClient> {
        self.clients.iter().filter(|c| {
            (c.app_present || c.config_exists)
                && !c
                    .claude_profile
                    .as_ref()
                    .is_some_and(|p| p.same_as.is_some())
        })
    }
}

pub fn collect(snapshot: &Snapshot, project: Option<&Path>, include_info: bool) -> ScanReport {
    let agents = agent_rows(snapshot);
    let secrets = secret_findings(snapshot);
    let drift = drift(snapshot);
    let (skills, startup) = match &snapshot.inventory {
        Some(inv) => (skills_summary(inv, include_info), budget(inv)),
        None => (SkillsSummary::default(), Vec::new()),
    };
    let mut counts = Counts::default();
    for a in &agents {
        if a.error.is_some() {
            counts.warnings += 1;
        }
    }
    counts.errors += secrets.len();
    for d in &drift {
        match d.kind {
            "conflict" => counts.warnings += 1,
            _ => counts.info += 1,
        }
    }
    for i in &skills.issues {
        match Severity::from_level(&i.level) {
            Severity::Error => counts.errors += 1,
            Severity::Warn => counts.warnings += 1,
            _ => counts.info += 1,
        }
    }
    let mut report = ScanReport {
        version: super::VERSION,
        project: project.map(|p| p.display().to_string()),
        agents,
        secrets,
        drift,
        skills,
        startup,
        counts,
        next_steps: Vec::new(),
    };
    report.next_steps = next_steps(&report);
    report
}

fn agent_rows(snapshot: &Snapshot) -> Vec<AgentRow> {
    let mut rows: Vec<AgentRow> = snapshot
        .present()
        .map(|c| AgentRow {
            id: c.id.clone(),
            name: display_name(c),
            config_path: format::home_relative(Path::new(&c.config_path)),
            servers: c
                .servers
                .iter()
                .chain(&c.plugin_servers)
                .filter(|s| !clients::detected_is_gateway(s))
                .count(),
            gateway: match c.entry_state {
                GatewayEntryState::Managed => "connected",
                GatewayEntryState::Customized => "customized",
                GatewayEntryState::Absent if c.gateway_installed => "connected",
                GatewayEntryState::Absent => "not-connected",
            },
            // The detected error is an OS or parser message that can quote config text.
            error: c
                .error
                .as_ref()
                .map(|_| "config can't be read: check its syntax and permissions".to_string()),
            installed: c.app_present,
        })
        .collect();
    rows.sort_by_cached_key(|a| a.name.to_lowercase());
    rows
}

pub fn display_name(c: &DetectedClient) -> String {
    match c.claude_profile.as_ref().and_then(|p| p.command.as_deref()) {
        Some(command) if !c.name.contains(command) => format!("{} ({command})", c.name),
        _ => c.name.clone(),
    }
}

/// Whether a plain setting is worth an alarm. The import flow vaults anything that might
/// be a credential, including long high-entropy values; a doctor only reports a setting
/// whose name says it is a credential, or whose value has a credential's shape. A path
/// list or a URL without user info is not one.
pub fn reportable(key: &str, value: Option<&str>) -> bool {
    if crate::import_credentials::secret_env(key, None) || key.eq_ignore_ascii_case("authorization")
    {
        return true;
    }
    let Some(v) = value.map(str::trim) else {
        return false;
    };
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_live_",
        "rk_live_",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "xox",
        "AKIA",
        "ASIA",
        "AIza",
        "ya29.",
        "-----BEGIN",
        "npm_",
        "pypi-",
        "hf_",
        "dop_v1_",
    ];
    if PREFIXES.iter().any(|p| v.starts_with(p)) {
        return true;
    }
    if url::Url::parse(v).is_ok_and(|u| url_has_userinfo(&u)) {
        return true;
    }
    // A JWT, or a long high-entropy blob that isn't a path, a list or a URL. Base64 can
    // hold '/', so only a leading one marks a path.
    let jwt = v.starts_with("eyJ") && v.matches('.').count() == 2;
    let blob = v.len() >= 32
        && !v.starts_with(['/', '~', '.'])
        && !v.contains(['\\', ':', ';', ',', ' '])
        && crate::import_credentials::secret_env("VALUE", Some(v));
    jwt || blob
}

/// What a reference is replaced with before a value is checked for credentials.
pub const REFERENCE: &str = "toolportref";

/// `value` with each reference (`${X}`, `${env:X}`, `{env:X}`, `{file:x}`, `$X`) replaced
/// by [`REFERENCE`], and whether there was one.
pub fn without_references(value: &str) -> (String, bool) {
    let mut out = String::with_capacity(value.len());
    let mut found = false;
    let mut rest = value;
    while !rest.is_empty() {
        let braced = if rest.starts_with("${") {
            Some(2)
        } else if rest.starts_with("{env:") || rest.starts_with("{file:") {
            Some(1)
        } else {
            None
        };
        if let Some(skip) = braced {
            if let Some(end) = rest[skip..].find('}') {
                out.push_str(REFERENCE);
                found = true;
                rest = &rest[skip + end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('$') {
            let len = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            if len > 0 && !after.starts_with(|c: char| c.is_ascii_digit()) {
                out.push_str(REFERENCE);
                found = true;
                rest = &after[len..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    (out, found)
}

/// A value whose credential comes from somewhere else (`${GITHUB_TOKEN}`, `${env:X}`,
/// `{env:X}`, `$X`, `Bearer ${T}`) is not a plaintext secret. The text around the
/// references still counts: `postgres://admin:hunter2@${HOST}` holds a password.
pub fn is_reference(value: &str) -> bool {
    let (rest, found) = without_references(value.trim());
    found && !reportable("", Some(&rest))
}

/// A user name or password in a URL that isn't a reference.
pub fn url_has_userinfo(url: &url::Url) -> bool {
    let named = |s: &str| !s.is_empty() && s != REFERENCE;
    named(url.username()) || url.password().is_some_and(named)
}

fn definition_value(definition: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let d = definition?;
    if key == crate::secrets::HTTP_AUTH_KEY {
        for field in ["headers", "http_headers"] {
            if let Some(obj) = d.get(field).and_then(|v| v.as_object()) {
                if let Some((_, v)) = obj
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                {
                    return v.as_str().map(String::from);
                }
            }
        }
        return None;
    }
    ["env", "environment", "envs", "headers", "http_headers"]
        .into_iter()
        .find_map(|field| d.get(field)?.get(key))
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
}

/// Plaintext credentials in one detected server, by Toolport's import rules.
pub fn server_secrets(
    client: &DetectedClient,
    server: &McpServer,
    definition: Option<&serde_json::Value>,
) -> Vec<SecretFinding> {
    let finding = |kind: &'static str, setting: String| SecretFinding {
        client_id: client.id.clone(),
        client_name: display_name(client),
        config_path: format::home_relative(Path::new(&client.config_path)),
        server: server.name.clone(),
        kind,
        setting,
        in_toolport: false,
    };
    let entry = crate::registry_controller::server_from_detected(server, &client.id);
    let import = match crate::import_credentials::Import::prepare(entry, definition) {
        Ok(import) => import,
        Err(_) => {
            return if server
                .command
                .as_deref()
                .is_some_and(crate::registry::arg_looks_secret)
            {
                vec![finding("command", "command".into())]
            } else {
                Vec::new()
            };
        }
    };
    let mut out = Vec::new();
    for (key, secret, present, _) in import.review_credentials() {
        if !secret || !present {
            continue;
        }
        if key == crate::secrets::IMPORTED_URL_KEY {
            if server.url.as_deref().is_some_and(|u| !is_reference(u)) {
                out.push(finding("url", "URL".into()));
            }
        } else if let Some(index) = key
            .strip_prefix("IMPORTED_ARG_")
            .and_then(|i| i.parse::<usize>().ok())
        {
            if server.args.get(index).is_some_and(|a| !is_reference(a)) {
                out.push(finding("arg", format!("argument {}", index + 1)));
            }
        } else if key == crate::secrets::HTTP_AUTH_KEY {
            if !definition_value(definition, &key).is_some_and(|v| is_reference(&v)) {
                out.push(finding("header", "Authorization".into()));
            }
        } else {
            let value = definition_value(definition, &key);
            if value.as_deref().is_some_and(is_reference) || !reportable(&key, value.as_deref()) {
                continue;
            }
            let header = definition.is_some_and(|d| {
                ["headers", "http_headers"]
                    .iter()
                    .any(|f| d.get(f).and_then(|h| h.get(&key)).is_some())
            });
            out.push(finding(if header { "header" } else { "env" }, key));
        }
    }
    out
}

pub fn secret_findings(snapshot: &Snapshot) -> Vec<SecretFinding> {
    let mut seen_files = BTreeSet::new();
    let mut out = BTreeSet::new();
    for client in snapshot.present() {
        if client.error.is_some() || client.uses_connectors {
            continue;
        }
        // Several rows can read one file (T3 Code instances, profile aliases): report it once.
        let file = std::fs::canonicalize(&client.config_path)
            .unwrap_or_else(|_| client.config_path.clone().into());
        for server in client.servers.iter().chain(&client.plugin_servers) {
            if clients::detected_is_gateway(server)
                || !seen_files.insert((file.clone(), server.name.clone()))
            {
                continue;
            }
            let definition = clients::import_definition(client, &server.name)
                .ok()
                .flatten();
            let held = snapshot.in_toolport(server);
            out.extend(
                server_secrets(client, server, definition.as_ref())
                    .into_iter()
                    .map(|f| SecretFinding {
                        in_toolport: held,
                        ..f
                    }),
            );
        }
    }
    out.into_iter().collect()
}

/// The program a server runs, or the scheme and host it connects to. Arguments and URL
/// paths are left out: an import moves credentials out of them.
fn target(command: Option<&str>, url: Option<&str>) -> String {
    if let Some(url) = url.filter(|u| !u.is_empty()) {
        return url::Url::parse(url)
            .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
            .unwrap_or_else(|_| url.to_string());
    }
    let command = command.unwrap_or("");
    Path::new(command)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| command.to_string())
}

/// The parts of a server that decide what it runs or connects to, with secrets masked.
fn identity(server: &McpServer) -> String {
    let url = server
        .url
        .as_deref()
        .map(crate::import_credentials::shown_url);
    let args = crate::import_credentials::shown_args(&server.args);
    format!(
        "{}|{}|{}|{}",
        server.transport,
        server.command.as_deref().unwrap_or(""),
        args.join("\u{1f}"),
        url.unwrap_or_default()
    )
}

pub fn drift(snapshot: &Snapshot) -> Vec<DriftFinding> {
    // name -> (shown name, identity -> clients, already in Toolport)
    type Slot = (String, BTreeMap<String, BTreeSet<String>>, bool);
    let mut by_name: BTreeMap<String, Slot> = BTreeMap::new();
    let mut files_seen: BTreeSet<(std::path::PathBuf, String)> = BTreeSet::new();
    for client in snapshot.present() {
        if client.error.is_some() || client.uses_connectors {
            continue;
        }
        let file = std::fs::canonicalize(&client.config_path)
            .unwrap_or_else(|_| client.config_path.clone().into());
        for server in &client.servers {
            if clients::detected_is_gateway(server)
                || !files_seen.insert((file.clone(), server.name.to_lowercase()))
            {
                continue;
            }
            let slot = by_name
                .entry(server.name.to_lowercase())
                .or_insert_with(|| (server.name.clone(), BTreeMap::new(), false));
            slot.2 |= snapshot.in_toolport(server);
            slot.1
                .entry(identity(server))
                .or_default()
                .insert(display_name(client));
        }
    }
    let mut out = Vec::new();
    for (_, (name, identities, in_toolport)) in by_name {
        let clients: BTreeSet<String> = identities.values().flatten().cloned().collect();
        if clients.len() < 2 {
            continue;
        }
        let clients: Vec<String> = clients.into_iter().collect();
        if identities.len() == 1 {
            out.push(DriftFinding {
                detail: if in_toolport {
                    format!(
                        "in Toolport, and still configured separately in {} agents; connect them to share one copy",
                        clients.len()
                    )
                } else {
                    format!(
                        "configured separately in {} agents; add it to Toolport once and every agent shares it",
                        clients.len()
                    )
                },
                server: name,
                kind: "duplicate",
                in_toolport,
                clients,
            });
        } else {
            out.push(DriftFinding {
                detail: format!(
                    "{} different definitions (command, arguments or URL differ)",
                    identities.len()
                ),
                server: name,
                kind: "conflict",
                in_toolport,
                clients,
            });
        }
    }
    out.sort_by_key(|d| (d.kind != "conflict", d.server.to_lowercase()));
    out
}

fn skills_summary(inv: &crate::skills::Inventory, include_info: bool) -> SkillsSummary {
    let shared = crate::clients::home().map(|h| h.join(".agents").join("skills"));
    let shared_folder = shared
        .as_ref()
        .filter(|p| p.is_dir())
        .map(|p| format::home_relative(p));
    let shared_readers = inv
        .agents
        .iter()
        .filter(|a| {
            a.roots.iter().any(|r| {
                r.exists && {
                    let p = r.path.replace('\\', "/");
                    p.ends_with("/.agents/skills") && p.starts_with('~')
                }
            })
        })
        .map(|a| a.name.clone())
        .collect();
    let issues = inv
        .issues
        .iter()
        .filter(|i| !i.dismissed)
        .filter(|i| include_info || i.level != "info" || i.fix_label.is_some())
        .map(|i| IssueRow {
            id: i.id.clone(),
            level: i.level.clone(),
            title: i.title.clone(),
            detail: i.detail.clone(),
            agents: i.agents.iter().map(|id| agent_name(inv, id)).collect(),
            fixable: i.fix_label.is_some(),
            fix_label: i.fix_label.clone(),
        })
        .collect();
    SkillsSummary {
        total: inv.skills.len(),
        rules_files: inv.rules.len(),
        shared_folder,
        shared_readers,
        issues,
    }
}

pub fn agent_name(inv: &crate::skills::Inventory, id: &str) -> String {
    inv.agents
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Tokens an agent loads before the first prompt: every rules file in its load chain
/// (imports included) and the name and description of every skill it lists. Counted with
/// the cl100k tokenizer, so figures are close to, not exactly, each model's own count.
pub fn budget(inv: &crate::skills::Inventory) -> Vec<BudgetRow> {
    let mut file_tokens: BTreeMap<&str, u64> = BTreeMap::new();
    let mut rows = Vec::new();
    for agent in &inv.agents {
        let mut row = BudgetRow {
            agent_id: agent.id.clone(),
            agent_name: agent.name.clone(),
            ..Default::default()
        };
        for rule in inv
            .rules
            .iter()
            .filter(|r| r.agents.contains_key(&agent.id))
        {
            let tokens = *file_tokens.entry(rule.key.as_str()).or_insert_with(|| {
                read_capped(Path::new(&rule.key))
                    .map(|t| crate::savings::count_tokens(&t))
                    .unwrap_or(0)
            });
            row.rules_tokens += tokens;
            row.rules_files.push(RulesTokens {
                path: rule.path.clone(),
                tokens,
            });
        }
        for skill in &inv.skills {
            let Some(state) = skill.states.get(&agent.id) else {
                continue;
            };
            let text = match state.state {
                crate::skills::State::On => {
                    let desc: String = skill
                        .description
                        .chars()
                        .take(crate::skills::DESCRIPTION_CAP)
                        .collect();
                    format!("- {}: {}\n", skill.name, desc)
                }
                crate::skills::State::NameOnly => format!("- {}\n", skill.name),
                _ => continue,
            };
            row.skills_loaded += 1;
            row.skills_tokens += crate::savings::count_tokens(&text);
        }
        row.total_tokens = row.rules_tokens + row.skills_tokens;
        if row.total_tokens > 0 {
            rows.push(row);
        }
    }
    rows.sort_by_key(|a| std::cmp::Reverse(a.total_tokens));
    rows
}

fn read_capped(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 2 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn next_steps(report: &ScanReport) -> Vec<String> {
    let mut steps = Vec::new();
    let fixable = report.skills.issues.iter().filter(|i| i.fixable).count();
    if !report.secrets.is_empty() || fixable > 0 {
        steps.push("Run 'toolport fix --dry-run' to preview safe fixes".into());
    }
    if report.drift.iter().any(|d| d.kind == "duplicate") {
        steps.push(
            "Run 'toolport init' in a repo to keep its servers, rules and skills in one toolport.toml"
                .into(),
        );
    }
    steps.push("Run 'toolport usage' to see where your tokens go".into());
    steps
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

pub fn render(report: &ScanReport, style: &Style, all: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        style.bold("toolport scan"),
        style.dim(&match &report.project {
            Some(p) => format!("· project {}", format::home_relative(Path::new(p))),
            None => String::new(),
        })
    ));

    // Agents
    let connected = report
        .agents
        .iter()
        .filter(|a| a.gateway == "connected")
        .count();
    out.push_str(&style.heading(&format!(
        "Agents  {}",
        style.dim(&format!(
            "{} found, {} connected to Toolport",
            report.agents.len(),
            connected
        ))
    )));
    if report.agents.is_empty() {
        out.push_str(&format!(
            "  {} No coding agents found on this machine.\n",
            style.mark(Severity::Info)
        ));
    }
    let shown = if all { usize::MAX } else { 14 };
    let rows: Vec<Vec<String>> = report
        .agents
        .iter()
        .take(shown)
        .map(|a| {
            let (mark, status) = match (&a.error, a.gateway) {
                (Some(e), _) => (style.mark(Severity::Warn), style.yellow(e)),
                (None, "connected") => (style.mark(Severity::Ok), style.green("gateway")),
                (None, "customized") => (style.mark(Severity::Info), style.dim("gateway (edited)")),
                (None, _) if a.servers == 0 => (style.mark(Severity::Info), style.dim("no MCP")),
                _ => (style.mark(Severity::Info), style.dim("direct")),
            };
            // Servers besides the Toolport entry, which a connected agent reaches through it.
            let servers = match (a.servers, a.gateway) {
                (0, "connected" | "customized") => style.dim("Toolport only"),
                (n, "connected" | "customized") => format!("Toolport + {n}"),
                (n, _) => format::plural(n, "server", "servers"),
            };
            vec![
                mark,
                a.name.clone(),
                servers,
                status,
                style.dim(&format::truncate_left(&a.config_path, 48)),
            ]
        })
        .collect();
    out.push_str(&format::table(
        &rows,
        &[
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Left,
            Align::Left,
        ],
        2,
    ));
    if report.agents.len() > shown {
        out.push_str(&format!(
            "  {}\n",
            style.dim(&format!(
                "and {} more (--all to list them)",
                report.agents.len() - shown
            ))
        ));
    }

    // Secrets
    out.push_str(&style.heading(&format!(
        "Plaintext secrets on disk  {}",
        if report.secrets.is_empty() {
            style.green("none")
        } else {
            style.red(&format!("{}", report.secrets.len()))
        }
    )));
    if report.secrets.is_empty() {
        out.push_str(&format!(
            "  {} No credentials sit in plain text in agent configs.\n",
            style.mark(Severity::Ok)
        ));
    } else {
        let rows: Vec<Vec<String>> = report
            .secrets
            .iter()
            .map(|s| {
                vec![
                    style.mark(Severity::Error),
                    s.server.clone(),
                    style.yellow(&s.setting),
                    s.client_name.clone(),
                    style.dim(&format::truncate_left(&s.config_path, 44)),
                    if s.in_toolport {
                        style.dim("in Toolport")
                    } else {
                        String::new()
                    },
                ]
            })
            .collect();
        out.push_str(&format::table(
            &rows,
            &[
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Left,
                Align::Left,
            ],
            2,
        ));
        if report.secrets.iter().any(|s| !s.in_toolport) {
            out.push_str(&format!(
                "  {}\n",
                style.hint(&format!(
                    "{} moves them into your OS keychain",
                    style.cmd("toolport fix --only secrets")
                ))
            ));
        }
        if report.secrets.iter().any(|s| s.in_toolport) {
            out.push_str(&format!(
                "  {}\n",
                style.hint(&format!(
                    "Toolport already holds the ones marked in Toolport; {} removes the agents' copies (needs the Toolport app or toolport-gateway)",
                    style.cmd("toolport fix --only secrets --connect")
                ))
            ));
        }
    }

    // Drift
    let conflicts = report.drift.iter().filter(|d| d.kind == "conflict").count();
    out.push_str(&style.heading(&format!(
        "MCP drift  {}",
        style.dim(&format!(
            "{} shared, {} conflicting",
            report.drift.len() - conflicts,
            conflicts
        ))
    )));
    if report.drift.is_empty() {
        out.push_str(&format!(
            "  {} Every server is defined in one place.\n",
            style.mark(Severity::Ok)
        ));
    }
    let limit = if all { usize::MAX } else { 8 };
    for d in report.drift.iter().take(limit) {
        let mark = if d.kind == "conflict" {
            style.mark(Severity::Warn)
        } else {
            style.mark(Severity::Info)
        };
        out.push_str(&format!(
            "  {} {}  {}\n      {}\n",
            mark,
            style.bold(&d.server),
            style.dim(&d.detail),
            style.dim(&format::truncate(&d.clients.join(", "), 90))
        ));
    }
    if report.drift.len() > limit {
        out.push_str(&format!(
            "  {}\n",
            style.dim(&format!("and {} more (--all)", report.drift.len() - limit))
        ));
    }

    // Skills & rules
    let s = &report.skills;
    out.push_str(&style.heading(&format!(
        "Skills & rules  {}",
        style.dim(&format!(
            "{} skills, {} rules files",
            s.total, s.rules_files
        ))
    )));
    match &s.shared_folder {
        Some(folder) => out.push_str(&format!(
            "  {} Works with skills.sh: {} is read by {}\n",
            style.mark(Severity::Ok),
            folder,
            if s.shared_readers.is_empty() {
                "no agent here yet".to_string()
            } else {
                format::truncate(&s.shared_readers.join(", "), 70)
            }
        )),
        None => out.push_str(&format!(
            "  {} No ~/.agents/skills yet. {} installs shared skills there.\n",
            style.mark(Severity::Info),
            style.cmd("npx skills add <owner/repo>")
        )),
    }
    if s.issues.is_empty() {
        out.push_str(&format!(
            "  {} No skills or rules problems.\n",
            style.mark(Severity::Ok)
        ));
    }
    for i in &s.issues {
        let sev = Severity::from_level(&i.level);
        let fix = if i.fixable {
            format!("  {}", style.cyan("[fixable]"))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {} {}{}\n      {}\n",
            style.mark(sev),
            i.title,
            fix,
            style.dim(&format::truncate(&i.detail, 100))
        ));
    }

    // Startup tax
    if !report.startup.is_empty() {
        out.push_str(&style.heading(&format!(
            "Turn-0 context  {}",
            style.dim("rules + skill listings each agent loads before your first prompt")
        )));
        let max = report
            .startup
            .iter()
            .map(|r| r.total_tokens)
            .max()
            .unwrap_or(1)
            .max(1);
        let rows: Vec<Vec<String>> = report
            .startup
            .iter()
            .take(if all { usize::MAX } else { 8 })
            .map(|r| {
                vec![
                    r.agent_name.clone(),
                    style.cyan(&style.bar(r.total_tokens as f64 / max as f64, 18)),
                    format!("{} tok", format::thousands(r.total_tokens)),
                    style.dim(&format!(
                        "rules {} · {} {}",
                        format::compact(r.rules_tokens),
                        format::plural(r.skills_loaded, "skill", "skills"),
                        format::compact(r.skills_tokens)
                    )),
                ]
            })
            .collect();
        out.push_str(&format::table(
            &rows,
            &[Align::Left, Align::Left, Align::Right, Align::Left],
            2,
        ));
    }

    // Summary
    let c = &report.counts;
    let line = if c.errors + c.warnings == 0 {
        format!("{} All clear", style.mark(Severity::Ok))
    } else {
        format!(
            "{} {}, {}",
            if c.errors > 0 {
                style.mark(Severity::Error)
            } else {
                style.mark(Severity::Warn)
            },
            style.red(&format::plural(c.errors, "problem", "problems")),
            style.yellow(&format::plural(c.warnings, "warning", "warnings")),
        )
    };
    out.push_str(&format!("\n{}\n", style.bold(&line)));
    for step in &report.next_steps {
        out.push_str(&format!("  {}\n", style.hint(step)));
    }
    out
}

pub fn command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let project = ctx.project_str();
    let snapshot = Snapshot::read(project.as_deref());
    if let Some(e) = &snapshot.inventory_error {
        return Err(CliError::new(format!("couldn't read skills: {e}")));
    }
    let all = args.has("all");
    let report = collect(&snapshot, ctx.project.as_deref(), all);
    if ctx.json {
        ctx.emit_json(&report)?;
    } else {
        super::out(&render(&report, &ctx.style, all));
    }
    Ok(
        if args.has("check") && report.counts.errors + report.counts.warnings > 0 {
            EXIT_FINDINGS
        } else {
            EXIT_OK
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: &str, servers: Vec<McpServer>) -> DetectedClient {
        DetectedClient {
            id: id.into(),
            name: id.into(),
            uses_connectors: false,
            location_removable: false,
            location_kept_by_t3_code: false,
            discovery: crate::clients::discovery_capabilities("claude-code"),
            config_path: format!("/nonexistent/{id}.json"),
            config_exists: true,
            app_present: true,
            servers,
            plugin_servers: Vec::new(),
            gateway_installed: false,
            entry_state: GatewayEntryState::Absent,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
        }
    }

    fn server(name: &str, command: &str, args: &[&str], env: &[&str]) -> McpServer {
        McpServer {
            name: name.into(),
            transport: "stdio".into(),
            command: Some(command.into()),
            args: args.iter().map(|s| s.to_string()).collect(),
            env_keys: env.iter().map(|s| s.to_string()).collect(),
            url: None,
        }
    }

    #[test]
    fn references_are_not_plaintext() {
        for v in [
            "${GITHUB_TOKEN}",
            "${env:X}",
            "${input:tok}",
            "{env:X}",
            "$TOKEN",
            "Bearer ${T}",
        ] {
            assert!(is_reference(v), "{v}");
        }
        for v in ["ghp_abc", "sk-live-123", "$", "plain", "$1abc"] {
            assert!(!is_reference(v), "{v}");
        }
        // Text around a reference still counts.
        for v in [
            "postgres://admin:hunter2@${DB_HOST}/app",
            "${PREFIX}ghp_1234567890abcdefghijklmnop",
        ] {
            assert!(!is_reference(v), "{v}");
        }
        assert!(is_reference("postgres://${DB_USER}:${DB_PASS}@db/app"));
        assert!(is_reference("https://api.example.com/${PATH_PART}?x=1"));
        assert_eq!(
            without_references("a${B}c{env:D}$E_1/x$"),
            ("atoolportrefctoolportreftoolportref/x$".into(), true)
        );
    }

    #[test]
    fn only_credential_shaped_settings_are_reported() {
        for (k, v) in [
            ("GITHUB_TOKEN", "anything"),
            ("API_KEY", "x"),
            ("LOG", "ghp_1234567890abcdefghijklmnop"),
            ("DB", "postgres://user:hunter2@db.example.com/app"),
            ("BLOB", "4f9c2b7e1a6d3f8e0b5c9a2d7e4f1b3c6a9d2e5f"),
            ("JWT", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl"),
            ("SIGNING", "q8Zr/Kx3+T0aLm9vB2nY/cW7eRtU5pH1sDf4GjQ6="),
        ] {
            assert!(reportable(k, Some(v)), "{k}");
        }
        for (k, v) in [
            (
                "NODE_REPL_NODE_MODULE_DIRS",
                "/home/a/.cache/node/x9Fq2/lib:/opt/node_modules",
            ),
            (
                "NODE_REPL_TRUSTED_SERVICES",
                "github.com,api.openai.com,registry.npmjs.org",
            ),
            ("ENDPOINT", "https://api.example.com/v1?region=eu"),
            ("LOG_LEVEL", "debug"),
            (
                "NODE_REPL_TRUSTED_CODE_PATHS",
                "/home/a/work/q8Zr2Kx3T0aLm9vB2nY/cW7eRtU5pH1",
            ),
        ] {
            assert!(!reportable(k, Some(v)), "{k}");
        }
        assert!(!reportable("SOMETHING", None));
    }

    #[test]
    fn plaintext_env_header_url_and_args_are_found_without_values() {
        let c = client("cursor", vec![]);
        let s = server(
            "github",
            "npx",
            &["-y", "server-github"],
            &["GITHUB_TOKEN", "LOG_LEVEL"],
        );
        let def = serde_json::json!({"command":"npx","env":{"GITHUB_TOKEN":"ghp_1234567890abcdefghijklmnopqrstuv","LOG_LEVEL":"debug"}});
        let found = server_secrets(&c, &s, Some(&def));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "env");
        assert_eq!(found[0].setting, "GITHUB_TOKEN");
        let text = serde_json::to_string(&found).unwrap();
        assert!(!text.contains("ghp_"));

        let def = serde_json::json!({"command":"npx","env":{"GITHUB_TOKEN":"${GITHUB_TOKEN}"}});
        assert!(server_secrets(&c, &s, Some(&def)).is_empty());

        let mut remote = server("linear", "", &[], &[]);
        remote.command = None;
        remote.transport = "http".into();
        remote.url = Some("https://mcp.example.com/sse?api_key=abcdef0123456789abcdef".into());
        let found = server_secrets(&c, &remote, Some(&serde_json::json!({"url": remote.url})));
        assert_eq!(
            found.iter().map(|f| f.kind).collect::<Vec<_>>(),
            vec!["url"]
        );

        let args = server(
            "pg",
            "npx",
            &["server-postgres", "postgresql://u:hunter2@db/x"],
            &[],
        );
        let found = server_secrets(&c, &args, Some(&serde_json::json!({})));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, "arg");
        assert_eq!(found[0].setting, "argument 2");
    }

    #[test]
    fn drift_separates_duplicates_from_conflicts() {
        let snapshot = Snapshot {
            clients: vec![
                client(
                    "claude-code",
                    vec![
                        server("github", "npx", &["a"], &[]),
                        server("fs", "npx", &["x"], &[]),
                    ],
                ),
                client(
                    "cursor",
                    vec![
                        server("GitHub", "npx", &["a"], &[]),
                        server("fs", "npx", &["y"], &[]),
                    ],
                ),
                client("codex", vec![server("solo", "x", &[], &[])]),
            ],
            inventory: None,
            inventory_error: None,
            toolport: Vec::new(),
        };
        let d = drift(&snapshot);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].server, "fs");
        assert_eq!(d[0].kind, "conflict");
        assert_eq!(d[1].kind, "duplicate");
        assert_eq!(d[1].clients, vec!["claude-code", "cursor"]);
        assert!(d[1].detail.contains("add it to Toolport"));

        // Once Toolport holds github, the advice is to connect the agents instead.
        let snapshot = Snapshot {
            toolport: vec![(
                "github".into(),
                target(Some("/usr/bin/npx"), None),
                vec!["a".into()],
            )],
            ..snapshot
        };
        let d = drift(&snapshot);
        assert!(d[1].in_toolport && d[1].detail.starts_with("in Toolport"));
        assert!(!d[0].in_toolport);
        let other_args = Snapshot {
            toolport: vec![(
                "github".into(),
                target(Some("npx"), None),
                vec!["some-other-server".into()],
            )],
            ..snapshot
        };
        assert!(!drift(&other_args)[1].in_toolport);
        let other_host = Snapshot {
            toolport: vec![(
                "github".into(),
                target(None, Some("https://a.example/mcp")),
                Vec::new(),
            )],
            ..other_args
        };
        assert!(!drift(&other_host)[1].in_toolport);
    }
}
