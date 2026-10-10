//! Retained pieces of the retired agent activity sensor (`--toolport-hook`).
//!
//! 2.0 removed the sensor that recorded what an agent does outside the gateway
//! (`Bash`, `Edit`, `Read`). Nothing records those native tool calls any more. What
//! stays here exists only to make the removal safe:
//!
//!   * [`noop_hook`] backs the `--toolport-hook` subcommand. Hook entries an earlier
//!     release installed into an AI client's settings still invoke it on every agent
//!     lifecycle event, so it stays a silent success. A stale entry must never error
//!     or block the client.
//!   * [`cleanup_on_startup`] removes those installed entries once, by [`HOOK_MARKER`]
//!     and nothing else, so a user's own hooks in the same file survive untouched.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// The literal that marks a hook entry as Toolport's, in the command it runs.
///
/// Doubles as the gateway subcommand flag, so one string is both "how the binary
/// knows it is being invoked as a hook" and "how we recognise our own entry later".
pub const HOOK_MARKER: &str = "--toolport-hook";

/// The one-time marker that stops [`cleanup_on_startup`] scanning again.
const CLEANUP_MARKER_FILE: &str = "agent-activity-removed";

/// The 2.0 entry point for `--toolport-hook`.
///
/// The sensor is gone, but hook entries an earlier Toolport installed still invoke
/// this subcommand on every agent lifecycle event. It stays a silent success: drain
/// the payload so the client's write cannot fail or block, print nothing, and let
/// the caller exit 0. The event argument is ignored.
pub fn noop_hook(mut reader: impl std::io::Read) {
    let _ = std::io::copy(&mut reader, &mut std::io::sink());
}

// ---------------------------------------------------------------------------
// Removal: identify Toolport's entries by marker, drop exactly those
// ---------------------------------------------------------------------------

/// True when this hook entry is one Toolport installed.
fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .map(|command| command.contains(HOOK_MARKER))
        .unwrap_or(false)
}

/// Return `root` with every Toolport hook entry removed, pruning each container the
/// removal empties.
///
/// Removal is per ENTRY, not per group: a user who added their own hook to the same
/// event, in the same group, keeps it. Pruning matters because a leftover
/// `"hooks": {}` or `"PostToolUse": []` is residue from a feature the user turned
/// off, and "clean up exactly what we wrote" includes the shape.
pub fn strip_hooks(root: &Value) -> Value {
    let mut out = root.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    let Some(hooks) = obj.get_mut("hooks").and_then(Value::as_object_mut) else {
        return out;
    };

    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        groups.retain_mut(|group| {
            if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = entries.len();
                entries.retain(|entry| !is_ours(entry));
                // Drop only a group this pass emptied. A pre-existing empty group belongs
                // to the user and must survive untouched.
                return before == entries.len() || !entries.is_empty();
            }
            true
        });
        if groups.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    out
}

/// Remove Toolport's hook entries from one settings file.
///
/// A file that is gone is success: the profile it belonged to was deleted, which is
/// a stronger form of removed. A file we cannot parse is NOT success - refusing
/// loudly beats rewriting a file we do not understand. The write goes through the
/// same CST rewrite and timestamped backup as every other client-config edit: it
/// rewrites only the `hooks` key, so comments and every other setting survive.
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
    let stripped = strip_hooks(&root);
    if stripped == root {
        return Ok(());
    }
    crate::clients::write_settings_json(path, original.as_deref(), &stripped)
}

/// Remove Toolport's retired hook entries from every Claude Code profile, once.
///
/// Hook entries an earlier release installed carry [`HOOK_MARKER`] and run the
/// `--toolport-hook` no-op, so they are harmless if any survive; the cleanup exists to
/// take them out of the user's files. A marker file in the data directory makes this a
/// one-time pass, and each profile is handled on its own so one unreadable file cannot
/// stop the others from being cleaned.
pub fn cleanup_on_startup() {
    cleanup_on_startup_with(&crate::clients::claude_settings_paths());
}

/// [`cleanup_on_startup`] over an explicit profile set and data directory, so tests
/// drive known files instead of the developer's real `~/.claude`.
fn cleanup_on_startup_with(profiles: &[PathBuf]) {
    let Some(marker) = crate::registry::conduit_dir().map(|dir| dir.join(CLEANUP_MARKER_FILE))
    else {
        return;
    };
    if marker.exists() {
        return;
    }
    for path in profiles {
        if let Err(error) = remove_at(path) {
            crate::gatewaylog::append(&format!(
                "toolport: could not remove the retired agent activity hook from {}: {error}",
                path.display()
            ));
        }
    }
    if let Err(error) = std::fs::write(&marker, b"1\n") {
        crate::gatewaylog::append(&format!(
            "toolport: could not write {}: {error}",
            marker.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn our_command() -> String {
        format!("\"/opt/Toolport/toolport-gateway\" {HOOK_MARKER} tool")
    }

    fn our_entry() -> Value {
        json!({ "type": "command", "command": our_command() })
    }

    fn our_group() -> Value {
        json!({ "hooks": [our_entry()] })
    }

    fn foreign_group() -> Value {
        json!({
            "matcher": "Bash",
            "hooks": [{ "type": "command", "command": "/usr/local/bin/my-linter" }]
        })
    }

    /// A marker-carrying entry that is not Toolport's, to prove removal is exact.
    fn guard_entry() -> Value {
        json!({ "type": "command", "command": "toolport-gateway --toolport-guard cursor" })
    }

    #[test]
    fn strip_removes_only_our_entry_from_a_shared_group() {
        // A user who hand-edited our group to add their own hook keeps it.
        let shared = json!({
            "hooks": {
                "PostToolUse": [{
                    "hooks": [our_entry(), { "type": "command", "command": "/usr/local/bin/my-linter" }]
                }]
            }
        });
        let out = strip_hooks(&shared);
        let entries = out["hooks"]["PostToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["command"], json!("/usr/local/bin/my-linter"));
    }

    #[test]
    fn strip_prunes_the_containers_it_empties() {
        let installed = json!({ "model": "opus", "hooks": { "PostToolUse": [our_group()] } });
        let out = strip_hooks(&installed);
        assert_eq!(
            out,
            json!({ "model": "opus" }),
            "no empty `hooks` object or event array may be left behind"
        );
    }

    #[test]
    fn strip_leaves_a_file_that_was_never_ours_untouched() {
        let theirs = json!({ "hooks": { "PostToolUse": [foreign_group()] } });
        assert_eq!(strip_hooks(&theirs), theirs);
    }

    #[test]
    fn strip_preserves_a_foreign_group_that_was_already_empty() {
        let theirs = json!({
            "hooks": {
                "PostToolUse": [{ "matcher": "Bash", "hooks": [] }]
            }
        });
        assert_eq!(strip_hooks(&theirs), theirs);
    }

    #[test]
    fn cleanup_leaves_a_user_hook_and_removes_only_the_marked_entry() {
        let env = scratch_env("cleanup-user-hook");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let original = serde_json::to_string_pretty(&json!({
            "model": "opus",
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [our_entry(), { "type": "command", "command": "/usr/local/bin/my-linter" }]
                }],
                "PreToolUse": [foreign_group()],
                "Stop": [{ "hooks": [guard_entry()] }]
            }
        }))
        .unwrap();
        std::fs::write(&path, &original).unwrap();

        cleanup_on_startup_with(&[path.clone()]);

        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert_eq!(root["model"], json!("opus"));
        let entries = root["hooks"]["PostToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "only the Toolport-marked entry may go");
        assert_eq!(entries[0]["command"], json!("/usr/local/bin/my-linter"));
        assert_eq!(root["hooks"]["PreToolUse"], json!([foreign_group()]));
        // A marker for a different Toolport feature is not ours to remove here.
        assert_eq!(root["hooks"]["Stop"], json!([{ "hooks": [guard_entry()] }]));
    }

    #[test]
    fn cleanup_runs_once_even_when_a_marked_entry_is_added_later() {
        let env = scratch_env("cleanup-once");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let seeded = serde_json::to_string_pretty(&json!({
            "hooks": { "PostToolUse": [our_group()] }
        }))
        .unwrap();
        std::fs::write(&path, &seeded).unwrap();

        cleanup_on_startup_with(&[path.clone()]);
        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert!(
            root.get("hooks").is_none(),
            "the first pass removes our block"
        );

        // A second pass must not run: a fresh marked entry is left alone.
        std::fs::write(&path, &seeded).unwrap();
        cleanup_on_startup_with(&[path.clone()]);
        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert!(
            root.get("hooks").is_some(),
            "the one-time marker must stop the second pass"
        );
    }

    #[test]
    fn removal_preserves_comments_and_unrelated_settings() {
        let env = scratch_env("removal-comments");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        // Embed the command as a JSON string literal: it contains quotes of its own.
        let command = serde_json::to_string(&our_command()).unwrap();
        let original = format!(
            "{{\n  // the model I actually want\n  \"model\": \"opus\",\n  \"permissions\": {{ \"allow\": [\"Bash(git status)\"] }},\n  \"hooks\": {{ \"PostToolUse\": [{{ \"hooks\": [{{ \"type\": \"command\", \"command\": {command} }}] }}] }}\n}}\n"
        );
        std::fs::write(&path, &original).unwrap();

        remove_at(&path).unwrap();

        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(restored.contains("// the model I actually want"));
        assert!(!restored.contains(HOOK_MARKER));
        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert_eq!(root["model"], json!("opus"));
        assert_eq!(root["permissions"]["allow"][0], json!("Bash(git status)"));
        assert!(
            root.get("hooks").is_none(),
            "removal must not leave an empty hooks object: {restored}"
        );
    }

    #[test]
    fn removal_leaves_a_foreign_hook_on_the_same_event_intact() {
        let env = scratch_env("foreign");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let seeded = serde_json::to_string_pretty(&json!({
            "hooks": { "PostToolUse": [foreign_group(), our_group()] }
        }))
        .unwrap();
        std::fs::write(&path, &seeded).unwrap();

        remove_at(&path).unwrap();

        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        let groups = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], foreign_group());
    }

    #[test]
    fn removing_from_a_profile_that_no_longer_exists_is_success() {
        let env = scratch_env("gone");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        // A profile the user deleted is a stronger form of "removed", not an error.
        assert!(remove_at(&path).is_ok());
    }

    #[test]
    fn a_malformed_settings_file_is_refused_rather_than_replaced() {
        let env = scratch_env("malformed");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();

        assert!(remove_at(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    struct ScratchEnv {
        dir: PathBuf,
        _override: crate::registry::DataDirOverride,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for ScratchEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn scratch_env(name: &str) -> ScratchEnv {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-hooks-{name}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let over = crate::registry::DataDirOverride::set(&dir);
        ScratchEnv {
            dir,
            _override: over,
            _lock: lock,
        }
    }
}
