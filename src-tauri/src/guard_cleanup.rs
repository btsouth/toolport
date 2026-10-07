//! Retired agent guard: the `--toolport-guard` no-op and one-time cleanup.
//!
//! Agent permissions (the native permission policy and the Cursor/Claude Code guard
//! hook that enforced it) were removed in 2.0. Two small pieces stay behind:
//!
//!   * The flag itself, `--toolport-guard AGENT`, is still a flag the gateway
//!     recognises. Hooks Toolport installed in a client's config keep naming it, and a
//!     stale hook must not fail; it always answers "allow" and exits 0.
//!   * A one-time removal at startup strips Toolport's own entries out of
//!     `~/.cursor/hooks.json` and every Claude Code `settings.json`, so the hook does
//!     not keep running. Only entries carrying [`GUARD_MARKER`] are touched; the
//!     `permissions` lists Toolport added to `settings.json` are the user's policy and
//!     are left in place.
//!
//! The removal runs once, recorded by a marker file in the data directory.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The literal that marks a hooks.json entry as Toolport's retired guard. Kept because
/// stale client hook entries still carry it and the gateway still recognises the flag.
pub const GUARD_MARKER: &str = "--toolport-guard";

/// Cap on the hook payload the no-op drains from stdin, so a client writing far more than
/// it needs (a `beforeReadFile` payload embeds the whole file) does not see a broken pipe
/// when the no-op exits. Nothing read is used.
const DRAIN_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// The decision the retired hook still prints: an unconditional allow, so a stale hook
/// with `failClosed` cannot block a call in 2.0. Cursor reads `permission`; Claude Code
/// reads `continue` and treats empty output as no opinion, so this shape is a safe allow
/// for both.
pub fn no_op_allow() -> String {
    json!({ "continue": true, "permission": "allow" }).to_string()
}

/// Drain (and discard) up to [`DRAIN_CAP_BYTES`] of stdin, then return [`no_op_allow`].
///
/// Reading stdin first keeps a client's write from failing once the process exits; the
/// bytes are never parsed. A read error just ends the drain.
pub fn run_no_op_hook() -> String {
    use std::io::Read;

    let mut stdin = std::io::stdin().lock();
    let mut remaining = DRAIN_CAP_BYTES;
    let mut buf = [0u8; 8192];
    while remaining > 0 {
        let want = (remaining as usize).min(buf.len());
        match stdin.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(read) => remaining -= read as u64,
            Err(_) => break,
        }
    }
    no_op_allow()
}

fn cursor_hooks_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cursor").join("hooks.json"))
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .map(|c| c.contains(GUARD_MARKER))
        .unwrap_or(false)
}

/// `root` with every guard entry removed, pruning an event list this emptied and then the
/// `hooks` object; `version` is left alone (it is the file's, not ours).
fn strip_guard(root: &Value) -> Value {
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

/// `root` with every guard entry removed from a Claude Code settings file, per entry so a
/// user's own hook in the same group survives, pruning only what this pass emptied.
fn strip_claude_guard(root: &Value) -> Value {
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

/// Which file shape a guard target has: Cursor's `hooks.json` is flat per event; a Claude
/// Code `settings.json` holds matcher groups.
fn is_cursor_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("hooks.json")
}

/// Remove the retired guard from one file, backing the file up first through the same
/// writer every client-config edit uses. A file that is gone is success (its profile was
/// deleted); one that cannot be parsed is not - refusing loudly beats rewriting a file we
/// do not understand.
pub fn remove_at(path: &Path) -> Result<(), String> {
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
    if is_cursor_file(path) {
        let stripped = strip_guard(&root);
        if stripped.get("hooks") == root.get("hooks") {
            return Ok(());
        }
        crate::clients::write_settings_key_for(
            "cursor",
            path,
            original.as_deref(),
            &stripped,
            "hooks",
        )
    } else {
        let stripped = strip_claude_guard(&root);
        if stripped == root {
            return Ok(());
        }
        crate::clients::write_settings_json(path, original.as_deref(), &stripped)
    }
}

/// The data-directory marker that records the one-time removal has run.
fn marker_path() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("agent-guard-removed"))
}

/// Remove the retired guard from every file it was installed into, once per data
/// directory. Called at startup by both desktop shells. Each file is independent: one
/// that cannot be read is logged and left alone, and the rest still run.
pub fn run_once() {
    let Some(marker) = marker_path() else {
        return;
    };
    if marker.exists() {
        return;
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(path) = cursor_hooks_path() {
        paths.push(path);
    }
    paths.extend(crate::clients::claude_settings_paths());
    for path in paths {
        if let Err(error) = remove_at(&path) {
            crate::gatewaylog::append(&format!(
                "toolport: could not remove the retired guard hook from {}: {error}",
                path.display()
            ));
        }
    }
    // Best-effort: if the marker cannot be written the removal simply runs again next
    // launch, and it is idempotent.
    let _ = std::fs::write(&marker, b"agent guard entries removed\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_hook_always_answers_allow() {
        let decision: Value = serde_json::from_str(&no_op_allow()).unwrap();
        assert_eq!(decision["continue"], json!(true));
        assert_eq!(decision["permission"], json!("allow"));
    }

    #[test]
    fn cursor_strip_leaves_the_users_own_hooks() {
        let theirs = json!({
            "version": 1,
            "hooks": {
                "beforeShellExecution": [
                    { "command": "./my-own.sh" },
                    { "command": "\"/opt/toolport/toolport-gateway\" --toolport-guard cursor", "failClosed": true }
                ],
                "stop": [{ "command": "./done.sh" }]
            }
        });
        let stripped = strip_guard(&theirs);
        assert_eq!(
            stripped["hooks"]["beforeShellExecution"],
            json!([{ "command": "./my-own.sh" }])
        );
        assert_eq!(stripped["hooks"]["stop"], theirs["hooks"]["stop"]);
        assert_eq!(stripped["version"], json!(1));
    }

    #[test]
    fn claude_strip_leaves_the_users_own_group_and_entries() {
        let theirs = json!({
            "permissions": { "deny": ["Bash(rm -rf *)"] },
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "./my-guard.sh" }] },
                    { "matcher": "Bash|Read|mcp__.*", "hooks": [{ "type": "command", "command": "\"/opt/toolport/toolport-gateway\" --toolport-guard claude-code" }] }
                ]
            }
        });
        let stripped = strip_claude_guard(&theirs);
        let groups = stripped["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["command"], "./my-guard.sh");
        // The user's own policy is untouched.
        assert_eq!(stripped["permissions"], theirs["permissions"]);
    }

    #[test]
    fn remove_at_strips_only_our_entry_over_a_scratch_file() {
        let _lock = crate::registry::data_dir_test_lock();
        let scratch =
            std::env::temp_dir().join(format!("toolport-guard-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(scratch.join("data"));

        let hooks = scratch.join(".cursor").join("hooks.json");
        std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        std::fs::write(
            &hooks,
            "{\n  \"version\": 1,\n  \"hooks\": { \"beforeShellExecution\": [{ \"command\": \"./mine.sh\" }, { \"command\": \"toolport-gateway --toolport-guard cursor\" }], \"stop\": [{ \"command\": \"./done.sh\" }] }\n}\n",
        )
        .unwrap();

        remove_at(&hooks).unwrap();
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert!(!text.contains(GUARD_MARKER), "{text}");
        assert!(
            text.contains("./mine.sh"),
            "the user's hook survives: {text}"
        );
        assert!(
            text.contains("./done.sh"),
            "an untouched event survives: {text}"
        );
        assert!(text.contains("\"version\": 1"), "{text}");

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
