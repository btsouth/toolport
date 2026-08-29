//! Personal agent rules — write the user's own rule set into every opted-in AI client.
//!
//! The desktop half of SBS-821 (`agent-rules` spec). The user authors one or more named
//! [`RuleSet`]s in the app; the active one is written into each client's global rules file so
//! Claude Code, Codex, Gemini CLI and the rest all read the same instructions without the user
//! hand-editing four files.
//!
//! This is the same write engine Team Instructions uses ([`crate::instructions`]), driven from
//! local state instead of a pulled org config: `(rule_set_id, revision)` stands in for
//! `(team_id, version)`, and every target carries [`Scope::Personal`] so a member of a Teams org
//! keeps both sets of rules in the same files without either clobbering the other.
//!
//! Two rules this module exists to enforce:
//!
//!   * **Opt-in per client.** Writing into someone's `~/.claude/rules` or `AGENTS.md` unasked is
//!     not something to do, so [`crate::registry::Registry::rules_client_enabled`] defaults to
//!     off and the UI previews the write first.
//!   * **Clean up exactly what we wrote.** Every applied path is recorded in
//!     `Registry::rules_targets`; anything in that list we do not re-write this pass is removed
//!     by path, so switching set, opting a client out, or uninstalling a client never strands a
//!     file. Same contract as `teams::apply_instructions_to`.

use crate::instructions::{self, ApplyState, Scope, Strategy, Target};
use crate::registry::{RuleSet, RulesProject};
use serde::{Deserialize, Serialize};

/// One client's row in the Rules view: whether it is opted in, where its rules file is, and what
/// state that file is in right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub id: String,
    pub name: String,
    /// User opt-in. A disabled client still reports a `state` (usually `Stale`) so the UI can
    /// show what WOULD happen without writing anything.
    pub enabled: bool,
    /// `None` when this client has no global-rules location we can write (Cursor, Warp).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub state: ApplyState,
    /// When `state` is [`ApplyState::Drifted`]: the body as it is on disk right now, so the UI
    /// can show the difference and offer to pull it into the set. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk: Option<String>,
}

/// Everything the Rules view needs, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesView {
    pub sets: Vec<RuleSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_set_id: Option<String>,
    pub clients: Vec<ClientStatus>,
    /// Registered project folders and their per-file state (SBS-1037).
    #[serde(default)]
    pub projects: Vec<ProjectStatus>,
}

/// A dry run of one client's write, so the user sees the exact bytes before the first apply
/// (SBS-821 acceptance criteria). Never touches disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesPreview {
    pub client_id: String,
    pub path: String,
    /// `"ownedFile"` when Toolport owns the whole file, `"sentinelBlock"` when it owns only the
    /// marked span in a file the user also edits. Drives how the UI frames the change.
    pub strategy: String,
    /// The file as it is now. Empty when it does not exist yet.
    pub before: String,
    /// The file as this apply would leave it.
    pub after: String,
    pub state: ApplyState,
}

/// One installed client and where its personal rules go. Deliberately NOT
/// [`crate::clients::DetectedClient`]: that type carries a client's whole MCP inventory and has no
/// cheap constructor, so depending on it here would make every apply test build a fake server
/// list. This is the only shape the apply logic needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientTarget {
    id: String,
    name: String,
    /// `None` when this client has no global-rules location we manage (Cursor, Warp), or is
    /// covered transitively by another client's file.
    target: Option<Target>,
}

/// Every installed client paired with its personal-rules target, including clients the user has
/// NOT opted in — the view lists them so they can be turned on.
fn installed_targets() -> Vec<ClientTarget> {
    crate::clients::detect_clients()
        .into_iter()
        .filter(|c| c.app_present)
        .map(|c| ClientTarget {
            target: crate::clients::client_rules_target(&c.id, Scope::Personal),
            id: c.id,
            name: c.name,
        })
        .collect()
}

/// The distinct paths to write this pass: opted-in clients only, de-duped by path so a file two
/// clients share (Claude Code + VS Code Copilot, Gemini CLI + Antigravity) is written once.
fn enabled_targets(reg: &crate::registry::Registry, installed: &[ClientTarget]) -> Vec<Target> {
    let mut seen = std::collections::HashSet::new();
    installed
        .iter()
        .filter(|c| reg.rules_client_enabled(&c.id))
        .filter_map(|c| c.target.clone())
        .filter(|t| seen.insert(t.path.clone()))
        .collect()
}

/// Read-only per-client state for the given set. Reports reality rather than what we last wrote,
/// so a hand-edited or deleted block shows `Stale` and a client installed since the last apply
/// shows up immediately.
fn status_from(
    reg: &crate::registry::Registry,
    installed: &[ClientTarget],
    set: Option<&RuleSet>,
) -> Vec<ClientStatus> {
    installed
        .iter()
        .map(|c| {
            let (state, on_disk) = match (&c.target, set) {
                (None, _) => (ApplyState::Unsupported, None),
                // No active set: the desired end state is "nothing of ours on disk", so the
                // question is presence, not content. Reporting Applied unconditionally here hid a
                // cleanup that failed — the block was still sitting in the file while the row
                // said "up to date".
                (Some(t), None) => {
                    if instructions::is_present(&t.path, Scope::Personal) {
                        (ApplyState::Stale, None)
                    } else {
                        (ApplyState::Applied, None)
                    }
                }
                (Some(t), Some(s)) => {
                    match instructions::current_state(t, &s.id, s.revision, &s.content) {
                        // Stale covers both "not written yet" and "written, then changed by
                        // hand". Only the second is drift, and only drift carries a body worth
                        // showing (SBS-1036).
                        ApplyState::Stale => {
                            match instructions::drifted_body(t, &s.id, s.revision, &s.content) {
                                Some(body) => (ApplyState::Drifted, Some(body)),
                                None => (ApplyState::Stale, None),
                            }
                        }
                        other => (other, None),
                    }
                }
            };
            ClientStatus {
                id: c.id.clone(),
                name: c.name.clone(),
                enabled: reg.rules_client_enabled(&c.id),
                path: c
                    .target
                    .as_ref()
                    .map(|t| t.path.to_string_lossy().to_string()),
                state,
                on_disk,
            }
        })
        .collect()
}

/// The whole Rules view. Read-only; scans every installed client's rules file, so callers run it
/// off the UI thread.
pub fn view() -> Result<RulesView, String> {
    view_with(&installed_targets())
}

fn view_with(installed: &[ClientTarget]) -> Result<RulesView, String> {
    let reg = crate::registry::load()?;
    let set = reg.active_rule_set().cloned();
    Ok(RulesView {
        clients: status_from(&reg, installed, set.as_ref()),
        sets: reg.rule_sets.clone(),
        active_set_id: reg.active_rule_set_id.clone(),
        projects: project_statuses(&reg, installed),
    })
}

/// Apply the active rule set to every opted-in client, then clean up anything we wrote before and
/// did not write now. Returns the refreshed view.
///
/// Best-effort per client, like the team writer: one unwritable file must not abort the rest. A
/// client that reports anything other than [`ApplyState::Applied`] is simply not recorded, so the
/// next pass tries it again.
pub fn apply() -> Result<RulesView, String> {
    apply_with(ApplyMode::Reconcile)
}

/// The explicit "Re-apply" / "Overwrite": make every opted-in client's file match the active set,
/// including a block the user edited by hand on disk. The one apply that is allowed to put
/// Toolport's text back over a [`ApplyState::Drifted`] block, because the user asked for exactly
/// that with the diff in front of them.
pub fn apply_overwriting_drift() -> Result<RulesView, String> {
    apply_with(ApplyMode::Overwrite)
}

/// Overwrite exactly ONE client's file from the set (the "Overwrite the file" action on that
/// client's drift card) and reconcile everything else. A drifted block in some other client's
/// file is left alone: the user resolved the one they were looking at, not all of them.
pub fn apply_overwriting_client(client_id: &str) -> Result<RulesView, String> {
    let installed = installed_targets();
    let Some(path) = installed
        .iter()
        .find(|c| c.id == client_id)
        .and_then(|c| c.target.as_ref())
        .map(|t| t.path.clone())
    else {
        return Err("That client has no rules file Toolport manages.".to_string());
    };
    apply_to(&installed, ApplyMode::OverwriteOnly(path))
}

/// Whether an apply may rewrite a block the user has edited on disk since Toolport wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyMode {
    /// Write what is missing or behind the set; leave a hand-edited current-revision block as
    /// it is and report it [`ApplyState::Drifted`]. What every automatic path uses: startup,
    /// saving the set, switching sets, toggling a client.
    Reconcile,
    /// Write everything that differs from the set, drift included.
    Overwrite,
    /// Reconcile, except that the one file at this path is written even if drifted.
    OverwriteOnly(std::path::PathBuf),
}

impl ApplyMode {
    fn overwrites(&self, path: &std::path::Path) -> bool {
        match self {
            ApplyMode::Reconcile => false,
            ApplyMode::Overwrite => true,
            ApplyMode::OverwriteOnly(only) => only == path,
        }
    }
}

fn apply_with(mode: ApplyMode) -> Result<RulesView, String> {
    let installed = installed_targets();
    apply_to(&installed, mode)
}

/// [`apply`] over an explicit client/target set, so tests drive a known set of files instead of
/// the developer's real machine.
fn apply_to(installed: &[ClientTarget], mode: ApplyMode) -> Result<RulesView, String> {
    // Hold the registry's cross-process lock from the ONE authoritative load through every file
    // write/cleanup and the rules_targets save. Rule mutations also use this lock, so an older
    // apply cannot write stale bytes after a newer set wins, and a placeholder/default snapshot
    // can never be mistaken for an intentional clear. Team writes do not invert this order:
    // `write_target`/`remove_recorded` release WRITE_LOCK before Teams calls registry::update.
    let (reg, ()) = crate::registry::update_authoritative(|reg| {
        let set = reg.active_rule_set().cloned();
        let prev_targets = reg.rules_targets.clone();
        let targets = enabled_targets(reg, installed);

        // Every path this apply still WANTS to own, whether or not writing it succeeded. Cleanup
        // is driven by this rather than by successful writes: a transient failure must keep the
        // previous good block and its cleanup record.
        let desired: Vec<String> = if set.is_some() {
            targets
                .iter()
                .map(|t| t.path.to_string_lossy().to_string())
                .collect()
        } else {
            Vec::new()
        };

        let mut written: Vec<String> = Vec::new();
        // Hand-edited current-revision blocks a reconcile left alone. They are still ours on
        // disk, so they stay on record like a successful write would.
        let mut left_drifted: Vec<String> = Vec::new();
        if let Some(s) = set.as_ref() {
            for target in &targets {
                if !mode.overwrites(&target.path)
                    && instructions::drifted_body(target, &s.id, s.revision, &s.content).is_some()
                {
                    left_drifted.push(target.path.to_string_lossy().to_string());
                    continue;
                }
                if instructions::write_target(target, &s.id, s.revision, &s.content)
                    == ApplyState::Applied
                {
                    written.push(target.path.to_string_lossy().to_string());
                }
            }
        }

        // Paths we could not clean stay recorded so the next pass retries.
        let mut uncleaned: Vec<String> = Vec::new();
        for old in &prev_targets {
            if !desired.iter().any(|d| d == old)
                && !instructions::remove_recorded(std::path::Path::new(old), Scope::Personal)
            {
                uncleaned.push(old.clone());
            }
        }

        // What we now own on disk: successful writes, still-desired previous good files, and
        // failed cleanups. Every path remains discoverable for a later reconciliation.
        let mut owned = written;
        for old in prev_targets
            .iter()
            .chain(uncleaned.iter())
            .chain(left_drifted.iter())
        {
            let still_wanted = desired.iter().any(|d| d == old) || uncleaned.contains(old);
            if still_wanted && !owned.contains(old) {
                owned.push(old.clone());
            }
        }
        reg.rules_targets = owned;
        Ok(())
    })?;

    let set = reg.active_rule_set().cloned();
    Ok(RulesView {
        clients: status_from(&reg, installed, set.as_ref()),
        sets: reg.rule_sets.clone(),
        active_set_id: reg.active_rule_set_id.clone(),
        projects: project_statuses(&reg, installed),
    })
}

/// Dry-run one client's write. `None` when the client has no rules location we manage, or when no
/// set is active (there is nothing to show).
///
/// Deliberately NOT gated on the client being installed: this answers "what would land here",
/// which is a fair question about a client the user is about to install, and the caller only
/// offers it for clients it detected anyway.
/// `content`, when given, is previewed INSTEAD of the saved set's content. That is what makes this
/// honest for an editor with unsaved text: the alternative (save first, then preview) would apply
/// the draft to every opted-in client's file, which is the exact opposite of what a dry run is for.
pub fn preview(client_id: &str, content: Option<&str>) -> Result<Option<RulesPreview>, String> {
    let reg = crate::registry::load()?;
    let Some(target) = crate::clients::client_rules_target(client_id, Scope::Personal) else {
        return Ok(None);
    };
    let Some(set) = reg.active_rule_set() else {
        return Ok(None);
    };
    let content = content.unwrap_or(set.content.as_str());
    preview_target(client_id, &target, set, content).map(Some)
}

fn preview_target(
    client_id: &str,
    target: &Target,
    set: &RuleSet,
    content: &str,
) -> Result<RulesPreview, String> {
    // An unreadable file must NOT read as empty. Preview is the safeguard the user leans on
    // before letting Toolport touch a file they own, and "" would render the dry-run as a
    // first-time write of a file that actually has content we could not see. Only a genuinely
    // absent file is empty; anything else is reported.
    let before = match std::fs::read_to_string(&target.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Could not read {}: {e}", target.path.display())),
    };
    let candidate = match target.strategy {
        Strategy::OwnedFile => {
            instructions::render_owned_file(Scope::Personal, &set.id, set.revision, content)
        }
        Strategy::SentinelBlock => {
            instructions::upsert_block(&before, Scope::Personal, &set.id, set.revision, content)
        }
    };
    let state = instructions::current_state(target, &set.id, set.revision, content);
    // Blocked, over-cap, and invalid writes leave the file untouched. Showing the candidate bytes
    // in those states would promise a result that write_target deliberately refuses to create.
    let after = match state {
        ApplyState::Applied | ApplyState::Stale => candidate,
        _ => before.clone(),
    };
    Ok(RulesPreview {
        client_id: client_id.to_string(),
        path: target.path.to_string_lossy().to_string(),
        strategy: match target.strategy {
            Strategy::OwnedFile => "ownedFile",
            Strategy::SentinelBlock => "sentinelBlock",
        }
        .to_string(),
        state,
        before,
        after,
    })
}

/// Create or update a set, then apply. Returns the refreshed view.
pub fn save_set(id: Option<&str>, name: &str, content: &str) -> Result<RulesView, String> {
    // Refuse content carrying Toolport's own markers, at the point the user submits it.
    //
    // `write_target` already refuses such content, but only at write time: without this the text
    // is persisted first, and then EVERY opted-in client reports a write error until the user
    // works out which invisible HTML comment is to blame. The realistic way in is copying out of
    // the preview panel, which shows the rendered file including its markers.
    //
    // Rejected, not auto-stripped: silently editing someone's rules is worse than telling them.
    if instructions::content_carries_a_marker(content) {
        return Err(
            "These rules contain Toolport's own marker comments (toolport:rules:start / :end, or \
             the team-instructions equivalents). Toolport uses those to find the block it owns, so \
             it cannot store them as rules. Remove them and save again — if you copied this out of \
             the preview, copy just your own text."
                .to_string(),
        );
    }
    crate::registry::update(|reg| reg.upsert_rule_set(id, name, content).map(|_| ()))?;
    apply()
}

/// Delete a set, then apply. Deleting the active set clears the selection, so the apply that
/// follows removes every file we wrote.
pub fn delete_set(id: &str) -> Result<RulesView, String> {
    // A project that applied this set loses it: its files are cleaned up like a remove, and the
    // project stays registered with no set, so the user sees what happened rather than a
    // dangling pointer to a set that no longer exists.
    let orphaned: Vec<RulesProject> = crate::registry::load()?
        .rules_projects
        .into_iter()
        .filter(|p| p.set_id.as_deref() == Some(id))
        .collect();
    // A path that could not be cleaned stays on the project's record, exactly as in
    // project_remove: cleanup is driven by that record, so dropping it would strand the block.
    let leftovers: Vec<(String, Vec<String>)> = orphaned
        .iter()
        .map(|p| (p.id.clone(), clean_project_targets(&p.targets, &[])))
        .collect();
    crate::registry::update(|reg| {
        reg.remove_rule_set(id);
        for p in reg.rules_projects.iter_mut() {
            if p.set_id.as_deref() == Some(id) {
                p.set_id = None;
                p.targets = leftovers
                    .iter()
                    .find(|(pid, _)| pid == &p.id)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
            }
        }
        Ok(())
    })?;
    apply()
}

/// Switch (or clear) the active set, then apply.
pub fn set_active(id: Option<&str>) -> Result<RulesView, String> {
    crate::registry::update(|reg| {
        reg.set_active_rule_set(id);
        Ok(())
    })?;
    apply()
}

/// Opt one client in or out, then apply. Opting out removes that client's file on the same pass.
pub fn set_client_enabled(client_id: &str, enabled: bool) -> Result<RulesView, String> {
    crate::registry::update(|reg| {
        reg.set_rules_client_enabled(client_id, enabled);
        Ok(())
    })?;
    apply()
}

// ---------------------------------------------------------------------------------------
// Project-level rules (SBS-1037)
// ---------------------------------------------------------------------------------------
//
// Global rules go into each client's home-directory file. Project rules go into the user's
// REPOSITORIES, which is a different consent question, so the model is deliberately narrower:
// a folder is only ever one the user registered (nothing is scanned for); inside it the unit
// of consent is a file, switched on per project; a file is written only by an explicit Apply
// for that project, never at startup; and Toolport writes only its own marked block or its
// own owned file, with the same writer and markers as everywhere else.
//
// Why files and not clients: at project level nearly every client reads the root `AGENTS.md`
// (Codex, Cursor, Copilot, Roo, Cline, Kiro, Goose, Devin, Pi, Oh My Pi), Gemini CLI and
// Antigravity read `GEMINI.md`, and Claude Code and VS Code read `.claude/rules/`. Offering a
// dozen client checkboxes that collapse onto one file would be theatre; the file IS the
// decision, and each one names the clients it reaches. Each mapping is cited in
// docs/agent-rules.md. Zed is left out on purpose: it reads only the FIRST of `.rules`,
// `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`,
// `AGENT.md`, `AGENTS.md`, ..., so whether it would read our block depends on files Toolport
// cannot see into, and Applied could be false.

/// One file Toolport can write inside a registered project folder, and the clients that read
/// it there (by client id, as `crate::clients::detect_clients` names them).
pub struct ProjectFile {
    pub key: &'static str,
    /// Path relative to the project root, forward slashes.
    pub rel: &'static str,
    pub strategy: Strategy,
    pub clients: &'static [&'static str],
}

pub const PROJECT_FILES: &[ProjectFile] = &[
    ProjectFile {
        key: "agents-md",
        rel: "AGENTS.md",
        strategy: Strategy::SentinelBlock,
        clients: &[
            "codex",
            "cursor",
            "github-copilot-cli",
            "kiro",
            "roo-code",
            "cline",
            "windsurf",
            "devin-cli",
            "goose",
            "pi",
            "omp",
        ],
    },
    ProjectFile {
        key: "gemini-md",
        rel: "GEMINI.md",
        strategy: Strategy::SentinelBlock,
        clients: &["gemini-cli", "antigravity"],
    },
    ProjectFile {
        key: "claude-rules",
        rel: ".claude/rules/toolport-rules.md",
        strategy: Strategy::OwnedFile,
        clients: &["claude-code", "vscode"],
    },
];

fn project_file(key: &str) -> Option<&'static ProjectFile> {
    PROJECT_FILES.iter().find(|f| f.key == key)
}

fn project_target(root: &std::path::Path, file: &ProjectFile) -> Target {
    let mut path = root.to_path_buf();
    for seg in file.rel.split('/') {
        path.push(seg);
    }
    Target {
        path,
        strategy: file.strategy,
        scope: Scope::Personal,
        char_cap: None,
        blocked_if_present: None,
    }
}

/// One project file's row in the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileStatus {
    pub key: String,
    pub rel_path: String,
    pub path: String,
    /// Display names of the DETECTED clients that read this file in a project.
    pub clients: Vec<String>,
    pub enabled: bool,
    pub state: ApplyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk: Option<String>,
}

/// One registered project in the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatus {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_id: Option<String>,
    /// Only the files at least one detected client reads; nothing is offered for a client that
    /// is not on the machine.
    pub files: Vec<ProjectFileStatus>,
}

/// The files offered for any project on this machine: those read by at least one installed
/// client, with the installed clients' display names.
fn offered_project_files(installed: &[ClientTarget]) -> Vec<(&'static ProjectFile, Vec<String>)> {
    PROJECT_FILES
        .iter()
        .filter_map(|f| {
            let names: Vec<String> = installed
                .iter()
                .filter(|c| f.clients.contains(&c.id.as_str()))
                .map(|c| c.name.clone())
                .collect();
            (!names.is_empty()).then_some((f, names))
        })
        .collect()
}

fn project_statuses(
    reg: &crate::registry::Registry,
    installed: &[ClientTarget],
) -> Vec<ProjectStatus> {
    let offered = offered_project_files(installed);
    reg.rules_projects
        .iter()
        .map(|p| {
            let set = p
                .set_id
                .as_deref()
                .and_then(|id| reg.rule_sets.iter().find(|s| s.id == id));
            let root = std::path::Path::new(&p.path);
            let files = offered
                .iter()
                .map(|(f, names)| {
                    let target = project_target(root, f);
                    let (state, on_disk) = match set {
                        None => {
                            if instructions::is_present(&target.path, Scope::Personal) {
                                (ApplyState::Stale, None)
                            } else {
                                (ApplyState::Applied, None)
                            }
                        }
                        Some(s) => {
                            match instructions::current_state(
                                &target, &s.id, s.revision, &s.content,
                            ) {
                                ApplyState::Stale => match instructions::drifted_body(
                                    &target, &s.id, s.revision, &s.content,
                                ) {
                                    Some(body) => (ApplyState::Drifted, Some(body)),
                                    None => (ApplyState::Stale, None),
                                },
                                other => (other, None),
                            }
                        }
                    };
                    ProjectFileStatus {
                        key: f.key.to_string(),
                        rel_path: f.rel.to_string(),
                        path: target.path.to_string_lossy().to_string(),
                        clients: names.clone(),
                        enabled: p.files.get(f.key).copied().unwrap_or(false),
                        state,
                        on_disk,
                    }
                })
                .collect();
            ProjectStatus {
                id: p.id.clone(),
                path: p.path.clone(),
                name: p.name.clone(),
                set_id: p.set_id.clone(),
                files,
            }
        })
        .collect()
}

/// Register a folder. Writes nothing: the project appears with every file off and no set.
pub fn project_add(path: &str) -> Result<RulesView, String> {
    let root = std::path::Path::new(path);
    if !root.is_absolute() {
        return Err("Choose a folder by its full path.".to_string());
    }
    if !root.is_dir() {
        return Err(format!("{path} is not a folder."));
    }
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("project")
        .to_string();
    crate::registry::update(|reg| {
        if reg.rules_projects.iter().any(|p| p.path == path) {
            return Err("That folder is already registered.".to_string());
        }
        let ids: Vec<String> = reg.rules_projects.iter().map(|p| p.id.clone()).collect();
        let id = crate::registry::unique_id(&crate::registry::slugify(&name), &ids);
        reg.rules_projects.push(RulesProject {
            id,
            path: path.to_string(),
            name,
            set_id: None,
            files: std::collections::HashMap::new(),
            targets: Vec::new(),
        });
        Ok(())
    })?;
    view()
}

/// Remove what `targets` holds of ours except the paths in `keep`. Returns the paths that could
/// NOT be cleaned (and so must stay on record).
fn clean_project_targets(targets: &[String], keep: &[String]) -> Vec<String> {
    let mut uncleaned = Vec::new();
    for t in targets {
        if keep.contains(t) {
            continue;
        }
        if !instructions::remove_recorded(std::path::Path::new(t), Scope::Personal) {
            uncleaned.push(t.clone());
        }
    }
    uncleaned
}

/// Unregister a folder, removing what Toolport wrote in it. A path that cannot be cleaned keeps
/// the project registered with that path on record, and says so, so a retry can finish the job
/// rather than stranding a block nothing will ever look for again.
pub fn project_remove(id: &str) -> Result<RulesView, String> {
    let Some(project) = crate::registry::load()?
        .rules_projects
        .into_iter()
        .find(|p| p.id == id)
    else {
        return Err("That project is no longer registered.".to_string());
    };
    let uncleaned = clean_project_targets(&project.targets, &[]);
    crate::registry::update(|reg| {
        if uncleaned.is_empty() {
            reg.rules_projects.retain(|p| p.id != id);
        } else if let Some(p) = reg.rules_projects.iter_mut().find(|p| p.id == id) {
            p.targets = uncleaned.clone();
        }
        Ok(())
    })?;
    if !uncleaned.is_empty() {
        return Err(format!(
            "Could not remove Toolport's block from {}. The project stays registered so you can try again.",
            uncleaned.join(", ")
        ));
    }
    view()
}

/// Pick (or clear) the set a project applies. Writes nothing; the files read Stale until Apply.
pub fn project_set_set(id: &str, set_id: Option<&str>) -> Result<RulesView, String> {
    crate::registry::update(|reg| {
        if let Some(sid) = set_id {
            if !reg.rule_sets.iter().any(|s| s.id == sid) {
                return Err("That rule set no longer exists.".to_string());
            }
        }
        let Some(p) = reg.rules_projects.iter_mut().find(|p| p.id == id) else {
            return Err("That project is no longer registered.".to_string());
        };
        p.set_id = set_id.map(str::to_string);
        Ok(())
    })?;
    view()
}

/// Switch one project file on (writes nothing until Apply) or off (removes that file's block or
/// owned file now, since leaving Toolport's text in a repo the user just said no to would be the
/// wrong default).
pub fn project_set_file_enabled(id: &str, key: &str, enabled: bool) -> Result<RulesView, String> {
    let Some(file) = project_file(key) else {
        return Err(format!("Unknown project file {key}."));
    };
    let project = crate::registry::load()?
        .rules_projects
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "That project is no longer registered.".to_string())?;
    let path = project_target(std::path::Path::new(&project.path), file)
        .path
        .to_string_lossy()
        .to_string();
    let mut still_recorded = project.targets.clone();
    if !enabled && project.targets.contains(&path) {
        if instructions::remove_recorded(std::path::Path::new(&path), Scope::Personal) {
            still_recorded.retain(|t| t != &path);
        } else {
            // Switching off means "Toolport's block is gone from this file". If it is not,
            // say so and leave the file switched ON, so the row keeps showing the real state
            // and the user can retry; an unchecked box over a block still in the repo would
            // be the lie project_remove and delete_set already refuse to tell.
            return Err(format!(
                "Could not remove Toolport's block from {path}. The file stays switched on so you can try again."
            ));
        }
    }
    crate::registry::update(|reg| {
        let Some(p) = reg.rules_projects.iter_mut().find(|p| p.id == id) else {
            return Err("That project is no longer registered.".to_string());
        };
        p.files.insert(key.to_string(), enabled);
        p.targets = still_recorded.clone();
        Ok(())
    })?;
    view()
}

/// The explicit Apply for one project. This is the ONLY path that writes a project file;
/// startup never does.
pub fn project_apply(id: &str) -> Result<RulesView, String> {
    apply_project_with(id, &installed_targets())
}

/// Write every switched-on file from the project's set, clean up anything recorded that is no
/// longer wanted, and record what is now ours. A file the user edited on disk since the last
/// apply is rewritten here: Apply is the user asking for exactly that, and the row showed
/// "Edited on disk" first.
fn apply_project_with(id: &str, installed: &[ClientTarget]) -> Result<RulesView, String> {
    let reg = crate::registry::load()?;
    let Some(project) = reg.rules_projects.iter().find(|p| p.id == id) else {
        return Err("That project is no longer registered.".to_string());
    };
    let Some(set) = project
        .set_id
        .as_deref()
        .and_then(|sid| reg.rule_sets.iter().find(|s| s.id == sid))
    else {
        return Err("Pick a rule set for this project first.".to_string());
    };
    let root = std::path::Path::new(&project.path);
    if !root.is_dir() {
        return Err(format!("{} is not a folder any more.", project.path));
    }
    let desired: Vec<(String, Target)> = offered_project_files(installed)
        .into_iter()
        .filter(|(f, _)| project.files.get(f.key).copied().unwrap_or(false))
        .map(|(f, _)| {
            let t = project_target(root, f);
            (t.path.to_string_lossy().to_string(), t)
        })
        .collect();
    let mut written = Vec::new();
    let mut refused = Vec::new();
    for (key, target) in &desired {
        match instructions::write_target(target, &set.id, set.revision, &set.content) {
            ApplyState::Applied => written.push(key.clone()),
            state => refused.push((key.clone(), state)),
        }
    }
    let desired_paths: Vec<String> = desired.iter().map(|(k, _)| k.clone()).collect();
    let uncleaned = clean_project_targets(&project.targets, &desired_paths);
    // What is ours on disk now: written, still-desired previous paths (a refused rewrite keeps
    // last-good, exactly as the global apply does), and failed cleanups.
    let mut owned = written;
    for old in project.targets.iter().chain(uncleaned.iter()) {
        let still_wanted = desired_paths.contains(old) || uncleaned.contains(old);
        if still_wanted && !owned.contains(old) {
            owned.push(old.clone());
        }
    }
    crate::registry::update(|reg| {
        if let Some(p) = reg.rules_projects.iter_mut().find(|p| p.id == id) {
            p.targets = owned.clone();
        }
        Ok(())
    })?;
    if let Some((path, state)) = refused.first() {
        // Report, do not hide: the view shows the state, but the button was pressed and it
        // did not do what it said.
        let why = match state {
            ApplyState::Error => "it could not be read or written",
            ApplyState::TooLong => "it would exceed the client's limit",
            ApplyState::BlockedOverride => "a local override file shadows it",
            _ => "it was refused",
        };
        return Err(format!(
            "{path} was not written: {why}. Everything else was applied."
        ));
    }
    view_with(installed)
}

/// Dry run of one project file, from the project's set. `None` when the project has no set.
pub fn project_preview(id: &str, key: &str) -> Result<Option<RulesPreview>, String> {
    let Some(file) = project_file(key) else {
        return Err(format!("Unknown project file {key}."));
    };
    let reg = crate::registry::load()?;
    let Some(project) = reg.rules_projects.iter().find(|p| p.id == id) else {
        return Err("That project is no longer registered.".to_string());
    };
    let Some(set) = project
        .set_id
        .as_deref()
        .and_then(|sid| reg.rule_sets.iter().find(|s| s.id == sid))
    else {
        return Ok(None);
    };
    let target = project_target(std::path::Path::new(&project.path), file);
    preview_target(key, &target, set, &set.content).map(Some)
}

/// Re-assert the active set at startup. Cheap in the common case: [`instructions::write_target`]
/// no-ops when the on-disk block already matches, so a normal launch touches no files. Exists so
/// a client updated (or reinstalled) since the last apply picks the rules back up without the
/// user opening the Rules tab.
/// A rules file already on this machine that a new set can start from (SBS-1035).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub client_id: String,
    pub client_name: String,
    pub path: String,
    pub bytes: u64,
}

/// What importing a file yields: the user's own text, with anything Toolport wrote removed.
/// Nothing is saved and the source file is not touched; the caller seeds an editor with it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportedRules {
    pub path: String,
    /// A suggested set name: "Imported from <client>" when the file is a client's, else from
    /// the file name.
    pub name: String,
    pub content: String,
    /// True when a Toolport block or owned-file header was stripped on the way in.
    pub stripped_ours: bool,
}

/// Largest file `import_file` will read. A rules file is prose; anything past this is not one.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;

/// Rules files the detected clients already have, for "Start from a file". Read-only.
pub fn import_candidates() -> Vec<ImportCandidate> {
    import_candidates_for(&installed_targets(), dirs::home_dir().as_deref())
}

/// The per-client candidate list, over an explicit target set so it can be tested without a
/// machine scan. Three sources, all the user's own writing:
///
/// * a sentinel-block target IS the user's file (`~/.codex/AGENTS.md`, `~/.gemini/GEMINI.md`,
///   `.goosehints`): everything outside Toolport's block is theirs;
/// * an owned-file target is entirely ours, so the candidates are the OTHER `.md` files in
///   that rules directory (`~/.roo/rules/*.md`, Kiro steering, Cline rules);
/// * Claude Code keeps its global memory in `~/.claude/CLAUDE.md`, beside the rules directory
///   rather than in it, so that file is added for the clients that resolve there.
///
/// Deduplicated by path (Gemini CLI and Antigravity name one file), absent or empty files
/// skipped, ordered by client name then path.
fn import_candidates_for(
    installed: &[ClientTarget],
    home: Option<&std::path::Path>,
) -> Vec<ImportCandidate> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |client: &ClientTarget, path: std::path::PathBuf| {
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if !meta.is_file() || meta.len() == 0 {
            return;
        }
        if !seen.insert(path.clone()) {
            return;
        }
        out.push(ImportCandidate {
            client_id: client.id.clone(),
            client_name: client.name.clone(),
            path: path.to_string_lossy().to_string(),
            bytes: meta.len(),
        });
    };
    for client in installed {
        let Some(target) = &client.target else {
            continue;
        };
        match target.strategy {
            Strategy::SentinelBlock => push(client, target.path.clone()),
            Strategy::OwnedFile => {
                if let Some(dir) = target.path.parent() {
                    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                        .map(|rd| {
                            rd.flatten()
                                .map(|e| e.path())
                                .filter(|p| p.extension().is_some_and(|e| e == "md"))
                                .filter(|p| {
                                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                                    !instructions::ALL_SCOPES
                                        .iter()
                                        .any(|s| s.owned_file_name() == name)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    files.sort();
                    for f in files {
                        push(client, f);
                    }
                }
                if matches!(client.id.as_str(), "claude-code" | "vscode") {
                    if let Some(home) = home {
                        push(client, home.join(".claude").join("CLAUDE.md"));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.client_name.cmp(&b.client_name).then(a.path.cmp(&b.path)));
    out
}

/// Read `path` and return the user's own text from it, with any Toolport block (either
/// scope) or owned-file header removed. Never writes: the file is read once and left exactly
/// as it was, and nothing is saved - the caller puts the text in an editor for the user to
/// review and save. Refuses a file that is too large to be rules, one that is not UTF-8, and
/// one whose remainder still carries a marker lookalike (which `save_set` would refuse anyway;
/// better to say so before the editor fills with it).
pub fn import_file(path: &str, client_name: Option<&str>) -> Result<ImportedRules, String> {
    use std::io::Read;
    let p = std::path::Path::new(path);
    // One handle for the check and the read, so a file swapped between the two cannot get a
    // larger body past the size limit; and the read itself is bounded regardless.
    let mut file = std::fs::File::open(p).map_err(|e| format!("Could not read {path}: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("Could not read {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file."));
    }
    let mut raw = Vec::new();
    file.by_ref()
        .take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| format!("Could not read {path}: {e}"))?;
    if raw.len() as u64 > MAX_IMPORT_BYTES {
        return Err(format!(
            "{path} is larger than {MAX_IMPORT_BYTES} bytes, which is too large to be a rules file."
        ));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| format!("{path} is not UTF-8 text, so it cannot be imported as rules."))?;
    let (content, stripped_ours) = strip_toolport_artifacts(&text);
    if instructions::content_carries_a_marker(&content) {
        return Err(format!(
            "{path} contains text that looks like Toolport's own marker comments outside any \
             block Toolport wrote. Remove those lines from the file (or from the text after \
             pasting it in) and try again."
        ));
    }
    let name = match client_name.map(str::trim).filter(|c| !c.is_empty()) {
        Some(client) => format!("Imported from {client}"),
        None => format!(
            "Imported from {}",
            p.file_name().and_then(|n| n.to_str()).unwrap_or("file")
        ),
    };
    Ok(ImportedRules {
        path: path.to_string(),
        name,
        content,
        stripped_ours,
    })
}

/// Everything in `text` that is the user's: an owned file of ours is entirely ours (empty
/// remainder); otherwise every block of either scope is cut out in place. Surrounding blank
/// lines are trimmed because this seeds an editor, not a file.
fn strip_toolport_artifacts(text: &str) -> (String, bool) {
    // An owned file of ours opens with a complete one-line header comment. Only that exact
    // shape counts; a line that merely starts like the header (a lookalike, or a truncated
    // copy) is the user's text and is kept rather than silently dropped as "ours".
    let first_line = text.trim_start().lines().next().unwrap_or("");
    if instructions::ALL_SCOPES.iter().any(|s| {
        first_line.starts_with(s.owned_header_prefix()) && first_line.trim_end().ends_with("-->")
    }) {
        return (String::new(), true);
    }
    let mut out = text.to_string();
    let mut stripped = false;
    for scope in instructions::ALL_SCOPES {
        // A file can hold more than one block of a scope only if someone hand-copied it; cut
        // them all, bounded by the fact that each removal shortens the text.
        while let Some(rest) = instructions::remove_block(&out, scope) {
            out = rest;
            stripped = true;
        }
    }
    (out.trim().to_string(), stripped)
}

pub fn apply_on_startup() {
    match crate::registry::load_resolved_with_source() {
        Ok((_, source)) if !source.is_authoritative() => {
            eprintln!(
                "toolport: could not inspect personal rules authoritatively at startup ({source:?})"
            );
            return;
        }
        Ok((reg, _)) if reg.active_rule_set().is_none() && reg.rules_targets.is_empty() => {
            return; // nothing configured and nothing written: skip the client scan entirely
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("toolport: could not inspect personal rules at startup: {error}");
            return;
        }
    }
    if let Err(error) = apply() {
        eprintln!("toolport: could not apply personal rules at startup: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique scratch dir per test; best-effort cleanup on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "toolport-rules-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn client(id: &str, target: Option<Target>) -> ClientTarget {
        ClientTarget {
            id: id.to_string(),
            name: id.to_string(),
            target,
        }
    }

    fn sentinel(path: PathBuf) -> Target {
        Target {
            path,
            strategy: Strategy::SentinelBlock,
            scope: Scope::Personal,
            char_cap: None,
            blocked_if_present: None,
        }
    }

    fn owned(path: PathBuf) -> Target {
        Target {
            path,
            strategy: Strategy::OwnedFile,
            scope: Scope::Personal,
            char_cap: None,
            blocked_if_present: None,
        }
    }

    fn set(id: &str, revision: i64, content: &str) -> RuleSet {
        RuleSet {
            id: id.to_string(),
            name: id.to_string(),
            content: content.to_string(),
            revision,
        }
    }

    // ---- project-level rules (SBS-1037) ----

    fn registered(s: &Scratch, name: &str) -> String {
        let root = s.path(name);
        std::fs::create_dir_all(&root).unwrap();
        root.to_string_lossy().to_string()
    }

    #[test]
    fn project_files_are_offered_only_for_detected_clients_and_register_writes_nothing() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));
        let root = registered(&s, "repo");
        // Codex and Claude Code are installed; no Gemini. Cursor has no GLOBAL target but still
        // reads the project AGENTS.md, so it must appear under that file.
        let installed = vec![
            client("codex", Some(sentinel(s.path("AGENTS.md")))),
            client(
                "claude-code",
                Some(owned(s.path("rules").join("toolport-rules.md"))),
            ),
            client("cursor", None),
        ];
        crate::registry::update(|reg| reg.upsert_rule_set(None, "Work", "Be brief.").map(|_| ()))
            .unwrap();
        project_add(&root).unwrap();
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.rules_projects.len(), 1);
        assert_eq!(reg.rules_projects[0].name, "repo");
        assert!(reg.rules_projects[0].set_id.is_none());
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "registering writes nothing"
        );

        let statuses = project_statuses(&reg, &installed);
        let keys: Vec<(&str, Vec<String>)> = statuses[0]
            .files
            .iter()
            .map(|f| (f.key.as_str(), f.clients.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("agents-md", vec!["codex".to_string(), "cursor".to_string()]),
                ("claude-rules", vec!["claude-code".to_string()]),
            ],
            "GEMINI.md is not offered with no Gemini client; Cursor rides on AGENTS.md"
        );
        assert!(statuses[0].files.iter().all(|f| !f.enabled));
        // Duplicate and non-folder registrations are refused.
        assert!(project_add(&root).is_err());
        assert!(project_add(&s.path("nope").to_string_lossy()).is_err());
        assert!(project_add("relative/path").is_err());
    }

    #[test]
    fn project_apply_writes_only_switched_on_files_and_remove_cleans_them() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));
        let root = registered(&s, "repo");
        let root_path = std::path::PathBuf::from(&root);
        const THEIRS: &str = "# Our conventions\n\nUse pnpm.\n";
        // The user already has an AGENTS.md in the repo; it must survive byte-for-byte.
        std::fs::write(root_path.join("AGENTS.md"), THEIRS).unwrap();
        let installed = vec![
            client("codex", Some(sentinel(s.path("AGENTS.md")))),
            client(
                "claude-code",
                Some(owned(s.path("rules").join("toolport-rules.md"))),
            ),
            client("gemini-cli", Some(sentinel(s.path("GEMINI.md")))),
        ];
        let set_id = crate::registry::update(|reg| reg.upsert_rule_set(None, "Work", "Be brief."))
            .unwrap()
            .1;
        project_add(&root).unwrap();
        let pid = crate::registry::load().unwrap().rules_projects[0]
            .id
            .clone();

        // No set yet: Apply refuses rather than writing nothing silently.
        assert!(apply_project_with(&pid, &installed)
            .unwrap_err()
            .contains("Pick a rule set"));
        project_set_set(&pid, Some(&set_id)).unwrap();
        assert!(project_set_set(&pid, Some("ghost")).is_err());
        // Picking a set writes nothing either.
        assert_eq!(
            std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap(),
            THEIRS
        );

        // Switch AGENTS.md on (still nothing written) and apply: the block is appended, the
        // user's text is untouched, the other files are not created.
        project_set_file_enabled(&pid, "agents-md", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap(),
            THEIRS
        );
        let view = apply_project_with(&pid, &installed).unwrap();
        let agents = std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap();
        assert!(agents.starts_with(THEIRS), "{agents}");
        assert!(agents.contains("Be brief."));
        assert!(!root_path.join("GEMINI.md").exists());
        assert!(!root_path.join(".claude").exists());
        let proj = view.projects.iter().find(|p| p.id == pid).unwrap();
        let file = |k: &str| proj.files.iter().find(|f| f.key == k).unwrap().clone();
        assert_eq!(file("agents-md").state, ApplyState::Applied);
        assert!(file("agents-md").enabled);
        assert_eq!(
            file("claude-rules").state,
            ApplyState::Stale,
            "off: not written"
        );
        assert_eq!(file("gemini-md").state, ApplyState::Stale);
        let reg = crate::registry::load().unwrap();
        assert_eq!(
            reg.rules_projects[0].targets,
            vec![root_path.join("AGENTS.md").to_string_lossy().to_string()]
        );

        // Switching a file OFF removes the block now; the user's text is exactly as before.
        project_set_file_enabled(&pid, "agents-md", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap(),
            THEIRS
        );
        assert!(crate::registry::load().unwrap().rules_projects[0]
            .targets
            .is_empty());

        // A toggle-off whose cleanup fails says so and leaves the file switched on and on
        // record, rather than showing an unchecked box over a block still in the repo.
        project_set_file_enabled(&pid, "agents-md", true).unwrap();
        apply_project_with(&pid, &installed).unwrap();
        std::fs::write(
            root_path.join("AGENTS.md"),
            format!(
                "{}\nunterminated",
                crate::instructions::PERSONAL_SENTINEL_START_PREFIX
            ),
        )
        .unwrap();
        let err = project_set_file_enabled(&pid, "agents-md", false).unwrap_err();
        assert!(err.contains("stays switched on"), "{err}");
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.rules_projects[0].files.get("agents-md"), Some(&true));
        assert_eq!(reg.rules_projects[0].targets.len(), 1);
        // Repair the file; the retry succeeds.
        std::fs::write(root_path.join("AGENTS.md"), THEIRS).unwrap();
        apply_project_with(&pid, &installed).unwrap();
        project_set_file_enabled(&pid, "agents-md", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap(),
            THEIRS
        );

        // An owned file in a nested dir is created on apply and deleted whole on remove.
        project_set_file_enabled(&pid, "claude-rules", true).unwrap();
        apply_project_with(&pid, &installed).unwrap();
        let owned_path = root_path
            .join(".claude")
            .join("rules")
            .join("toolport-rules.md");
        assert!(owned_path.exists());
        project_remove(&pid).unwrap();
        assert!(!owned_path.exists(), "remove cleans what was written");
        assert_eq!(
            std::fs::read_to_string(root_path.join("AGENTS.md")).unwrap(),
            THEIRS
        );
        assert!(crate::registry::load().unwrap().rules_projects.is_empty());
    }

    #[test]
    fn deleting_a_set_a_project_used_cleans_that_project_and_startup_never_writes_projects() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));
        let root = registered(&s, "repo");
        let root_path = std::path::PathBuf::from(&root);
        let installed = vec![client("codex", Some(sentinel(s.path("AGENTS.md"))))];
        let set_id = crate::registry::update(|reg| reg.upsert_rule_set(None, "Work", "Be brief."))
            .unwrap()
            .1;
        project_add(&root).unwrap();
        let pid = crate::registry::load().unwrap().rules_projects[0]
            .id
            .clone();
        project_set_set(&pid, Some(&set_id)).unwrap();
        project_set_file_enabled(&pid, "agents-md", true).unwrap();

        // Startup reconciles GLOBAL rules only. A project with a set and a file switched on is
        // left exactly as it is until its own Apply.
        apply_on_startup();
        assert!(
            !root_path.join("AGENTS.md").exists(),
            "startup must never write a project file"
        );

        apply_project_with(&pid, &installed).unwrap();
        assert!(root_path.join("AGENTS.md").exists());

        // Deleting the set the project applied removes its files and clears the pointer; the
        // project itself stays so the user sees why it is empty.
        delete_set(&set_id).unwrap();
        assert!(
            !root_path.join("AGENTS.md").exists(),
            "a file Toolport created alone is gone"
        );
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.rules_projects.len(), 1);
        assert!(reg.rules_projects[0].set_id.is_none());
        assert!(reg.rules_projects[0].targets.is_empty());

        // A file whose block cannot be cleaned (an unterminated marker) stays on the project's
        // record when its set is deleted, so a later remove can still try.
        let set2 = crate::registry::update(|reg| reg.upsert_rule_set(None, "Two", "Two."))
            .unwrap()
            .1;
        project_set_set(&pid, Some(&set2)).unwrap();
        apply_project_with(&pid, &installed).unwrap();
        let agents = root_path.join("AGENTS.md");
        std::fs::write(
            &agents,
            format!(
                "{}\nunterminated",
                crate::instructions::PERSONAL_SENTINEL_START_PREFIX
            ),
        )
        .unwrap();
        delete_set(&set2).unwrap();
        let reg = crate::registry::load().unwrap();
        assert!(reg.rules_projects[0].set_id.is_none());
        assert_eq!(
            reg.rules_projects[0].targets,
            vec![agents.to_string_lossy().to_string()],
            "the uncleanable path stays on record"
        );
        assert!(agents.exists());
    }

    // ---- import an existing file as a seed (SBS-1035) ----

    #[test]
    fn import_strips_toolport_blocks_and_leaves_the_source_untouched() {
        let s = Scratch::new();
        let path = s.path("AGENTS.md");
        let mine = "# My rules\n\nBe terse.\n";
        std::fs::write(&path, mine).unwrap();
        // Both a team block and a personal block land in the same file; both are ours.
        assert_eq!(
            instructions::write_target(&sentinel(path.clone()), "set1", 1, "personal text"),
            ApplyState::Applied
        );
        let team = Target {
            scope: Scope::Team,
            ..sentinel(path.clone())
        };
        assert_eq!(
            instructions::write_target(&team, "team1", 1, "org text"),
            ApplyState::Applied
        );
        let before = std::fs::read(&path).unwrap();

        let imported = import_file(path.to_str().unwrap(), Some("Codex")).expect("imports");
        assert_eq!(imported.content, "# My rules\n\nBe terse.");
        assert!(imported.stripped_ours);
        assert_eq!(imported.name, "Imported from Codex");
        assert_eq!(std::fs::read(&path).unwrap(), before, "import is read-only");

        // Without a client name the file name names the set.
        let plain = import_file(path.to_str().unwrap(), None).unwrap();
        assert_eq!(plain.name, "Imported from AGENTS.md");
    }

    #[test]
    fn import_of_a_file_that_is_only_ours_yields_an_empty_seed() {
        let s = Scratch::new();
        let path = s.path("toolport-rules.md");
        assert_eq!(
            instructions::write_target(&owned(path.clone()), "set1", 1, "ours"),
            ApplyState::Applied
        );
        let imported = import_file(path.to_str().unwrap(), Some("Claude Code")).unwrap();
        assert_eq!(imported.content, "");
        assert!(
            imported.stripped_ours,
            "an owned file is entirely Toolport's"
        );

        // A line that only LOOKS like the header is the user's text, not ours.
        let lookalike = s.path("lookalike.md");
        std::fs::write(
            &lookalike,
            format!(
                "{} but not really\nrules\n",
                Scope::Team.owned_header_prefix()
            ),
        )
        .unwrap();
        let imported = import_file(lookalike.to_str().unwrap(), None).unwrap();
        assert!(
            imported.content.contains("but not really"),
            "{}",
            imported.content
        );
        assert!(!imported.stripped_ours);

        // A file with nothing of ours in it imports verbatim (trimmed) and says so.
        let theirs = s.path("CLAUDE.md");
        std::fs::write(&theirs, "\n\nAlways run tests.\n\n").unwrap();
        let imported = import_file(theirs.to_str().unwrap(), None).unwrap();
        assert_eq!(imported.content, "Always run tests.");
        assert!(!imported.stripped_ours);
    }

    #[test]
    fn import_refuses_marker_lookalikes_oversize_and_non_utf8() {
        let s = Scratch::new();
        let lookalike = s.path("weird.md");
        std::fs::write(
            &lookalike,
            format!("rules\n{} stray\n", Scope::Personal.sentinel_start_prefix()),
        )
        .unwrap();
        let err = import_file(lookalike.to_str().unwrap(), None).unwrap_err();
        assert!(err.contains("marker"), "{err}");

        let big = s.path("big.md");
        std::fs::write(&big, vec![b'a'; (MAX_IMPORT_BYTES + 1) as usize]).unwrap();
        let err = import_file(big.to_str().unwrap(), None).unwrap_err();
        assert!(err.contains("too large"), "{err}");

        let binary = s.path("bin.md");
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x41]).unwrap();
        let err = import_file(binary.to_str().unwrap(), None).unwrap_err();
        assert!(err.contains("UTF-8"), "{err}");

        let missing = s.path("nope.md");
        assert!(import_file(missing.to_str().unwrap(), None).is_err());
    }

    #[test]
    fn import_candidates_are_the_users_own_files_not_ours() {
        let s = Scratch::new();
        let home = s.path("home");
        // Claude Code: an owned-file target; the rules dir holds our file (excluded), a sibling
        // the user wrote (included), and a non-markdown file (excluded). Plus ~/.claude/CLAUDE.md.
        let rules_dir = home.join(".claude").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("toolport-rules.md"), "ours").unwrap();
        std::fs::write(rules_dir.join("toolport-team-rules.md"), "org").unwrap();
        std::fs::write(rules_dir.join("style.md"), "mine").unwrap();
        std::fs::write(rules_dir.join("notes.txt"), "not rules").unwrap();
        std::fs::write(home.join(".claude").join("CLAUDE.md"), "memory").unwrap();
        // Codex: a sentinel target that exists; Gemini + Antigravity share one file; a
        // sentinel target that does not exist yet; an empty file; an unsupported client.
        let agents = s.path("AGENTS.md");
        std::fs::write(&agents, "codex rules").unwrap();
        let gemini = s.path("GEMINI.md");
        std::fs::write(&gemini, "gemini rules").unwrap();
        let empty = s.path("empty.md");
        std::fs::write(&empty, "").unwrap();
        let installed = vec![
            client(
                "claude-code",
                Some(owned(rules_dir.join("toolport-rules.md"))),
            ),
            client("codex", Some(sentinel(agents.clone()))),
            client("gemini-cli", Some(sentinel(gemini.clone()))),
            client("antigravity", Some(sentinel(gemini.clone()))),
            client("goose", Some(sentinel(s.path("missing/.goosehints")))),
            client("pi", Some(sentinel(empty))),
            client("cursor", None),
        ];
        let found = import_candidates_for(&installed, Some(&home));
        let paths: Vec<(&str, String)> = found
            .iter()
            .map(|c| (c.client_id.as_str(), c.path.clone()))
            .collect();
        assert_eq!(
            paths,
            vec![
                (
                    "claude-code",
                    home.join(".claude")
                        .join("CLAUDE.md")
                        .to_string_lossy()
                        .to_string()
                ),
                (
                    "claude-code",
                    rules_dir.join("style.md").to_string_lossy().to_string()
                ),
                ("codex", agents.to_string_lossy().to_string()),
                ("gemini-cli", gemini.to_string_lossy().to_string()),
            ],
            "ours excluded, non-md excluded, shared file once, missing and empty skipped"
        );
        assert!(found.iter().all(|c| c.bytes > 0));
    }

    // ---- registry-level set management (no filesystem) ----

    #[test]
    fn a_new_set_becomes_active_when_nothing_else_is() {
        let mut reg = crate::registry::Registry::default();
        let id = reg
            .upsert_rule_set(None, "Work", "Always run tests.")
            .expect("create");
        assert_eq!(reg.active_rule_set_id.as_deref(), Some(id.as_str()));
        assert_eq!(reg.active_rule_set().map(|s| s.revision), Some(1));

        // A SECOND set does not steal the selection.
        let other = reg
            .upsert_rule_set(None, "Personal", "Be brief.")
            .expect("create");
        assert_ne!(other, id, "ids must be unique");
        assert_eq!(reg.active_rule_set_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn revision_moves_on_content_change_only() {
        let mut reg = crate::registry::Registry::default();
        let id = reg.upsert_rule_set(None, "Work", "v1").expect("create");
        assert_eq!(reg.active_rule_set().unwrap().revision, 1);

        // A rename rides in the marker but is not a content change, so rewriting every client's
        // file for it would be pure churn.
        reg.upsert_rule_set(Some(&id), "Renamed", "v1")
            .expect("update");
        assert_eq!(reg.active_rule_set().unwrap().revision, 1);
        assert_eq!(reg.active_rule_set().unwrap().name, "Renamed");

        reg.upsert_rule_set(Some(&id), "Renamed", "v2")
            .expect("update");
        assert_eq!(reg.active_rule_set().unwrap().revision, 2);
    }

    #[test]
    fn saving_against_an_unknown_id_errors_rather_than_duplicating() {
        let mut reg = crate::registry::Registry::default();
        let id = reg.upsert_rule_set(None, "Work", "v1").expect("create");
        let err = reg
            .upsert_rule_set(Some("deleted-in-another-window"), "Work", "v2")
            .expect_err("an unknown id must not create");
        assert!(
            err.contains("no longer exists"),
            "unexpected message: {err}"
        );
        assert_eq!(reg.rule_sets.len(), 1, "must not grow a duplicate set");
        assert_eq!(reg.active_rule_set().unwrap().id, id);
        assert_eq!(
            reg.active_rule_set().unwrap().content,
            "v1",
            "the real set must be untouched"
        );
    }

    #[test]
    fn removing_the_active_set_clears_the_selection() {
        let mut reg = crate::registry::Registry::default();
        let a = reg.upsert_rule_set(None, "A", "a").expect("create");
        let b = reg.upsert_rule_set(None, "B", "b").expect("create");
        reg.remove_rule_set(&a);
        assert_eq!(
            reg.active_rule_set_id, None,
            "must not silently promote another set's rules onto the user's machine"
        );
        assert_eq!(reg.rule_sets.len(), 1);

        reg.set_active_rule_set(Some(&b));
        assert_eq!(reg.active_rule_set_id.as_deref(), Some(b.as_str()));
        reg.set_active_rule_set(Some("nope"));
        assert_eq!(
            reg.active_rule_set_id, None,
            "unknown id clears, never panics"
        );
    }

    #[test]
    fn a_client_is_opted_out_until_the_user_says_otherwise() {
        let mut reg = crate::registry::Registry::default();
        assert!(
            !reg.rules_client_enabled("claude-code"),
            "absent must mean off"
        );
        reg.set_rules_client_enabled("claude-code", true);
        assert!(reg.rules_client_enabled("claude-code"));
        reg.set_rules_client_enabled("claude-code", false);
        assert!(!reg.rules_client_enabled("claude-code"));
        assert!(
            reg.rules_clients.contains_key("claude-code"),
            "an explicit off is stored, so the UI can tell it from never-seen"
        );
    }

    // ---- target selection ----

    #[test]
    fn only_opted_in_clients_are_written_and_shared_paths_collapse() {
        let s = Scratch::new();
        let shared = s.path("GEMINI.md");
        let installed = vec![
            client("gemini-cli", Some(sentinel(shared.clone()))),
            client("antigravity", Some(sentinel(shared.clone()))),
            client("codex", Some(sentinel(s.path("AGENTS.md")))),
            client("cursor", None),
        ];
        let mut reg = crate::registry::Registry::default();

        assert!(
            enabled_targets(&reg, &installed).is_empty(),
            "nothing is written before the user opts a client in"
        );

        reg.set_rules_client_enabled("gemini-cli", true);
        reg.set_rules_client_enabled("antigravity", true);
        let targets = enabled_targets(&reg, &installed);
        assert_eq!(
            targets.len(),
            1,
            "Gemini and Antigravity share one file; it must be written once"
        );
        assert_eq!(targets[0].path, shared);
    }

    #[test]
    fn an_unsupported_client_is_reported_not_skipped() {
        let installed = vec![client("cursor", None)];
        let reg = crate::registry::Registry::default();
        let rows = status_from(&reg, &installed, Some(&set("s", 1, "c")));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, ApplyState::Unsupported);
        assert_eq!(rows[0].path, None);
        assert!(!rows[0].enabled);
    }

    /// With no active set, the desired end state is "nothing of ours on disk", so the row must
    /// report PRESENCE. Reporting Applied unconditionally told the user their rules were up to
    /// date while a block we failed to clean was still sitting in the file.
    #[test]
    fn with_no_active_set_the_row_reports_whether_our_block_is_gone() {
        let s = Scratch::new();
        let target = sentinel(s.path("AGENTS.md"));
        let installed = vec![client("codex", Some(target.clone()))];
        let reg = crate::registry::Registry::default();

        // Nothing on disk: the end state is reached.
        assert_eq!(
            status_from(&reg, &installed, None)[0].state,
            ApplyState::Applied
        );

        // Our block still there after a failed cleanup: NOT settled.
        instructions::write_target(&target, "work", 1, "Be brief.");
        assert_eq!(
            status_from(&reg, &installed, None)[0].state,
            ApplyState::Stale,
            "a leftover block must not read as up to date"
        );
    }

    // ---- write / status round trip, straight through the instructions engine ----

    #[test]
    fn a_shared_file_keeps_user_bytes_and_reports_applied() {
        let s = Scratch::new();
        let path = s.path("AGENTS.md");
        let user = "# Mine\nAlways run tests.\n";
        std::fs::write(&path, user).unwrap();
        let target = sentinel(path.clone());
        let rules = set("work", 3, "Be brief.");

        assert_eq!(
            instructions::write_target(&target, &rules.id, rules.revision, &rules.content),
            ApplyState::Applied
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(user), "user bytes preserved");
        assert!(after.contains("Be brief."));

        let installed = vec![client("codex", Some(target.clone()))];
        let mut reg = crate::registry::Registry::default();
        reg.set_rules_client_enabled("codex", true);
        assert_eq!(
            status_from(&reg, &installed, Some(&rules))[0].state,
            ApplyState::Applied
        );

        // A newer revision of the same set reads as Stale until it is applied.
        let bumped = set("work", 4, "Be brief.");
        assert_eq!(
            status_from(&reg, &installed, Some(&bumped))[0].state,
            ApplyState::Stale
        );
    }

    /// The three cases the SBS-821 acceptance criteria name, per strategy: a fresh file, a file
    /// that already carries our block, and a file with user content and no block.
    #[test]
    fn each_strategy_handles_fresh_existing_block_and_foreign_file() {
        let s = Scratch::new();
        let rules = set("work", 1, "Be brief.");

        // Fresh file.
        let fresh = sentinel(s.path("fresh.md"));
        assert_eq!(
            instructions::write_target(&fresh, &rules.id, rules.revision, &rules.content),
            ApplyState::Applied
        );
        assert!(fresh.path.exists());

        // Already carries our block: idempotent, byte-identical.
        let before = std::fs::read_to_string(&fresh.path).unwrap();
        assert_eq!(
            instructions::write_target(&fresh, &rules.id, rules.revision, &rules.content),
            ApplyState::Applied
        );
        assert_eq!(std::fs::read_to_string(&fresh.path).unwrap(), before);

        // User content, no block: appended to, never replaced.
        let foreign = sentinel(s.path("foreign.md"));
        let user = "# Mine\nkeep me\n";
        std::fs::write(&foreign.path, user).unwrap();
        assert_eq!(
            instructions::write_target(&foreign, &rules.id, rules.revision, &rules.content),
            ApplyState::Applied
        );
        assert!(std::fs::read_to_string(&foreign.path)
            .unwrap()
            .starts_with(user));

        // Owned files are ours whole, and a foreign file at the owned path is never deleted.
        let own = owned(s.path("rules").join(Scope::Personal.owned_file_name()));
        assert_eq!(
            instructions::write_target(&own, &rules.id, rules.revision, &rules.content),
            ApplyState::Applied
        );
        assert!(std::fs::read_to_string(&own.path)
            .unwrap()
            .starts_with(instructions::PERSONAL_OWNED_HEADER_PREFIX));
    }

    // ---- end-to-end apply, against a redirected registry ----
    //
    // These drive `apply_to` for real: it loads and writes the registry, so each holds the
    // process-global data-dir guard and points the registry at a scratch dir. The client targets
    // are synthetic scratch paths, so no real client file on the developer's machine is touched.

    /// Seed the (redirected) registry with one set and the given opted-in clients, then run
    /// `apply_to`. Callers must already hold the data-dir guard and override.
    fn seed_and_apply(content: &str, enabled: &[&str], installed: &[ClientTarget]) -> RulesView {
        crate::registry::update(|reg| {
            // First call creates (the id is not there yet), later calls update in place.
            if reg.rule_sets.iter().any(|s| s.id == "work") {
                reg.upsert_rule_set(Some("work"), "Work", content)?;
            } else {
                reg.upsert_rule_set(None, "Work", content)?;
            }
            for id in enabled {
                reg.set_rules_client_enabled(id, true);
            }
            Ok(())
        })
        .expect("seed the registry");
        apply_to(installed, ApplyMode::Reconcile).expect("apply")
    }

    #[test]
    fn apply_writes_opted_in_clients_and_records_what_it_wrote() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let base = s.path("data");
        let _data_dir = crate::registry::DataDirOverride::set(&base);

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let claude = client(
            "claude-code",
            Some(owned(
                s.path("rules").join(Scope::Personal.owned_file_name()),
            )),
        );
        let cursor = client("cursor", None);
        let installed = vec![codex.clone(), claude.clone(), cursor.clone()];

        // Only Codex is opted in.
        let view = seed_and_apply("Be brief.", &["codex"], &installed);

        let codex_path = codex.target.clone().unwrap().path;
        let claude_path = claude.target.clone().unwrap().path;
        assert!(codex_path.exists(), "opted-in client is written");
        assert!(!claude_path.exists(), "opted-out client is left alone");

        let reg = crate::registry::load().unwrap();
        assert_eq!(
            reg.rules_targets,
            vec![codex_path.to_string_lossy().to_string()],
            "only the written path is recorded"
        );

        let by_id = |id: &str| view.clients.iter().find(|c| c.id == id).unwrap().clone();
        assert_eq!(by_id("codex").state, ApplyState::Applied);
        assert!(by_id("codex").enabled);
        assert_eq!(by_id("claude-code").state, ApplyState::Stale);
        assert!(!by_id("claude-code").enabled);
        assert_eq!(by_id("cursor").state, ApplyState::Unsupported);
    }

    /// SBS-1036: a block the user edited on disk after Toolport wrote it is reported, not
    /// silently reverted. Only the explicit Overwrite puts the set's text back.
    #[test]
    fn a_hand_edited_block_is_reported_as_drift_and_only_overwrite_reverts_it() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));
        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let claude = client(
            "claude-code",
            Some(owned(
                s.path("rules").join(Scope::Personal.owned_file_name()),
            )),
        );
        let installed = vec![codex.clone(), claude.clone()];
        seed_and_apply("Be brief.", &["codex", "claude-code"], &installed);
        let codex_path = codex.target.clone().unwrap().path;
        let claude_path = claude.target.clone().unwrap().path;

        // The user tunes both files by hand, inside what Toolport wrote.
        for path in [&codex_path, &claude_path] {
            let text = std::fs::read_to_string(path).unwrap();
            std::fs::write(path, text.replace("Be brief.", "Be brief. And kind.")).unwrap();
        }
        let edited_codex = std::fs::read_to_string(&codex_path).unwrap();
        let edited_claude = std::fs::read_to_string(&claude_path).unwrap();

        // A reconcile (startup, a toggle, a save that did not change content) leaves both alone
        // and reports them, body included, so the UI can show the difference.
        let view = apply_to(&installed, ApplyMode::Reconcile).unwrap();
        assert_eq!(std::fs::read_to_string(&codex_path).unwrap(), edited_codex);
        assert_eq!(
            std::fs::read_to_string(&claude_path).unwrap(),
            edited_claude
        );
        let by_id =
            |v: &RulesView, id: &str| v.clients.iter().find(|c| c.id == id).unwrap().clone();
        for id in ["codex", "claude-code"] {
            let row = by_id(&view, id);
            assert_eq!(row.state, ApplyState::Drifted, "{id}");
            assert_eq!(row.on_disk.as_deref(), Some("Be brief. And kind."), "{id}");
        }
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.rules_targets.len(), 2, "drifted files stay on record");

        // A change to the SET is a newer revision: the user moved the source of truth, and the
        // reconcile writes it (the hand edit goes; the UI offered Pull first).
        let view = seed_and_apply("Be brief. Always.", &[], &installed);
        assert!(std::fs::read_to_string(&codex_path)
            .unwrap()
            .contains("Be brief. Always."));
        assert_eq!(by_id(&view, "codex").state, ApplyState::Applied);

        // Drift BOTH again. Overwriting one client's file leaves the other's edit alone.
        for path in [&codex_path, &claude_path] {
            let text = std::fs::read_to_string(path).unwrap();
            std::fs::write(path, text.replace("Always.", "Never.")).unwrap();
        }
        let view = apply_to(&installed, ApplyMode::Reconcile).unwrap();
        assert_eq!(by_id(&view, "codex").state, ApplyState::Drifted);
        assert_eq!(by_id(&view, "claude-code").state, ApplyState::Drifted);
        let view = apply_to(&installed, ApplyMode::OverwriteOnly(claude_path.clone())).unwrap();
        assert_eq!(
            by_id(&view, "claude-code").state,
            ApplyState::Applied,
            "the one asked for"
        );
        assert_eq!(
            by_id(&view, "codex").state,
            ApplyState::Drifted,
            "the other is untouched"
        );
        assert!(std::fs::read_to_string(&codex_path)
            .unwrap()
            .contains("Never."));
        // And the explicit overwrite-everything reverts the rest.
        let view = apply_to(&installed, ApplyMode::Overwrite).unwrap();
        assert!(std::fs::read_to_string(&codex_path)
            .unwrap()
            .contains("Be brief. Always."));
        assert_eq!(by_id(&view, "codex").state, ApplyState::Applied);
        assert_eq!(by_id(&view, "codex").on_disk, None);

        // An absent block is plain Stale and a reconcile writes it.
        std::fs::remove_file(&codex_path).unwrap();
        let view = apply_to(&installed, ApplyMode::Reconcile).unwrap();
        assert!(codex_path.exists());
        assert_eq!(by_id(&view, "codex").state, ApplyState::Applied);
    }

    #[test]
    fn opting_a_client_out_removes_only_that_clients_file() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let zed = client("zed", Some(sentinel(s.path("zed-AGENTS.md"))));
        let installed = vec![codex.clone(), zed.clone()];
        let codex_path = codex.target.clone().unwrap().path;
        let zed_path = zed.target.clone().unwrap().path;

        // A file the user already owns, so we can prove only our span goes.
        let user = "# Mine\nkeep me\n";
        std::fs::write(&codex_path, user).unwrap();

        seed_and_apply("Be brief.", &["codex", "zed"], &installed);
        assert!(zed_path.exists());
        assert!(std::fs::read_to_string(&codex_path)
            .unwrap()
            .contains("Be brief."));

        crate::registry::update(|reg| {
            reg.set_rules_client_enabled("codex", false);
            Ok(())
        })
        .unwrap();
        apply_to(&installed, ApplyMode::Reconcile).unwrap();

        assert_eq!(
            std::fs::read_to_string(&codex_path).unwrap(),
            user,
            "the opted-out client's file is back to the user's own bytes"
        );
        assert!(zed_path.exists(), "the other client is untouched");
        let reg = crate::registry::load().unwrap();
        assert_eq!(
            reg.rules_targets,
            vec![zed_path.to_string_lossy().to_string()]
        );
    }

    /// A write that fails must NOT be treated as an opt-out. The client is still enabled, so its
    /// last known-good block stays on disk and its path stays recorded; a transient failure must
    /// not cost the user the rules they already had.
    #[test]
    fn a_failed_write_keeps_the_previous_good_block_and_its_record() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let installed = vec![codex.clone()];
        let path = codex.target.clone().unwrap().path;

        seed_and_apply("Good rules.", &["codex"], &installed);
        let good = std::fs::read_to_string(&path).unwrap();
        assert!(good.contains("Good rules."));

        // Content carrying our own frozen marker is refused by `write_target` (it would corrupt
        // the block), which is the cheapest way to drive a real per-client failure: exactly what a
        // user pasting an existing AGENTS.md into the editor would hit.
        crate::registry::update(|reg| {
            let id = reg.active_rule_set().unwrap().id.clone();
            reg.upsert_rule_set(
                Some(&id),
                "Work",
                &format!("Bad {} rules.", instructions::PERSONAL_SENTINEL_END),
            )?;
            Ok(())
        })
        .unwrap();
        let view = apply_to(&installed, ApplyMode::Reconcile).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            good,
            "the previous good block must survive a failed rewrite"
        );
        assert_eq!(
            crate::registry::load().unwrap().rules_targets,
            vec![path.to_string_lossy().to_string()],
            "a still-desired path stays recorded, or nothing would ever clean it up"
        );
        assert_eq!(
            view.clients[0].state,
            ApplyState::Error,
            "and the failure is reported rather than hidden"
        );
    }

    /// Preview must render the DRAFT without saving it. Saving applies to every opted-in client,
    /// so a save-then-preview would make the dry run a write, which is the one thing this control
    /// promises not to do.
    #[test]
    fn preview_renders_unsaved_content_without_touching_disk() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        crate::registry::update(|reg| {
            reg.upsert_rule_set(None, "Work", "Saved rules.")?;
            Ok(())
        })
        .unwrap();

        let target = sentinel(s.path("AGENTS.md"));
        std::fs::write(&target.path, "# Mine\n").unwrap();
        let before = std::fs::read_to_string(&target.path).unwrap();

        let set = crate::registry::load()
            .unwrap()
            .active_rule_set()
            .cloned()
            .unwrap();

        let preview = preview_target("codex", &target, &set, "Draft rules.").unwrap();
        assert!(preview.after.contains("Draft rules."));
        assert!(!preview.after.contains("Saved rules."));
        assert_eq!(preview.state, ApplyState::Stale);

        // And nothing was written: the file is byte-identical and the set still holds saved text.
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), before);
        assert_eq!(
            crate::registry::load()
                .unwrap()
                .active_rule_set()
                .unwrap()
                .content,
            "Saved rules."
        );
    }

    #[test]
    fn preview_matches_refused_writes_and_rejects_unreadable_files() {
        let s = Scratch::new();
        let path = s.path("AGENTS.md");
        let before = "# Mine\n";
        std::fs::write(&path, before).unwrap();
        let rules = set("work", 1, "A rule that does not fit.");

        let capped = Target {
            char_cap: Some(before.chars().count()),
            ..sentinel(path.clone())
        };
        let capped_preview = preview_target("windsurf", &capped, &rules, &rules.content).unwrap();
        assert_eq!(capped_preview.state, ApplyState::TooLong);
        assert_eq!(capped_preview.after, before);
        assert_eq!(
            instructions::write_target(&capped, &rules.id, rules.revision, &rules.content),
            ApplyState::TooLong
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let shadow = s.path("shadow.md");
        std::fs::write(&shadow, "local override").unwrap();
        let blocked = Target {
            blocked_if_present: Some(shadow),
            ..sentinel(path.clone())
        };
        let blocked_preview = preview_target("codex", &blocked, &rules, &rules.content).unwrap();
        assert_eq!(blocked_preview.state, ApplyState::BlockedOverride);
        assert_eq!(blocked_preview.after, before);
        assert_eq!(
            instructions::write_target(&blocked, &rules.id, rules.revision, &rules.content),
            ApplyState::BlockedOverride
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let directory = s.path("unreadable");
        std::fs::create_dir_all(&directory).unwrap();
        let err = preview_target("codex", &sentinel(directory), &rules, &rules.content)
            .expect_err("an unreadable target must not preview as empty");
        assert!(err.contains("Could not read"), "unexpected error: {err}");
    }

    /// Marker text must be refused when the user submits it, not when the write fails. Realistic
    /// way in: copying out of the preview panel, which shows the rendered file with its markers.
    #[test]
    fn saving_rules_that_contain_our_markers_is_refused_up_front() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        crate::registry::update(|reg| {
            reg.upsert_rule_set(None, "Work", "Good rules.")?;
            Ok(())
        })
        .unwrap();
        let id = crate::registry::load()
            .unwrap()
            .active_rule_set()
            .unwrap()
            .id
            .clone();

        for bad in [
            format!(
                "{} set=x v=1 -->",
                instructions::PERSONAL_SENTINEL_START_PREFIX
            ),
            instructions::PERSONAL_SENTINEL_END.to_string(),
            instructions::SENTINEL_END.to_string(),
        ] {
            let err = save_set(Some(&id), "Work", &bad).expect_err("must be refused");
            assert!(err.contains("marker"), "unhelpful message: {err}");
        }

        // Refused, so the good rules are still what is stored: no half-written state.
        assert_eq!(
            crate::registry::load()
                .unwrap()
                .active_rule_set()
                .unwrap()
                .content,
            "Good rules."
        );
    }

    /// A path we tried and failed to clean must stay on record, or nothing will ever come back to
    /// it. Driven through a directory-in-the-way, which makes both the rewrite and the delete fail
    /// on every platform without needing permission games.
    #[test]
    fn a_path_we_could_not_clean_stays_on_record_for_the_next_pass() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let installed = vec![codex.clone()];
        let path = codex.target.clone().unwrap().path;

        seed_and_apply("Be brief.", &["codex"], &installed);
        assert!(path.exists());
        let recorded = path.to_string_lossy().to_string();

        // Leave our START marker in the file with no END. `remove_recorded` will not guess where
        // the block ends, so it reports "not cleaned" and the block stays put.
        std::fs::write(
            &path,
            format!(
                "{} set=x v=1 -->\nno end marker\n",
                instructions::PERSONAL_SENTINEL_START_PREFIX
            ),
        )
        .unwrap();

        // Opt the client out: the path is no longer desired, so cleanup runs and fails.
        crate::registry::update(|reg| {
            reg.set_rules_client_enabled("codex", false);
            Ok(())
        })
        .unwrap();
        apply_to(&installed, ApplyMode::Reconcile).unwrap();

        assert_eq!(
            crate::registry::load().unwrap().rules_targets,
            vec![recorded],
            "a path we failed to clean must stay recorded so the next pass retries it"
        );
    }

    #[test]
    fn a_recovered_registry_cannot_trigger_rules_cleanup() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let data = s.path("data");
        let _data_dir = crate::registry::DataDirOverride::set(&data);
        let target = sentinel(s.path("AGENTS.md"));
        let path = target.path.to_string_lossy().to_string();

        assert_eq!(
            instructions::write_target(&target, "work", 1, "Keep this rule."),
            ApplyState::Applied
        );
        let before = std::fs::read_to_string(&target.path).unwrap();

        crate::registry::update(|reg| {
            reg.rule_sets.push(set("work", 1, "Keep this rule."));
            reg.active_rule_set_id = Some("work".into());
            reg.rules_targets = vec![path.clone()];
            Ok(())
        })
        .unwrap();
        // A second save creates an N-1 backup containing the active set and recorded target.
        crate::registry::update(|reg| {
            reg.deny_destructive = !reg.deny_destructive;
            Ok(())
        })
        .unwrap();
        std::fs::write(data.join("registry.json"), "{ not json").unwrap();

        let error = apply_to(&[], ApplyMode::Reconcile)
            .expect_err("backup recovery must refuse filesystem changes");

        assert!(
            error.contains("not authoritative"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&target.path).unwrap(),
            before,
            "a recovered snapshot must not remove recorded client rules"
        );
    }

    /// A rules mutation and its reconciliation are separate calls, so two UI workers can interleave
    /// them. Each apply must reconcile the fresh registry state while holding the same cross-process
    /// lock as mutations; whichever set is active at the end must also be the bytes on disk.
    #[test]
    fn concurrent_applies_leave_the_active_sets_bytes_on_disk() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let installed = vec![codex.clone()];
        let path = codex.target.clone().unwrap().path;

        let (_, (a, b)) = crate::registry::update(|reg| {
            let a = reg.upsert_rule_set(None, "A", "Rules A.")?;
            let b = reg.upsert_rule_set(None, "B", "Rules B.")?;
            reg.set_rules_client_enabled("codex", true);
            Ok((a, b))
        })
        .unwrap();

        for _ in 0..20 {
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    crate::registry::update(|reg| {
                        reg.set_active_rule_set(Some(&a));
                        Ok(())
                    })
                    .unwrap();
                    apply_to(&installed, ApplyMode::Reconcile).unwrap();
                });
                scope.spawn(|| {
                    crate::registry::update(|reg| {
                        reg.set_active_rule_set(Some(&b));
                        Ok(())
                    })
                    .unwrap();
                    apply_to(&installed, ApplyMode::Reconcile).unwrap();
                });
            });

            let reg = crate::registry::load().unwrap();
            let active = reg.active_rule_set().unwrap();
            assert_eq!(
                instructions::current_state(
                    codex.target.as_ref().unwrap(),
                    &active.id,
                    active.revision,
                    &active.content,
                ),
                ApplyState::Applied,
                "disk and registry diverged for active set {}",
                active.id
            );
            assert_eq!(reg.rules_targets, vec![path.to_string_lossy().to_string()]);
        }
    }

    #[test]
    fn switching_sets_rewrites_in_place_and_clearing_removes_everything() {
        let _dirs = crate::registry::data_dir_test_lock();
        let s = Scratch::new();
        let _data_dir = crate::registry::DataDirOverride::set(s.path("data"));

        let codex = client("codex", Some(sentinel(s.path("AGENTS.md"))));
        let installed = vec![codex.clone()];
        let path = codex.target.clone().unwrap().path;

        seed_and_apply("Rules A.", &["codex"], &installed);
        assert!(std::fs::read_to_string(&path).unwrap().contains("Rules A."));

        // A second set replaces the first set's span rather than stacking a second block.
        crate::registry::update(|reg| {
            let id = reg
                .upsert_rule_set(None, "Other", "Rules B.")
                .expect("create");
            reg.set_active_rule_set(Some(&id));
            Ok(())
        })
        .unwrap();
        apply_to(&installed, ApplyMode::Reconcile).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("Rules B."));
        assert!(
            !after.contains("Rules A."),
            "the old set's span is replaced, not appended to"
        );
        assert_eq!(
            after
                .matches(instructions::PERSONAL_SENTINEL_START_PREFIX)
                .count(),
            1,
            "exactly one personal block, whichever set wrote it"
        );

        // Clearing the selection takes our file away and forgets the recorded path.
        crate::registry::update(|reg| {
            reg.set_active_rule_set(None);
            Ok(())
        })
        .unwrap();
        let view = apply_to(&installed, ApplyMode::Reconcile).unwrap();
        assert!(!path.exists(), "a file that held only our block is removed");
        let reg = crate::registry::load().unwrap();
        assert!(reg.rules_targets.is_empty(), "nothing left to clean up");
        assert_eq!(
            view.clients[0].state,
            ApplyState::Applied,
            "with no active set there is nothing to be stale about"
        );
    }

    /// Cleanup is by RECORDED path, so opting a client out (or switching sets) removes exactly the
    /// file we wrote and leaves the user's own bytes and any team block alone.
    #[test]
    fn cleanup_removes_only_our_span() {
        let s = Scratch::new();
        let path = s.path("AGENTS.md");
        let user = "# Mine\nkeep me\n";
        std::fs::write(&path, user).unwrap();
        let personal = sentinel(path.clone());
        let team = Target {
            scope: Scope::Team,
            ..sentinel(path.clone())
        };
        instructions::write_target(&team, "team_abc", 1, "Org rule");
        instructions::write_target(&personal, "work", 1, "Be brief.");

        instructions::remove_recorded(&path, Scope::Personal);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(user), "user bytes survive");
        assert!(
            after.contains("Org rule"),
            "the team block is not ours to remove"
        );
        assert!(!after.contains("Be brief."), "our span is gone");
    }
}
