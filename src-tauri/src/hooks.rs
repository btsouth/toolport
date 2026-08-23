//! Native-agent hook sensor - record what an agent does outside the gateway.
//!
//! The sensor half of SBS-822 (`agent-permissions` spec). Toolport governs calls that
//! go through the gateway; it is blind to what Claude Code does natively (`Bash`,
//! `Edit`, `Read`), because none of that is MCP. Agent harnesses expose hooks - a
//! command the harness runs at a lifecycle point, fed a JSON payload on stdin - so
//! this module installs three of them and records one line per event.
//!
//! Three properties this module exists to hold:
//!
//!   * **It cannot block a tool call.** `PreToolUse` is the blocking event and is
//!     deliberately not registered ([`SENSOR_EVENTS`]). "The sensor cannot stop your
//!     agent" is therefore structural, not a promise that the code is correct.
//!   * **It stores no content.** Hook payloads carry `tool_input` and `tool_response`:
//!     the `Bash` command line, the bytes of an `Edit`, whatever a `Read` returned.
//!     [`row`] keeps a tool name and an [`crate::audit::args_hash`] and drops the rest,
//!     the same contract the gateway audit log already holds.
//!   * **It cleans up exactly what it wrote.** JSON has no comments, so a settings
//!     block cannot carry a sentinel the way [`crate::instructions`] markers do.
//!     Instead every entry Toolport installs carries [`HOOK_MARKER`] in its command,
//!     and removal drops entries by that marker and nothing else - the same
//!     "identify what we wrote by content" rule as `instructions::remove_recorded`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The literal that marks a hook entry as Toolport's, in the command it runs.
///
/// Doubles as the gateway subcommand flag, so one string is both "how the binary
/// knows it is being invoked as a hook" and "how we recognise our own entry later".
pub const HOOK_MARKER: &str = "--toolport-hook";

/// The harness lifecycle events the sensor registers, paired with the argument the
/// hook command passes back to us.
///
/// `PreToolUse` is absent on purpose: it is the event whose exit status can refuse a
/// user's tool call, and this ship has no enforcement story yet (see the spec's
/// "Why the sensor ships alone"). `UserPromptSubmit` and `Notification` are absent
/// because their payload is the user's prompt text and there is no redaction path
/// for it here.
const SENSOR_EVENTS: [(&str, &str); 3] = [
    ("SessionStart", "session-start"),
    ("PostToolUse", "tool"),
    ("SessionEnd", "session-end"),
];

/// Seconds the harness should allow a hook before giving up on it. A wedged sensor
/// must cost the user a bounded pause, never a hung agent.
const HOOK_TIMEOUT_SECS: u64 = 5;

/// Cap on the payload read from stdin.
///
/// A `PostToolUse` payload embeds `tool_response`: the body of a `Read`, a `Bash`
/// process's stdout. [`row`] drops all of it, but an unbounded `read_to_string` would
/// allocate and parse the whole thing first, so a large native result could stall the
/// agent until the harness's hook timeout, or OOM this process and lose the row
/// entirely (SBS-822 review; SBS-930 is the same shape on the gateway's stderr drain).
/// Generous next to any real payload's identifying fields, which are all this keeps.
const MAX_HOOK_STDIN_BYTES: u64 = 1024 * 1024;

/// Trim the sensor log once it passes this size.
const MAX_HOOK_LOG_BYTES: u64 = 4 * 1024 * 1024;
/// Lines kept when trimming. Rows are small (no content), so this stays well inside
/// the byte cap; if it did not, every append past the cap would re-trim.
const KEEP_HOOK_LINES: usize = 10_000;

/// One agent settings file and what the sensor's state in it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStatus {
    /// Absolute path to the profile's `settings.json`.
    pub path: String,
    /// True when this file currently carries Toolport's hook entries.
    pub installed: bool,
    /// Why this profile could not be read or written, when that is the case. A
    /// profile that is merely not installed reports `None` here; an unreadable or
    /// malformed one reports the reason instead of silently looking "not installed".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Everything the (future) hooks view needs, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksView {
    /// The user's opt-in. Off by default.
    pub enabled: bool,
    /// The harness events the sensor registers, for the UI to name them honestly.
    pub events: Vec<String>,
    /// Every Claude Code profile found, installed or not.
    pub profiles: Vec<ProfileStatus>,
    /// Absent when no gateway binary is resolvable, which is the one condition that
    /// makes installing impossible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

/// A dry run: the exact hook block that would be written, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksPreview {
    pub path: String,
    /// The file as it is now (empty string when it does not exist yet).
    pub before: String,
    /// The file as it would be after the write.
    pub after: String,
    /// Why this profile has no dry run, when that is the case. Mirrors
    /// [`ProfileStatus::error`], and for the same reason: one profile we cannot read
    /// must not answer for the ones we can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// The hook block: pure functions over a settings value
// ---------------------------------------------------------------------------

/// The command the harness runs for one event.
///
/// Quoted because the harness hands this to a shell and installed paths contain
/// spaces on every platform Toolport ships to (`C:\Program Files\...`,
/// `/Applications/...`).
fn hook_command(binary: &Path, event_arg: &str) -> String {
    format!("\"{}\" {HOOK_MARKER} {event_arg}", binary.display())
}

/// Refuse paths that a shell can reinterpret inside the quoted command word.
fn validate_hook_binary(binary: &Path) -> Result<(), String> {
    let path = binary.to_string_lossy();
    let unsafe_char = path.chars().any(|c| {
        c.is_control()
            || matches!(c, '"' | '`' | '$')
            || (cfg!(not(windows)) && c == '\\')
    });
    if unsafe_char {
        return Err(format!(
            "gateway path cannot be represented safely in a hook command: {}",
            binary.display()
        ));
    }
    Ok(())
}

/// One matcher group holding one Toolport hook.
///
/// No `matcher` key: an absent matcher observes every tool, which is what a sensor
/// wants, and it avoids guessing a matcher dialect that differs between harness
/// versions.
fn matcher_group(command: String) -> Value {
    json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": HOOK_TIMEOUT_SECS,
        }]
    })
}

/// True when this hook entry is one Toolport installed.
fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .map(|command| command.contains(HOOK_MARKER))
        .unwrap_or(false)
}

/// True when `root` currently carries any Toolport hook entry.
pub fn is_installed(root: &Value) -> bool {
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    hooks.values().any(|groups| {
        groups
            .as_array()
            .map(|groups| {
                groups.iter().any(|group| {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .map(|entries| entries.iter().any(is_ours))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
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

/// Return `root` with Toolport's sensor entries present exactly once per event.
///
/// Built as strip-then-add so it is idempotent: applying it to a file we already
/// wrote, or to one carrying an entry from an older build that pointed at a
/// superseded gateway binary, converges on one current entry per event rather than
/// accumulating them.
pub fn upsert_hooks(root: &Value, binary: &Path) -> Result<Value, String> {
    validate_hook_binary(binary)?;
    let mut out = strip_hooks(root);
    let obj = out
        .as_object_mut()
        .ok_or_else(|| "settings root is not a JSON object".to_string())?;

    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "`hooks` is present but is not an object".to_string())?;

    for (event, arg) in SENSOR_EVENTS {
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("`hooks.{event}` is present but is not an array"))?;
        groups.push(matcher_group(hook_command(binary, arg)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The sensor log
// ---------------------------------------------------------------------------

/// Where hook rows are recorded.
///
/// Deliberately NOT `audit.jsonl`. One active Claude Code session fires
/// `PostToolUse` on every `Read`, `Edit` and `Bash`, which is one to two orders of
/// magnitude more rows than gateway traffic; sharing the file would push the
/// governance rows out through the audit log's own trim, so the compliance artifact
/// would be evicted by telemetry. Separate files, separate caps; SBS-823 merges them
/// at read time.
pub fn log_path() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("hooks.jsonl"))
}

/// Outcome of one observed native tool call.
///
/// `None` means unknown and must never be counted as success. The harness does not
/// promise a machine-readable outcome on every tool, so an absent `ok` is the normal
/// case rather than an error - which is exactly why defaulting it to `true` would
/// quietly inflate a success rate (the SBS-932 rule, applied before the bug).
pub fn hook_call_ok(entry: &Value) -> Option<bool> {
    entry.get("ok").and_then(Value::as_bool)
}

/// Read a tool call's outcome out of a `PostToolUse` payload, when it says.
fn tool_outcome(payload: &Value) -> Option<bool> {
    let response = payload.get("tool_response")?;
    if let Some(success) = response.get("success").and_then(Value::as_bool) {
        return Some(success);
    }
    // An `error` that is present and not null is a failure. Its VALUE is not read:
    // an error string can carry the same content the rest of this module drops.
    match response.get("error") {
        Some(Value::Null) | None => None,
        Some(_) => Some(false),
    }
}

/// Build the row for one hook event.
///
/// Pure, so the "no content is ever stored" contract is unit-testable against a
/// payload carrying a secret. Everything recorded is either a name, an identifier,
/// a path, or a hash.
pub fn row(event_arg: &str, payload: &Value) -> Value {
    let mut entry = json!({
        "ts": epoch_millis(),
        "event": event_arg,
        "agent": "claude-code",
    });

    if let Some(session) = payload.get("session_id").and_then(Value::as_str) {
        entry["sessionId"] = json!(session);
    }
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
        entry["cwd"] = json!(cwd);
    }
    if let Some(tool) = payload.get("tool_name").and_then(Value::as_str) {
        entry["tool"] = json!(tool);
    }
    // Identity without content, from the same canonical hash the gateway audit uses,
    // so a repeated call is recognisable across both logs.
    if let Some(input) = payload.get("tool_input") {
        entry["argsHash"] = json!(crate::audit::args_hash(input));
    }
    if let Some(ok) = tool_outcome(payload) {
        entry["ok"] = json!(ok);
    }
    entry
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read a hook payload from `reader`, bounded by [`MAX_HOOK_STDIN_BYTES`].
///
/// Returns the bytes read and whether the cap was reached. A payload at the cap is
/// truncated, so it will not parse, and [`handle_event`] records the flagged
/// `malformed` row rather than guessing at half a document.
///
/// This bounds SIZE, not TIME: the read still ends at EOF. The harness closes the pipe
/// after writing and kills a hook that overruns its own timeout, so waiting on EOF is
/// bounded by the caller rather than by us.
pub fn read_payload(reader: impl std::io::Read) -> (String, bool) {
    read_payload_capped(reader, MAX_HOOK_STDIN_BYTES)
}

/// [`read_payload`] with an explicit cap. The guard (SBS-1059) reads far more than the
/// sensor: a `beforeReadFile` payload embeds the file's content, and a guard that cannot
/// see a call must fail closed, so a small cap would turn every large read into a denial.
pub fn read_payload_capped(reader: impl std::io::Read, cap: u64) -> (String, bool) {
    use std::io::Read as _;

    // BYTES, not a String, and this is load-bearing for the exit-0 guarantee:
    //
    //   * `String::truncate` panics when the index is not a char boundary. A capped
    //     payload whose 1 MiB mark lands inside a multi-byte codepoint - any non-ASCII
    //     in a `tool_input` or a `Bash` stdout - would panic the hook process, which
    //     exits non-zero, which is the one thing this path must never do.
    //   * `read_to_string` fails with `InvalidData` on invalid UTF-8 and leaves the
    //     buffer as it found it. Cutting a stream at a fixed byte count produces
    //     exactly that whenever the cut lands mid-codepoint, so the payload, and the
    //     fact that it was truncated at all, would both be silently lost.
    //
    // Truncating bytes and converting lossily cannot do either. A cut codepoint
    // becomes U+FFFD, the JSON then fails to parse, and the event is recorded as the
    // flagged `malformed` row, which is the honest outcome.
    let mut bytes: Vec<u8> = Vec::new();
    // cap + 1 so filling the cap is distinguishable from a payload that ends exactly
    // on it.
    let _ = reader.take(cap + 1).read_to_end(&mut bytes);
    let truncated = bytes.len() as u64 > cap;
    if truncated {
        bytes.truncate(cap as usize);
    }
    (String::from_utf8_lossy(&bytes).into_owned(), truncated)
}

/// Handle one hook invocation, from the gateway binary's `--toolport-hook` path.
///
/// **Never fails the caller.** The binary exits 0 whatever happens here, because a
/// non-zero exit from a hook is a signal to the harness, and on some events that
/// signal stops the user's work. Nothing is printed to stdout either: the harness
/// reads hook stdout, so noise there lands in the user's session. Diagnostics go to
/// the gateway log.
pub fn handle_event(event_arg: &str, stdin: &str) {
    if !SENSOR_EVENTS.iter().any(|(_, arg)| *arg == event_arg) {
        // A settings.json written by another build naming an event this one does not
        // record. Dropping it is correct; saying so is what makes it debuggable.
        crate::gatewaylog::append(&format!(
            "toolport: ignoring unknown hook event {event_arg:?}"
        ));
        return;
    }

    let entry = match serde_json::from_str::<Value>(stdin) {
        Ok(payload) => row(event_arg, &payload),
        Err(error) => {
            crate::gatewaylog::append(&format!(
                "toolport: hook '{event_arg}' payload did not parse: {error}"
            ));
            // Recorded anyway, flagged, and with no `tool` key so no reader can
            // mistake it for an observed call. Silence here would be indistinguishable
            // from the agent doing nothing.
            json!({
                "ts": epoch_millis(),
                "event": event_arg,
                "agent": "claude-code",
                "malformed": true,
            })
        }
    };

    let Some(path) = log_path() else {
        crate::gatewaylog::append("toolport: hook row dropped, no data directory");
        return;
    };
    if let Err(error) = crate::registry::append_line_locked(
        &path,
        &entry.to_string(),
        MAX_HOOK_LOG_BYTES,
        KEEP_HOOK_LINES,
        None,
    ) {
        crate::gatewaylog::append(&format!("toolport: hook row dropped: {error}"));
    }
}

/// The most recent `limit` rows, newest first.
///
/// A missing file is an empty log. Any other IO error is returned, so a caller cannot
/// show "no agent activity" for a log it simply failed to read (SBS-873).
///
/// Read as bytes, not `read_to_string`. This is the same log
/// [`crate::registry::append_line_locked`] just conceded can hold invalid UTF-8 - one
/// hook process killed mid-write leaves a half-codepoint line - and `read_to_string`
/// answers `Err(InvalidData)` for the whole file over that one byte. The activity view
/// would then report "couldn't read your agent activity" for every row until a rotation
/// happened to trim the bad line out. Lossy decoding costs the torn line, which
/// `from_str` was going to reject anyway, and keeps every intact row (SBS-822 review).
pub fn read_recent(limit: usize) -> std::io::Result<Vec<Value>> {
    let Some(path) = log_path() else {
        return Ok(Vec::new());
    };
    let content = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(content
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(limit)
        .collect())
}

// ---------------------------------------------------------------------------
// Install / remove
// ---------------------------------------------------------------------------

/// The gateway binary the hook command should invoke.
///
/// The same resolution client MCP install uses, so the sensor inherits the install
/// location, version pinning, pruning and signing that path already has.
///
/// NOT `gateway_publish::client_gateway_path` on its own: that returns `None` unless
/// `should_publish_client_gateway()`, which is hard-false on macOS and Linux and on any
/// Windows `cargo run`. Using it alone made the sensor un-installable everywhere except
/// a packaged Windows build, while `hook_command` quoted `/Applications/...` paths it
/// could never actually produce (SBS-822 review).
fn hook_binary() -> Option<PathBuf> {
    crate::clients::resolve_gateway_path().filter(|p| p.is_file())
}

/// [`hook_binary`] for callers that must not write.
///
/// `resolve_gateway_path` publishes the bundled gateway, and copies one on AppImage,
/// when nothing is published yet. Rendering a read-only view or a dry run must not
/// create files, so this resolves only what already exists.
fn hook_binary_readonly() -> Option<PathBuf> {
    crate::clients::resolve_gateway_path_readonly()
}

/// Re-apply at app start, so the sensor keeps pointing at a gateway that exists.
///
/// The published gateway path is VERSIONED, and the reaper prunes superseded builds.
/// An update therefore leaves every installed hook naming a binary that is about to
/// disappear, and the harness would run a missing command once per tool call. Because
/// [`upsert_hooks`] is idempotent and rewrites a stale binary path, one apply on the
/// launch after an update repairs every profile; a launch with nothing to change
/// writes no bytes ([`install_at`] compares first).
pub fn apply_on_startup() {
    let enabled = crate::registry::load()
        .map(|reg| reg.hooks_enabled || !reg.hook_targets.is_empty())
        .unwrap_or(false);
    if !enabled {
        return;
    }
    if let Err(error) = apply() {
        crate::gatewaylog::append(&format!("toolport: hook sensor startup apply failed: {error}"));
    }
}

/// Current state, without writing anything.
pub fn view() -> HooksView {
    let reg = crate::registry::load().unwrap_or_default();
    let binary = hook_binary_readonly();
    let profiles = crate::clients::claude_settings_paths()
        .into_iter()
        .map(|path| match crate::clients::read_settings_json(&path) {
            Ok((root, _)) => ProfileStatus {
                path: path.display().to_string(),
                installed: is_installed(&root),
                error: None,
            },
            Err(error) => ProfileStatus {
                path: path.display().to_string(),
                installed: false,
                error: Some(error),
            },
        })
        .collect();

    HooksView {
        enabled: reg.hooks_enabled,
        events: SENSOR_EVENTS
            .iter()
            .map(|(event, _)| (*event).to_string())
            .collect(),
        profiles,
        binary: binary.map(|p| p.display().to_string()),
    }
}

/// Turn the sensor on or off, then make every profile match.
///
/// The opt-in and the install commit together. Persisting `hooks_enabled = true` first
/// and applying second meant a failed install left the registry saying the user opted
/// in with nothing installed, `view()` reporting `enabled` with no profile, and every
/// later startup retrying the same failing apply, with no path back (SBS-822 review).
pub fn set_enabled(enabled: bool) -> Result<HooksView, String> {
    apply_with(Some(enabled))?;
    Ok(view())
}

/// Write the sensor into every profile when enabled, and remove it from every
/// recorded target when not.
///
/// A profile that fails is reported, not fatal: one unreadable `settings.json` must
/// not stop the other profiles from being installed or, more importantly, cleaned.
pub fn apply() -> Result<Vec<ProfileStatus>, String> {
    apply_with(None)
}

/// [`apply`], optionally committing an opt-in change in the same transaction.
///
/// Everything runs inside `registry::update_authoritative`, holding the registry's
/// cross-process lock from the one authoritative load through every settings write and
/// the `hook_targets` save. Three things that buys, all of which the earlier
/// load-then-write-then-update shape got wrong (SBS-822 review):
///
///   * `registry::load` discards its [`crate::registry::LoadSource`], so a backup
///     snapshot read because the primary was missing or unreadable looked like live
///     opt-in state. `update_authoritative` refuses a non-authoritative registry
///     BEFORE the closure runs, so no settings file is touched on that path. Previously
///     the refusal came after the writes.
///   * A concurrent `set_enabled(false)` could commit while a slow apply was mid-flight,
///     and the apply would then reinstall the sensor the user just turned off.
///   * An `Err` out of the closure means nothing is saved, so a failed install cannot
///     leave `hooks_enabled = true` behind.
///
/// Matches `rules::apply_to`, which holds the same lock across the same shape of work.
fn apply_with(set_enabled: Option<bool>) -> Result<Vec<ProfileStatus>, String> {
    apply_to(
        set_enabled,
        &crate::clients::claude_settings_paths(),
        &hook_binary,
    )
}

/// [`apply_with`] over an explicit profile set and binary, so tests drive known files
/// instead of the developer's real `~/.claude`. Same rationale as `rules::apply_to`.
///
/// The binary arrives as a resolver, not a path, because resolving it is not free:
/// `hook_binary` goes through `clients::resolve_gateway_path`, which publishes the
/// bundled gateway on packaged Windows and copies one into the data directory on
/// AppImage. Passing an already-resolved `Option<&Path>` meant turning the sensor OFF -
/// or a startup pass that only had stale targets to clean up - created files for a
/// feature nobody was installing. Only the enable branch calls it (SBS-822 review).
fn apply_to(
    set_enabled: Option<bool>,
    profiles: &[PathBuf],
    resolve_binary: &dyn Fn() -> Option<PathBuf>,
) -> Result<Vec<ProfileStatus>, String> {
    let (_, statuses) = crate::registry::update_authoritative(|reg| {
        if let Some(enabled) = set_enabled {
            reg.hooks_enabled = enabled;
        }
        let mut statuses: Vec<ProfileStatus> = Vec::new();
        let previous: Vec<String> = reg.hook_targets.clone();

        // Every profile that SHOULD carry the sensor right now. Empty when the user has
        // opted out, which is what turns this pass into a full removal.
        let candidates: Vec<String> = if reg.hooks_enabled {
            profiles.iter().map(|p| p.display().to_string()).collect()
        } else {
            Vec::new()
        };

        let mut written: Vec<String> = Vec::new();
        if reg.hooks_enabled {
            // Resolved before any write. Failing here aborts the whole transaction, so
            // an un-installable build leaves the opt-in exactly as it was.
            let binary = resolve_binary().ok_or_else(|| {
                "no gateway binary is available to run as a hook".to_string()
            })?;
            for path in candidates.iter().map(Path::new) {
                statuses.push(match install_at(path, &binary) {
                    Ok(()) => {
                        written.push(path.display().to_string());
                        ProfileStatus {
                            path: path.display().to_string(),
                            installed: true,
                            error: None,
                        }
                    }
                    Err(error) => ProfileStatus {
                        path: path.display().to_string(),
                        installed: false,
                        error: Some(error),
                    },
                });
            }
        }

        // Clean what we recorded and no longer want: a profile that disappeared, or
        // every profile once the user opts out. Same contract as `rules_targets`.
        //
        // Cleanup is keyed on "no longer a candidate", NOT on "failed to write this
        // pass". A transient read failure must not be read as "this profile is gone"
        // and trigger an uninstall of a sensor that is working.
        let mut unremovable: Vec<String> = Vec::new();
        for stale in reg.hook_targets.iter().filter(|p| !candidates.contains(p)) {
            if let Err(error) = remove_at(Path::new(stale)) {
                // Keep it on the list. Dropping a path whose removal failed would
                // strand the file forever, because nothing would ever try again
                // (SBS-914 is the same bug on the rules path).
                unremovable.push(stale.clone());
                statuses.push(ProfileStatus {
                    path: stale.clone(),
                    installed: false,
                    error: Some(error),
                });
            }
        }

        // What we now believe may carry our block on disk. A candidate earns a place by
        // being written this pass, OR by already being recorded.
        //
        // The second half matters and is not the same as "record every candidate". A
        // profile whose `settings.json` we could not parse may still hold a block from
        // an earlier successful pass, and we cannot tell, precisely because we could not
        // read it; forgetting it would strand a sensor that is still firing (SBS-914).
        // But a profile we have never written and cannot read has no such history, so it
        // is not recorded, and a broken file the user never enabled does not become a
        // permanent retry.
        let mut targets: Vec<String> = candidates
            .iter()
            .filter(|p| written.contains(p) || previous.contains(p))
            .cloned()
            .collect();
        targets.extend(unremovable);
        reg.hook_targets = targets;
        Ok(statuses)
    })?;
    Ok(statuses)
}

/// Install the sensor into one settings file. No-op (and no backup) when the file
/// already says exactly what we would write.
fn install_at(path: &Path, binary: &Path) -> Result<(), String> {
    let (root, original) = crate::clients::read_settings_json(path)?;
    let updated = upsert_hooks(&root, binary)?;
    if updated == root {
        return Ok(());
    }
    crate::clients::write_settings_json(path, original.as_deref(), &updated)
}

/// Remove the sensor from one settings file.
///
/// A file that is gone is success: the profile it belonged to was deleted, which is a
/// stronger form of removed. A file we cannot parse is NOT success - refusing loudly
/// beats rewriting a file we do not understand.
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

/// The exact bytes the write would produce, for every profile. Writes nothing, anywhere.
///
/// `after` goes through the same renderer as the real write
/// ([`crate::clients::render_settings_json`]), not `to_string_pretty`. Pretty-printing
/// the value reformats the document and drops every comment, so the dry run would tell
/// the user that enabling the sensor is about to destroy their annotations when the
/// actual write preserves them (SBS-822 review).
///
/// The binary is resolved read-only for the same reason the doc says "writes nothing":
/// the normal resolver publishes a versioned gateway and a manifest into the data dir
/// when none exists, which a dry run must not do. When nothing is published yet this
/// reports that instead of publishing one.
pub fn preview() -> Result<Vec<HooksPreview>, String> {
    let binary = hook_binary_readonly().ok_or_else(|| {
        "no gateway binary has been published yet, so there is nothing to preview".to_string()
    })?;
    preview_to(&crate::clients::claude_settings_paths(), &binary)
}

/// [`preview`] over an explicit profile set and binary, so tests drive known files
/// instead of the developer's real `~/.claude`. Same rationale as [`apply_to`].
///
/// Per profile, not all-or-nothing. `?`-ing the first bad `settings.json` out meant one
/// hand-broken file - a stray comma in a `.claude-backup` nobody uses - hid the dry run
/// for every healthy profile, and the error named none of them. [`apply_to`] already
/// takes the opposite position, so the preview also disagreed with what the install it
/// is previewing would actually do (SBS-822 review).
fn preview_to(profiles: &[PathBuf], binary: &Path) -> Result<Vec<HooksPreview>, String> {
    let mut out = Vec::new();
    for path in profiles {
        let rendered = crate::clients::read_settings_json(path).and_then(|(root, original)| {
            let updated = upsert_hooks(&root, binary)?;
            let after = crate::clients::render_settings_json(original.as_deref(), &updated)?;
            Ok((original.unwrap_or_default(), after))
        });
        out.push(match rendered {
            Ok((before, after)) => HooksPreview {
                path: path.display().to_string(),
                before,
                after,
                error: None,
            },
            Err(error) => HooksPreview {
                path: path.display().to_string(),
                before: String::new(),
                after: String::new(),
                error: Some(error),
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary() -> PathBuf {
        PathBuf::from("/opt/Toolport/toolport-gateway")
    }

    fn our_command() -> String {
        hook_command(&binary(), "tool")
    }

    fn foreign_group() -> Value {
        json!({
            "matcher": "Bash",
            "hooks": [{ "type": "command", "command": "/usr/local/bin/my-linter" }]
        })
    }

    #[test]
    fn upsert_into_a_fresh_file_registers_every_sensor_event_once() {
        let out = upsert_hooks(&json!({}), &binary()).unwrap();
        let hooks = out["hooks"].as_object().unwrap();

        assert_eq!(hooks.len(), SENSOR_EVENTS.len());
        for (event, arg) in SENSOR_EVENTS {
            let groups = hooks[event].as_array().unwrap();
            assert_eq!(groups.len(), 1, "{event} should have exactly one group");
            let entries = groups[0]["hooks"].as_array().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["command"], json!(hook_command(&binary(), arg)));
            assert_eq!(entries[0]["type"], json!("command"));
        }
    }

    #[test]
    fn pre_tool_use_is_never_registered() {
        let out = upsert_hooks(&json!({}), &binary()).unwrap();
        assert!(
            out["hooks"].get("PreToolUse").is_none(),
            "the sensor must not register on the blocking event"
        );
        assert!(!SENSOR_EVENTS.iter().any(|(event, _)| *event == "PreToolUse"));
    }

    #[test]
    fn upsert_is_idempotent_and_refreshes_a_stale_binary_path() {
        let once = upsert_hooks(&json!({}), &binary()).unwrap();
        let twice = upsert_hooks(&once, &binary()).unwrap();
        assert_eq!(once, twice, "re-applying must not accumulate entries");

        // An entry an older build wrote, pointing at a gateway that has since been
        // pruned, is replaced rather than joined.
        let stale = upsert_hooks(&json!({}), Path::new("/old/toolport-gateway")).unwrap();
        let fresh = upsert_hooks(&stale, &binary()).unwrap();
        let entries = fresh["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(
            entries[0]["hooks"][0]["command"],
            json!(hook_command(&binary(), "tool"))
        );
    }

    #[test]
    fn foreign_hooks_and_unrelated_settings_survive_a_round_trip() {
        let before = json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(git status)"] },
            "hooks": {
                "PostToolUse": [foreign_group()],
                "PreToolUse": [foreign_group()],
            }
        });

        let installed = upsert_hooks(&before, &binary()).unwrap();
        assert_eq!(installed["model"], json!("opus"));
        assert_eq!(installed["permissions"]["allow"][0], json!("Bash(git status)"));
        // Ours is added alongside theirs, and their PreToolUse hook is untouched.
        assert_eq!(installed["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(installed["hooks"]["PreToolUse"], json!([foreign_group()]));

        let removed = strip_hooks(&installed);
        assert_eq!(removed, before, "uninstall must restore the file exactly");
    }

    #[test]
    fn strip_removes_only_our_entry_from_a_shared_group() {
        // A user who hand-edited our group to add their own hook keeps it.
        let shared = json!({
            "hooks": {
                "PostToolUse": [{
                    "hooks": [
                        { "type": "command", "command": our_command() },
                        { "type": "command", "command": "/usr/local/bin/my-linter" },
                    ]
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
        let installed = upsert_hooks(&json!({ "model": "opus" }), &binary()).unwrap();
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
        assert!(!is_installed(&theirs));
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
    fn is_installed_tracks_upsert_and_strip() {
        let empty = json!({});
        assert!(!is_installed(&empty));
        let installed = upsert_hooks(&empty, &binary()).unwrap();
        assert!(is_installed(&installed));
        assert!(!is_installed(&strip_hooks(&installed)));
    }

    #[test]
    fn a_hooks_key_of_the_wrong_type_is_refused_not_overwritten() {
        let broken = json!({ "hooks": "off" });
        assert!(upsert_hooks(&broken, &binary()).is_err());

        let broken_event = json!({ "hooks": { "PostToolUse": {} } });
        let err = upsert_hooks(&broken_event, &binary()).unwrap_err();
        assert!(err.contains("PostToolUse"), "unexpected error: {err}");
    }

    #[test]
    fn a_non_object_root_is_refused() {
        assert!(upsert_hooks(&json!([1, 2, 3]), &binary()).is_err());
    }

    #[test]
    fn the_command_is_quoted_so_an_installed_path_with_spaces_survives_a_shell() {
        let command = hook_command(Path::new("/Applications/My Toolport/toolport-gateway"), "tool");
        assert!(command.starts_with('"'));
        assert!(command.contains("My Toolport"));
        assert!(command.ends_with(&format!("{HOOK_MARKER} tool")));
    }

    #[test]
    fn a_shell_metacharacter_in_the_binary_path_is_refused() {
        let error = upsert_hooks(
            &json!({}),
            Path::new("/home/user/Toolport$(touch injected)/toolport-gateway"),
        )
        .unwrap_err();
        assert!(error.contains("cannot be represented safely"));
    }

    #[test]
    fn a_row_stores_identity_and_never_content() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let payload = json!({
            "session_id": "sess-1",
            "cwd": "/home/dev/app",
            "tool_name": "Bash",
            "tool_input": { "command": format!("aws configure set key {secret}") },
            "tool_response": { "success": true, "stdout": secret },
        });

        let row = row("tool", &payload);
        let text = row.to_string();

        assert!(
            !text.contains(secret),
            "a hook row must never carry payload content: {text}"
        );
        assert!(!text.contains("aws configure"));
        assert_eq!(row["tool"], json!("Bash"));
        assert_eq!(row["sessionId"], json!("sess-1"));
        assert_eq!(row["cwd"], json!("/home/dev/app"));
        assert_eq!(row["ok"], json!(true));
        assert_eq!(
            row["argsHash"],
            json!(crate::audit::args_hash(&payload["tool_input"]))
        );
        assert!(row["ts"].as_u64().is_some());
    }

    #[test]
    fn an_undecidable_outcome_is_absent_rather_than_success() {
        // No tool_response at all.
        let row = row("tool", &json!({ "tool_name": "Read" }));
        assert!(row.get("ok").is_none());
        assert_eq!(hook_call_ok(&row), None);

        // A response that simply does not say.
        let quiet = row_for(json!({ "tool_response": { "stdout": "hi" } }));
        assert!(quiet.get("ok").is_none());

        // An explicit null error is not a failure, and not a success either.
        let null_error = row_for(json!({ "tool_response": { "error": Value::Null } }));
        assert!(null_error.get("ok").is_none());
    }

    #[test]
    fn an_error_in_the_response_is_a_failed_call() {
        let failed = row_for(json!({ "tool_response": { "error": "no such file" } }));
        assert_eq!(hook_call_ok(&failed), Some(false));
        assert!(
            !failed.to_string().contains("no such file"),
            "the error VALUE must not be stored, only the outcome"
        );

        let explicit = row_for(json!({ "tool_response": { "success": false } }));
        assert_eq!(hook_call_ok(&explicit), Some(false));
    }

    fn row_for(payload: Value) -> Value {
        row("tool", &payload)
    }

    #[test]
    fn a_session_row_carries_no_tool_and_no_outcome() {
        let row = row("session-start", &json!({ "session_id": "s", "cwd": "/w" }));
        assert!(row.get("tool").is_none());
        assert!(row.get("ok").is_none());
        assert!(row.get("argsHash").is_none());
        assert_eq!(row["event"], json!("session-start"));
    }

    #[test]
    fn hook_call_ok_never_infers_success_from_a_missing_field() {
        assert_eq!(hook_call_ok(&json!({ "tool": "Bash" })), None);
        assert_eq!(hook_call_ok(&json!({ "ok": true })), Some(true));
        assert_eq!(hook_call_ok(&json!({ "ok": "yes" })), None);
    }

    // --- on-disk behavior ---------------------------------------------------

    /// A scratch directory that is ALSO the process-global data dir for the test.
    ///
    /// Both halves are required. `install_at` on an existing file takes a backup, and
    /// backups land under `registry::conduit_dir()`, so a test without the override
    /// writes into the real Toolport data directory and races every other test doing
    /// the same. That is what failed on CI Windows with "the system cannot find the
    /// path specified" while passing locally.
    ///
    /// Field order IS drop order: the override is cleared before the lock is released,
    /// so no other test can observe this scratch dir.
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
        ScratchEnv { dir, _override: over, _lock: lock }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn install_creates_a_settings_file_that_did_not_exist() {
        let env = scratch_env("fresh");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");

        install_at(&path, &binary()).unwrap();

        let root: Value = serde_json::from_str(&read(&path)).unwrap();
        assert!(is_installed(&root));
        // A file we created holds nothing but our block.
        assert_eq!(root.as_object().unwrap().len(), 1);

    }

    #[test]
    fn install_preserves_comments_and_every_unrelated_setting() {
        let env = scratch_env("comments");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let original = "{\n  // the model I actually want\n  \"model\": \"opus\",\n  \"permissions\": { \"allow\": [\"Bash(git status)\"] }\n}\n";
        std::fs::write(&path, original).unwrap();

        install_at(&path, &binary()).unwrap();

        let after = read(&path);
        assert!(
            after.contains("// the model I actually want"),
            "the comment must survive the write: {after}"
        );
        assert!(after.contains("\"model\": \"opus\""));
        assert!(after.contains(HOOK_MARKER));

        // Removing puts every byte outside the `hooks` key back as it was.
        remove_at(&path).unwrap();
        let restored = read(&path);
        assert!(restored.contains("// the model I actually want"));
        assert!(!restored.contains(HOOK_MARKER));
        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert!(
            root.get("hooks").is_none(),
            "uninstall must not leave an empty hooks object: {restored}"
        );

    }

    #[test]
    fn install_is_a_no_op_when_the_file_already_says_what_we_would_write() {
        let env = scratch_env("noop");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");

        install_at(&path, &binary()).unwrap();
        let first = read(&path);
        install_at(&path, &binary()).unwrap();

        assert_eq!(first, read(&path), "a second apply must not rewrite bytes");

    }

    #[test]
    fn a_duplicate_hooks_key_is_refused_and_the_file_is_left_alone() {
        let env = scratch_env("dupe");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        // Ambiguous: a rewrite would silently drop one of the two.
        let original = "{ \"hooks\": {}, \"hooks\": {} }";
        std::fs::write(&path, original).unwrap();

        assert!(install_at(&path, &binary()).is_err());
        assert_eq!(read(&path), original, "the file must be untouched");

    }

    #[test]
    fn a_malformed_settings_file_is_refused_rather_than_replaced() {
        let env = scratch_env("malformed");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();

        assert!(install_at(&path, &binary()).is_err());
        assert!(remove_at(&path).is_err(), "removal must not rewrite it either");
        assert_eq!(read(&path), original);

    }

    #[test]
    fn removing_from_a_profile_that_no_longer_exists_is_success() {
        let env = scratch_env("gone");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        // A profile the user deleted between apply and remove is a stronger form of
        // "removed", not an error to report.
        assert!(remove_at(&path).is_ok());
    }

    #[test]
    fn removing_leaves_a_foreign_hook_on_the_same_event_intact() {
        let env = scratch_env("foreign");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "PostToolUse": [foreign_group()] }
            }))
            .unwrap(),
        )
        .unwrap();

        install_at(&path, &binary()).unwrap();
        remove_at(&path).unwrap();

        let root: Value = crate::clients::read_settings_json(&path).unwrap().0;
        assert_eq!(root["hooks"]["PostToolUse"], json!([foreign_group()]));

    }

    #[test]
    fn handle_event_records_a_row_without_a_registry() {
        // The hook path runs before any gateway startup, so it must work against a
        // data directory that holds nothing at all - no registry.json, no keychain.
        // This is the "does not touch the registry" property from the spec.
        let env = scratch_env("no-registry");
        let dir = env.dir.clone();

        handle_event(
            "tool",
            &json!({ "session_id": "s1", "tool_name": "Bash", "tool_input": { "command": "ls" } })
                .to_string(),
        );

        let rows = read_recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tool"], json!("Bash"));
        assert_eq!(rows[0]["sessionId"], json!("s1"));
        assert!(!dir.join("registry.json").exists(), "no registry was created");

    }

    #[test]
    fn handle_event_survives_junk_stdin_and_an_unknown_event() {
        let _env = scratch_env("junk");

        // Unparseable payload: recorded, flagged, and with no `tool` key so no reader
        // can mistake it for an observed call.
        handle_event("tool", "not json at all");
        // An event this build does not record is dropped, not recorded as junk.
        handle_event("no-such-event\nforged-record", "{}");

        let rows = read_recent(10).unwrap();
        assert_eq!(rows.len(), 1, "only the malformed-payload row is kept");
        assert_eq!(rows[0]["malformed"], json!(true));
        assert!(rows[0].get("tool").is_none());
        assert!(rows[0].get("ok").is_none());

        let gateway_log =
            std::fs::read_to_string(crate::registry::gateway_log_path().unwrap()).unwrap();
        assert_eq!(
            gateway_log.lines().count(),
            2,
            "an unknown event must not forge another log record: {gateway_log}"
        );
        assert!(gateway_log.contains(r#"no-such-event\nforged-record"#));
    }

    #[test]
    fn recorded_rows_are_one_json_line_each_and_newest_first() {
        let _env = scratch_env("order");

        handle_event("session-start", &json!({ "session_id": "s1" }).to_string());
        handle_event("tool", &json!({ "session_id": "s1", "tool_name": "Read" }).to_string());
        handle_event("session-end", &json!({ "session_id": "s1" }).to_string());

        let raw = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert_eq!(raw.lines().count(), 3, "one line per event: {raw}");

        let rows = read_recent(10).unwrap();
        assert_eq!(rows[0]["event"], json!("session-end"));
        assert_eq!(rows[2]["event"], json!("session-start"));

    }

    #[test]
    fn startup_apply_does_nothing_when_the_sensor_was_never_turned_on() {
        // The overwhelmingly common launch. It must not scan profiles, resolve a
        // gateway binary, or write to the registry, because it runs on the same
        // thread as every other launch task.
        let env = scratch_env("startup-off");
        let dir = env.dir.clone();

        apply_on_startup();

        assert!(
            !dir.join("registry.json").exists(),
            "a launch with the sensor off must not write anything"
        );

    }

    #[test]
    fn a_target_whose_removal_failed_stays_recorded_for_the_next_pass() {
        // The SBS-914 shape: dropping a path we could not clean strands the file,
        // because nothing would ever try again.
        let env = scratch_env("retry");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json }").unwrap();

        assert!(
            remove_at(&path).is_err(),
            "an unparseable file must not report a successful removal"
        );
        assert!(path.exists(), "and must not be deleted or rewritten");

    }

    #[test]
    fn the_hook_binary_resolves_on_every_platform_not_just_packaged_windows() {
        // `gateway_publish::client_gateway_path` is gated on
        // `should_publish_client_gateway()`, which is false on macOS, Linux, and any
        // Windows `cargo run`. Resolving through it alone made the sensor
        // un-installable everywhere but a packaged Windows build (SBS-822 review).
        // This test runs on all three CI platforms, and in dev, where that gate is off.
        assert!(
            !crate::gateway_publish::should_publish_client_gateway(),
            "test premise: this build is not publish-capable, so a publish-only \
             resolver returns None here"
        );
        assert!(
            crate::gateway_publish::client_gateway_path().is_none(),
            "test premise: the publish-gated resolver is the one that returns None"
        );
        // `hook_binary` is this resolver, filtered to a file that exists. It is the
        // difference between the two that the bug was: one of them can answer on this
        // platform and the other cannot.
        assert!(
            crate::clients::resolve_gateway_path().is_some(),
            "the resolver the sensor uses must still answer where the publish-gated \
             one cannot"
        );
    }

    #[test]
    fn a_capped_payload_is_recorded_as_malformed_and_never_carries_its_content() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        // A single tool_response far past the cap, the shape a big Read or Bash
        // produces.
        let huge = json!({
            "session_id": "s1",
            "tool_name": "Read",
            "tool_response": { "stdout": format!("{}{secret}", "x".repeat(2 * 1024 * 1024)) },
        })
        .to_string();

        let (payload, truncated) = read_payload(huge.as_bytes());
        assert!(truncated, "a payload past the cap must report as truncated");
        assert_eq!(payload.len() as u64, MAX_HOOK_STDIN_BYTES);
        assert!(
            !payload.contains(secret),
            "the cap must land before the tail of the response"
        );

        // Truncated JSON does not parse, so this lands on the flagged row rather than
        // a guess at half a document.
        assert!(serde_json::from_str::<Value>(&payload).is_err());
    }

    #[test]
    fn a_payload_inside_the_cap_is_read_whole() {
        let payload = json!({ "session_id": "s1", "tool_name": "Bash" }).to_string();
        let (read, truncated) = read_payload(payload.as_bytes());
        assert!(!truncated);
        assert_eq!(read, payload);
    }

    #[test]
    fn a_cap_landing_inside_a_codepoint_does_not_panic() {
        // The exit-0 guarantee's sharpest edge. `String::truncate` panics off a char
        // boundary, and a hook process that panics exits non-zero, which on some events
        // stops the user's work. Three-byte characters mean the 1 MiB mark lands inside
        // one, not on it.
        let mut payload = String::from("{\"tool_input\":\"");
        while payload.len() < (MAX_HOOK_STDIN_BYTES as usize + 64) {
            payload.push('あ');
        }

        let (read, truncated) = read_payload(payload.as_bytes());
        assert!(truncated);
        // Bounded, but not by the byte cap exactly: a cut codepoint becomes U+FFFD,
        // which is 3 bytes and can be wider than the 1-2 bytes it replaced. At most one
        // sits at the end, so the slack is a constant, not a multiplier.
        assert!(
            read.len() <= MAX_HOOK_STDIN_BYTES as usize + 3,
            "payload grew past the cap by more than one replacement char: {}",
            read.len()
        );
        // Lossy conversion, so a cut codepoint is a replacement char, never a panic and
        // never invalid UTF-8.
        assert!(std::str::from_utf8(read.as_bytes()).is_ok());
    }

    #[test]
    fn invalid_utf8_input_still_produces_a_payload() {
        // `read_to_string` would fail with InvalidData and leave the buffer empty, so
        // both the payload AND the fact it was oversized would be lost. Reading bytes
        // cannot do that.
        let mut raw: Vec<u8> = b"{\"tool_name\":\"Bash\",\"x\":\"".to_vec();
        raw.push(0xff); // never valid UTF-8
        raw.extend_from_slice(b"\"}");

        let (read, truncated) = read_payload(raw.as_slice());
        assert!(!truncated);
        assert!(read.contains("Bash"), "the readable part must survive: {read}");
        assert!(read.contains('\u{fffd}'), "the bad byte becomes a replacement char");
    }

    #[test]
    fn a_profile_we_never_wrote_and_cannot_read_is_not_recorded_as_a_target() {
        // Recording a candidate we never wrote makes a file the user broke into a
        // permanent retry: every later pass tries to remove a block that was never
        // there and fails on the same parse error.
        let env = scratch_env("never-written");
        let dir = env.dir.clone();
        let profile = dir.join("settings.json");
        std::fs::write(&profile, "{ not json").unwrap();

        let statuses = apply_to(Some(true), &[profile.clone()], &|| Some(binary())).unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].installed);
        assert!(statuses[0].error.is_some(), "the failure must be reported");
        assert!(
            crate::registry::load().unwrap().hook_targets.is_empty(),
            "a profile we never wrote must not be recorded as ours"
        );

    }

    #[test]
    fn a_profile_we_did_write_stays_recorded_when_a_later_pass_cannot_read_it() {
        // The other half, and the one the naive "record only successful writes" rule
        // gets wrong: a block we really did write must stay tracked through a failure,
        // or a later disable will not clean it and the sensor keeps firing (SBS-914).
        let env = scratch_env("written-then-broken");
        let dir = env.dir.clone();
        let profile = dir.join("settings.json");

        apply_to(Some(true), &[profile.clone()], &|| Some(binary())).unwrap();
        assert_eq!(
            crate::registry::load().unwrap().hook_targets,
            vec![profile.display().to_string()]
        );

        // The user (or another tool) breaks the file after we installed into it.
        std::fs::write(&profile, "{ broken").unwrap();
        apply_to(None, &[profile.clone()], &|| Some(binary())).unwrap();
        assert_eq!(
            crate::registry::load().unwrap().hook_targets,
            vec![profile.display().to_string()],
            "a profile we wrote must stay recorded through a read failure"
        );

    }

    #[test]
    fn preview_text_is_the_bytes_install_actually_writes() {
        // The dry run must not claim a comment is about to disappear when the real
        // write keeps it (SBS-822 review).
        let env = scratch_env("preview-bytes");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");
        let original =
            "{\n  // the model I actually want\n  \"model\": \"opus\"\n}\n";
        std::fs::write(&path, original).unwrap();

        // Drive preview() itself, not just the renderer underneath it, so swapping the
        // dry run back to `to_string_pretty` is caught.
        let previews = preview_to(&[path.clone()], &binary()).unwrap();
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].before, original);

        install_at(&path, &binary()).unwrap();
        assert_eq!(
            previews[0].after,
            read(&path),
            "preview text and the written file must be byte-identical"
        );
        assert!(previews[0].after.contains("// the model I actually want"));

    }

    #[test]
    fn preview_writes_nothing_at_all() {
        let env = scratch_env("preview-readonly");
        let dir = env.dir.clone();
        let path = dir.join("settings.json");

        // A profile with no settings file yet: the dry run must not create one.
        let previews = preview_to(&[path.clone()], &binary()).unwrap();
        assert!(previews[0].before.is_empty());
        assert!(previews[0].after.contains(HOOK_MARKER));
        assert!(!path.exists(), "a dry run must not create the settings file");

    }

    #[test]
    fn one_unreadable_profile_does_not_hide_the_preview_for_the_healthy_ones() {
        let env = scratch_env("preview-degrades");
        let dir = env.dir.clone();
        let broken = dir.join("broken.json");
        let healthy = dir.join("healthy.json");
        std::fs::write(&broken, "{ this is not json").unwrap();
        std::fs::write(&healthy, "{ \"model\": \"opus\" }").unwrap();

        // Broken first, so an all-or-nothing `?` would take the healthy one down with it.
        let previews = preview_to(&[broken.clone(), healthy.clone()], &binary()).unwrap();

        assert_eq!(previews.len(), 2, "every profile gets a row");
        assert!(
            previews[0].error.is_some(),
            "the unreadable profile must say why it has no dry run"
        );
        assert!(previews[0].after.is_empty());
        assert!(
            previews[1].error.is_none() && previews[1].after.contains(HOOK_MARKER),
            "the healthy profile still previews: {:?}",
            previews[1]
        );

    }

    #[test]
    fn enabling_does_not_persist_the_opt_in_when_the_install_cannot_run() {
        // A failed apply must leave the registry exactly as it was. Otherwise `view()`
        // reports enabled with nothing installed and every startup retries the same
        // failing apply forever, with no path back (SBS-822 review).
        let env = scratch_env("enable-fails");
        let dir = env.dir.clone();
        let profile = dir.join(".claude").join("settings.json");

        // No resolvable binary is the real-world case this models: every non-Windows
        // build before a gateway is published.
        let err = apply_to(Some(true), &[profile.clone()], &|| None).unwrap_err();
        assert!(err.contains("gateway binary"), "unexpected error: {err}");

        assert!(
            !crate::registry::load().unwrap().hooks_enabled,
            "a failed enable must not leave hooks_enabled = true behind"
        );
        assert!(
            !profile.exists(),
            "and must not have written a settings file first"
        );

    }

    #[test]
    fn enabling_commits_the_opt_in_and_the_targets_together() {
        let env = scratch_env("enable-commits");
        let dir = env.dir.clone();
        let profile = dir.join(".claude").join("settings.json");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();

        apply_to(Some(true), &[profile.clone()], &|| Some(binary())).unwrap();

        let reg = crate::registry::load().unwrap();
        assert!(reg.hooks_enabled);
        assert_eq!(reg.hook_targets, vec![profile.display().to_string()]);
        assert!(is_installed(
            &crate::clients::read_settings_json(&profile).unwrap().0
        ));

        // Opting out removes the sensor and forgets the target in one transaction.
        // Deliberately with NO binary: turning the sensor off must never depend on
        // resolving one, or a user whose gateway went missing could not uninstall the
        // hooks it left in their settings.
        apply_to(Some(false), &[profile.clone()], &|| None).unwrap();
        let reg = crate::registry::load().unwrap();
        assert!(!reg.hooks_enabled);
        assert!(reg.hook_targets.is_empty());
        assert!(!is_installed(
            &crate::clients::read_settings_json(&profile).unwrap().0
        ));

    }

    #[test]
    fn read_recent_keeps_every_intact_row_around_a_torn_one() {
        let env = scratch_env("torn");
        let dir = env.dir.clone();
        let path = dir.join("hooks.jsonl");

        // A hook killed mid-write leaves a half-codepoint line. `append_line_locked`
        // already had to concede this file can hold invalid UTF-8; a reader that answers
        // `Err` for the whole log over that one byte would blank the activity view until
        // some later rotation happened to trim it out.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(br#"{"event":"tool","tool":"Read"}"#);
        bytes.push(b'\n');
        bytes.extend_from_slice(&[0xE2, 0x82]); // a truncated euro sign
        bytes.push(b'\n');
        bytes.extend_from_slice(br#"{"event":"tool","tool":"Bash"}"#);
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();

        let rows = read_recent(10).expect("one torn line must not fail the whole log");
        let tools: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("tool").and_then(Value::as_str))
            .collect();
        assert_eq!(tools, vec!["Bash", "Read"], "newest first, torn line dropped");

    }

    #[test]
    fn disabling_never_resolves_a_gateway_binary() {
        let env = scratch_env("disable-lazy");
        let dir = env.dir.clone();
        let profile = dir.join("settings.json");
        apply_to(Some(true), &[profile.clone()], &|| Some(binary())).unwrap();

        // Resolving is not free: `hook_binary` publishes the bundled gateway on packaged
        // Windows and copies one into the data directory on AppImage. Turning the sensor
        // OFF must not create files for a feature the user is removing.
        let calls = std::cell::Cell::new(0u32);
        apply_to(Some(false), &[profile.clone()], &|| {
            calls.set(calls.get() + 1);
            Some(binary())
        })
        .unwrap();
        assert_eq!(calls.get(), 0, "the disable path asked for a binary");

    }

    #[test]
    fn read_recent_reports_an_unreadable_log_instead_of_saying_nothing_happened() {
        let env = scratch_env("unreadable");
        let dir = env.dir.clone();

        // A directory where the log file should be: readable as a path, not as a file.
        std::fs::create_dir_all(dir.join("hooks.jsonl")).unwrap();
        assert!(
            read_recent(10).is_err(),
            "an unreadable log must not read as an empty one (SBS-873)"
        );

    }
}
