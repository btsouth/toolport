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
use crate::registry::RuleSet;
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
}

/// Everything the Rules view needs, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesView {
    pub sets: Vec<RuleSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_set_id: Option<String>,
    pub clients: Vec<ClientStatus>,
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
/// clients share (Gemini CLI + Antigravity) is written once.
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
            let state = match (&c.target, set) {
                (None, _) => ApplyState::Unsupported,
                // No active set: the desired end state is "nothing of ours on disk", so the
                // question is presence, not content. Reporting Applied unconditionally here hid a
                // cleanup that failed — the block was still sitting in the file while the row
                // said "up to date".
                (Some(t), None) => {
                    if instructions::is_present(&t.path, Scope::Personal) {
                        ApplyState::Stale
                    } else {
                        ApplyState::Applied
                    }
                }
                (Some(t), Some(s)) => instructions::current_state(t, &s.id, s.revision, &s.content),
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
            }
        })
        .collect()
}

/// The whole Rules view. Read-only; scans every installed client's rules file, so callers run it
/// off the UI thread.
pub fn view() -> Result<RulesView, String> {
    let reg = crate::registry::load()?;
    let installed = installed_targets();
    let set = reg.active_rule_set().cloned();
    Ok(RulesView {
        clients: status_from(&reg, &installed, set.as_ref()),
        sets: reg.rule_sets.clone(),
        active_set_id: reg.active_rule_set_id.clone(),
    })
}

/// Apply the active rule set to every opted-in client, then clean up anything we wrote before and
/// did not write now. Returns the refreshed view.
///
/// Best-effort per client, like the team writer: one unwritable file must not abort the rest. A
/// client that reports anything other than [`ApplyState::Applied`] is simply not recorded, so the
/// next pass tries it again.
pub fn apply() -> Result<RulesView, String> {
    let installed = installed_targets();
    apply_to(&installed)
}

/// [`apply`] over an explicit client/target set, so tests drive a known set of files instead of
/// the developer's real machine.
fn apply_to(installed: &[ClientTarget]) -> Result<RulesView, String> {
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
        if let Some(s) = set.as_ref() {
            for target in &targets {
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
                && !instructions::remove_recorded(
                    std::path::Path::new(old),
                    Scope::Personal,
                )
            {
                uncleaned.push(old.clone());
            }
        }

        // What we now own on disk: successful writes, still-desired previous good files, and
        // failed cleanups. Every path remains discoverable for a later reconciliation.
        let mut owned = written;
        for old in prev_targets.iter().chain(uncleaned.iter()) {
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
    crate::registry::update(|reg| {
        reg.remove_rule_set(id);
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

/// Re-assert the active set at startup. Cheap in the common case: [`instructions::write_target`]
/// no-ops when the on-disk block already matches, so a normal launch touches no files. Exists so
/// a client updated (or reinstalled) since the last apply picks the rules back up without the
/// user opening the Rules tab.
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

    // ---- registry-level set management (no filesystem) ----

    #[test]
    fn a_new_set_becomes_active_when_nothing_else_is() {
        let mut reg = crate::registry::Registry::default();
        let id = reg.upsert_rule_set(None, "Work", "Always run tests.").expect("create");
        assert_eq!(reg.active_rule_set_id.as_deref(), Some(id.as_str()));
        assert_eq!(reg.active_rule_set().map(|s| s.revision), Some(1));

        // A SECOND set does not steal the selection.
        let other = reg.upsert_rule_set(None, "Personal", "Be brief.").expect("create");
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
        reg.upsert_rule_set(Some(&id), "Renamed", "v1").expect("update");
        assert_eq!(reg.active_rule_set().unwrap().revision, 1);
        assert_eq!(reg.active_rule_set().unwrap().name, "Renamed");

        reg.upsert_rule_set(Some(&id), "Renamed", "v2").expect("update");
        assert_eq!(reg.active_rule_set().unwrap().revision, 2);
    }

    #[test]
    fn saving_against_an_unknown_id_errors_rather_than_duplicating() {
        let mut reg = crate::registry::Registry::default();
        let id = reg.upsert_rule_set(None, "Work", "v1").expect("create");
        let err = reg
            .upsert_rule_set(Some("deleted-in-another-window"), "Work", "v2")
            .expect_err("an unknown id must not create");
        assert!(err.contains("no longer exists"), "unexpected message: {err}");
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
        assert_eq!(reg.active_rule_set_id, None, "unknown id clears, never panics");
    }

    #[test]
    fn a_client_is_opted_out_until_the_user_says_otherwise() {
        let mut reg = crate::registry::Registry::default();
        assert!(!reg.rules_client_enabled("claude-code"), "absent must mean off");
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
        assert!(std::fs::read_to_string(&foreign.path).unwrap().starts_with(user));

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
        apply_to(installed).expect("apply")
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
            Some(owned(s.path("rules").join(Scope::Personal.owned_file_name()))),
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
        assert!(std::fs::read_to_string(&codex_path).unwrap().contains("Be brief."));

        crate::registry::update(|reg| {
            reg.set_rules_client_enabled("codex", false);
            Ok(())
        })
        .unwrap();
        apply_to(&installed).unwrap();

        assert_eq!(
            std::fs::read_to_string(&codex_path).unwrap(),
            user,
            "the opted-out client's file is back to the user's own bytes"
        );
        assert!(zed_path.exists(), "the other client is untouched");
        let reg = crate::registry::load().unwrap();
        assert_eq!(reg.rules_targets, vec![zed_path.to_string_lossy().to_string()]);
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
        let view = apply_to(&installed).unwrap();

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
            format!("{} set=x v=1 -->", instructions::PERSONAL_SENTINEL_START_PREFIX),
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
        apply_to(&installed).unwrap();

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

        let error = apply_to(&[]).expect_err("backup recovery must refuse filesystem changes");

        assert!(error.contains("not authoritative"), "unexpected error: {error}");
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
                    apply_to(&installed).unwrap();
                });
                scope.spawn(|| {
                    crate::registry::update(|reg| {
                        reg.set_active_rule_set(Some(&b));
                        Ok(())
                    })
                    .unwrap();
                    apply_to(&installed).unwrap();
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
            let id = reg.upsert_rule_set(None, "Other", "Rules B.").expect("create");
            reg.set_active_rule_set(Some(&id));
            Ok(())
        })
        .unwrap();
        apply_to(&installed).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("Rules B."));
        assert!(!after.contains("Rules A."), "the old set's span is replaced, not appended to");
        assert_eq!(
            after.matches(instructions::PERSONAL_SENTINEL_START_PREFIX).count(),
            1,
            "exactly one personal block, whichever set wrote it"
        );

        // Clearing the selection takes our file away and forgets the recorded path.
        crate::registry::update(|reg| {
            reg.set_active_rule_set(None);
            Ok(())
        })
        .unwrap();
        let view = apply_to(&installed).unwrap();
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
        assert!(after.contains("Org rule"), "the team block is not ours to remove");
        assert!(!after.contains("Be brief."), "our span is gone");
    }
}
