//! Native permission policy for Claude Code - the enforcer half of SBS-820 item 2
//! (SBS-1058), settings-based.
//!
//! Toolport's gateway policies govern MCP calls. They cannot stop Claude Code from
//! running `rm -rf`, force-pushing, or reading `.env`, because none of that is MCP.
//! Claude Code does have a native switch for exactly that: `permissions.deny` /
//! `permissions.ask` / `permissions.allow` in its `settings.json`, evaluated on every
//! native tool call, deny first, and - per its own docs - regardless of what any hook
//! says. So a policy written in Claude Code's own rule syntax needs no hook to be
//! enforced; it needs to be in every profile's settings file, and to come back out
//! cleanly.
//!
//! Three properties this module holds, inherited from [`crate::hooks`]:
//!
//!   * **Off by default, empty by default.** Nothing is written until the user turns it
//!     on, and the rule list starts empty. Presets are offered, never pre-applied.
//!   * **It adds only its own strings and removes only those.** JSON arrays of strings
//!     cannot carry a marker, so ownership is by record: for each file, the registry
//!     holds exactly the strings Toolport added. A rule the user already had is not
//!     added and not recorded, so turning Toolport off never takes away a rule the user
//!     wrote themselves.
//!   * **Every profile.** `CLAUDE_CONFIG_DIR` makes `~/.claude` and `~/.claude-work`
//!     both real; a policy in only one of them would quietly not apply in the other.
//!
//! Only Claude Code, for now. Cursor has hooks but no settings-level rule list, Codex
//! has approval/sandbox settings of a different shape, Gemini CLI is mixed; the UI and
//! docs say so rather than pretending. A `PreToolUse` hook that asks through the app's
//! approval broker is the next step (phase 2), not this one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use crate::registry::{PermissionAction, PermissionRule};

/// One Claude Code settings file and what the policy's state in it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStatus {
    /// Absolute path to the profile's `settings.json`.
    pub path: String,
    /// `applied` (every rule present), `stale` (policy changed or file edited since), `off`
    /// (policy disabled and nothing of ours recorded there), or `error`.
    pub state: String,
    /// How many of the policy's rules Toolport itself added to this file (the rest, if
    /// any, were already there and are the user's).
    pub added: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A rule the UI offers as a one-click add. Never applied on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub label: String,
    pub rules: Vec<PermissionRule>,
}

/// Everything the Agent permissions view needs, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsView {
    pub enabled: bool,
    pub rules: Vec<PermissionRule>,
    pub profiles: Vec<ProfileStatus>,
    pub presets: Vec<Preset>,
}

/// A dry run: the exact bytes one profile's file would hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsPreview {
    pub path: String,
    pub before: String,
    pub after: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn rule(pattern: &str, action: PermissionAction) -> PermissionRule {
    PermissionRule {
        pattern: pattern.to_string(),
        action,
    }
}

/// The presets offered in the UI. Each is a judgement about what a person almost always
/// means by "don't let it do the dangerous thing"; the list is short on purpose.
pub fn presets() -> Vec<Preset> {
    use PermissionAction::{Ask, Deny};
    vec![
        Preset {
            label: "Never delete recursively".into(),
            rules: vec![rule("Bash(rm -rf *)", Deny), rule("Bash(rm -r *)", Deny)],
        },
        Preset {
            label: "Never force-push".into(),
            rules: vec![
                rule("Bash(git push --force*)", Deny),
                rule("Bash(git push -f *)", Deny),
                rule("Bash(git push * --force*)", Deny),
            ],
        },
        Preset {
            label: "Ask before any git push".into(),
            rules: vec![rule("Bash(git push*)", Ask)],
        },
        Preset {
            label: "Never read .env files".into(),
            rules: vec![rule("Read(./.env)", Deny), rule("Read(./.env.*)", Deny)],
        },
        Preset {
            label: "Never read SSH keys".into(),
            rules: vec![rule("Read(~/.ssh/**)", Deny)],
        },
    ]
}

/// Claude Code's rule syntax: `Tool` or `Tool(specifier)`. A bare tool name removes the
/// tool (deny) or pre-approves it (allow); a specifier scopes it. MCP tools are
/// `mcp__server__tool`. We check shape, not semantics: Claude Code owns the meaning.
pub fn validate_pattern(pattern: &str) -> Result<(), String> {
    let p = pattern.trim();
    if p.is_empty() {
        return Err("A rule needs a pattern, such as Bash(rm -rf *).".to_string());
    }
    if p != pattern {
        return Err("A pattern must not start or end with whitespace.".to_string());
    }
    if p.contains('\n') || p.contains('\r') {
        return Err("A pattern is one line.".to_string());
    }
    let (tool, spec) = match p.find('(') {
        Some(i) => (&p[..i], Some(&p[i..])),
        None => (p, None),
    };
    let tool_ok = !tool.is_empty()
        && tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':');
    if !tool_ok {
        return Err(format!(
            "\"{tool}\" is not a tool name. Use Claude Code's syntax: a tool such as Bash, \
             Read, Edit, WebFetch, or mcp__server__tool, optionally followed by (pattern)."
        ));
    }
    if let Some(spec) = spec {
        if !spec.ends_with(')') || spec.len() < 3 {
            return Err(
                "A specifier is written in parentheses after the tool, such as Bash(rm -rf *)."
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Validate a whole policy: every pattern well-formed, no duplicate pattern.
pub fn validate_rules(rules: &[PermissionRule]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for r in rules {
        validate_pattern(&r.pattern)?;
        if !seen.insert(r.pattern.as_str()) {
            return Err(format!(
                "\"{}\" appears more than once. A pattern maps to one action.",
                r.pattern
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pure functions over a settings value
// ---------------------------------------------------------------------------

fn list(root: &Value, action: PermissionAction) -> Option<&Vec<Value>> {
    root.get("permissions")?.get(action.list_key())?.as_array()
}

fn contains(root: &Value, r: &PermissionRule) -> bool {
    list(root, r.action)
        .map(|l| l.iter().any(|v| v.as_str() == Some(r.pattern.as_str())))
        .unwrap_or(false)
}

/// Return `root` with exactly `added` removed from the lists they were added to, pruning a
/// list, and then the `permissions` object, that this pass emptied. A list the user left
/// empty and we then filled is indistinguishable afterwards from one we created, so it is
/// pruned too; an empty list carries no rule, so nothing of theirs is lost. A string present
/// more than once (the user duplicated it) loses one occurrence, ours.
pub fn strip_rules(root: &Value, added: &[PermissionRule]) -> Value {
    let mut out = root.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    let Some(perms) = obj.get_mut("permissions").and_then(Value::as_object_mut) else {
        return out;
    };
    for r in added {
        let key = r.action.list_key();
        let Some(arr) = perms.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        if let Some(i) = arr
            .iter()
            .position(|v| v.as_str() == Some(r.pattern.as_str()))
        {
            arr.remove(i);
            if arr.is_empty() {
                perms.remove(key);
            }
        }
    }
    if perms.is_empty() {
        obj.remove("permissions");
    }
    out
}

/// Return `root` carrying every rule in `rules`, plus the list of rules THIS pass owns:
/// `previously_added` is removed first (so a rule dropped from the policy leaves, and a
/// rule whose action changed moves lists), then each rule not already present is added
/// and recorded. A rule already present was the user's and is neither touched nor
/// recorded. Idempotent over its own output.
pub fn upsert_rules(
    root: &Value,
    rules: &[PermissionRule],
    previously_added: &[PermissionRule],
) -> Result<(Value, Vec<PermissionRule>), String> {
    let mut out = strip_rules(root, previously_added);
    let obj = out
        .as_object_mut()
        .ok_or_else(|| "settings root is not a JSON object".to_string())?;
    let mut added = Vec::new();
    for r in rules {
        // Already there (the user's own, or a duplicate pattern across actions is not
        // our business): do not add, do not own.
        if contains(&Value::Object(obj.clone()), r) {
            continue;
        }
        let perms = obj
            .entry("permissions".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let perms = perms
            .as_object_mut()
            .ok_or_else(|| "`permissions` is present but is not an object".to_string())?;
        let arr = perms
            .entry(r.action.list_key().to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let arr = arr.as_array_mut().ok_or_else(|| {
            format!(
                "`permissions.{}` is present but is not an array",
                r.action.list_key()
            )
        })?;
        arr.push(Value::String(r.pattern.clone()));
        added.push(r.clone());
    }
    Ok((out, added))
}

/// True when every policy rule is present in `root`.
pub fn is_applied(root: &Value, rules: &[PermissionRule]) -> bool {
    rules.iter().all(|r| contains(root, r))
}

// ---------------------------------------------------------------------------
// Apply / view / preview
// ---------------------------------------------------------------------------

/// Reconcile at startup: a profile created since the last apply (a new
/// `CLAUDE_CONFIG_DIR`) picks the policy up, and a file edited by hand is put back -
/// these are restrictions the user asked for, so, unlike agent rules, a hand edit is
/// not a reason to stand down. No-op when the feature is off and nothing is recorded.
pub fn apply_on_startup() {
    let active = crate::registry::load()
        .map(|reg| reg.agent_permissions_enabled || !reg.agent_permission_targets.is_empty())
        .unwrap_or(false);
    if !active {
        return;
    }
    if let Err(error) = apply() {
        crate::gatewaylog::append(&format!(
            "toolport: agent permissions startup apply failed: {error}"
        ));
    }
}

/// Current state, without writing anything.
pub fn view() -> PermissionsView {
    let reg = crate::registry::load().unwrap_or_default();
    view_with(&reg, &crate::clients::claude_settings_paths())
}

fn view_with(reg: &crate::registry::Registry, profiles: &[PathBuf]) -> PermissionsView {
    let profiles = profiles
        .iter()
        .map(|path| {
            let key = path.display().to_string();
            let recorded = reg
                .agent_permission_targets
                .get(&key)
                .cloned()
                .unwrap_or_default();
            let added = recorded.len();
            match crate::clients::read_settings_json(path) {
                Ok((root, _)) => {
                    // A rule we added that the policy no longer has, still on disk, is a
                    // write that did not land: it keeps enforcing, so the row must not read
                    // Applied just because the surviving rules are present.
                    let leftover = recorded
                        .iter()
                        .any(|r| !reg.agent_permission_rules.contains(r) && contains(&root, r));
                    let state = if reg.agent_permissions_enabled {
                        // An empty policy is trivially applied.
                        if is_applied(&root, &reg.agent_permission_rules) && !leftover {
                            "applied"
                        } else {
                            "stale"
                        }
                    } else if added > 0 {
                        "stale"
                    } else {
                        "off"
                    };
                    ProfileStatus {
                        path: key,
                        state: state.to_string(),
                        added,
                        error: None,
                    }
                }
                Err(error) => ProfileStatus {
                    path: key,
                    state: "error".to_string(),
                    added,
                    error: Some(error),
                },
            }
        })
        .collect();
    PermissionsView {
        enabled: reg.agent_permissions_enabled,
        rules: reg.agent_permission_rules.clone(),
        profiles,
        presets: presets(),
    }
}

/// Turn the policy on or off, then make every profile match. Opt-in and write commit
/// together (see [`crate::hooks::set_enabled`] for why).
pub fn set_enabled(enabled: bool) -> Result<PermissionsView, String> {
    report(apply_with(Some(enabled), None)?)?;
    Ok(view())
}

/// Replace the rule list, then make every profile match. Validated first; nothing is
/// written for an invalid policy.
pub fn set_rules(rules: Vec<PermissionRule>) -> Result<PermissionsView, String> {
    validate_rules(&rules)?;
    report(apply_with(None, Some(rules))?)?;
    Ok(view())
}

/// A profile that could not be written or cleaned is an error the caller must see, not a
/// row that quietly reads wrong. The registry change itself stands (the policy is what the
/// user asked for; the other profiles got it), so the message names the file(s) and the
/// view, refreshed, shows their real state.
fn report(statuses: Vec<ProfileStatus>) -> Result<(), String> {
    let failed: Vec<String> = statuses
        .iter()
        .filter(|s| s.state == "error")
        .map(|s| format!("{}: {}", s.path, s.error.clone().unwrap_or_default()))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The policy was saved, but {} could not be updated: {}",
            if failed.len() == 1 {
                "one profile"
            } else {
                "some profiles"
            },
            failed.join("; ")
        ))
    }
}

/// Write the policy into every profile when enabled, and remove what was recorded
/// everywhere when not.
pub fn apply() -> Result<Vec<ProfileStatus>, String> {
    apply_with(None, None)
}

fn apply_with(
    set_enabled: Option<bool>,
    set_rules: Option<Vec<PermissionRule>>,
) -> Result<Vec<ProfileStatus>, String> {
    apply_to(
        set_enabled,
        set_rules,
        &crate::clients::claude_settings_paths(),
    )
}

/// [`apply_with`] over an explicit profile set, so tests drive known files. Runs inside
/// `registry::update_authoritative`, for the reasons [`crate::hooks::apply_to`] gives.
fn apply_to(
    set_enabled: Option<bool>,
    set_rules: Option<Vec<PermissionRule>>,
    profiles: &[PathBuf],
) -> Result<Vec<ProfileStatus>, String> {
    let (_, statuses) = crate::registry::update_authoritative(|reg| {
        if let Some(enabled) = set_enabled {
            reg.agent_permissions_enabled = enabled;
        }
        if let Some(rules) = set_rules {
            reg.agent_permission_rules = rules;
        }
        let rules = reg.agent_permission_rules.clone();
        let candidates: Vec<String> = if reg.agent_permissions_enabled {
            profiles.iter().map(|p| p.display().to_string()).collect()
        } else {
            Vec::new()
        };
        let mut statuses = Vec::new();
        let mut targets: HashMap<String, Vec<PermissionRule>> = HashMap::new();

        for path in &candidates {
            let previous = reg
                .agent_permission_targets
                .get(path)
                .cloned()
                .unwrap_or_default();
            match install_at(Path::new(path), &rules, &previous) {
                Ok(added) => {
                    let n = added.len();
                    if !added.is_empty() {
                        targets.insert(path.clone(), added);
                    }
                    statuses.push(ProfileStatus {
                        path: path.clone(),
                        state: "applied".into(),
                        added: n,
                        error: None,
                    });
                }
                Err(error) => {
                    // Keep what we believed we had there: we could not see the file, so
                    // we cannot say our strings are gone (same reasoning as hooks).
                    if !previous.is_empty() {
                        targets.insert(path.clone(), previous);
                    }
                    statuses.push(ProfileStatus {
                        path: path.clone(),
                        state: "error".into(),
                        added: 0,
                        error: Some(error),
                    });
                }
            }
        }

        // Clean what we recorded and no longer want: every profile when off, or one that
        // disappeared from the candidate list. A failed removal stays recorded.
        for (path, previous) in reg.agent_permission_targets.iter() {
            if candidates.contains(path) {
                continue;
            }
            if let Err(error) = remove_at(Path::new(path), previous) {
                targets.insert(path.clone(), previous.clone());
                statuses.push(ProfileStatus {
                    path: path.clone(),
                    state: "error".into(),
                    added: previous.len(),
                    error: Some(error),
                });
            }
        }
        reg.agent_permission_targets = targets;
        Ok(statuses)
    })?;
    Ok(statuses)
}

/// Write the policy into one file and return what THIS write owns there. No write when
/// the file already says exactly what it should.
fn install_at(
    path: &Path,
    rules: &[PermissionRule],
    previous: &[PermissionRule],
) -> Result<Vec<PermissionRule>, String> {
    let (root, original) = crate::clients::read_settings_json(path)?;
    let (updated, added) = upsert_rules(&root, rules, previous)?;
    if updated != root {
        crate::clients::write_settings_key(path, original.as_deref(), &updated, "permissions")?;
    }
    Ok(added)
}

/// Remove what we added from one file. Gone file = success; unparseable = error.
fn remove_at(path: &Path, added: &[PermissionRule]) -> Result<(), String> {
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
    let stripped = strip_rules(&root, added);
    if stripped == root {
        return Ok(());
    }
    crate::clients::write_settings_key(path, original.as_deref(), &stripped, "permissions")
}

/// The exact bytes each profile would hold with the CURRENT registry policy (or `rules`
/// when given, so an edited-but-unsaved policy can be previewed). Writes nothing.
pub fn preview(rules: Option<Vec<PermissionRule>>) -> Result<Vec<PermissionsPreview>, String> {
    let reg = crate::registry::load()?;
    let rules = rules.unwrap_or_else(|| reg.agent_permission_rules.clone());
    validate_rules(&rules)?;
    Ok(preview_to(
        &reg,
        &rules,
        &crate::clients::claude_settings_paths(),
    ))
}

fn preview_to(
    reg: &crate::registry::Registry,
    rules: &[PermissionRule],
    profiles: &[PathBuf],
) -> Vec<PermissionsPreview> {
    profiles
        .iter()
        .map(|path| {
            let key = path.display().to_string();
            let previous = reg
                .agent_permission_targets
                .get(&key)
                .cloned()
                .unwrap_or_default();
            let rendered = crate::clients::read_settings_json(path).and_then(|(root, original)| {
                let (updated, _) = upsert_rules(&root, rules, &previous)?;
                let after = crate::clients::render_settings_key(
                    original.as_deref(),
                    &updated,
                    "permissions",
                )?;
                Ok((original.unwrap_or_default(), after))
            });
            match rendered {
                Ok((before, after)) => PermissionsPreview {
                    path: key,
                    before,
                    after,
                    error: None,
                },
                Err(error) => PermissionsPreview {
                    path: key,
                    before: String::new(),
                    after: String::new(),
                    error: Some(error),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use PermissionAction::{Allow, Ask, Deny};

    #[test]
    fn patterns_follow_claude_codes_syntax() {
        for ok in [
            "Bash",
            "Bash(rm -rf *)",
            "Read(./.env)",
            "WebFetch(domain:example.com)",
            "mcp__github__create_issue",
            "Edit(src/**/*.ts)",
        ] {
            validate_pattern(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in ["", " Bash", "Bash(", "Bash()", "rm -rf *", "Bash(x)\nRead"] {
            assert!(validate_pattern(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_rules(&[rule("Bash(x)", Deny), rule("Bash(x)", Ask)]).is_err());
        assert!(validate_rules(&[rule("Bash(x)", Deny), rule("Bash(y)", Ask)]).is_ok());
    }

    #[test]
    fn upsert_adds_only_what_is_missing_and_strip_removes_only_what_was_added() {
        let theirs = json!({
            "theme": "dark",
            "permissions": { "deny": ["Read(./.env)"], "allow": ["Bash(npm test)"] }
        });
        let policy = vec![
            rule("Read(./.env)", Deny), // already theirs
            rule("Bash(rm -rf *)", Deny),
            rule("Bash(git push*)", Ask),
        ];
        let (with, added) = upsert_rules(&theirs, &policy, &[]).unwrap();
        assert_eq!(
            added,
            vec![rule("Bash(rm -rf *)", Deny), rule("Bash(git push*)", Ask)]
        );
        assert_eq!(
            with["permissions"]["deny"],
            json!(["Read(./.env)", "Bash(rm -rf *)"])
        );
        assert_eq!(with["permissions"]["ask"], json!(["Bash(git push*)"]));
        assert_eq!(
            with["permissions"]["allow"],
            json!(["Bash(npm test)"]),
            "untouched"
        );
        assert_eq!(with["theme"], "dark");
        assert!(is_applied(&with, &policy));

        // Idempotent over its own output; what it owns does not grow.
        let (again, added2) = upsert_rules(&with, &policy, &added).unwrap();
        assert_eq!(again, with);
        assert_eq!(added2, added);

        // Strip takes back exactly ours; the user's deny and allow survive, the emptied
        // `ask` list is pruned, `permissions` itself stays because it is not empty.
        let back = strip_rules(&with, &added);
        assert_eq!(back, theirs);

        // A rule whose action changed moves lists on the next upsert.
        let moved = vec![rule("Bash(git push*)", Deny)];
        let (moved_root, moved_added) = upsert_rules(&with, &moved, &added).unwrap();
        assert_eq!(moved_root["permissions"].get("ask"), None);
        assert_eq!(
            moved_root["permissions"]["deny"],
            json!(["Read(./.env)", "Bash(git push*)"])
        );
        assert_eq!(moved_added, moved);
    }

    #[test]
    fn a_file_with_no_permissions_gets_them_and_loses_them_whole() {
        let fresh = json!({ "model": "opus" });
        let policy = vec![rule("Bash(rm -rf *)", Deny)];
        let (with, added) = upsert_rules(&fresh, &policy, &[]).unwrap();
        assert_eq!(
            with,
            json!({ "model": "opus", "permissions": { "deny": ["Bash(rm -rf *)"] } })
        );
        assert_eq!(
            strip_rules(&with, &added),
            fresh,
            "the shape we created is removed with it"
        );
        // A list the user left empty cannot be told from one we created once we have
        // filled and emptied it, so it is pruned; it carried no rule, so nothing is lost.
        let empty = json!({ "permissions": { "deny": [] } });
        let (w, a) = upsert_rules(&empty, &policy, &[]).unwrap();
        assert_eq!(strip_rules(&w, &a), json!({}));
        // Allow rules land in allow.
        let (w, _) = upsert_rules(&fresh, &[rule("Bash(npm test)", Allow)], &[]).unwrap();
        assert_eq!(w["permissions"]["allow"], json!(["Bash(npm test)"]));
    }

    #[test]
    fn presets_are_valid_policies() {
        for p in presets() {
            validate_rules(&p.rules).unwrap_or_else(|e| panic!("{}: {e}", p.label));
        }
    }

    /// End to end over scratch files: opt in writes, a policy change rewrites, a hand edit
    /// is put back by startup, turning off removes exactly ours, and a profile we could not
    /// parse is reported but does not stop the others.
    #[test]
    fn apply_writes_reconciles_and_cleans_exactly_what_it_owns() {
        let _dirs = crate::registry::data_dir_test_lock();
        let scratch =
            std::env::temp_dir().join(format!("toolport-agent-permissions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(scratch.join("data"));
        let a = scratch.join("a").join("settings.json");
        let b = scratch.join("b").join("settings.json");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(
            &a,
            "{\n  \"theme\": \"dark\",\n  \"permissions\": { \"deny\": [\"Read(./.env)\"] }\n}\n",
        )
        .unwrap();
        // b does not exist yet: a fresh profile.
        let profiles = vec![a.clone(), b.clone()];
        let policy = vec![rule("Read(./.env)", Deny), rule("Bash(rm -rf *)", Deny)];

        // Rules set while OFF: stored, nothing written.
        apply_to(None, Some(policy.clone()), &profiles).unwrap();
        assert!(!std::fs::read_to_string(&a).unwrap().contains("rm -rf"));
        assert!(!b.exists());

        // On: both written; `a` owns only the rule it did not already have.
        let statuses = apply_to(Some(true), None, &profiles).unwrap();
        assert!(
            statuses.iter().all(|s| s.state == "applied"),
            "{statuses:?}"
        );
        let reg = crate::registry::load().unwrap();
        assert_eq!(
            reg.agent_permission_targets[&a.display().to_string()],
            vec![rule("Bash(rm -rf *)", Deny)]
        );
        assert_eq!(
            reg.agent_permission_targets[&b.display().to_string()],
            policy
        );
        let a_text = std::fs::read_to_string(&a).unwrap();
        assert!(
            a_text.contains("\"theme\": \"dark\""),
            "user formatting kept: {a_text}"
        );
        assert!(a_text.contains("Bash(rm -rf *)"));
        assert!(std::fs::read_to_string(&b)
            .unwrap()
            .contains("Read(./.env)"));
        let view = view_with(&reg, &profiles);
        assert!(view.profiles.iter().all(|p| p.state == "applied"));
        assert_eq!(view.profiles[0].added, 1);
        assert_eq!(view.profiles[1].added, 2);

        // Hand-edit b to drop a rule: stale in the view, and startup puts it back.
        std::fs::write(
            &b,
            "{\n  \"permissions\": { \"deny\": [\"Read(./.env)\"] }\n}\n",
        )
        .unwrap();
        assert_eq!(view_with(&reg, &profiles).profiles[1].state, "stale");
        apply_to(None, None, &profiles).unwrap();
        assert!(std::fs::read_to_string(&b).unwrap().contains("rm -rf"));

        // Policy change: the dropped rule leaves every file, the new one arrives.
        let policy2 = vec![rule("Read(./.env)", Deny), rule("Bash(git push*)", Ask)];
        apply_to(None, Some(policy2.clone()), &profiles).unwrap();
        let a_text = std::fs::read_to_string(&a).unwrap();
        assert!(
            !a_text.contains("rm -rf") && a_text.contains("git push*"),
            "{a_text}"
        );

        // A profile that cannot be parsed is reported, the other still applies, and the
        // broken one's record is kept (we cannot see whether our strings are in it).
        std::fs::write(&b, "{ not json").unwrap();
        let statuses = apply_to(None, None, &profiles).unwrap();
        assert_eq!(statuses.iter().filter(|s| s.state == "error").count(), 1);
        assert!(crate::registry::load()
            .unwrap()
            .agent_permission_targets
            .contains_key(&b.display().to_string()));
        // And the user-facing setters say so rather than returning a clean view.
        let err = report(statuses).unwrap_err();
        assert!(
            err.contains("could not be updated") && err.contains("settings.json"),
            "{err}"
        );
        std::fs::write(&b, "{}\n").unwrap();
        apply_to(None, None, &profiles).unwrap();

        // A rule dropped from the policy whose removal did not land (here: the file was
        // restored by hand to an older state that still carries it) must not read Applied
        // just because the remaining rules are present.
        let policy3 = vec![rule("Read(./.env)", Deny)];
        apply_to(None, Some(policy3.clone()), &profiles).unwrap();
        std::fs::write(&b, "{\n  \"permissions\": { \"deny\": [\"Read(./.env)\"], \"ask\": [\"Bash(git push*)\"] }\n}\n").unwrap();
        let reg = crate::registry::load().unwrap();
        // Simulate the record still naming the dropped rule (a failed strip keeps it).
        let mut reg_with_leftover = reg.clone();
        reg_with_leftover
            .agent_permission_targets
            .get_mut(&b.display().to_string())
            .unwrap()
            .push(rule("Bash(git push*)", Ask));
        assert_eq!(
            view_with(&reg_with_leftover, &profiles).profiles[1].state,
            "stale"
        );
        // Put the hand-edited file back to a plain state so the final teardown below is
        // about our own strings only.
        std::fs::write(&b, "{}\n").unwrap();
        apply_to(None, Some(policy2.clone()), &profiles).unwrap();

        // Off: exactly ours leaves; the user's own Read(./.env) in `a` stays.
        apply_to(Some(false), None, &profiles).unwrap();
        let a_text = std::fs::read_to_string(&a).unwrap();
        assert!(
            a_text.contains("Read(./.env)") && !a_text.contains("git push"),
            "{a_text}"
        );
        assert!(!std::fs::read_to_string(&b).unwrap().contains("git push"));
        assert!(crate::registry::load()
            .unwrap()
            .agent_permission_targets
            .is_empty());
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
