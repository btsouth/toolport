//! Guard hook - enforce the permission policy in agents that have hooks but no rule list.
//! Phase 2 of SBS-820 item 2 (SBS-1059), Cursor first.
//!
//! Claude Code enforces [`crate::agent_permissions`]' rules natively from its settings.
//! Cursor has no such list; what it has is hooks: `beforeShellExecution`,
//! `beforeMCPExecution` and `beforeReadFile` run a command with the call on stdin and
//! read `{"permission": "allow" | "deny" | "ask"}` back. So the SAME rules - the user
//! writes them once, in Claude Code's syntax - are evaluated here, by the gateway binary
//! invoked as `--toolport-guard cursor`, against Cursor's payload.
//!
//! What carries over and what does not, said plainly because the rule language is
//! Claude Code's: `Bash(...)` rules guard shell commands; `Read(...)` rules guard file
//! reads; `mcp__server__tool` rules guard MCP tools (Cursor names the tool, not the
//! server, so the server segment cannot be checked and is ignored); `Edit`, `WebFetch`,
//! `Agent` and parameter rules have no Cursor event and do nothing here. Matching follows
//! Claude Code's documented semantics where they are documented: `*` spans anything
//! including spaces, a trailing ` *` needs a word boundary, `:*` is the same as ` *`, a
//! compound command (`&&`, `||`, `;`, `|`) is denied or asked if ANY part matches and
//! allowed only if EVERY part matches, a handful of wrappers (`timeout N`, `time`, `nice`,
//! `nohup`) are stripped first, Read paths are `~/`, `//absolute`, or relative to the
//! workspace root, with `*` and `**` globs.
//!
//! Three properties, inherited from the sensor:
//!
//!   * **Off by default, observe before enforce.** `Observe` installs the hook but always
//!     answers allow and records what it WOULD have decided; `Enforce` lets deny and ask
//!     act. `failClosed` is set on the hook only in `Enforce`.
//!   * **Ask is Cursor's own prompt.** `permission: "ask"` makes Cursor confirm with the
//!     user in its own UI; routing the ask through Toolport's approval window is a later
//!     step, not this one.
//!   * **It adds only entries carrying [`GUARD_MARKER`] and removes only those**, the
//!     sensor's ownership rule, in `~/.cursor/hooks.json`.
//!
//! When no rule matches, the answer is Cursor's own canonical "proceed" response
//! (`{"continue": true, "permission": "allow"}`); when Toolport cannot judge (a payload it
//! does not understand), it answers the same and records that it could not, because a
//! guard that breaks the user's work over its own parse error is worse than one that
//! stands aside - and in `Enforce` Cursor's `failClosed` still covers a crash or timeout.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub use crate::registry::GuardMode;
use crate::registry::{PermissionAction, PermissionRule};

/// The literal that marks a hooks.json entry as Toolport's guard, and the flag the gateway
/// binary recognises.
pub const GUARD_MARKER: &str = "--toolport-guard";

/// Cursor events the guard registers, with the subject each carries.
const CURSOR_EVENTS: [&str; 3] = [
    "beforeShellExecution",
    "beforeMCPExecution",
    "beforeReadFile",
];

/// Seconds Cursor should allow the hook. A wedged guard must cost a bounded pause.
const GUARD_TIMEOUT_SECS: u64 = 10;

/// Cap on the payload read from stdin. Generous on purpose: a `beforeReadFile` payload
/// embeds the file's content, and in Enforce a call the guard cannot see is DENIED, so a
/// small cap would turn every large read into a refusal. Past this the payload is
/// truncated, unparseable, and - in Enforce - denied, which is also what stops an agent
/// from padding a denied command past the cap to slip it through.
pub const MAX_GUARD_STDIN_BYTES: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Policy evaluation (pure)
// ---------------------------------------------------------------------------

/// What a rule is being matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject<'a> {
    Shell(&'a str),
    Read {
        path: &'a str,
        /// Workspace root, for rules written relative to the project.
        root: Option<&'a str>,
        home: Option<&'a str>,
    },
    Mcp {
        tool: &'a str,
    },
}

/// The outcome of matching a policy against a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    /// `None` when no rule matched: Toolport has no opinion.
    pub action: Option<PermissionAction>,
    /// The rule that decided it.
    pub rule: Option<String>,
}

fn split_rule(pattern: &str) -> (&str, Option<&str>) {
    match pattern.find('(') {
        Some(i) if pattern.ends_with(')') => {
            (&pattern[..i], Some(&pattern[i + 1..pattern.len() - 1]))
        }
        _ => (pattern, None),
    }
}

/// `*` matches any sequence of characters (including spaces and `/`); everything else
/// is literal. Full-string match.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                let rest = &p[1..];
                (0..=t.len()).any(|i| go(rest, &t[i..]))
            }
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

/// Path glob: `**` spans directories, `*` and `?` stay within one segment. A pattern that
/// names a directory also matches everything under it.
fn path_glob_match(pattern: &str, path: &str) -> bool {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                re.push_str(".*");
                i += 2;
                // a `**/` swallows the slash too so `a/**/b` matches `a/b`
                if i < chars.len() && chars[i] == '/' {
                    re.pop();
                    re.pop();
                    re.push_str("(?:.*/)?");
                    i += 1;
                }
                continue;
            }
            re.push_str("[^/]*");
        } else if c == '?' {
            re.push_str("[^/]");
        } else {
            if regex_syntax_special(c) {
                re.push('\\');
            }
            re.push(c);
        }
        i += 1;
    }
    let dir_re = format!("{re}(?:/.*)?$");
    regex::Regex::new(&dir_re)
        .map(|r| r.is_match(path))
        .unwrap_or(false)
}

fn regex_syntax_special(c: char) -> bool {
    matches!(
        c,
        '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
    )
}

/// Command separators Claude Code recognises; a compound command is judged per part.
fn split_compound(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            cur.push(c);
            i += 1;
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if two == "&&" || two == "||" || two == "|&" {
            parts.push(std::mem::take(&mut cur));
            i += 2;
            continue;
        }
        // An ampersand next to `>` belongs to a redirection (`2>&1`, `&>`, `>&`).
        let redirect_ampersand = c == '&'
            && (i.checked_sub(1).is_some_and(|j| chars[j] == '>')
                || chars.get(i + 1) == Some(&'>'));
        if c == ';' || c == '|' || c == '\n' || (c == '&' && !redirect_ampersand) {
            parts.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Drop leading wrappers - `timeout [options] DURATION`, `time [options]`, `nice [options]`,
/// `nohup` - so `Bash(npm test *)` also sees `timeout 30 npm test`, as Claude Code does.
/// Options are consumed properly (`nice -n 19`, `timeout --preserve-status -s KILL 5`), so a
/// wrapper cannot smuggle a denied command past a rule by carrying a flag the stripper did
/// not expect. When a wrapper's shape is not understood the command is left as it is.
fn strip_wrappers(command: &str) -> String {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    while let Some(&first) = words.first() {
        // `/usr/bin/timeout` is `timeout`: a wrapper named by path is still that wrapper.
        let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
        match name {
            "nohup" => {
                words.remove(0);
            }
            "time" => {
                words.remove(0);
                // `time -p`, `time -v`, `time -f FMT`, `time -o FILE`, `--format=`, ...
                while let Some(&w) = words.first() {
                    if !w.starts_with('-') {
                        break;
                    }
                    let takes_value = matches!(w, "-f" | "-o" | "--format" | "--output");
                    words.remove(0);
                    if takes_value && !words.is_empty() {
                        words.remove(0);
                    }
                }
            }
            "nice" => {
                words.remove(0);
                while let Some(&w) = words.first() {
                    if !w.starts_with('-') {
                        break;
                    }
                    let takes_value = matches!(w, "-n" | "--adjustment");
                    words.remove(0);
                    if takes_value && !words.is_empty() {
                        words.remove(0);
                    }
                }
            }
            "timeout" => {
                words.remove(0);
                while let Some(&w) = words.first() {
                    if !w.starts_with('-') {
                        break;
                    }
                    let takes_value = matches!(w, "-s" | "-k" | "--signal" | "--kill-after");
                    words.remove(0);
                    if takes_value && !words.is_empty() {
                        words.remove(0);
                    }
                }
                // The duration, then the command.
                if !words.is_empty() {
                    words.remove(0);
                }
            }
            _ => break,
        }
    }
    words.join(" ")
}

/// Every reading of a command part a deny or ask must be checked against: as written, and
/// with wrappers stripped. Matching any of them is enough to refuse; an allow rule has to
/// match the stripped form, which is the command that actually runs.
fn readings(part: &str) -> Vec<String> {
    let stripped = strip_wrappers(part);
    let mut v = vec![part.to_string()];
    if stripped != part {
        v.push(stripped);
    }
    v
}

/// Claude Code's Bash specifier semantics on ONE (non-compound) command.
fn bash_spec_matches(spec: &str, command: &str) -> bool {
    let spec = spec
        .strip_suffix(":*")
        .map(|s| format!("{s} *"))
        .unwrap_or_else(|| spec.to_string());
    if spec == "*" {
        return true;
    }
    if let Some(base) = spec.strip_suffix(" *") {
        // Word boundary: the prefix, then end of string or a space.
        return glob_match(base, command) || glob_match(&format!("{base} *"), command);
    }
    glob_match(&spec, command)
}

fn resolve_read_spec(spec: &str, root: Option<&str>, home: Option<&str>) -> String {
    let spec = spec.replace('\\', "/");
    let resolved = if let Some(rest) = spec.strip_prefix("~/") {
        format!("{}/{}", home.unwrap_or("~").trim_end_matches('/'), rest)
    } else if let Some(rest) = spec.strip_prefix("//") {
        format!("/{rest}")
    } else if spec.starts_with('/') {
        spec
    } else {
        let rel = spec.strip_prefix("./").unwrap_or(&spec);
        match root {
            Some(r) => format!("{}/{}", r.trim_end_matches('/').replace('\\', "/"), rel),
            None => rel.to_string(),
        }
    };
    normalize_path(&resolved)
}

/// Lexical normalisation so two spellings of one file compare equal: backslashes to
/// slashes, `.` and doubled slashes dropped, `..` folded, and on the case-insensitive
/// filesystems (macOS, Windows) everything lower-cased. A path that reaches a denied file
/// through `..` or a different case must not slip past the rule.
fn normalize_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    let joined = if absolute {
        format!("/{joined}")
    } else {
        joined
    };
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        joined.to_lowercase()
    } else {
        joined
    }
}

fn rule_matches(rule: &PermissionRule, subject: &Subject) -> bool {
    let (tool, spec) = split_rule(&rule.pattern);
    match subject {
        Subject::Shell(command) => {
            if !glob_match(tool, "Bash") {
                return false;
            }
            let Some(spec) = spec else { return true };
            let parts = split_compound(command);
            if parts.is_empty() {
                return false;
            }
            match rule.action {
                // A deny or ask fires if any part, in any reading, matches; an allow covers
                // the command only if every part's stripped form is allowed (Claude Code's
                // compound-command rule).
                PermissionAction::Deny | PermissionAction::Ask => parts
                    .iter()
                    .any(|p| readings(p).iter().any(|r| bash_spec_matches(spec, r))),
                PermissionAction::Allow => parts
                    .iter()
                    .all(|p| bash_spec_matches(spec, &strip_wrappers(p))),
            }
        }
        Subject::Read { path, root, home } => {
            if !glob_match(tool, "Read") {
                return false;
            }
            let Some(spec) = spec else { return true };
            let want = resolve_read_spec(spec, *root, *home);
            let have = normalize_path(path);
            path_glob_match(&want, &have)
        }
        Subject::Mcp { tool: name } => {
            let Some(rest) = tool.strip_prefix("mcp__") else {
                return tool == "*" && spec.is_none();
            };
            // `mcp__server__tool`: Cursor's payload names the tool only, so the server
            // segment cannot be checked; match the tool part (or the whole remainder, for a
            // payload that happens to carry `server__tool`).
            if let Some((_, tool_glob)) = rest.split_once("__") {
                return glob_match(tool_glob, name) || glob_match(rest, name);
            }
            glob_match(rest, name)
        }
    }
}

/// Match every rule; deny beats ask beats allow; `None` when nothing matched.
pub fn evaluate(rules: &[PermissionRule], subject: &Subject) -> Verdict {
    let mut best: Option<&PermissionRule> = None;
    let rank = |a: PermissionAction| match a {
        PermissionAction::Deny => 3,
        PermissionAction::Ask => 2,
        PermissionAction::Allow => 1,
    };
    for r in rules {
        if rule_matches(r, subject)
            && best
                .map(|b| rank(r.action) > rank(b.action))
                .unwrap_or(true)
        {
            best = Some(r);
        }
    }
    Verdict {
        action: best.map(|b| b.action),
        rule: best.map(|b| b.pattern.clone()),
    }
}

// ---------------------------------------------------------------------------
// The hook itself: Cursor payload in, decision out
// ---------------------------------------------------------------------------

/// Cursor's canonical "proceed" response.
fn allow_response() -> Value {
    json!({ "continue": true, "permission": "allow" })
}

fn decision_response(action: PermissionAction, rule: &str, what: &str) -> Value {
    match action {
        PermissionAction::Deny => json!({
            "continue": true,
            "permission": "deny",
            "user_message": format!("Toolport: blocked by your permission rule {rule}."),
            "agent_message": format!("Toolport blocked {what}: it matches the user's permission rule {rule} (deny). Do not retry it or work around it; explain and ask the user how to proceed."),
        }),
        PermissionAction::Ask => json!({
            "continue": true,
            "permission": "ask",
            "user_message": format!("Toolport: your permission rule {rule} asks before this."),
            "agent_message": format!("{what} matches the user's permission rule {rule} (ask); wait for their answer."),
        }),
        PermissionAction::Allow => allow_response(),
    }
}

/// The subject Cursor's payload describes, and a short human label for it.
fn cursor_subject<'a>(
    event: &str,
    payload: &'a Value,
    home: Option<&'a str>,
) -> Option<(Subject<'a>, String)> {
    let root = payload
        .get("workspace_roots")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .or_else(|| payload.get("cwd").and_then(Value::as_str));
    match event {
        "beforeShellExecution" => {
            let command = payload.get("command")?.as_str()?;
            Some((
                Subject::Shell(command),
                format!("the shell command `{}`", truncate(command, 120)),
            ))
        }
        "beforeReadFile" => {
            let path = payload.get("file_path")?.as_str()?;
            Some((
                Subject::Read { path, root, home },
                format!("reading `{}`", truncate(path, 160)),
            ))
        }
        "beforeMCPExecution" => {
            let tool = payload.get("tool_name")?.as_str()?;
            Some((
                Subject::Mcp { tool },
                format!("the MCP tool `{}`", truncate(tool, 80)),
            ))
        }
        _ => None,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Run the guard for one Cursor call: returns the JSON to print and the exit code. Always
/// exit 0 with a complete response; `deny` is expressed in the JSON, as Cursor documents.
/// `truncated` says the payload hit [`MAX_GUARD_STDIN_BYTES`] and is not whole.
pub fn handle_stdin(agent: &str, stdin: &str, truncated: bool) -> (String, i32) {
    let out = handle(agent, stdin, truncated);
    (out.to_string(), 0)
}

/// What the guard answers when it cannot judge a call: in Enforce, deny - an agent must
/// not be able to slip a call past a rule by making the payload unreadable (padding it past
/// the cap, say), and Cursor's `failClosed` would not catch a well-formed allow. Otherwise
/// allow, and record that it could not judge.
fn cannot_judge(reg: &crate::registry::Registry, why: &str) -> Value {
    if reg.guard_cursor_mode == GuardMode::Enforce {
        json!({
            "continue": true,
            "permission": "deny",
            "user_message": format!("Toolport: this call was refused because the guard could not read it ({why}). Switch the Cursor guard to Observe if this keeps happening."),
            "agent_message": format!("Toolport could not evaluate this call ({why}) and is enforcing, so it was refused. Do not retry it in a different form; tell the user."),
        })
    } else {
        allow_response()
    }
}

fn registry_unavailable(why: &str) -> Value {
    json!({
        "continue": true,
        "permission": "deny",
        "user_message": format!("Toolport: this call was refused because the guard could not load its policy ({why})."),
        "agent_message": format!("Toolport could not load its permission policy ({why}), so it refused this call. Tell the user."),
    })
}

fn handle(agent: &str, stdin: &str, truncated: bool) -> Value {
    if agent != "cursor" {
        crate::gatewaylog::append(&format!(
            "toolport: guard invoked for unknown agent {agent:?}"
        ));
        return allow_response();
    }
    let reg = match crate::registry::load_resolved_with_source() {
        Ok((reg, source)) if source.is_authoritative() => reg,
        Ok((_reg, source)) => {
            crate::gatewaylog::append(&format!(
                "toolport: guard registry is not authoritative: {source:?}"
            ));
            return registry_unavailable("the registry was recovered or unreadable");
        }
        Err(error) => {
            crate::gatewaylog::append(&format!(
                "toolport: guard could not load the registry: {error}"
            ));
            return registry_unavailable("the registry could not be read");
        }
    };
    // Cursor on Windows is documented to prefix hook stdin with a UTF-8 BOM.
    let stdin = stdin.trim_start_matches('\u{FEFF}');
    if truncated {
        record(
            json!({ "agent": "cursor", "event": "guard", "malformed": true, "truncated": true, "decision": if reg.guard_cursor_mode == GuardMode::Enforce { "deny" } else { "allow" }, "mode": reg.guard_cursor_mode }),
        );
        return cannot_judge(&reg, "the payload was larger than the guard reads");
    }
    let payload: Value = match serde_json::from_str(stdin) {
        Ok(v) => v,
        Err(error) => {
            record(
                json!({ "agent": "cursor", "event": "guard", "malformed": true, "decision": if reg.guard_cursor_mode == GuardMode::Enforce { "deny" } else { "allow" }, "mode": reg.guard_cursor_mode }),
            );
            crate::gatewaylog::append(&format!("toolport: guard payload did not parse: {error}"));
            return cannot_judge(&reg, "the payload did not parse");
        }
    };
    let event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("workspace_roots")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
    let Some((subject, what)) = cursor_subject(&event, &payload, home.as_deref()) else {
        // An event we did not register for, or one of ours missing its subject field. The
        // first is benign (allow); the second, in Enforce, is a call we cannot judge.
        let registered = CURSOR_EVENTS.contains(&event.as_str());
        record(
            json!({ "agent": "cursor", "event": "guard", "hookEvent": event, "cwd": cwd, "unhandled": true, "decision": if registered && reg.guard_cursor_mode == GuardMode::Enforce { "deny" } else { "allow" }, "mode": reg.guard_cursor_mode }),
        );
        return if registered {
            cannot_judge(&reg, "the call had no command, path or tool to check")
        } else {
            allow_response()
        };
    };
    let verdict = evaluate(&reg.agent_permission_rules, &subject);
    let (tool, hash_src) = match &subject {
        Subject::Shell(c) => ("Bash", (*c).to_string()),
        Subject::Read { path, .. } => ("Read", (*path).to_string()),
        Subject::Mcp { tool } => ("mcp", (*tool).to_string()),
    };
    let enforced = reg.guard_cursor_mode == GuardMode::Enforce;
    let decision = match (enforced, verdict.action) {
        (_, None) => "allow",
        (true, Some(a)) => a.list_key(),
        (false, Some(_)) => "allow",
    };
    record(json!({
        "agent": "cursor",
        "event": "guard",
        "hookEvent": event,
        "cwd": cwd,
        "tool": tool,
        "argsHash": crate::audit::args_hash(&json!({ "subject": hash_src })),
        "mode": reg.guard_cursor_mode,
        "wouldBe": verdict.action.map(|a| a.list_key()),
        "rule": verdict.rule,
        "decision": decision,
    }));
    match (enforced, verdict.action) {
        (true, Some(action)) => {
            decision_response(action, verdict.rule.as_deref().unwrap_or("?"), &what)
        }
        _ => allow_response(),
    }
}

/// One line in the sensor log (SBS-822's), so Agent activity and the SBS-823 dashboard see
/// guard decisions next to the tool calls they were about. No content: a tool name, a hash.
fn record(mut row: Value) {
    if let Some(obj) = row.as_object_mut() {
        obj.insert(
            "ts".into(),
            json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)),
        );
    }
    let Some(path) = crate::hooks::log_path() else {
        return;
    };
    let _ =
        crate::registry::append_line_locked(&path, &row.to_string(), 4 * 1024 * 1024, 10_000, None);
}

// ---------------------------------------------------------------------------
// Install into ~/.cursor/hooks.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardProfile {
    pub path: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardView {
    pub cursor_mode: GuardMode,
    /// Absent when Cursor's config directory cannot be resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<GuardProfile>,
    pub events: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardPreview {
    pub path: String,
    pub before: String,
    pub after: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn cursor_hooks_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cursor").join("hooks.json"))
}

fn guard_command(binary: &Path) -> String {
    format!("\"{}\" {GUARD_MARKER} cursor", binary.display())
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .map(|c| c.contains(GUARD_MARKER))
        .unwrap_or(false)
}

pub fn is_installed(root: &Value) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .map(|e| e.iter().any(is_ours))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// `root` with every guard entry removed, pruning an event list this emptied and then the
/// `hooks` object; `version` is left alone (it is the file's, not ours).
pub fn strip_guard(root: &Value) -> Value {
    let mut out = root.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    let Some(hooks) = obj.get_mut("hooks").and_then(Value::as_object_mut) else {
        return out;
    };
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        if let Some(list) = hooks.get_mut(&event).and_then(Value::as_array_mut) {
            let before = list.len();
            list.retain(|e| !is_ours(e));
            if list.is_empty() && before > 0 {
                hooks.remove(&event);
            }
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    out
}

/// `root` carrying exactly one guard entry per Cursor event. Strip-then-add, so idempotent
/// and convergent across binary paths; `failClosed` only when enforcing.
pub fn upsert_guard(root: &Value, binary: &Path, enforce: bool) -> Result<Value, String> {
    let mut out = strip_guard(root);
    let obj = out
        .as_object_mut()
        .ok_or_else(|| "hooks.json root is not a JSON object".to_string())?;
    obj.entry("version".to_string()).or_insert(json!(1));
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "`hooks` is present but is not an object".to_string())?;
    for event in CURSOR_EVENTS {
        let list = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let list = list
            .as_array_mut()
            .ok_or_else(|| format!("`hooks.{event}` is present but is not an array"))?;
        list.push(json!({
            "command": guard_command(binary),
            "timeout": GUARD_TIMEOUT_SECS,
            "failClosed": enforce,
        }));
    }
    Ok(out)
}

pub fn apply_on_startup() {
    let active = crate::registry::load()
        .map(|reg| !reg.guard_cursor_mode.is_off() || !reg.guard_targets.is_empty())
        .unwrap_or(false);
    if !active {
        return;
    }
    if let Err(error) = apply_with(None) {
        crate::gatewaylog::append(&format!("toolport: guard startup apply failed: {error}"));
    }
}

pub fn view() -> GuardView {
    let reg = crate::registry::load().unwrap_or_default();
    view_with(&reg, cursor_hooks_path().as_deref())
}

fn view_with(reg: &crate::registry::Registry, cursor: Option<&Path>) -> GuardView {
    let cursor = cursor.map(|path| match crate::clients::read_settings_json(path) {
        Ok((root, _)) => GuardProfile {
            path: path.display().to_string(),
            installed: is_installed(&root),
            error: None,
        },
        Err(error) => GuardProfile {
            path: path.display().to_string(),
            installed: false,
            error: Some(error),
        },
    });
    GuardView {
        cursor_mode: reg.guard_cursor_mode,
        cursor,
        events: CURSOR_EVENTS.iter().map(|e| (*e).to_string()).collect(),
        binary: crate::clients::resolve_gateway_path_readonly().map(|p| p.display().to_string()),
    }
}

/// Set Cursor's mode and make the file match, in one transaction.
pub fn set_cursor_mode(mode: GuardMode) -> Result<GuardView, String> {
    report(apply_with(Some(mode))?)?;
    Ok(view())
}

fn report(profiles: Vec<GuardProfile>) -> Result<(), String> {
    let failed: Vec<String> = profiles
        .iter()
        .filter_map(|p| p.error.as_ref().map(|e| format!("{}: {e}", p.path)))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The mode was saved, but the hooks file could not be updated: {}",
            failed.join("; ")
        ))
    }
}

fn apply_with(set_mode: Option<GuardMode>) -> Result<Vec<GuardProfile>, String> {
    apply_to(set_mode, cursor_hooks_path().as_deref(), &|| {
        crate::clients::resolve_gateway_path().filter(|p| p.is_file())
    })
}

fn apply_to(
    set_mode: Option<GuardMode>,
    cursor: Option<&Path>,
    resolve_binary: &dyn Fn() -> Option<PathBuf>,
) -> Result<Vec<GuardProfile>, String> {
    let (_, statuses) = crate::registry::update_authoritative(|reg| {
        if let Some(mode) = set_mode {
            reg.guard_cursor_mode = mode;
        }
        let mut statuses = Vec::new();
        let mut targets: Vec<String> = Vec::new();
        let want = match (reg.guard_cursor_mode, cursor) {
            (GuardMode::Off, _) | (_, None) => None,
            (mode, Some(path)) => Some((path.to_path_buf(), mode == GuardMode::Enforce)),
        };
        if let Some((path, enforce)) = &want {
            let binary = resolve_binary()
                .ok_or_else(|| "no gateway binary is available to run as a hook".to_string())?;
            let key = path.display().to_string();
            match install_at(path, &binary, *enforce) {
                Ok(()) => {
                    targets.push(key.clone());
                    statuses.push(GuardProfile {
                        path: key,
                        installed: true,
                        error: None,
                    });
                }
                Err(error) => {
                    if reg.guard_targets.contains(&key) {
                        targets.push(key.clone());
                    }
                    statuses.push(GuardProfile {
                        path: key,
                        installed: false,
                        error: Some(error),
                    });
                }
            }
        }
        let wanted: Vec<String> = want.iter().map(|(p, _)| p.display().to_string()).collect();
        for stale in reg.guard_targets.iter().filter(|t| !wanted.contains(t)) {
            if let Err(error) = remove_at(Path::new(stale)) {
                targets.push(stale.clone());
                statuses.push(GuardProfile {
                    path: stale.clone(),
                    installed: false,
                    error: Some(error),
                });
            }
        }
        reg.guard_targets = targets;
        Ok(statuses)
    })?;
    Ok(statuses)
}

fn install_at(path: &Path, binary: &Path, enforce: bool) -> Result<(), String> {
    let (root, original) = crate::clients::read_settings_json(path)?;
    let updated = upsert_guard(&root, binary, enforce)?;
    if updated.get("hooks") == root.get("hooks") {
        return Ok(());
    }
    crate::clients::write_settings_key_for("cursor", path, original.as_deref(), &updated, "hooks")
}

fn remove_at(path: &Path) -> Result<(), String> {
    let (root, original) = match crate::clients::read_settings_json(path) {
        Ok(pair) => pair,
        Err(error) => {
            if !path.exists() {
                return Ok(());
            }
            return Err(error);
        }
    };
    if original.is_none() {
        return Ok(());
    }
    let stripped = strip_guard(&root);
    if stripped == root {
        return Ok(());
    }
    crate::clients::write_settings_key_for("cursor", path, original.as_deref(), &stripped, "hooks")
}

/// The bytes `~/.cursor/hooks.json` would hold in `mode` (Off = with the guard removed).
pub fn preview(mode: GuardMode) -> Result<Option<GuardPreview>, String> {
    let Some(path) = cursor_hooks_path() else {
        return Ok(None);
    };
    let binary = crate::clients::resolve_gateway_path_readonly().ok_or_else(|| {
        "no gateway binary has been published yet, so there is nothing to preview".to_string()
    })?;
    Ok(Some(preview_at(&path, &binary, mode)))
}

fn preview_at(path: &Path, binary: &Path, mode: GuardMode) -> GuardPreview {
    let rendered = crate::clients::read_settings_json(path).and_then(|(root, original)| {
        let updated = match mode {
            GuardMode::Off => strip_guard(&root),
            m => upsert_guard(&root, binary, m == GuardMode::Enforce)?,
        };
        let after = crate::clients::render_settings_key(original.as_deref(), &updated, "hooks")?;
        Ok((original.unwrap_or_default(), after))
    });
    match rendered {
        Ok((before, after)) => GuardPreview {
            path: path.display().to_string(),
            before,
            after,
            error: None,
        },
        Err(error) => GuardPreview {
            path: path.display().to_string(),
            before: String::new(),
            after: String::new(),
            error: Some(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionAction::{Allow, Ask, Deny};

    fn rule(p: &str, a: PermissionAction) -> PermissionRule {
        PermissionRule {
            pattern: p.into(),
            action: a,
        }
    }

    #[test]
    fn bash_rules_follow_claude_codes_matching() {
        let deny = |p: &str| vec![rule(p, Deny)];
        let hit = |rules: &[PermissionRule], cmd: &str| {
            evaluate(rules, &Subject::Shell(cmd)).action == Some(Deny)
        };
        // Exact, prefix with word boundary, `:*`, wildcards anywhere, bare tool.
        assert!(hit(&deny("Bash(npm run build)"), "npm run build"));
        assert!(!hit(&deny("Bash(npm run build)"), "npm run build --watch"));
        assert!(hit(&deny("Bash(ls *)"), "ls"));
        assert!(hit(&deny("Bash(ls *)"), "ls -la"));
        assert!(
            !hit(&deny("Bash(ls *)"), "lsof -i"),
            "a trailing ` *` needs a word boundary"
        );
        assert!(hit(&deny("Bash(ls:*)"), "ls -la"));
        assert!(hit(&deny("Bash(git * main)"), "git push origin main"));
        assert!(hit(&deny("Bash(* --version)"), "node --version"));
        assert!(hit(&deny("Bash"), "anything at all"));
        assert!(hit(&deny("Bash(*)"), "anything at all"));
        // Compound: a deny fires if any part matches; wrappers are stripped first.
        assert!(hit(&deny("Bash(rm -rf *)"), "echo hi && rm -rf /tmp/x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "cd /tmp; rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "timeout 30 rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "nohup rm -rf x"));
        // Wrappers with options cannot smuggle a denied command past the rule.
        assert!(hit(&deny("Bash(rm -rf *)"), "nice -n 19 rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "nice --adjustment 5 rm -rf x"));
        assert!(hit(
            &deny("Bash(rm -rf *)"),
            "timeout --preserve-status -s KILL 5 rm -rf x"
        ));
        assert!(hit(&deny("Bash(rm -rf *)"), "timeout -k 2 10 rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "time -p nohup rm -rf x"));
        // A wrapper whose own name is denied is still denied as written.
        assert!(hit(&deny("Bash(timeout *)"), "timeout 5 ls"));
        // Background `&`, `|&`, and a wrapper named by path are all seen through.
        assert!(hit(&deny("Bash(rm -rf *)"), "sleep 1 & rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "ls |& rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), "/usr/bin/timeout 5 rm -rf x"));
        assert!(hit(&deny("Bash(rm -rf *)"), r"C:\tools\timeout 5 rm -rf x"));
        assert!(hit(
            &deny("Bash(rm -rf *)"),
            "/usr/bin/nice -n 5 /usr/bin/nohup rm -rf x"
        ));
        let allow_redirects = vec![rule("Bash(npm test *)", Allow)];
        assert_eq!(
            evaluate(&allow_redirects, &Subject::Shell("npm test 2>&1")).action,
            Some(Allow)
        );
        assert_eq!(
            evaluate(&allow_redirects, &Subject::Shell("npm test &>out.log")).action,
            Some(Allow)
        );
        assert!(
            !hit(&deny("Bash(rm -rf *)"), "echo 'rm -rf' && ls"),
            "quoted operators do not split; the echo is not rm"
        );
        // Allow covers a compound only when every part is allowed.
        let allow = vec![rule("Bash(npm *)", Allow)];
        assert_eq!(
            evaluate(&allow, &Subject::Shell("npm test && npm run lint")).action,
            Some(Allow)
        );
        assert_eq!(
            evaluate(&allow, &Subject::Shell("npm test && curl evil")).action,
            None
        );
        // Precedence: deny beats ask beats allow when several match.
        let mixed = vec![
            rule("Bash(git push*)", Ask),
            rule("Bash(git *)", Allow),
            rule("Bash(git push --force*)", Deny),
        ];
        assert_eq!(
            evaluate(&mixed, &Subject::Shell("git push --force origin main")).action,
            Some(Deny)
        );
        assert_eq!(
            evaluate(&mixed, &Subject::Shell("git push origin main")).action,
            Some(Ask)
        );
        assert_eq!(
            evaluate(&mixed, &Subject::Shell("git status")).action,
            Some(Allow)
        );
        assert_eq!(evaluate(&mixed, &Subject::Shell("ls")).action, None);
        // A Read rule never matches a shell command and vice versa.
        assert_eq!(
            evaluate(&[rule("Read(./.env)", Deny)], &Subject::Shell("cat .env")).action,
            None
        );
    }

    #[test]
    fn read_rules_resolve_against_workspace_and_home() {
        let deny = |p: &str| vec![rule(p, Deny)];
        let hit = |rules: &[PermissionRule], path: &str| {
            evaluate(
                rules,
                &Subject::Read {
                    path,
                    root: Some("/work/repo"),
                    home: Some("/home/u"),
                },
            )
            .action
                == Some(Deny)
        };
        assert!(hit(&deny("Read(./.env)"), "/work/repo/.env"));
        assert!(
            !hit(&deny("Read(./.env)"), "/work/repo/sub/.env"),
            "`./.env` is the root one"
        );
        assert!(hit(&deny("Read(./.env.*)"), "/work/repo/.env.local"));
        assert!(hit(&deny("Read(~/.ssh/**)"), "/home/u/.ssh/id_ed25519"));
        assert!(
            hit(&deny("Read(~/.ssh)"), "/home/u/.ssh/id_ed25519"),
            "a directory rule covers what is under it"
        );
        assert!(!hit(&deny("Read(~/.ssh/**)"), "/home/u/.sshx/key"));
        assert!(hit(&deny("Read(src/**/*.key)"), "/work/repo/src/a/b/c.key"));
        assert!(
            hit(&deny("Read(src/**/*.key)"), "/work/repo/src/c.key"),
            "`**/` also matches zero directories"
        );
        assert!(
            !hit(&deny("Read(src/*.key)"), "/work/repo/src/a/c.key"),
            "`*` stays in one segment"
        );
        assert!(hit(&deny("Read(//etc/passwd)"), "/etc/passwd"));
        // Lexically equivalent spellings reach the same rule.
        assert!(hit(
            &deny("Read(~/.ssh/**)"),
            "/home/u/proj/../.ssh/id_ed25519"
        ));
        assert!(hit(&deny("Read(./.env)"), "/work/repo/./sub/..//.env"));
        assert!(hit(
            &deny("Read(~/.ssh/**)"),
            "/home/u/.ssh/./keys/../id_rsa"
        ));
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert!(hit(&deny("Read(~/.ssh/**)"), "/home/u/.SSH/ID_RSA"));
        }
        assert!(hit(&deny("Read"), "/anything"));
        assert!(!hit(&deny("Bash(cat *)"), "/work/repo/.env"));
    }

    #[test]
    fn mcp_rules_match_the_tool_name_and_globs() {
        let deny = |p: &str| vec![rule(p, Deny)];
        let hit = |rules: &[PermissionRule], tool: &str| {
            evaluate(rules, &Subject::Mcp { tool }).action == Some(Deny)
        };
        assert!(hit(&deny("mcp__github__create_issue"), "create_issue"));
        assert!(hit(
            &deny("mcp__github__create_issue"),
            "github__create_issue"
        ));
        assert!(!hit(&deny("mcp__github__create_issue"), "list_issues"));
        assert!(hit(&deny("mcp__github__*"), "anything"));
        assert!(hit(&deny("mcp__*"), "anything"));
        assert!(hit(&deny("*"), "anything"), "a bare `*` is every tool");
        assert!(!hit(&deny("Bash"), "create_issue"));
    }

    #[test]
    fn responses_follow_cursors_shape_and_observe_never_blocks() {
        let r = decision_response(Deny, "Bash(rm -rf *)", "the shell command `rm -rf x`");
        assert_eq!(r["permission"], "deny");
        assert_eq!(r["continue"], true);
        assert!(r["user_message"]
            .as_str()
            .unwrap()
            .contains("Bash(rm -rf *)"));
        assert!(r["agent_message"]
            .as_str()
            .unwrap()
            .contains("Do not retry"));
        assert_eq!(registry_unavailable("test failure")["permission"], "deny");
        let r = decision_response(Ask, "Bash(git push*)", "x");
        assert_eq!(r["permission"], "ask");
        assert_eq!(decision_response(Allow, "x", "y"), allow_response());
        assert_eq!(
            allow_response(),
            serde_json::json!({ "continue": true, "permission": "allow" })
        );
    }

    #[test]
    fn unjudgeable_input_is_denied_only_when_enforcing() {
        let enforce = crate::registry::Registry {
            guard_cursor_mode: GuardMode::Enforce,
            ..Default::default()
        };
        let observe = crate::registry::Registry {
            guard_cursor_mode: GuardMode::Observe,
            ..Default::default()
        };
        assert_eq!(cannot_judge(&enforce, "x")["permission"], "deny");
        assert_eq!(cannot_judge(&observe, "x"), allow_response());
        // A BOM-prefixed payload parses once the BOM is stripped.
        let bom = "\u{FEFF}{\"hook_event_name\":\"beforeShellExecution\",\"command\":\"ls\"}";
        assert!(serde_json::from_str::<Value>(bom).is_err());
        assert!(serde_json::from_str::<Value>(bom.trim_start_matches('\u{FEFF}')).is_ok());
    }

    #[test]
    fn cursor_payloads_map_to_subjects() {
        let home = Some("/home/u");
        let p = serde_json::json!({ "hook_event_name": "beforeShellExecution", "command": "rm -rf x", "cwd": "/w", "workspace_roots": ["/w"] });
        let (s, what) = cursor_subject("beforeShellExecution", &p, home).unwrap();
        assert_eq!(s, Subject::Shell("rm -rf x"));
        assert!(what.contains("rm -rf x"));
        let p = serde_json::json!({ "hook_event_name": "beforeReadFile", "file_path": "/w/.env", "workspace_roots": ["/w"] });
        let (s, _) = cursor_subject("beforeReadFile", &p, home).unwrap();
        assert_eq!(
            s,
            Subject::Read {
                path: "/w/.env",
                root: Some("/w"),
                home
            }
        );
        let p = serde_json::json!({ "hook_event_name": "beforeMCPExecution", "tool_name": "create_issue", "tool_input": "{}" });
        let (s, _) = cursor_subject("beforeMCPExecution", &p, home).unwrap();
        assert_eq!(
            s,
            Subject::Mcp {
                tool: "create_issue"
            }
        );
        assert!(cursor_subject("afterFileEdit", &p, home).is_none());
        assert!(cursor_subject("beforeShellExecution", &serde_json::json!({}), home).is_none());
    }

    #[test]
    fn hooks_json_gets_exactly_our_entries_and_loses_exactly_them() {
        let theirs = serde_json::json!({
            "version": 1,
            "hooks": { "beforeShellExecution": [{ "command": "./my-own.sh" }], "stop": [{ "command": "./done.sh" }] }
        });
        let bin = Path::new("/opt/toolport/toolport-gateway");
        let with = upsert_guard(&theirs, bin, true).unwrap();
        assert!(is_installed(&with));
        assert!(!is_installed(&theirs));
        let shell = with["hooks"]["beforeShellExecution"].as_array().unwrap();
        assert_eq!(shell.len(), 2, "ours is added beside theirs");
        assert_eq!(shell[0]["command"], "./my-own.sh");
        assert_eq!(
            shell[1]["command"],
            "\"/opt/toolport/toolport-gateway\" --toolport-guard cursor"
        );
        assert_eq!(shell[1]["failClosed"], true);
        assert_eq!(
            with["hooks"]["beforeMCPExecution"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(with["hooks"]["beforeReadFile"].as_array().unwrap().len(), 1);
        assert_eq!(with["hooks"]["stop"], theirs["hooks"]["stop"]);
        // Idempotent, and re-upserting with observe flips only failClosed.
        assert_eq!(upsert_guard(&with, bin, true).unwrap(), with);
        let observe = upsert_guard(&with, bin, false).unwrap();
        assert_eq!(observe["hooks"]["beforeReadFile"][0]["failClosed"], false);
        // Strip gives theirs back, pruning only the lists we created.
        assert_eq!(strip_guard(&with), theirs);
        let fresh = upsert_guard(&serde_json::json!({}), bin, false).unwrap();
        assert_eq!(fresh["version"], 1);
        assert_eq!(
            strip_guard(&fresh),
            serde_json::json!({ "version": 1 }),
            "version was ours to add but is the file's to keep"
        );
    }

    #[test]
    fn apply_installs_flips_and_removes_over_a_scratch_file() {
        let _dirs = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!("toolport-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(scratch.join("data"));
        let hooks = scratch.join(".cursor").join("hooks.json");
        std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        std::fs::write(&hooks, "{\n  \"version\": 1,\n  \"hooks\": { \"stop\": [{ \"command\": \"./done.sh\" }] }\n}\n").unwrap();
        let bin = scratch.join("toolport-gateway");
        std::fs::write(&bin, "").unwrap();
        let resolve = || Some(bin.clone());

        // Off: nothing written.
        apply_to(None, Some(&hooks), &resolve).unwrap();
        assert!(!std::fs::read_to_string(&hooks)
            .unwrap()
            .contains("toolport-guard"));

        // Observe: installed, failClosed false, user's hook kept and formatting too.
        let st = apply_to(Some(GuardMode::Observe), Some(&hooks), &resolve).unwrap();
        assert!(st[0].installed && st[0].error.is_none(), "{st:?}");
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert!(
            text.contains("toolport-guard cursor")
                && text.contains("\"failClosed\": false")
                && text.contains("./done.sh"),
            "{text}"
        );
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.guard_targets, vec![hooks.display().to_string()]);
        assert!(view_with(&reg, Some(&hooks)).cursor.unwrap().installed);

        // Enforce: same entries, failClosed true.
        apply_to(Some(GuardMode::Enforce), Some(&hooks), &resolve).unwrap();
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert!(
            text.contains("\"failClosed\": true") && !text.contains("\"failClosed\": false"),
            "{text}"
        );

        // A hand-removed entry comes back at startup reconcile.
        std::fs::write(&hooks, "{\n  \"version\": 1,\n  \"hooks\": { \"stop\": [{ \"command\": \"./done.sh\" }] }\n}\n").unwrap();
        apply_to(None, Some(&hooks), &resolve).unwrap();
        assert!(std::fs::read_to_string(&hooks)
            .unwrap()
            .contains("toolport-guard"));

        // Off: exactly ours leaves; the user's stop hook and version stay.
        apply_to(Some(GuardMode::Off), Some(&hooks), &resolve).unwrap();
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert!(
            !text.contains("toolport-guard")
                && text.contains("./done.sh")
                && text.contains("\"version\": 1"),
            "{text}"
        );
        assert!(crate::registry::load().unwrap().guard_targets.is_empty());
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
