//! `toolport.toml`: one checked-in file that sets up every agent for a repo.
//!
//! ```toml
//! version = 1
//! name = "acme-api"
//!
//! [rules]
//! source = "AGENTS.md"            # every agent reads this file
//!
//! [[skills]]
//! path = ".agents/skills/release" # a skill in this repo
//!
//! [[skills]]
//! source = "vercel-labs/agent-skills@frontend-design"   # from skills.sh
//!
//! [[servers]]
//! name = "github"
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-github"]
//! env = ["GITHUB_TOKEN"]          # names only; values go in the OS keychain
//! [servers.secret_refs]
//! GITHUB_TOKEN = "op://Engineering/GitHub/token"
//! ```
//!
//! The file never holds a credential. `toolport up` refuses one that does, using the same
//! detectors a reviewed import and a shared setup use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::format::{self, Severity, Style};
use super::scan::Snapshot;
use super::{Args, CliError, Ctx, EXIT_OK};
use crate::import_credentials as creds;

pub const FILE_NAME: &str = "toolport.toml";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Rules>,
    #[serde(default)]
    pub skills: Vec<SkillSpec>,
    #[serde(default)]
    pub servers: Vec<ServerSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub source: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct SkillSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Env var names the server needs. Their values come from a secret reference, your
    /// environment, or a prompt, and are stored in the OS keychain.
    #[serde(default)]
    pub env: Vec<String>,
    /// Non-secret settings, written as plain values.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// Password manager references (`op://`, `doppler://`, `env:` ...) for names in `env`.
    #[serde(default)]
    pub secret_refs: BTreeMap<String, String>,
}

impl ServerSpec {
    pub fn transport(&self) -> &str {
        match self.transport.as_deref() {
            Some(t) => t,
            None if self.command.is_some() => "stdio",
            None => "http",
        }
    }
}

/// Parse and validate. Every problem is reported at once, with where it is.
pub fn load(text: &str) -> Result<Manifest, Vec<String>> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| vec![clean_toml_error(&e)])?;
    validate(&manifest)?;
    Ok(manifest)
}

fn clean_toml_error(e: &toml::de::Error) -> String {
    let msg = e.message().trim().to_string();
    match e.span() {
        Some(span) => format!("{msg} (at byte {})", span.start),
        None => msg,
    }
}

fn env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A relative path that stays inside the project.
fn inside(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && p.is_relative()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Why a URL carries a credential, if it does.
fn url_secret(url: &str) -> Option<&'static str> {
    let (url, _) = super::scan::without_references(url);
    let parsed = url::Url::parse(&url).ok()?;
    if super::scan::url_has_userinfo(&parsed) {
        return Some("a user name or password");
    }
    for (key, value) in parsed.query_pairs() {
        if creds::secret_url_name(&key)
            || (!super::scan::is_reference(&value) && creds::secret_env("VALUE", Some(&value)))
        {
            return Some("a credential in its query string");
        }
    }
    if creds::secret_url_path(&parsed) {
        return Some("a credential in its path");
    }
    None
}

/// Whether a plain setting looks like a secret. One that refers to another value is
/// still a secret when the text around the reference is credential-shaped.
fn value_secret(key: &str, value: &str) -> bool {
    if super::scan::without_references(value).1 {
        return !super::scan::is_reference(value);
    }
    creds::secret_word_is_last(key) || creds::secret_env("VALUE", Some(value))
}

/// Whether an argument holds a credential. `masked` is its entry in
/// `registry::secret_arg_mask`, which judges an argument by the flag before it.
fn arg_secret(arg: &str, masked: bool) -> bool {
    if super::scan::without_references(arg).1 {
        // Judge the value of `--flag=value` on its own, so a URL in it is parsed as one.
        let value = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => value,
            _ => arg,
        };
        return !super::scan::is_reference(value) || url_secret(value).is_some();
    }
    masked || url_secret(arg).is_some()
}

pub fn validate(m: &Manifest) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if m.version != 1 {
        errors.push(format!(
            "version = {} isn't supported; this toolport reads version = 1",
            m.version
        ));
    }
    if let Some(rules) = &m.rules {
        if !inside(&rules.source) {
            errors.push(format!(
                "[rules] source '{}' must be a relative path inside the project",
                rules.source
            ));
        }
    }
    for (i, skill) in m.skills.iter().enumerate() {
        let at = format!("[[skills]] #{}", i + 1);
        match (&skill.path, &skill.source) {
            (Some(_), Some(_)) | (None, None) => {
                errors.push(format!("{at}: set exactly one of path or source"))
            }
            (Some(path), None) if !inside(path) => errors.push(format!(
                "{at}: path '{path}' must be a relative path inside the project"
            )),
            (None, Some(source)) if parse_skill_source(source).is_none() => errors.push(format!(
                "{at}: source '{source}' should look like owner/repo or owner/repo@skill"
            )),
            _ => {}
        }
    }
    let mut names = BTreeSet::new();
    for s in &m.servers {
        let at = format!("server '{}'", s.name);
        if s.name.trim().is_empty() || s.name.len() > 64 || s.name.contains(['/', '\\', '\0']) {
            errors.push(format!(
                "server name '{}' must be 1 to 64 characters without slashes",
                s.name
            ));
        }
        if !names.insert(s.name.to_lowercase()) {
            errors.push(format!("{at} is listed twice"));
        }
        match s.transport() {
            _ if s.transport.is_none() && s.command.is_none() && s.url.is_none() => errors.push(
                format!("{at} needs a command (stdio) or a url (http or sse)"),
            ),
            "stdio" => {
                if s.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    errors.push(format!("{at}: stdio servers need a command"));
                }
                if s.url.is_some() {
                    errors.push(format!("{at}: stdio servers take a command, not a url"));
                }
            }
            "http" | "sse" => {
                if s.url.is_none() {
                    errors.push(format!("{at}: {} servers need a url", s.transport()));
                }
                if s.command.is_some() || !s.args.is_empty() {
                    errors.push(format!(
                        "{at}: {} servers take a url, not a command",
                        s.transport()
                    ));
                }
            }
            other => errors.push(format!(
                "{at}: transport '{other}' isn't one of stdio, http or sse"
            )),
        }
        if s.command
            .as_deref()
            .is_some_and(crate::registry::arg_looks_secret)
        {
            errors.push(format!(
                "{at}: the command contains a credential. Move it to env"
            ));
        }
        let mask = crate::registry::secret_arg_mask(&s.args);
        for (i, (arg, masked)) in s.args.iter().zip(mask).enumerate() {
            if !arg_secret(arg, masked) {
                continue;
            }
            if let Some(why) = url_secret(arg) {
                errors.push(format!("{at}: argument {} is a URL with {why}", i + 1));
            } else {
                errors.push(format!(
                    "{at}: argument {} contains a credential. Move it to env and refer to it as ${{NAME}}",
                    i + 1
                ));
            }
        }
        if let Some(url) = &s.url {
            // Never echo the URL: an unparseable one can still hold a password.
            match url::Url::parse(&super::scan::without_references(url).0) {
                Err(_) => errors.push(format!("{at}: the url isn't a valid URL")),
                Ok(_) => {
                    if let Some(why) = url_secret(url) {
                        errors.push(format!("{at}: the url has {why}. Move it to env"));
                    }
                }
            }
        }
        let env: BTreeSet<&str> = s.env.iter().map(String::as_str).collect();
        for name in &s.env {
            if !env_name(name) {
                errors.push(format!(
                    "{at}: env name '{name}' must be letters, digits and _"
                ));
            }
        }
        for (key, value) in &s.values {
            if !env_name(key) {
                errors.push(format!(
                    "{at}: value name '{key}' must be letters, digits and _"
                ));
            }
            if env.contains(key.as_str()) {
                errors.push(format!("{at}: '{key}' is in both env and values"));
            }
            if value_secret(key, value) {
                errors.push(format!(
                    "{at}: values.{key} looks like a credential. List '{key}' under env instead"
                ));
            }
        }
        for (key, reference) in &s.secret_refs {
            if !env.contains(key.as_str()) {
                errors.push(format!(
                    "{at}: secret_refs.{key} names a variable that isn't in env"
                ));
            }
            if crate::secret_refs::parse(reference).is_err() {
                errors.push(format!(
                    "{at}: secret_refs.{key} isn't a supported reference (op://, doppler://, infisical://, vault://, bws://, bw://, keeper://, dl://, lpass://, env:)"
                ));
            }
        }
        if errors.is_empty() {
            if let Err(e) = crate::secret_refs::validate_server(&to_entry(s, false, None)) {
                errors.push(format!("{at}: {}", e.message));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// `owner/repo@skill` or `owner/repo` (every skill in the repo).
pub fn parse_skill_source(source: &str) -> Option<(String, Option<String>)> {
    let (repo, skill) = match source.split_once('@') {
        Some((r, s)) if !s.is_empty() => (r, Some(s.to_string())),
        Some(_) => return None,
        None => (source, None),
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let (owner, name) = repo.split_once('/')?;
    (ok(owner) && ok(name) && skill.as_deref().is_none_or(ok)).then(|| (repo.to_string(), skill))
}

/// The exact command the skills CLI takes for a source.
pub fn skills_add_command(source: &str) -> String {
    match parse_skill_source(source) {
        Some((repo, Some(skill))) => format!("npx skills add {repo} --skill {skill}"),
        Some((repo, None)) => format!("npx skills add {repo}"),
        None => format!("npx skills add {source}"),
    }
}

/// Whether a stdio server runs something from the project, so it has to start in the
/// project folder: its command or an argument (or the value of `--flag=value`) is a
/// relative path such as `./server.js` or `dist/index.js`, or names something that exists
/// in the project. Package names (`@scope/pkg`), URLs, `owner/repo` style values and
/// absolute or `~`/`${VAR}` paths don't count, so the same `npx` server in two projects
/// stays one definition.
fn runs_project_files(s: &ServerSpec, project: &Path) -> bool {
    let relative = |token: &str| {
        let t = match token.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => value,
            _ if token.starts_with('-') => return false,
            _ => token,
        }
        .trim();
        if t.is_empty()
            || t.contains("://")
            || t.starts_with(['@', '~', '$'])
            || Path::new(t).is_absolute()
        {
            return false;
        }
        if t == "." || t == ".." || t.starts_with("./") || t.starts_with("../") {
            return true;
        }
        if cfg!(windows) && (t.starts_with(".\\") || t.starts_with("..\\")) {
            return true;
        }
        let path = Path::new(t);
        project.join(path).exists()
            || (t.contains(['/', '\\']) && !t.contains(':') && path.extension().is_some())
    };
    s.transport() == "stdio"
        && (s.command.as_deref().is_some_and(relative) || s.args.iter().any(|a| relative(a)))
}

fn normalize_project_root(project: &Path) -> String {
    let resolved = std::fs::canonicalize(project)
        .or_else(|_| std::path::absolute(project))
        .unwrap_or_else(|_| project.to_path_buf());
    resolved.to_string_lossy().into_owned()
}

fn owning_project(entry: &crate::registry::ServerEntry) -> Option<&str> {
    entry
        .unknown_fields
        .get("projectRoot")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn same_project_owner(
    existing: &crate::registry::ServerEntry,
    wanted: &crate::registry::ServerEntry,
) -> bool {
    existing.source.as_deref() == Some("project")
        && owning_project(existing).is_some()
        && owning_project(existing) == owning_project(wanted)
}

/// The registry entry a manifest server becomes. Env vars are secret with no value; their
/// values only ever reach the keychain. A stdio server that runs files from the project
/// gets the project folder as its working directory, so its relative paths resolve there
/// and not wherever the gateway was started.
pub fn to_entry(
    s: &ServerSpec,
    enabled: bool,
    project: Option<&Path>,
) -> crate::registry::ServerEntry {
    let mut env: Vec<serde_json::Value> = s
        .env
        .iter()
        .map(|key| match s.secret_refs.get(key) {
            Some(r) => serde_json::json!({"key": key, "secret": true, "source": {"ref": r}}),
            None => serde_json::json!({"key": key, "secret": true}),
        })
        .collect();
    env.extend(
        s.values
            .iter()
            .map(|(k, v)| serde_json::json!({"key": k, "value": v, "secret": false})),
    );
    let mut entry = serde_json::json!({
        "enabled": enabled,
        "id": "",
        "name": s.name,
        "transport": s.transport(),
        "args": s.args,
        "env": env,
        "source": "project",
    });
    if let Some(c) = &s.command {
        entry["command"] = serde_json::json!(c);
    }
    if let Some(u) = &s.url {
        entry["url"] = serde_json::json!(u);
    }
    if let Some(project) = project {
        let owner = normalize_project_root(project);
        entry["projectRoot"] = serde_json::json!(owner);
        if runs_project_files(s, project) {
            let dir = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
            entry["cwd"] = serde_json::json!(dir.to_string_lossy());
        }
    }
    serde_json::from_value(entry).expect("manifest server maps onto a registry entry")
}

// ---------------------------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------------------------

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn toml_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| toml_str(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// What `init` found in the project.
#[derive(Debug, Default)]
pub struct Found {
    pub rules: Option<String>,
    pub skills: Vec<String>,
    pub servers: Vec<(ServerSpec, String)>,
    pub skipped: Vec<String>,
}

pub fn inspect(project: &Path) -> Found {
    let mut found = Found::default();
    for name in ["AGENTS.md", "CLAUDE.md", "GEMINI.md"] {
        if project.join(name).is_file() {
            found.rules = Some(name.into());
            break;
        }
    }
    let mut seen = BTreeSet::new();
    for root in [
        ".agents/skills",
        ".claude/skills",
        ".cursor/skills",
        ".opencode/skills",
    ] {
        let dir = project.join(root);
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("SKILL.md").is_file())
            .collect();
        entries.sort();
        for path in entries {
            let real = std::fs::canonicalize(&path).unwrap_or(path.clone());
            if seen.insert(real) {
                let rel = path
                    .strip_prefix(project)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                found.skills.push(rel);
            }
        }
    }
    let mut names = BTreeSet::new();
    for (file, key) in [
        (".mcp.json", "mcpServers"),
        (".cursor/mcp.json", "mcpServers"),
        (".vscode/mcp.json", "servers"),
        (".gemini/settings.json", "mcpServers"),
    ] {
        let Ok(text) = std::fs::read_to_string(project.join(file)) else {
            continue;
        };
        let Ok(doc) = json5::from_str::<serde_json::Value>(&text) else {
            found.skipped.push(format!("{file}: couldn't parse it"));
            continue;
        };
        let Some(servers) = doc.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, def) in servers {
            if !names.insert(name.to_lowercase()) {
                continue;
            }
            match server_from_json(name, def) {
                Ok(spec) => found.servers.push((spec, file.to_string())),
                Err(why) => found.skipped.push(format!("{name} ({file}): {why}")),
            }
        }
    }
    found
}

fn server_from_json(name: &str, def: &serde_json::Value) -> Result<ServerSpec, String> {
    let s = |k: &str| def.get(k).and_then(|v| v.as_str()).map(String::from);
    let mut spec = ServerSpec {
        name: name.into(),
        command: s("command"),
        url: s("url").or_else(|| s("httpUrl")).or_else(|| s("serverUrl")),
        args: def
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    let kind = s("type").or_else(|| s("transport"));
    spec.transport = match kind.as_deref() {
        Some("sse") => Some("sse".into()),
        Some("http" | "streamable-http" | "streamableHttp") => Some("http".into()),
        _ if spec.command.is_some() => None,
        _ => Some("http".into()),
    };
    if spec
        .command
        .as_deref()
        .is_some_and(crate::registry::arg_looks_secret)
    {
        return Err("its command holds a credential; move it to an env var first".into());
    }
    let mask = crate::registry::secret_arg_mask(&spec.args);
    if spec.args.iter().zip(mask).any(|(a, m)| arg_secret(a, m)) {
        return Err("an argument holds a credential; move it to an env var first".into());
    }
    if let Some(url) = &spec.url {
        if url_secret(url).is_some() {
            return Err("its URL holds a credential; add it by hand without the secret".into());
        }
    }
    if def
        .get("headers")
        .and_then(|h| h.as_object())
        .is_some_and(|h| !h.is_empty())
    {
        return Err("it sends HTTP headers, which toolport.toml doesn't describe yet".into());
    }
    if let Some(env) = def.get("env").and_then(|v| v.as_object()) {
        for (key, value) in env {
            let value = value.as_str().unwrap_or_default();
            if super::scan::is_reference(value) || value_secret(key, value) || value.is_empty() {
                spec.env.push(key.clone());
            } else {
                spec.values.insert(key.clone(), value.to_string());
            }
        }
    }
    Ok(spec)
}

pub fn render_manifest(name: &str, found: &Found) -> String {
    let mut out = String::new();
    out.push_str("# toolport.toml: one setup for every coding agent in this repo.\n");
    out.push_str(
        "# Run 'npx @toolport/cli up' to link these rules, skills and servers into each agent.\n",
    );
    out.push_str(
        "# Never put a secret here: list the variable under env and toolport keeps the value\n",
    );
    out.push_str("# in your OS keychain.\n\n");
    out.push_str("version = 1\n");
    out.push_str(&format!("name = {}\n", toml_str(name)));
    if let Some(rules) = &found.rules {
        out.push_str("\n# The rules file every agent reads. Agents that look for another file name get a\n# pointer to this one.\n");
        out.push_str(&format!("[rules]\nsource = {}\n", toml_str(rules)));
    }
    if found.skills.is_empty() {
        out.push_str("\n# Skills: a folder in this repo, or one from skills.sh.\n# [[skills]]\n# path = \".agents/skills/my-skill\"\n# [[skills]]\n# source = \"owner/repo@skill\"\n");
    }
    for skill in &found.skills {
        out.push_str(&format!("\n[[skills]]\npath = {}\n", toml_str(skill)));
    }
    for (s, file) in &found.servers {
        out.push_str(&format!(
            "\n# From {file}\n[[servers]]\nname = {}\n",
            toml_str(&s.name)
        ));
        if let Some(t) = &s.transport {
            out.push_str(&format!("transport = {}\n", toml_str(t)));
        }
        if let Some(c) = &s.command {
            out.push_str(&format!("command = {}\n", toml_str(c)));
        }
        if !s.args.is_empty() {
            out.push_str(&format!("args = {}\n", toml_list(&s.args)));
        }
        if let Some(u) = &s.url {
            out.push_str(&format!("url = {}\n", toml_str(u)));
        }
        if !s.env.is_empty() {
            out.push_str(&format!("env = {}\n", toml_list(&s.env)));
        }
        if !s.values.is_empty() {
            out.push_str("[servers.values]\n");
            for (k, v) in &s.values {
                out.push_str(&format!("{k} = {}\n", toml_str(v)));
            }
        }
    }
    if found.servers.is_empty() {
        out.push_str("\n# MCP servers: env lists names only.\n# [[servers]]\n# name = \"github\"\n# command = \"npx\"\n# args = [\"-y\", \"@modelcontextprotocol/server-github\"]\n# env = [\"GITHUB_TOKEN\"]\n");
    }
    for skipped in &found.skipped {
        out.push_str(&format!("\n# Not included: {skipped}\n"));
    }
    out
}

pub fn init_command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let project = ctx.project.clone().ok_or_else(|| {
        CliError::new("couldn't tell which folder to set up").hint("pass --project <DIR>")
    })?;
    let found = inspect(&project);
    let name = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let text = render_manifest(&name, &found);
    // What init writes must be what up accepts.
    if let Err(errors) = load(&text) {
        return Err(CliError::new(format!(
            "generated manifest didn't validate: {}",
            errors.join("; ")
        )));
    }
    let out_path = args.value("out").unwrap_or(FILE_NAME);
    if out_path == "-" {
        super::out(&text);
        return Ok(EXIT_OK);
    }
    let path = project.join(out_path);
    if path.exists() && !args.has("force") {
        return Err(
            CliError::new(format!("{} already exists", format::home_relative(&path)))
                .hint("pass --force to replace it, or --out - to print"),
        );
    }
    std::fs::write(&path, &text)
        .map_err(|e| CliError::new(format!("couldn't write {}: {e}", path.display())))?;
    if ctx.json {
        ctx.emit_json(&serde_json::json!({
            "path": path,
            "rules": found.rules,
            "skills": found.skills,
            "servers": found.servers.iter().map(|(s, _)| &s.name).collect::<Vec<_>>(),
            "skipped": found.skipped,
        }))?;
        return Ok(EXIT_OK);
    }
    let s = &ctx.style;
    let mut msg = format!(
        "{} Wrote {}\n",
        s.mark(Severity::Ok),
        s.bold(&format::home_relative(&path))
    );
    msg.push_str(&format!(
        "  rules: {} · {} · {}\n",
        found.rules.as_deref().unwrap_or("none"),
        format::plural(found.skills.len(), "skill", "skills"),
        format::plural(found.servers.len(), "server", "servers")
    ));
    for skipped in &found.skipped {
        msg.push_str(&format!("  {} {}\n", s.mark(Severity::Warn), skipped));
    }
    msg.push_str(&format!(
        "  {}\n",
        s.hint(&format!(
            "commit it, then {} sets up each agent",
            s.cmd("toolport up")
        ))
    ));
    super::out(&msg);
    Ok(EXIT_OK)
}

// ---------------------------------------------------------------------------------------------
// up
// ---------------------------------------------------------------------------------------------

/// Agents that read their own rules file name and follow `@path` imports in it.
const IMPORT_RULES: &[(&str, &str)] = &[
    ("claude-code", "CLAUDE.md"),
    ("gemini-cli", "GEMINI.md"),
    ("qwen-code", "QWEN.md"),
];

/// Coding agents `up` sets up when `--agents` isn't given.
const CODING_AGENTS: &[&str] = &[
    "claude-code",
    "codex",
    "cursor",
    "gemini-cli",
    "opencode",
    "windsurf",
    "vscode",
    "zed",
    "amp",
    "kilo-code",
    "roo-code",
    "cline",
    "github-copilot-cli",
    "qwen-code",
    "droid",
    "goose",
    "crush",
    "kiro",
    "junie",
    "antigravity",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Step {
    RulesPointer {
        agent: String,
        path: String,
        create: bool,
        line: String,
    },
    RulesLink {
        agent: String,
        path: String,
        target: String,
    },
    RulesOk {
        agent: String,
        path: String,
    },
    SkillLink {
        skill: String,
        agent: String,
        link: String,
        target: String,
    },
    SkillOk {
        skill: String,
        agents: Vec<String>,
    },
    SkillInstall {
        source: String,
        command: String,
    },
    ServerAdd {
        name: String,
        /// What it runs or connects to, credentials masked. Shown before anything runs.
        runs: String,
        needs: Vec<String>,
        from_env: Vec<String>,
        refs: Vec<String>,
        warnings: Vec<String>,
    },
    ServerSame {
        name: String,
    },
    /// In Toolport already, with credentials still missing from an earlier `up`.
    ServerFinish {
        name: String,
        id: String,
        needs: Vec<String>,
        from_env: Vec<String>,
        /// Switch it on once every credential is set: only for a server `up` added and
        /// left off for that reason, never one the user turned off.
        enable: bool,
    },
    /// In Toolport from an earlier `up`, with settings the manifest has since changed.
    ServerUpdate {
        name: String,
        id: String,
        /// What changes, by name only.
        changes: Vec<String>,
        needs: Vec<String>,
        from_env: Vec<String>,
        enable: bool,
    },
    ServerConflict {
        name: String,
        detail: String,
    },
    Connect {
        agent_id: String,
        agent: String,
    },
    Problem {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpReport {
    pub file: String,
    pub dry_run: bool,
    pub steps: Vec<Step>,
    pub gateway_available: bool,
    pub applied: Vec<String>,
    pub failed: Vec<String>,
    pub notes: Vec<String>,
}

fn kind(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// Clients to set up: `--agents` (ids or kinds), else the coding agents present here.
fn selected_clients<'a>(
    snapshot: &'a Snapshot,
    wanted: &[String],
) -> Vec<&'a crate::clients::DetectedClient> {
    snapshot
        .present()
        .filter(|c| c.error.is_none() && c.app_present)
        .filter(|c| {
            if wanted.is_empty() {
                CODING_AGENTS.contains(&kind(&c.id))
            } else {
                wanted.iter().any(|w| w == &c.id || w == kind(&c.id))
            }
        })
        .collect()
}

fn mentions(text: &str, source: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        l == format!("@{source}") || l == format!("@./{source}")
    })
}

pub fn plan_up(
    manifest: &Manifest,
    project: &Path,
    snapshot: &Snapshot,
    wanted: &[String],
) -> Vec<Step> {
    let mut steps = Vec::new();
    let clients = selected_clients(snapshot, wanted);
    let kinds: BTreeSet<&str> = clients.iter().map(|c| kind(&c.id)).collect();
    for w in wanted {
        if !snapshot.present().any(|c| &c.id == w || kind(&c.id) == w) {
            steps.push(Step::Problem {
                message: format!("agent '{w}' isn't installed here"),
            });
        }
    }

    // Rules.
    if let Some(rules) = &manifest.rules {
        let source = project.join(&rules.source);
        if !source.is_file() {
            steps.push(Step::Problem {
                message: format!("rules source {} doesn't exist", rules.source),
            });
        } else {
            for (agent, file) in IMPORT_RULES {
                if !kinds.contains(agent) || rules.source == *file {
                    continue;
                }
                let path = project.join(file);
                let rel = path
                    .strip_prefix(project)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let line = format!("@{}", rules.source);
                match std::fs::read_to_string(&path) {
                    Ok(text) if mentions(&text, &rules.source) => steps.push(Step::RulesOk {
                        agent: (*agent).into(),
                        path: rel,
                    }),
                    Ok(_)
                        if std::fs::canonicalize(&path).ok()
                            == std::fs::canonicalize(&source).ok() =>
                    {
                        steps.push(Step::RulesOk {
                            agent: (*agent).into(),
                            path: rel,
                        })
                    }
                    Ok(_) => steps.push(Step::RulesPointer {
                        agent: (*agent).into(),
                        path: rel,
                        create: false,
                        line,
                    }),
                    Err(_) => steps.push(Step::RulesPointer {
                        agent: (*agent).into(),
                        path: rel,
                        create: true,
                        line,
                    }),
                }
            }
            let native = kinds
                .iter()
                .any(|k| !IMPORT_RULES.iter().any(|(a, _)| a == k));
            if native && rules.source != "AGENTS.md" {
                let agents_md = project.join("AGENTS.md");
                if agents_md.exists() {
                    if std::fs::canonicalize(&agents_md).ok() != std::fs::canonicalize(&source).ok()
                    {
                        steps.push(Step::Problem {
                            message: format!(
                                "AGENTS.md exists and isn't {}; agents that read AGENTS.md will use it instead",
                                rules.source
                            ),
                        });
                    }
                } else {
                    steps.push(Step::RulesLink {
                        agent: "agents that read AGENTS.md".into(),
                        path: "AGENTS.md".into(),
                        target: rules.source.clone(),
                    });
                }
            } else if native {
                steps.push(Step::RulesOk {
                    agent: "agents that read AGENTS.md".into(),
                    path: "AGENTS.md".into(),
                });
            }
        }
    }

    // Skills.
    let inventory = snapshot.inventory.as_ref();
    let home = crate::clients::home();
    let expand = |display: &str| -> PathBuf {
        match (display.strip_prefix('~'), &home) {
            (Some(rest), Some(h)) => h.join(rest.trim_start_matches(['/', '\\'])),
            _ => PathBuf::from(display),
        }
    };
    for skill in &manifest.skills {
        if let Some(path) = &skill.path {
            let folder = project.join(path);
            if !folder.join("SKILL.md").is_file() {
                steps.push(Step::Problem {
                    message: format!("skill {path} has no SKILL.md"),
                });
                continue;
            }
            let name = folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let real = std::fs::canonicalize(&folder).unwrap_or(folder.clone());
            let loaded: BTreeSet<String> = inventory
                .and_then(|inv| {
                    inv.skills
                        .iter()
                        .find(|s| std::fs::canonicalize(&s.folder).ok().as_ref() == Some(&real))
                })
                .map(|s| s.states.keys().cloned().collect())
                .unwrap_or_default();
            let mut ok_agents = Vec::new();
            for client in &clients {
                let agent = inventory.and_then(|inv| inv.agents.iter().find(|a| a.id == client.id));
                let Some(agent) = agent.filter(|a| a.supports_skills) else {
                    continue;
                };
                if loaded.contains(&agent.id) {
                    ok_agents.push(agent.name.clone());
                    continue;
                }
                let Some(root) = agent
                    .roots
                    .iter()
                    .find(|r| r.label == "project" && expand(&r.path).starts_with(project))
                else {
                    continue;
                };
                let link = expand(&root.path).join(&name);
                if link.exists() || std::fs::symlink_metadata(&link).is_ok() {
                    continue;
                }
                if steps.iter().any(|s| matches!(s, Step::SkillLink { link: l, .. } if *l == link.display().to_string())) {
                    continue;
                }
                steps.push(Step::SkillLink {
                    skill: name.clone(),
                    agent: agent.name.clone(),
                    link: link.display().to_string(),
                    target: real.display().to_string(),
                });
            }
            if !ok_agents.is_empty() {
                steps.push(Step::SkillOk {
                    skill: name,
                    agents: ok_agents,
                });
            }
        } else if let Some(source) = &skill.source {
            let wanted_name = parse_skill_source(source).and_then(|(_, s)| s);
            let present = wanted_name.as_ref().is_some_and(|n| {
                inventory.is_some_and(|inv| inv.skills.iter().any(|s| &s.name == n))
            });
            if present {
                steps.push(Step::SkillOk {
                    skill: wanted_name.unwrap(),
                    agents: vec!["installed".into()],
                });
            } else {
                steps.push(Step::SkillInstall {
                    source: source.clone(),
                    command: skills_add_command(source),
                });
            }
        }
    }

    // Servers.
    let registry = crate::registry::load().unwrap_or_default();
    for s in &manifest.servers {
        let entry = to_entry(s, false, Some(project));
        if let Some(existing) = registry
            .servers
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&s.name))
        {
            steps.push(plan_existing(s, existing, &entry));
            continue;
        }
        let item = crate::sharing_controller::SetupImportItem {
            name: s.name.clone(),
            transport: entry.transport.clone(),
            command: entry.command.clone(),
            args: entry.args.clone(),
            url: entry.url.clone(),
            is_new: true,
            reference_review: Vec::new(),
        };
        let warnings = crate::sharing_controller::import_item_warnings(&item)
            .into_iter()
            .map(String::from)
            .collect();
        let refs: Vec<String> = s.secret_refs.keys().cloned().collect();
        let needs: Vec<String> = s
            .env
            .iter()
            .filter(|k| !s.secret_refs.contains_key(*k))
            .cloned()
            .collect();
        let from_env = in_environment(&needs);
        let runs = match &entry.url {
            Some(url) => crate::import_credentials::shown_url(url),
            None => std::iter::once(entry.command.clone().unwrap_or_default())
                .chain(crate::import_credentials::shown_args(&entry.args))
                .collect::<Vec<_>>()
                .join(" "),
        };
        steps.push(Step::ServerAdd {
            name: s.name.clone(),
            runs,
            needs,
            from_env,
            refs,
            warnings,
        });
    }
    if steps.iter().any(|s| {
        matches!(
            s,
            Step::ServerAdd { .. }
                | Step::ServerSame { .. }
                | Step::ServerFinish { .. }
                | Step::ServerUpdate { .. }
        )
    }) {
        for c in &clients {
            if !c.gateway_installed && !c.uses_connectors {
                steps.push(Step::Connect {
                    agent_id: c.id.clone(),
                    agent: super::scan::display_name(c),
                });
            }
        }
    }
    steps
}

/// What `up` does with a manifest server Toolport already has under the same name. A
/// A different command, arguments, url or working folder is always a conflict: what a repo
/// runs never changes silently, and another repo can never redirect an existing server to
/// its own files or inherit its keychain secrets. Changed settings (plain values, password
/// manager references, new env names) update only a server added by the same project and
/// are shown before they apply; on a server added by another project or outside `up` they
/// are a conflict too.
fn plan_existing(
    s: &ServerSpec,
    existing: &crate::registry::ServerEntry,
    wanted: &crate::registry::ServerEntry,
) -> Step {
    if existing.transport != wanted.transport
        || existing.command != wanted.command
        || existing.args != wanted.args
        || existing.url != wanted.url
        || existing.cwd != wanted.cwd
    {
        return Step::ServerConflict {
            name: s.name.clone(),
            detail: "Toolport already has a server with this name and a different definition; rename one or remove it first".into(),
        };
    }
    let owned_here = same_project_owner(existing, wanted);
    let updated = with_manifest_settings(existing, wanted);
    let changes = setting_changes(existing, &updated);
    if !changes.is_empty() && !owned_here {
        return Step::ServerConflict {
            name: s.name.clone(),
            detail: format!(
                "Toolport already has a server with this name from another setup with different settings ({}); change them in Toolport or remove it first",
                changes.join(", ")
            ),
        };
    }
    let missing_in = |entry: &crate::registry::ServerEntry| -> Vec<String> {
        s.env
            .iter()
            .filter(|k| !s.secret_refs.contains_key(*k))
            .filter(|k| !has_credential(entry, k))
            .cloned()
            .collect()
    };
    // Switch it on once every credential is set: only for a server this project's `up`
    // added and left off for that reason, never one the user turned off or another repo added.
    let waiting = !existing.enabled && owned_here && !missing_in(existing).is_empty();
    let missing = missing_in(&updated);
    if !changes.is_empty() {
        return Step::ServerUpdate {
            name: s.name.clone(),
            id: existing.id.clone(),
            changes,
            from_env: in_environment(&missing),
            needs: missing,
            enable: waiting,
        };
    }
    if missing.is_empty() {
        Step::ServerSame {
            name: s.name.clone(),
        }
    } else {
        Step::ServerFinish {
            name: s.name.clone(),
            id: existing.id.clone(),
            from_env: in_environment(&missing),
            needs: missing,
            enable: !existing.enabled && owned_here,
        }
    }
}

/// `existing` with the settings the manifest declares: its plain values, password manager
/// references and env names. Everything else, including settings added in Toolport,
/// working directory and keychain values, is kept.
fn with_manifest_settings(
    existing: &crate::registry::ServerEntry,
    wanted: &crate::registry::ServerEntry,
) -> crate::registry::ServerEntry {
    let mut out = existing.clone();
    for var in &wanted.env {
        match out.env.iter_mut().find(|v| v.key == var.key) {
            Some(v) => {
                v.secret = var.secret;
                v.value = var.value.clone();
                v.unknown_fields.remove("source");
                if let Some(source) = var.unknown_fields.get("source") {
                    v.unknown_fields.insert("source".into(), source.clone());
                }
            }
            None => out.env.push(var.clone()),
        }
    }
    out
}

/// Names of the settings that differ, for the plan. Never shows a value.
fn setting_changes(
    before: &crate::registry::ServerEntry,
    after: &crate::registry::ServerEntry,
) -> Vec<String> {
    let mut changes = Vec::new();
    for var in &after.env {
        let reference = crate::secret_refs::reference_for(var);
        match before.env.iter().find(|v| v.key == var.key) {
            None => changes.push(format!("adds {}", var.key)),
            Some(old) if crate::secret_refs::reference_for(old) != reference => {
                changes.push(match reference {
                    Some(_) => format!("{} from a new reference", var.key),
                    None => format!("{} no longer from a reference", var.key),
                })
            }
            Some(old) if old.secret != var.secret || old.value != var.value => {
                changes.push(var.key.clone())
            }
            Some(_) => {}
        }
    }
    changes
}

fn render_steps(r: &UpReport, style: &Style) -> String {
    let mut out = format!(
        "{} {}\n",
        style.bold("toolport up"),
        style.dim(&format!(
            "· {}{}",
            r.file,
            if r.dry_run { " · dry run" } else { "" }
        ))
    );
    let add = style.cyan("+");
    let ok = style.mark(Severity::Ok);
    let mut section = "";
    let mut head = |out: &mut String, name: &'static str| {
        if section != name {
            out.push_str(&style.heading(name));
            section = name;
        }
    };
    for step in &r.steps {
        match step {
            Step::RulesPointer {
                agent,
                path,
                create,
                line,
            } => {
                head(&mut out, "Rules");
                out.push_str(&format!(
                    "  {add} {} {path} with '{line}' {}\n",
                    if *create { "create" } else { "add a line to" },
                    style.dim(&format!("({agent})"))
                ));
            }
            Step::RulesLink {
                agent,
                path,
                target,
            } => {
                head(&mut out, "Rules");
                out.push_str(&format!(
                    "  {add} link {path} -> {target} {}\n",
                    style.dim(&format!("({agent})"))
                ));
            }
            Step::RulesOk { agent, path } => {
                head(&mut out, "Rules");
                out.push_str(&format!(
                    "  {ok} {path} {}\n",
                    style.dim(&format!("({agent})"))
                ));
            }
            Step::SkillLink {
                skill, agent, link, ..
            } => {
                head(&mut out, "Skills");
                out.push_str(&format!(
                    "  {add} link {skill} for {agent} {}\n",
                    style.dim(&format::home_relative(Path::new(link)))
                ));
            }
            Step::SkillOk { skill, agents } => {
                head(&mut out, "Skills");
                out.push_str(&format!(
                    "  {ok} {skill} {}\n",
                    style.dim(&format::truncate(&agents.join(", "), 80))
                ));
            }
            Step::SkillInstall { command, .. } => {
                head(&mut out, "Skills");
                out.push_str(&format!(
                    "  {} not installed: run {}\n",
                    style.mark(Severity::Warn),
                    style.cmd(command)
                ));
            }
            Step::ServerAdd {
                name,
                runs,
                needs,
                from_env,
                refs,
                warnings,
            } => {
                head(&mut out, "Servers");
                out.push_str(&format!("  {add} add {}\n", style.bold(name)));
                out.push_str(&format!(
                    "      {} {}\n",
                    style.dim(if runs.contains("://") && !runs.contains(' ') {
                        "connects to"
                    } else {
                        "runs"
                    }),
                    runs
                ));
                for w in warnings {
                    out.push_str(&format!("      {} {}\n", style.yellow("!"), w));
                }
                if !refs.is_empty() {
                    out.push_str(&format!(
                        "      {}\n",
                        style.dim(&format!("from your password manager: {}", refs.join(", ")))
                    ));
                }
                let ask: Vec<&String> = needs.iter().filter(|n| !from_env.contains(n)).collect();
                if !from_env.is_empty() {
                    out.push_str(&format!(
                        "      {}\n",
                        style.dim(&format!(
                            "from your environment into the keychain: {}",
                            from_env.join(", ")
                        ))
                    ));
                }
                if !ask.is_empty() {
                    out.push_str(&format!(
                        "      {}\n",
                        style.dim(&format!(
                            "asks for: {}",
                            ask.iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    ));
                }
            }
            Step::ServerFinish {
                name,
                needs,
                from_env,
                enable,
                ..
            } => {
                head(&mut out, "Servers");
                let ask: Vec<&str> = needs
                    .iter()
                    .filter(|n| !from_env.contains(n))
                    .map(String::as_str)
                    .collect();
                let mut parts = Vec::new();
                if !from_env.is_empty() {
                    parts.push(format!("from your environment: {}", from_env.join(", ")));
                }
                if !ask.is_empty() {
                    parts.push(format!("asks for: {}", ask.join(", ")));
                }
                out.push_str(&format!(
                    "  {add} finish {} {}\n      {}\n",
                    style.bold(name),
                    style.dim(if *enable {
                        "(in Toolport, off until its credentials are set)"
                    } else {
                        "(in Toolport, missing credentials)"
                    }),
                    style.dim(&parts.join(" · "))
                ));
            }
            Step::ServerUpdate {
                name,
                changes,
                needs,
                from_env,
                ..
            } => {
                head(&mut out, "Servers");
                out.push_str(&format!(
                    "  {} update {}\n      {}\n",
                    style.cyan("~"),
                    style.bold(name),
                    style.dim(&format!("changes: {}", changes.join(", ")))
                ));
                let ask: Vec<&str> = needs
                    .iter()
                    .filter(|n| !from_env.contains(n))
                    .map(String::as_str)
                    .collect();
                if !from_env.is_empty() {
                    out.push_str(&format!(
                        "      {}\n",
                        style.dim(&format!(
                            "from your environment into the keychain: {}",
                            from_env.join(", ")
                        ))
                    ));
                }
                if !ask.is_empty() {
                    out.push_str(&format!(
                        "      {}\n",
                        style.dim(&format!("asks for: {}", ask.join(", ")))
                    ));
                }
            }
            Step::ServerSame { name } => {
                head(&mut out, "Servers");
                out.push_str(&format!(
                    "  {ok} {name} {}\n",
                    style.dim("already in Toolport")
                ));
            }
            Step::ServerConflict { name, detail } => {
                head(&mut out, "Servers");
                out.push_str(&format!(
                    "  {} {name} {}\n",
                    style.mark(Severity::Warn),
                    style.dim(detail)
                ));
            }
            Step::Connect { agent, .. } => {
                head(&mut out, "Agents");
                out.push_str(&format!(
                    "  {add} connect {agent} to the Toolport gateway{}\n",
                    if r.gateway_available {
                        String::new()
                    } else {
                        style.dim(" (needs the Toolport app or toolport-gateway)")
                    }
                ));
            }
            Step::Problem { message } => {
                head(&mut out, "Problems");
                out.push_str(&format!("  {} {message}\n", style.mark(Severity::Error)));
            }
        }
    }
    out
}

/// Whether applying would change anything. Connecting an agent needs a gateway install, so
/// without one those steps are shown but can't be applied.
fn has_changes(steps: &[Step], gateway: bool) -> bool {
    steps.iter().any(|s| match s {
        Step::RulesPointer { .. }
        | Step::RulesLink { .. }
        | Step::SkillLink { .. }
        | Step::ServerAdd { .. } => true,
        Step::ServerFinish { .. } | Step::ServerUpdate { .. } => true,
        Step::Connect { .. } => gateway,
        _ => false,
    })
}

fn relative_link(from_dir: &Path, target: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = target.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c.as_os_str());
    }
    out
}

fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rel = relative_link(link.parent().unwrap_or(Path::new(".")), target);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(rel, link)
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(rel, link)
        } else {
            std::os::windows::fs::symlink_file(rel, link)
        }
    }
}

fn apply_up(ctx: &Ctx, manifest: &Manifest, project: &Path, report: &mut UpReport) {
    for step in report.steps.clone() {
        match step {
            Step::RulesPointer {
                path,
                create,
                line,
                agent,
            } => {
                let file = project.join(&path);
                let result = if create {
                    std::fs::write(&file, format!("{line}\n"))
                } else {
                    std::fs::read_to_string(&file).and_then(|text| {
                        let sep = if text.ends_with('\n') || text.is_empty() {
                            ""
                        } else {
                            "\n"
                        };
                        std::fs::write(&file, format!("{text}{sep}\n{line}\n"))
                    })
                };
                match result {
                    Ok(()) => report
                        .applied
                        .push(format!("{path} reads {} ({agent})", &line[1..])),
                    Err(e) => report.failed.push(format!("{path}: {e}")),
                }
            }
            Step::RulesLink { path, target, .. } => {
                match symlink(&project.join(&target), &project.join(&path)) {
                    Ok(()) => report.applied.push(format!("linked {path} -> {target}")),
                    Err(e) => report.failed.push(format!("{path}: {e}")),
                }
            }
            Step::SkillLink {
                skill,
                agent,
                link,
                target,
            } => match symlink(Path::new(&target), Path::new(&link)) {
                Ok(()) => report.applied.push(format!("linked {skill} for {agent}")),
                Err(e) => report.failed.push(format!("{skill} for {agent}: {e}")),
            },
            Step::ServerAdd {
                name,
                needs,
                from_env,
                ..
            } => {
                let Some(spec) = manifest.servers.iter().find(|s| s.name == name) else {
                    continue;
                };
                match add_server(ctx, spec, project, &needs, &from_env) {
                    Ok(msg) => report.applied.push(msg),
                    Err(e) => report.failed.push(format!("{name}: {e}")),
                }
            }
            Step::ServerFinish {
                name,
                id,
                needs,
                from_env,
                enable,
            } => {
                let Some(spec) = manifest.servers.iter().find(|s| s.name == name) else {
                    continue;
                };
                match finish_server(ctx, spec, &id, &needs, &from_env, enable) {
                    Ok(Some(msg)) => report.applied.push(msg),
                    Ok(None) => report.notes.push(format!(
                        "{name} still needs {}: run toolport up again in a terminal, or set {} in the environment",
                        needs.join(", "),
                        if needs.len() == 1 { "it" } else { "them" }
                    )),
                    Err(e) => report.failed.push(format!("{name}: {e}")),
                }
            }
            Step::ServerUpdate {
                name,
                id,
                needs,
                from_env,
                enable,
                ..
            } => {
                let Some(spec) = manifest.servers.iter().find(|s| s.name == name) else {
                    continue;
                };
                match update_server(ctx, spec, project, &id, &needs, &from_env, enable) {
                    Ok(msg) => report.applied.push(msg),
                    Err(e) => report.failed.push(format!("{name}: {e}")),
                }
            }
            Step::Connect { agent_id, agent } => {
                if !report.gateway_available {
                    continue;
                }
                match crate::clients::install_gateway(&agent_id, None) {
                    Ok(_) => report
                        .applied
                        .push(format!("connected {agent} to the gateway")),
                    Err(e) => report.failed.push(format!("{agent}: {e}")),
                }
            }
            _ => {}
        }
    }
    if !report.gateway_available
        && report
            .steps
            .iter()
            .any(|s| matches!(s, Step::Connect { .. }))
    {
        report.notes.push(
            "Servers are in Toolport, but no gateway is installed to share them with your agents. Install the Toolport app (toolport.app) or toolport-gateway, then run toolport up again.".into(),
        );
    }
}

/// Names in `keys` that are set in this process's environment.
fn in_environment(keys: &[String]) -> Vec<String> {
    keys.iter()
        .filter(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
        .cloned()
        .collect()
}

/// Whether Toolport has a value for this server setting: a plain value, a reference, or a
/// keychain entry. Reads the keychain without ever asking to unlock it.
fn has_credential(entry: &crate::registry::ServerEntry, key: &str) -> bool {
    let Some(var) = entry.env.iter().find(|v| v.key == key) else {
        return false;
    };
    if var.value.as_deref().is_some_and(|v| !v.is_empty())
        || crate::secret_refs::reference_for(var).is_some()
    {
        return true;
    }
    crate::secrets::without_prompts(|| {
        crate::secrets::get_secret_without_prompt(&entry.id, key)
            .is_ok_and(|v| v.is_some_and(|v| !v.is_empty()))
    })
}

/// Values for `needs`: from the environment when listed in `from_env`, else from a hidden
/// prompt on a terminal. Returns the values and the names still missing.
fn collect_values(
    ctx: &Ctx,
    server: &str,
    needs: &[String],
    from_env: &[String],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut values = Vec::new();
    let mut missing = Vec::new();
    for key in needs {
        if from_env.contains(key) {
            if let Ok(v) = std::env::var(key) {
                if !v.is_empty() {
                    values.push((key.clone(), v));
                    continue;
                }
            }
        }
        if ctx.interactive && !ctx.json {
            let prompt = format!(
                "  {} for {} {}: ",
                ctx.style.bold(key),
                server,
                ctx.style.dim("(hidden, Enter to skip)")
            );
            if let Some(v) = super::read_secret(&prompt) {
                values.push((key.clone(), v));
                continue;
            }
        }
        missing.push(key.clone());
    }
    (values, missing)
}

/// Save each value in the keychain. On a failure, the ones this call already saved are
/// deleted again, so a failed run leaves no stray credentials behind.
fn store_values(id: &str, values: &[(String, String)]) -> Result<(), String> {
    for (i, (key, value)) in values.iter().enumerate() {
        if let Err(e) = crate::secrets::set_secret(id, key, value) {
            for (saved, _) in &values[..i] {
                let _ = crate::secrets::delete_secret(id, saved);
            }
            return Err(format!("couldn't save {key} in the keychain: {e}"));
        }
    }
    Ok(())
}

fn enable(id: &str) -> Result<(), String> {
    crate::registry::update(|r| {
        let profile = r.default_access_id();
        crate::registry_controller::apply_server_enabled(r, &profile, id, true, true)?;
        let _ = r.set_access_server(&profile, id, true);
        Ok(())
    })
    .map(|_| ())
}

fn still_off(name: &str, missing: &[String]) -> String {
    format!(
        "added {name} (off until {} {} set: run toolport up again)",
        missing.join(", "),
        if missing.len() == 1 { "is" } else { "are" }
    )
}

/// Add one server: registry first (disabled), then each credential into the keychain, then
/// enable it. Any keychain failure removes the server and its saved credentials again, so
/// nothing is half set up.
fn add_server(
    ctx: &Ctx,
    spec: &ServerSpec,
    project: &Path,
    needs: &[String],
    from_env: &[String],
) -> Result<String, String> {
    let (values, missing) = collect_values(ctx, &spec.name, needs, from_env);
    let entry = to_entry(spec, false, Some(project));
    let (_, id) = crate::registry::update(|r| Ok(r.add_server(entry)))?;
    if let Err(e) = store_values(&id, &values) {
        let _ = crate::registry::update(|r| r.remove_server(&id));
        return Err(e);
    }
    if missing.is_empty() {
        enable(&id)?;
        Ok(format!("added {} (enabled)", spec.name))
    } else {
        Ok(still_off(&spec.name, &missing))
    }
}

/// Apply the manifest's changed settings to a server an earlier `up` added, then ask for
/// any credential that is now missing. Keychain values and settings added in Toolport stay.
fn update_server(
    ctx: &Ctx,
    spec: &ServerSpec,
    project: &Path,
    id: &str,
    needs: &[String],
    from_env: &[String],
    turn_on: bool,
) -> Result<String, String> {
    let wanted = to_entry(spec, false, Some(project));
    crate::registry::update(|r| {
        let existing = r
            .servers
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("{} is no longer in Toolport", spec.name))?;
        if existing.transport != wanted.transport
            || existing.command != wanted.command
            || existing.args != wanted.args
            || existing.url != wanted.url
            || existing.cwd != wanted.cwd
            || !same_project_owner(existing, &wanted)
        {
            return Err(format!(
                "{} changed in Toolport since the preview; review and run toolport up again",
                spec.name
            ));
        }
        let updated = with_manifest_settings(existing, &wanted);
        crate::secret_refs::validate_server(&updated).map_err(|e| e.message)?;
        r.update_server(updated)
    })?;
    let (values, missing) = collect_values(ctx, &spec.name, needs, from_env);
    store_values(id, &values)?;
    if !missing.is_empty() {
        return Ok(format!(
            "updated {} ({} still missing: run toolport up again)",
            spec.name,
            missing.join(", ")
        ));
    }
    if turn_on {
        enable(id)?;
        return Ok(format!("updated {} (enabled)", spec.name));
    }
    Ok(format!("updated {}", spec.name))
}

/// Supply the credentials an earlier `up` left missing. `None` when no value was given.
fn finish_server(
    ctx: &Ctx,
    spec: &ServerSpec,
    id: &str,
    needs: &[String],
    from_env: &[String],
    turn_on: bool,
) -> Result<Option<String>, String> {
    let (values, missing) = collect_values(ctx, &spec.name, needs, from_env);
    if values.is_empty() {
        return Ok(None);
    }
    store_values(id, &values)?;
    if !missing.is_empty() {
        return Ok(Some(format!(
            "saved {} for {} ({} still missing)",
            values
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            spec.name,
            missing.join(", ")
        )));
    }
    if turn_on {
        enable(id)?;
        Ok(Some(format!("finished {} (enabled)", spec.name)))
    } else {
        Ok(Some(format!("finished {}", spec.name)))
    }
}

pub fn up_command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let project = ctx.project.clone().ok_or_else(|| {
        CliError::new("couldn't tell which folder to set up").hint("pass --project <DIR>")
    })?;
    let file = match args.value("file") {
        Some(f) => {
            let p = PathBuf::from(f);
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or(project.clone()).join(p)
            }
        }
        None => project.join(FILE_NAME),
    };
    let text = std::fs::read_to_string(&file).map_err(|_| {
        CliError::new(format!("no {} found", format::home_relative(&file)))
            .hint("run 'toolport init' to create one from this project")
    })?;
    let manifest = load(&text).map_err(|errors| {
        let mut e = CliError::new(format!(
            "{} isn't valid:\n{}",
            format::home_relative(&file),
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        if errors.iter().any(|e| e.contains("credential") || e.contains("password")) {
            e = e.hint("keep secrets out of toolport.toml: list the variable under env and toolport stores the value in your keychain");
        }
        e
    })?;
    let project_str = project.to_string_lossy().into_owned();
    let snapshot = Snapshot::read(Some(&project_str));
    let wanted = args.list("agents");
    let mut report = UpReport {
        file: format::home_relative(&file),
        dry_run: args.has("dry-run"),
        steps: plan_up(&manifest, &project, &snapshot, &wanted),
        gateway_available: crate::clients::resolve_gateway_path_readonly().is_some(),
        ..Default::default()
    };
    let problems = report
        .steps
        .iter()
        .any(|s| matches!(s, Step::Problem { .. }));
    let changes = has_changes(&report.steps, report.gateway_available);
    if report.dry_run || !changes {
        if ctx.json {
            ctx.emit_json(&report)?;
        } else {
            let mut text = render_steps(&report, &ctx.style);
            text.push_str(&format!(
                "\n  {}\n",
                if !changes {
                    format!(
                        "{} Everything that can be set up here is set up.",
                        ctx.style.mark(Severity::Ok)
                    )
                } else {
                    ctx.style.hint(&format!(
                        "run {} to apply",
                        ctx.style.cmd("toolport up --yes")
                    ))
                }
            ));
            super::out(&text);
        }
        return Ok(if problems { super::EXIT_ERROR } else { EXIT_OK });
    }
    if !ctx.json {
        super::out(&render_steps(&report, &ctx.style));
    }
    if !args.has("yes") {
        if !ctx.interactive {
            return Err(CliError::new("not applying without confirmation")
                .hint("run with --yes to apply, or --dry-run to preview"));
        }
        if !ctx.confirm("\nApply this setup?") {
            super::out("Nothing changed.");
            return Ok(EXIT_OK);
        }
    }
    apply_up(ctx, &manifest, &project, &mut report);
    if ctx.json {
        ctx.emit_json(&report)?;
    } else {
        let s = &ctx.style;
        let mut text = String::from("\n");
        for a in &report.applied {
            text.push_str(&format!("  {} {a}\n", s.mark(Severity::Ok)));
        }
        for f in &report.failed {
            text.push_str(&format!("  {} {f}\n", s.mark(Severity::Error)));
        }
        for n in &report.notes {
            text.push_str(&format!("  {} {}\n", s.mark(Severity::Info), s.dim(n)));
        }
        super::out(&text);
    }
    Ok(if report.failed.is_empty() && !problems {
        EXIT_OK
    } else {
        super::EXIT_ERROR
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
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
[servers.secret_refs]
GITHUB_TOKEN = "op://Engineering/GitHub/token"
[[servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
"#;

    #[test]
    fn a_clean_manifest_loads() {
        let m = load(GOOD).unwrap();
        assert_eq!(m.servers.len(), 2);
        assert_eq!(m.servers[1].transport(), "http");
        let entry = to_entry(&m.servers[0], false, None);
        assert!(entry
            .env
            .iter()
            .all(|e| e.key != "GITHUB_TOKEN" || (e.secret && e.value.is_none())));
        assert_eq!(
            crate::secret_refs::reference_for(
                entry.env.iter().find(|e| e.key == "GITHUB_TOKEN").unwrap()
            ),
            Some("op://Engineering/GitHub/token")
        );
    }

    #[test]
    fn literal_secrets_are_refused_everywhere() {
        let cases = [
            ("values", "[[servers]]\nname=\"a\"\ncommand=\"x\"\n[servers.values]\nAPI_KEY=\"sk-live-abcdefghijklmnop\"\n"),
            ("values entropy", "[[servers]]\nname=\"a\"\ncommand=\"x\"\n[servers.values]\nMODE=\"ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n"),
            ("args", "[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"--token=abc123secretvalue\"]\n"),
            ("arg url", "[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"postgres://user:hunter2@db/x\"]\n"),
            ("url query", "[[servers]]\nname=\"a\"\nurl=\"https://x.example/mcp?api_key=abc\"\n"),
            ("url userinfo", "[[servers]]\nname=\"a\"\nurl=\"https://u:p@x.example/mcp\"\n"),
            // A reference elsewhere in the same string doesn't hide a literal secret.
            ("arg url and reference", "[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"--db\", \"postgres://admin:hunter2@${DB_HOST}/app\"]\nenv=[\"DB_HOST\"]\n"),
            ("value and reference", "[[servers]]\nname=\"a\"\ncommand=\"x\"\nenv=[\"DB_HOST\"]\n[servers.values]\nDATABASE_URL=\"postgres://admin:hunter2@${DB_HOST}/app\"\n"),
            ("flag url and reference", "[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"--url=https://mcp.x.com/sse?api_key=sk-live-abc123&team=${TEAM}\"]\nenv=[\"TEAM\"]\n"),
            ("url and reference", "[[servers]]\nname=\"a\"\nurl=\"https://u:hunter2@${HOST}/mcp\"\nenv=[\"HOST\"]\n"),
        ];
        for (label, body) in cases {
            let text = format!("version = 1\n{body}");
            let errors = load(&text).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains("credential") || e.contains("password")),
                "{label}: {errors:?}"
            );
        }
        // A reference in an argument is fine.
        load("version = 1\n[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"--token=${TOKEN}\"]\nenv=[\"TOKEN\"]\n").unwrap();
        load("version = 1\n[[servers]]\nname=\"a\"\ncommand=\"x\"\nargs=[\"postgres://${DB_USER}:${DB_PASS}@db/app\"]\nenv=[\"DB_USER\", \"DB_PASS\"]\n").unwrap();
        // A URL that fails to parse is never echoed back.
        let errors =
            load("version = 1\n[[servers]]\nname=\"a\"\nurl=\"not a url hunter2\"\n").unwrap_err();
        assert!(errors.iter().all(|e| !e.contains("hunter2")), "{errors:?}");
    }

    #[test]
    fn structural_errors_are_all_reported() {
        let errors = load(
            "version = 2\n[rules]\nsource=\"../x\"\n[[skills]]\n[[servers]]\nname=\"a\"\n[[servers]]\nname=\"A\"\ncommand=\"x\"\nenv=[\"bad-name\"]\n[servers.secret_refs]\nOTHER=\"op://a/b/c\"\n",
        )
        .unwrap_err();
        let text = errors.join("\n");
        for needle in [
            "version = 2",
            "../x",
            "exactly one",
            "needs a command (stdio) or a url",
            "listed twice",
            "bad-name",
            "isn't in env",
        ] {
            assert!(text.contains(needle), "missing {needle}: {text}");
        }
        assert!(load("version = 1\nunknown = 1\n").is_err());
    }

    #[test]
    fn skill_sources() {
        assert_eq!(
            skills_add_command("vercel-labs/agent-skills@frontend-design"),
            "npx skills add vercel-labs/agent-skills --skill frontend-design"
        );
        assert_eq!(skills_add_command("a/b"), "npx skills add a/b");
        assert!(parse_skill_source("nope").is_none());
        assert!(parse_skill_source("a/b@").is_none());
    }

    #[test]
    fn init_output_is_secret_free_and_valid() {
        let dir = std::env::temp_dir().join(format!("toolport-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".agents/skills/release")).unwrap();
        std::fs::write(
            dir.join(".agents/skills/release/SKILL.md"),
            "---\nname: release\n---\n",
        )
        .unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# rules\n").unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            r#"{"mcpServers":{
                "github":{"command":"npx","args":["-y","server-github"],"env":{"GITHUB_TOKEN":"ghp_1234567890abcdefghijklmnopqrstuvwxyz","LOG_LEVEL":"debug"}},
                "pg":{"command":"npx","args":["server-postgres","postgresql://u:hunter2@db/x"]},
                "pg2":{"command":"npx","args":["server-postgres","postgres://admin:hunter2@${DB_HOST}/app"],"env":{"DATABASE_URL":"postgres://admin:hunter2@${DB_HOST}/app"}},
                "remote":{"type":"http","url":"https://mcp.example.com/mcp"}
            }}"#,
        )
        .unwrap();
        let found = inspect(&dir);
        let text = render_manifest("demo", &found);
        assert!(
            !text.contains("ghp_") && !text.contains("hunter2"),
            "{text}"
        );
        let m = load(&text).unwrap();
        assert_eq!(m.rules.unwrap().source, "AGENTS.md");
        assert_eq!(m.skills[0].path.as_deref(), Some(".agents/skills/release"));
        let gh = m.servers.iter().find(|s| s.name == "github").unwrap();
        assert_eq!(gh.env, vec!["GITHUB_TOKEN"]);
        assert_eq!(
            gh.values.get("LOG_LEVEL").map(String::as_str),
            Some("debug")
        );
        assert!(m.servers.iter().all(|s| s.name != "pg"));
        assert!(text.contains("Not included: pg"));
        assert!(m.servers.iter().all(|s| s.name != "pg2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_links() {
        assert_eq!(
            relative_link(
                Path::new("/p/.claude/skills"),
                Path::new("/p/.agents/skills/x")
            ),
            PathBuf::from("../../.agents/skills/x")
        );
    }

    fn spec(toml: &str) -> ServerSpec {
        load(&format!("version = 1\n[[servers]]\nname = \"s\"\n{toml}"))
            .unwrap()
            .servers
            .remove(0)
    }

    #[test]
    fn servers_that_run_project_files_start_in_the_project() {
        let project =
            std::env::temp_dir().join(format!("toolport-manifest-cwd-{}", std::process::id()));
        std::fs::create_dir_all(project.join("tools")).unwrap();
        std::fs::write(project.join("server.py"), "").unwrap();
        let cwd = |toml: &str| to_entry(&spec(toml), false, Some(&project)).cwd;
        let here = Some(project.to_string_lossy().into_owned());
        for toml in [
            "command = \"node\"\nargs = [\"./server.js\"]",
            "command = \"node\"\nargs = [\"dist/index.js\"]",
            "command = \"uv\"\nargs = [\"run\", \"server.py\"]",
            "command = \"uv\"\nargs = [\"run\", \"--directory=tools\", \"srv\"]",
            "command = \"./bin/server\"",
        ] {
            assert_eq!(cwd(toml), here, "{toml}");
        }
        for toml in [
            "command = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-github\"]",
            "command = \"uvx\"\nargs = [\"--from\", \"git+https://github.com/o/r\", \"srv\"]",
            "command = \"docker\"\nargs = [\"run\", \"ghcr.io/o/r:latest\"]",
            "command = \"gh-mcp\"\nargs = [\"--repo\", \"owner/repo\"]",
            "command = \"node\"\nargs = [\"/opt/server.js\"]",
            "url = \"https://mcp.example.com/mcp\"",
        ] {
            assert_eq!(cwd(toml), None, "{toml}");
        }
        // Validation has no project: no working folder.
        assert_eq!(
            to_entry(
                &spec("command = \"node\"\nargs = [\"./s.js\"]"),
                false,
                None
            )
            .cwd,
            None
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    fn existing(toml: &str, source: &str, project: Option<&Path>) -> crate::registry::ServerEntry {
        let mut e = to_entry(&spec(toml), true, project);
        e.id = "s".into();
        e.source = Some(source.into());
        e
    }

    #[test]
    fn changed_values_and_references_update_a_project_server() {
        const BASE: &str = "command = \"npx\"\nargs = [\"-y\", \"pkg\"]\nenv = [\"TOKEN\"]\n[servers.secret_refs]\nTOKEN = \"op://Eng/Item/token\"\n[servers.values]\nLOG_LEVEL = \"debug\"";
        let repo_a = Path::new("/tmp/toolport-test-repo-a");
        let repo_b = Path::new("/tmp/toolport-test-repo-b");
        let old = existing(BASE, "project", Some(repo_a));
        let plan_in = |toml: &str, e: &crate::registry::ServerEntry, dir: &Path| {
            let s = spec(toml);
            plan_existing(&s, e, &to_entry(&s, false, Some(dir)))
        };
        assert!(matches!(
            plan_in(BASE, &old, repo_a),
            Step::ServerSame { .. }
        ));

        let value = BASE.replace("\"debug\"", "\"info\"");
        match plan_in(&value, &old, repo_a) {
            Step::ServerUpdate { changes, needs, .. } => {
                assert_eq!(changes, vec!["LOG_LEVEL".to_string()]);
                assert!(needs.is_empty());
            }
            other => panic!("{other:?}"),
        }
        let reference = BASE.replace("op://Eng/Item/token", "op://Eng/Other/token");
        match plan_in(&reference, &old, repo_a) {
            Step::ServerUpdate { changes, .. } => {
                assert_eq!(changes, vec!["TOKEN from a new reference".to_string()])
            }
            other => panic!("{other:?}"),
        }
        // The update keeps what the manifest doesn't declare and applies what it does.
        let s = spec(&value);
        let updated = with_manifest_settings(&old, &to_entry(&s, false, Some(repo_a)));
        assert_eq!(updated.id, "s");
        let level = updated.env.iter().find(|v| v.key == "LOG_LEVEL").unwrap();
        assert_eq!(level.value.as_deref(), Some("info"));
        assert!(!level.secret);
        assert_eq!(
            crate::secret_refs::reference_for(
                updated.env.iter().find(|v| v.key == "TOKEN").unwrap()
            ),
            Some("op://Eng/Item/token")
        );
        assert!(matches!(
            plan_in(&value, &updated, repo_a),
            Step::ServerSame { .. }
        ));

        // Another repo cannot update or hijack settings of repo A's server.
        assert!(matches!(
            plan_in(&value, &old, repo_b),
            Step::ServerConflict { .. }
        ));
        assert!(matches!(
            plan_in(&reference, &old, repo_b),
            Step::ServerConflict { .. }
        ));
        // Another repo with a local script under the same name gets a conflict, never a
        // cwd swap that could run repo B's script with repo A's keychain secrets.
        const LOCAL: &str = "command = \"node\"\nargs = [\"./server.js\"]\nenv = [\"TOKEN\"]";
        let local_a = existing(LOCAL, "project", Some(repo_a));
        assert!(matches!(
            plan_in(LOCAL, &local_a, repo_b),
            Step::ServerConflict { .. }
        ));

        // Never changed on a server the user added some other way.
        match plan_in(&value, &existing(BASE, "manual", None), repo_a) {
            Step::ServerConflict { detail, .. } => {
                assert!(
                    detail.contains("LOG_LEVEL") && !detail.contains("info"),
                    "{detail}"
                )
            }
            other => panic!("{other:?}"),
        }
        // A different command is always a conflict.
        assert!(matches!(
            plan_in(&BASE.replace("\"pkg\"", "\"other\""), &old, repo_a),
            Step::ServerConflict { .. }
        ));
    }
}
