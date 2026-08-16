//! Tool-call audit log.
//!
//! Every tool call routed through the gateway is appended here as one JSON line.
//! This is the artifact the governance/MSP story is built on: a record of which
//! AI tool invoked which server's tool, and when. Local and append-only.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Trim the log once it passes this size, so it can't grow without bound.
const MAX_AUDIT_BYTES: u64 = 4 * 1024 * 1024;
/// Cap on a stored error message. Enough to show why a call failed, bounded so a
/// pathological error string can't bloat the log line.
const MAX_AUDIT_ERR_CHARS: usize = 600;
/// How many of the most recent lines to keep when trimming. Comfortably more than
/// any dashboard window, so the trim is invisible to the stats/log views.
const KEEP_LINES: usize = 5000;

pub fn audit_path() -> Option<PathBuf> {
    // Same anchor as the registry, so the app and a client-spawned gateway (which
    // may run under MSIX virtualization) write to the *same* audit log.
    Some(crate::registry::conduit_dir()?.join("audit.jsonl"))
}

/// Delete the audit log (called when the user clears retained activity). Returns
/// `Err` only on a real removal failure; a missing file (nothing to clear) is
/// success, so the caller can honestly confirm the log is gone rather than report a
/// false "cleared". Local and irreversible; the next call re-creates the file.
pub fn try_clear() -> std::io::Result<()> {
    let Some(path) = audit_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Append a tool-call record including how long the call took. Powers the
/// in-app latency/error-rate dashboard. `error` is a short message for a failed
/// call so the Activity view can show *why* it failed; it is an error string
/// only, never tool arguments or result data, which stay out of this
/// append-only governance log.
pub fn record_timed(
    server: &str,
    tool: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
) {
    record_timed_with_hash(server, tool, ok, duration_ms, error, client, None);
}

/// One pseudonymization pass's outcome, recorded so a reader can tell whether PII
/// redaction did anything on this call -- and, far more importantly, when it did not
/// fully apply.
///
/// Only the COUNT is ever recorded. The values are the entire point of the feature and
/// must not reach this append-only log, which already holds only an `argsHash` for the
/// same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiiPass {
    /// How many values were replaced with tokens.
    pub replaced: usize,
    /// False when the pass left values in the clear: the session map hit its cap, or
    /// the result exceeded the scan cap. This path fails OPEN by design, so an
    /// incomplete pass is the case a governance reader most needs to see.
    pub complete: bool,
}

/// Same as [`record_timed`], optionally attaching a canonical args hash (SOU-171 export).
/// Never stores the arguments themselves.
pub fn record_timed_with_hash(
    server: &str,
    tool: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
    args_hash: Option<&str>,
) {
    record_timed_with_pii(
        server,
        tool,
        ok,
        duration_ms,
        error,
        client,
        None,
        args_hash,
        None,
    )
}

/// Same as [`record_timed_with_hash`], also recording the call's pseudonymization pass.
///
/// `pii` is `None` when PII redaction was off for this call, which is deliberately
/// distinct from `Some(PiiPass { replaced: 0, .. })` -- "the feature is not on" and "the
/// feature ran and found nothing" are different facts, and collapsing them would make a
/// disabled feature look like a clean call (SBS-607).
#[allow(clippy::too_many_arguments)]
pub fn record_timed_with_pii(
    server: &str,
    tool: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
    client_name: Option<&str>,
    args_hash: Option<&str>,
    pii: Option<PiiPass>,
) {
    write_line(&timed_entry(
        server,
        tool,
        ok,
        duration_ms,
        error,
        client,
        client_name,
        args_hash,
        pii,
    ));
}

/// Build the tool-call audit entry. Pure (no I/O) so the record's shape is unit-testable,
/// like [`decision_entry`] on the approval path.
#[allow(clippy::too_many_arguments)]
fn timed_entry(
    server: &str,
    tool: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
    client_name: Option<&str>,
    args_hash: Option<&str>,
    pii: Option<PiiPass>,
) -> Value {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": server,
        "tool": tool,
        "ok": ok,
    });
    if let Some(pass) = pii {
        entry["piiReplaced"] = json!(pass.replaced);
        // Written only when something WAS left in the clear, so the anomalous case is
        // greppable in the log rather than buried in a field that is usually `false`.
        if !pass.complete {
            entry["piiIncomplete"] = json!(true);
        }
    }
    if let Some(ms) = duration_ms {
        entry["durationMs"] = json!(ms);
    }
    // Which client made the call (a registered HTTP client's label), so the audit
    // log answers "who invoked this?". Absent for the local stdio client / open tokens.
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    if let Some(name) = client_name.filter(|name| !name.is_empty()) {
        entry["clientName"] = json!(name);
    }
    if let Some(h) = args_hash.filter(|h| !h.is_empty()) {
        entry["argsHash"] = json!(h);
    }
    if !ok {
        if let Some(err) = error {
            let trimmed: String = err.trim().chars().take(MAX_AUDIT_ERR_CHARS).collect();
            if !trimmed.is_empty() {
                entry["error"] = json!(trimmed);
            }
        }
    }
    entry
}

/// Record a destructive call that was held for confirmation. This is the
/// confirm-destructive feature working, not a failure, so `ok: true` keeps it out of
/// the error rate; the `held` flag lets the UI mark it as held rather than as a
/// (misleading) successful destructive call.
pub fn record_held(server: &str, tool: &str, client: Option<&str>) {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": server,
        "tool": tool,
        "ok": true,
        "held": true,
    });
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    write_line(&entry);
}

/// Build the audit entry for a gated HITL decision. Pure (no I/O) so it's unit-testable.
/// `ok:true` keeps governance outcomes out of the error rate; `held` is true for every
/// blocked outcome and false for `approved` (which ran), so the held-row UI stays honest;
/// the added fields (`kind`, `reason`, `decision`,
/// `argsHash`) let a governance / Approvals view tell *why* a call was gated and *which*
/// way it resolved (approved vs denied vs no-response vs unreachable vs stale-state) apart -
/// which the old flat `record_held` collapsed into one indistinguishable record. `reason`
/// is the snake_case [`crate::approval::ApprovalReason`]; `decision` is `approved` |
/// `denied` | `no_response` | `unreachable` | `stale_state` (the last: a human approved but
/// the arguments were mutated before execute, so the stale approval was rejected). The RAW
/// arguments are never stored - only `argsHash` - so the log proves which exact call was
/// decided without persisting secrets/PII from arguments.
fn decision_entry(
    server: &str,
    tool: &str,
    client: Option<&str>,
    reason: &str,
    decision: &str,
    args_hash: &str,
    held_ms: Option<u64>,
) -> Value {
    // `held` = the call was gated and did NOT run. An `approved` decision ran, so it is not
    // held (it must not inflate the held count); every non-approval was blocked, so it is.
    // `ok:true` throughout keeps governance outcomes (a deny, a timeout) out of the error rate.
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": server,
        "tool": tool,
        "ok": true,
        "held": decision != "approved",
        "kind": "approval",
        "reason": reason,
        "decision": decision,
        "argsHash": args_hash,
    });
    // The approval wait, recorded as `heldMs` (not `durationMs`) so a governance view can
    // tell how long a human was asked apart from a call's downstream execution duration.
    if let Some(ms) = held_ms {
        entry["heldMs"] = json!(ms);
    }
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    entry
}

/// Record a gated HITL decision (the human approved/denied it, it timed out, or the
/// broker was unreachable). Replaces the flat `record_held` on the approval path so the
/// audit can distinguish the outcomes. Hashes the arguments; never stores them raw.
pub fn record_decision(
    server: &str,
    tool: &str,
    client: Option<&str>,
    reason: &str,
    decision: &str,
    args: &Value,
    held_ms: Option<u64>,
) {
    write_line(&decision_entry(
        server,
        tool,
        client,
        reason,
        decision,
        &args_hash(args),
        held_ms,
    ));
}

/// Record a Routine management outcome without persisting source, schemas, or invocation
/// arguments. `routine_id` and `content_hash` are stable identifiers safe for governance.
pub fn record_routine(
    action: &str,
    routine_id: &str,
    content_hash: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
) {
    write_line(&routine_entry(
        action,
        routine_id,
        content_hash,
        ok,
        duration_ms,
        error,
        client,
    ));
}

/// Record objective Code Run promotion evidence without retaining source, input, arguments,
/// intermediate results, final results, or approval tokens.
/// One advisor hint actually rendered into a tool result. Carries tool names, counts,
/// and the candidate runId - never argument values or draft source - so hint-shown →
/// save conversion is measurable from the audit log alone.
pub fn record_advisor_hint(
    tier: &str,
    tool: &str,
    calls: usize,
    run_id: Option<&str>,
    client: Option<&str>,
) {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": "toolport",
        "tool": "routine.advisor.hint_shown",
        "kind": "routine",
        "action": "hint_shown",
        "tier": tier,
        "patternTool": tool,
        "calls": calls,
    });
    if let Some(run_id) = run_id {
        entry["runId"] = json!(run_id);
    }
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        entry["client"] = json!(client);
    }
    write_line(&entry);
}

/// A strong candidate was handed to the desktop app's passive suggestion area. The
/// decision is recorded at publish time (delivery is fire-and-forget), carrying only
/// identity and counts - never source, schema, or argument values.
pub fn record_suggestion_published(
    definition_fingerprint: &str,
    calls: usize,
    provenance: crate::routines::EvidenceProvenance,
    client: Option<&str>,
) {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": "toolport",
        "tool": "routine.suggestion.published",
        "kind": "routine",
        "action": "suggestion_published",
        "definitionFingerprint": definition_fingerprint,
        "calls": calls,
        "provenance": provenance,
    });
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        entry["client"] = json!(client);
    }
    write_line(&entry);
}

pub fn record_candidate(
    assessment: &crate::routine_candidates::CandidateAssessment,
    calls: usize,
    duration_ms: u64,
    client: Option<&str>,
) {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": "toolport",
        "tool": "code.run.candidate_assessed",
        "kind": "routine",
        "action": "candidate_assessed",
        "runId": assessment.run_id,
        "sourceHash": assessment.source_hash,
        "eligible": assessment.eligible,
        "promotionAvailable": assessment.promotion_available,
        "recommendation": assessment.recommendation,
        "reasonCodes": assessment.reason_codes,
        "observedTools": assessment.observed_tools,
        "riskClass": assessment.risk_class,
        "provenance": assessment.provenance,
        "calls": calls,
        "durationMs": duration_ms,
    });
    if let Some(reason) = assessment.promotion_unavailable_reason {
        entry["promotionUnavailableReason"] = json!(reason);
    }
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        entry["client"] = json!(client);
    }
    write_line(&entry);
}

fn routine_entry(
    action: &str,
    routine_id: &str,
    content_hash: &str,
    ok: bool,
    duration_ms: Option<u64>,
    error: Option<&str>,
    client: Option<&str>,
) -> Value {
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        "server": "toolport",
        "tool": format!("routine.{action}"),
        "kind": "routine",
        "action": action,
        "routineId": routine_id,
        "contentHash": content_hash,
        "ok": ok,
    });
    if let Some(ms) = duration_ms {
        entry["durationMs"] = json!(ms);
    }
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    if !ok {
        if let Some(error) = error {
            let trimmed: String = error.trim().chars().take(MAX_AUDIT_ERR_CHARS).collect();
            if !trimmed.is_empty() {
                entry["error"] = json!(trimmed);
            }
        }
    }
    entry
}

/// A stable SHA-256 (hex) of a call's arguments over a canonical JSON serialization
/// (object keys sorted recursively), so the same logical call always hashes the same
/// regardless of key order. This is the content-binding foundation: it proves "the exact
/// call that was approved is the one that ran" without persisting the arguments themselves.
pub fn args_hash(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    {
        let mut writer = DigestWriter(&mut hasher);
        write_canonical_json(&mut writer, value);
    }
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

struct DigestWriter<'a>(&'a mut sha2::Sha256);

impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stream canonical JSON into a writer instead of recursively allocating a
/// `String` for every object, array, and scalar. Object keys remain sorted,
/// preserving the exact stable hash contract used by approvals and audit export.
fn write_canonical_json(writer: &mut impl std::io::Write, value: &Value) {
    match value {
        Value::Object(map) => {
            let _ = writer.write_all(b"{");
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    let _ = writer.write_all(b",");
                }
                write_json_scalar(writer, key);
                let _ = writer.write_all(b":");
                write_canonical_json(writer, &map[key]);
            }
            let _ = writer.write_all(b"}");
        }
        Value::Array(arr) => {
            let _ = writer.write_all(b"[");
            for (index, item) in arr.iter().enumerate() {
                if index > 0 {
                    let _ = writer.write_all(b",");
                }
                write_canonical_json(writer, item);
            }
            let _ = writer.write_all(b"]");
        }
        scalar => write_json_scalar(writer, scalar),
    }
}

fn write_json_scalar<T: serde::Serialize>(writer: &mut impl std::io::Write, value: &T) {
    let _ = serde_json::to_writer(&mut *writer, value);
}

#[cfg(test)]
fn canonical_json(value: &Value) -> String {
    let mut bytes = Vec::new();
    write_canonical_json(&mut bytes, value);
    String::from_utf8(bytes).unwrap_or_default()
}

/// Build the audit entry for an agent-control server toggle. Pure (no I/O) so the
/// scope-proof invariant is unit-testable: on a denied out-of-scope attempt the lookup
/// never resolves the target, so `resolvedServerId` is null and the record can't reveal
/// whether an out-of-scope server exists. `decision` is one of `enabled`, `disabled`,
/// `noop_already`, `unresolved`, `agent_control_off`.
fn agent_toggle_entry(
    client: Option<&str>,
    profile: &str,
    action: &str,
    requested_target: &str,
    resolved_server_id: Option<&str>,
    decision: &str,
    scoped: bool,
) -> Value {
    let ok = matches!(decision, "enabled" | "disabled" | "noop_already");
    let mut entry = json!({
        "ts": epoch_millis() as u64,
        // A synthetic server/tool pair so the audit table renders this like any row.
        "server": "agent-control",
        "tool": action,
        "ok": ok,
        "event": "agent_control.server_toggle",
        "requestedTarget": requested_target,
        // Null on a scoped miss: the whole point is that a denial doesn't name (or even
        // confirm the existence of) an out-of-scope server.
        "resolvedServerId": resolved_server_id,
        "decision": decision,
        "knownListScope": if scoped { "client_allowed_only" } else { "all" },
        "profile": profile,
    });
    if let Some(c) = client.filter(|c| !c.is_empty()) {
        entry["client"] = json!(c);
    }
    entry
}

/// Record an agent-control server toggle (toolport_enable_server / _disable_server) to
/// the audit log, so the log carries proof of the scope decision, not just the behavior.
pub fn record_agent_toggle(
    client: Option<&str>,
    profile: &str,
    action: &str,
    requested_target: &str,
    resolved_server_id: Option<&str>,
    decision: &str,
    scoped: bool,
) {
    write_line(&agent_toggle_entry(
        client,
        profile,
        action,
        requested_target,
        resolved_server_id,
        decision,
        scoped,
    ));
}

/// Append one entry as a single JSON line. Every client-spawned gateway takes the
/// same sibling-file lock through append, the size decision, and any rotation, so
/// a rotator cannot replace a record another process just reported as written.
fn write_line(entry: &Value) {
    let Some(path) = audit_path() else {
        return;
    };
    write_line_at(&path, entry);
}

fn write_line_at(path: &Path, entry: &Value) {
    write_line_at_with_rotation_hook(path, entry, None);
}

fn write_line_at_with_rotation_hook(
    path: &Path,
    entry: &Value,
    after_snapshot: Option<&mut dyn FnMut()>,
) {
    // Atomic replacement protects readers from partial files, but only this
    // shared cross-process critical section prevents a stale rotation snapshot
    // from replacing an append that completed in another gateway.
    let _lock = match crate::registry::lock_at(path) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!(
                "toolport: audit record dropped because the lock for '{}' could not be acquired: {error}",
                path.display()
            );
            return;
        }
    };
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    };
    // The registry normally created this directory before any call. Avoid a
    // redundant create-directory syscall on every append, but retain the
    // standalone/first-writer behavior by creating it and retrying on NotFound.
    let mut file = match open() {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match open() {
                Ok(file) => file,
                Err(_) => return,
            }
        }
        Err(_) => return,
    };
    let line = format!("{entry}\n");
    if file.write_all(line.as_bytes()).is_err() {
        return;
    }
    // Query size through the handle we already opened instead of reopening the
    // path for metadata. Drop it before rotation so Windows can atomically
    // replace the log when the cap is crossed.
    let size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    drop(file);
    rotate_if_large(path, size, after_snapshot);
}

/// Trim the audit log to its most recent `KEEP_LINES` lines once it exceeds the
/// size cap, so it stays bounded over months of use. Best-effort: a failure here
/// never affects the call being logged.
fn rotate_if_large(path: &Path, size: u64, after_snapshot: Option<&mut dyn FnMut()>) {
    if size <= MAX_AUDIT_BYTES {
        return;
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Some(hook) = after_snapshot {
            hook();
        }
        let trimmed = trimmed_tail(&content, KEEP_LINES);
        // Atomic + unique temp: every client's gateway shares this file, so a
        // bespoke fixed temp name could let two rotations collide.
        let _ = crate::registry::atomic_write(path, &trimmed);
    }
}

/// Keep the last `keep` non-empty lines of `content`, newline-terminated.
fn trimmed_tail(content: &str, keep: usize) -> String {
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(keep);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The most recent `limit` entries, newest first.
///
/// A missing file is an empty log: nothing has been written yet. Any other IO
/// error is returned so a caller cannot treat an unreadable existing file as
/// "no tool calls" / Protection active (SBS-873). Unparseable lines are skipped
/// — a mid-write or corrupt line is not an IO failure.
pub fn read_recent(limit: usize) -> std::io::Result<Vec<Value>> {
    let Some(path) = audit_path() else {
        return Ok(Vec::new());
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // Filter BEFORE take, matching `inspect::read_recent` and `searchtrace`. Taking
    // first let an unparseable line consume a slot, so one corrupt row among the
    // newest entries returned a short page and dropped older valid history that
    // should have filled it.
    Ok(content
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(limit)
        .collect())
}

/// Average and 95th-percentile of a duration sample, in ms. `None` when the
/// sample is empty (e.g. older records logged before latency was tracked).
fn latency(durs: &mut [u64]) -> (Option<u64>, Option<u64>) {
    if durs.is_empty() {
        return (None, None);
    }
    let sum: u64 = durs.iter().copied().fold(0u64, u64::saturating_add);
    let avg = sum / durs.len() as u64;
    durs.sort_unstable();
    // Nearest-rank p95.
    let idx = (((durs.len() as f64) * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(durs.len() - 1);
    (Some(avg), Some(durs[idx]))
}

/// Every retained entry, newest first. The log is size-capped (see `MAX_AUDIT_BYTES`), so
/// this stays bounded no matter how long Toolport has been running.
///
/// Same empty-vs-unreadable contract as [`read_recent`] (SBS-873).
pub fn read_all() -> std::io::Result<Vec<Value>> {
    let Some(path) = audit_path() else {
        return Ok(Vec::new());
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(content
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Aggregate the FULL retained log into per-server stats plus global totals: call volume,
/// error rate, and latency per server, computed locally. Totals are the real count of what's
/// retained (the byte cap bounds it), not a fixed window, so the error rate stays consistent
/// with the call count instead of being taken over an arbitrary slice.
pub fn stats() -> std::io::Result<Value> {
    Ok(aggregate(&read_all()?))
}

/// Pure aggregation of audit entries into per-server + global stats. Split from
/// `stats` so the dashboard math is testable without touching the on-disk log.
fn aggregate(entries: &[Value]) -> Value {
    use std::collections::HashMap;

    #[derive(Default)]
    struct ToolAgg {
        calls: u64,
        errors: u64,
        durs: Vec<u64>,
        last_ts: u64,
    }

    #[derive(Default)]
    struct Agg {
        calls: u64,
        errors: u64,
        durs: Vec<u64>,
        last_ts: u64,
        tools: HashMap<String, ToolAgg>,
    }

    let mut by_server: HashMap<String, Agg> = HashMap::new();
    let mut total = 0u64;
    let mut errors = 0u64;

    for e in entries {
        let server = e.get("server").and_then(|v| v.as_str()).unwrap_or("?");
        let tool = e.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
        let ok = e.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
        let ts = e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        let dur = e.get("durationMs").and_then(|v| v.as_u64());

        total += 1;
        if !ok {
            errors += 1;
        }
        let a = by_server.entry(server.to_string()).or_default();
        a.calls += 1;
        if !ok {
            a.errors += 1;
        }
        if let Some(d) = dur {
            a.durs.push(d);
        }
        a.last_ts = a.last_ts.max(ts);

        let t = a.tools.entry(tool.to_string()).or_default();
        t.calls += 1;
        if !ok {
            t.errors += 1;
        }
        if let Some(d) = dur {
            t.durs.push(d);
        }
        t.last_ts = t.last_ts.max(ts);
    }

    let mut servers: Vec<Value> = by_server
        .into_iter()
        .map(|(server, mut a)| {
            let (avg, p95) = latency(&mut a.durs);
            // Per-tool breakdown, busiest tool first.
            let mut tools: Vec<Value> = a
                .tools
                .into_iter()
                .map(|(tool, mut t)| {
                    let (tavg, tp95) = latency(&mut t.durs);
                    json!({
                        "tool": tool,
                        "calls": t.calls,
                        "errors": t.errors,
                        "errorRate": if t.calls > 0 { t.errors as f64 / t.calls as f64 } else { 0.0 },
                        "avgMs": tavg,
                        "p95Ms": tp95,
                        "lastTs": t.last_ts,
                    })
                })
                .collect();
            tools.sort_by(|x, y| {
                y.get("calls")
                    .and_then(|v| v.as_u64())
                    .cmp(&x.get("calls").and_then(|v| v.as_u64()))
            });
            json!({
                "server": server,
                "calls": a.calls,
                "errors": a.errors,
                "errorRate": if a.calls > 0 { a.errors as f64 / a.calls as f64 } else { 0.0 },
                "avgMs": avg,
                "p95Ms": p95,
                "lastTs": a.last_ts,
                "tools": tools,
            })
        })
        .collect();
    // Busiest servers first.
    servers.sort_by(|x, y| {
        y.get("calls")
            .and_then(|v| v.as_u64())
            .cmp(&x.get("calls").and_then(|v| v.as_u64()))
    });

    json!({
        "total": total,
        "errors": errors,
        "errorRate": if total > 0 { errors as f64 / total as f64 } else { 0.0 },
        "servers": servers,
    })
}

/// The columns exported to CSV, in order. Keys match the audit entry JSON.
const CSV_COLUMNS: &[&str] = &[
    "ts",
    "server",
    "tool",
    "client",
    "ok",
    "held",
    "kind",
    "reason",
    "decision",
    "argsHash",
    "durationMs",
    "heldMs",
    "action",
    "error",
    // Appended at the END so a consumer reading the export positionally keeps working.
    // How many values this call's result had pseudonymized, and whether any were left in
    // the clear. Counts only -- the values never enter this log.
    "piiReplaced",
    "piiIncomplete",
    "clientName",
];

/// Render audit `entries` as CSV (RFC-4180-ish: CRLF rows, quoted cells, doubled
/// internal quotes). Any cell whose text begins with a spreadsheet formula trigger
/// is prefixed with `'` so opening the file in Excel/Sheets can't execute it: the
/// audit log holds tool names and error text from untrusted downstream servers.
pub fn to_csv(entries: &[Value]) -> String {
    let mut out = String::new();
    out.push_str(&CSV_COLUMNS.join(","));
    out.push_str("\r\n");
    for e in entries {
        let row: Vec<String> = CSV_COLUMNS
            .iter()
            .map(|col| csv_cell(e.get(*col)))
            .collect();
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    out
}

/// One CSV cell: stringify the JSON value, neutralize a leading formula trigger,
/// then quote and escape.
fn csv_cell(value: Option<&Value>) -> String {
    let raw = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    };
    // Formula-injection guard (OWASP): a cell starting with one of these could be
    // executed as a formula by a spreadsheet, so shift it behind a quote.
    let guarded = if raw.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{raw}")
    } else {
        raw
    };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oversized_audit_content() -> String {
        let padding = "x".repeat(420);
        let mut content = String::with_capacity(MAX_AUDIT_BYTES as usize + 512 * 1024);
        for seq in 0..(KEEP_LINES * 2) {
            content.push_str(&json!({"server":"old","seq":seq,"padding":padding}).to_string());
            content.push('\n');
        }
        assert!(content.len() as u64 > MAX_AUDIT_BYTES);
        content
    }

    fn wait_for_path(path: &Path, label: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(path.exists(), "timed out waiting for {label}");
    }

    fn wait_for_child(child: &mut std::process::Child, label: &str) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match child.try_wait().expect("poll audit child") {
                Some(status) => return status,
                None if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("timed out waiting for {label}");
                }
            }
        }
    }

    struct AuditProcessFixture {
        root: PathBuf,
        children: Vec<std::process::Child>,
    }

    impl AuditProcessFixture {
        fn new(root: PathBuf) -> Self {
            std::fs::create_dir_all(&root).unwrap();
            Self {
                root,
                children: Vec::new(),
            }
        }
    }

    impl Drop for AuditProcessFixture {
        fn drop(&mut self) {
            for child in &mut self.children {
                if !matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn to_csv_has_header_and_a_row() {
        let entries = vec![json!({
            "ts": 1, "server": "gh", "tool": "search", "ok": true, "durationMs": 42
        })];
        let csv = to_csv(&entries);
        assert!(csv.starts_with(
            "ts,server,tool,client,ok,held,kind,reason,decision,argsHash,durationMs,heldMs,action,error,piiReplaced,piiIncomplete,clientName\r\n"
        ));
        assert!(csv.contains("\"gh\""));
        assert!(csv.contains("\"search\""));
        assert!(csv.contains("\"42\""));
        // A missing column renders as an empty quoted cell, not the word "null".
        assert!(csv.contains("\"\""));
        assert!(!csv.contains("null"));
        assert!(csv.ends_with("\r\n"));
    }

    #[test]
    fn to_csv_neutralizes_formula_injection() {
        // A malicious tool name / error that a spreadsheet would execute as a formula.
        let csv = to_csv(&[json!({
            "tool": "=cmd|'/c calc'!A1", "error": "@SUM(1+1)"
        })]);
        assert!(csv.contains("\"'=cmd|'/c calc'!A1\""), "got {csv}");
        assert!(csv.contains("\"'@SUM(1+1)\""), "got {csv}");
        // A benign value is left untouched (no stray leading quote).
        let benign = to_csv(&[json!({ "tool": "search" })]);
        assert!(benign.contains("\"search\""));
        assert!(!benign.contains("'search"));
    }

    #[test]
    fn to_csv_escapes_embedded_quotes() {
        let csv = to_csv(&[json!({ "error": "he said \"hi\"" })]);
        assert!(csv.contains("\"he said \"\"hi\"\"\""), "got {csv}");
    }

    #[test]
    fn routine_entry_records_duration_without_sensitive_payloads() {
        let success = routine_entry(
            "run",
            "routine-1",
            "deadbeef",
            true,
            Some(37),
            None,
            Some("cursor-work"),
        );
        assert_eq!(success["server"], "toolport");
        assert_eq!(success["tool"], "routine.run");
        assert_eq!(success["kind"], "routine");
        assert_eq!(success["action"], "run");
        assert_eq!(success["routineId"], "routine-1");
        assert_eq!(success["contentHash"], "deadbeef");
        assert_eq!(success["durationMs"], 37);
        assert_eq!(success["client"], "cursor-work");
        assert_eq!(success["ok"], true);
        assert!(success.get("source").is_none());
        assert!(success.get("inputSchema").is_none());
        assert!(success.get("arguments").is_none());

        let failed = routine_entry(
            "save",
            "routine-2",
            "cafebabe",
            false,
            None,
            Some("  validation failed  "),
            None,
        );
        assert!(failed.get("durationMs").is_none());
        assert!(failed.get("client").is_none());
        assert_eq!(failed["error"], "validation failed");
    }

    #[test]
    fn agent_toggle_denial_record_proves_scope_without_leaking() {
        // A scoped client's out-of-scope toggle: the lookup never resolves the target,
        // so the record must carry resolvedServerId=null, decision=unresolved, and a
        // client-scoped known-list flag, and must NOT name any out-of-scope server.
        let e = agent_toggle_entry(
            Some("cursor-work"),
            "coding",
            "enable",
            "Beta",
            None,
            "unresolved",
            true,
        );
        assert_eq!(e["event"], "agent_control.server_toggle");
        assert!(
            e["resolvedServerId"].is_null(),
            "a scoped miss must not resolve a server"
        );
        assert_eq!(e["decision"], "unresolved");
        assert_eq!(e["knownListScope"], "client_allowed_only");
        assert_eq!(e["ok"], false);
        assert_eq!(e["requestedTarget"], "Beta");
        assert_eq!(e["client"], "cursor-work");

        // A successful in-scope toggle resolves the real server id and reads as ok.
        let ok = agent_toggle_entry(
            None,
            "coding",
            "disable",
            "gh",
            Some("gh"),
            "disabled",
            true,
        );
        assert_eq!(ok["resolvedServerId"], "gh");
        assert_eq!(ok["decision"], "disabled");
        assert_eq!(ok["ok"], true);
        // Unattributed (local/stdio) call omits the client field entirely.
        assert!(ok.get("client").is_none());
    }

    #[test]
    fn latency_avg_and_p95() {
        let mut d = vec![10u64, 20, 30, 40, 100];
        let (avg, p95) = latency(&mut d);
        assert_eq!(avg, Some(40)); // (10+20+30+40+100)/5
        assert_eq!(p95, Some(100)); // nearest-rank p95 of 5 samples = last
        let (a, p) = latency(&mut []);
        assert_eq!((a, p), (None, None));
    }

    #[test]
    fn aggregate_groups_and_sorts_by_volume() {
        let entries = vec![
            json!({"server":"github","ok":true,"ts":100,"durationMs":10}),
            json!({"server":"github","ok":false,"ts":200,"durationMs":30}),
            json!({"server":"stripe","ok":true,"ts":150,"durationMs":20}),
            json!({"server":"github","ok":true,"ts":50}), // no duration
        ];
        let s = aggregate(&entries);
        assert_eq!(s["total"], 4);
        assert_eq!(s["errors"], 1);
        assert_eq!(s["errorRate"], 0.25);

        let servers = s["servers"].as_array().unwrap();
        // Busiest first: github (3 calls) before stripe (1).
        assert_eq!(servers[0]["server"], "github");
        assert_eq!(servers[0]["calls"], 3);
        assert_eq!(servers[0]["errors"], 1);
        assert_eq!(servers[0]["lastTs"], 200);
        assert_eq!(servers[0]["avgMs"], 20); // only the two durations: (10+30)/2
        assert_eq!(servers[1]["server"], "stripe");
        assert_eq!(servers[1]["calls"], 1);
    }

    #[test]
    fn aggregate_breaks_down_by_tool() {
        let entries = vec![
            json!({"server":"github","tool":"search","ok":true,"ts":10,"durationMs":10}),
            json!({"server":"github","tool":"search","ok":false,"ts":20,"durationMs":30}),
            json!({"server":"github","tool":"create_issue","ok":true,"ts":15,"durationMs":50}),
        ];
        let s = aggregate(&entries);
        let tools = s["servers"][0]["tools"].as_array().unwrap();
        // Busiest tool first: search (2 calls) before create_issue (1).
        assert_eq!(tools[0]["tool"], "search");
        assert_eq!(tools[0]["calls"], 2);
        assert_eq!(tools[0]["errors"], 1);
        assert_eq!(tools[0]["avgMs"], 20); // (10+30)/2
        assert_eq!(tools[1]["tool"], "create_issue");
        assert_eq!(tools[1]["calls"], 1);
    }

    #[test]
    fn aggregate_handles_empty() {
        let s = aggregate(&[]);
        assert_eq!(s["total"], 0);
        assert_eq!(s["errorRate"], 0.0);
        assert_eq!(s["servers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn trimmed_tail_keeps_last_n_lines() {
        assert_eq!(trimmed_tail("a\nb\nc\nd\ne\n", 2), "d\ne\n");
        // Fewer lines than the cap -> unchanged (re-normalized with trailing \n).
        assert_eq!(trimmed_tail("x\ny\n", 5), "x\ny\n");
        // Blank lines are dropped.
        assert_eq!(trimmed_tail("a\n\n\nb\n", 5), "a\nb\n");
        assert_eq!(trimmed_tail("", 5), "");
    }

    #[test]
    fn audit_append_creates_missing_parent_and_writes_one_json_line() {
        let root = std::env::temp_dir().join(format!(
            "toolport-audit-append-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        let path = root.join("nested").join("audit.jsonl");
        write_line_at(&path, &json!({"server":"fixture","ok":true}));
        let content = std::fs::read_to_string(&path).expect("audit line");
        assert!(content.ends_with('\n'));
        assert_eq!(content.lines().count(), 1);
        let entry: Value = serde_json::from_str(content.trim()).expect("valid JSON");
        assert_eq!(entry["server"], "fixture");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn audit_rotation_sentinel_child() {
        let Some(path) = std::env::var_os("TOOLPORT_AUDIT_SENTINEL_PATH") else {
            return;
        };
        let ready = PathBuf::from(
            std::env::var_os("TOOLPORT_AUDIT_SENTINEL_READY").expect("sentinel ready path"),
        );
        let go = PathBuf::from(
            std::env::var_os("TOOLPORT_AUDIT_SENTINEL_GO").expect("sentinel go path"),
        );
        let attempting = PathBuf::from(
            std::env::var_os("TOOLPORT_AUDIT_SENTINEL_ATTEMPTING")
                .expect("sentinel attempting path"),
        );
        let done = PathBuf::from(
            std::env::var_os("TOOLPORT_AUDIT_SENTINEL_DONE").expect("sentinel done path"),
        );

        std::fs::write(&ready, "ready").expect("signal sentinel child ready");
        wait_for_path(&go, "sentinel start signal");
        std::fs::write(&attempting, "attempting").expect("signal sentinel append attempt");
        write_line_at(
            Path::new(&path),
            &json!({"server":"sentinel","marker":"between-snapshot-and-replace"}),
        );
        std::fs::write(done, "done").expect("signal sentinel append complete");
    }

    #[test]
    fn rotation_serializes_a_sentinel_append_started_after_its_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "toolport-audit-sentinel-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        let mut fixture = AuditProcessFixture::new(root.clone());
        let path = root.join("audit.jsonl");
        let ready = root.join("ready");
        let go = root.join("go");
        let attempting = root.join("attempting");
        let done = root.join("done");
        std::fs::write(&path, oversized_audit_content()).unwrap();

        {
            let mut after_snapshot = || {
                fixture.children.push(
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "audit::tests::audit_rotation_sentinel_child",
                            "--nocapture",
                        ])
                        .env("TOOLPORT_AUDIT_SENTINEL_PATH", &path)
                        .env("TOOLPORT_AUDIT_SENTINEL_READY", &ready)
                        .env("TOOLPORT_AUDIT_SENTINEL_GO", &go)
                        .env("TOOLPORT_AUDIT_SENTINEL_ATTEMPTING", &attempting)
                        .env("TOOLPORT_AUDIT_SENTINEL_DONE", &done)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .expect("spawn independent sentinel appender"),
                );
                wait_for_path(&ready, "sentinel child readiness");
                std::fs::write(&go, "go").unwrap();
                wait_for_path(&attempting, "sentinel append attempt");
                std::thread::sleep(std::time::Duration::from_millis(100));
                assert!(
                    !done.exists(),
                    "a separate process must not append while rotation holds the lock"
                );
            };

            write_line_at_with_rotation_hook(
                &path,
                &json!({"server":"rotator","marker":"snapshot-owner"}),
                Some(&mut after_snapshot),
            );
        }
        let status = wait_for_child(&mut fixture.children[0], "sentinel child");
        assert!(status.success(), "sentinel child failed: {status}");
        assert!(
            done.exists(),
            "sentinel append must finish after rotation unlocks"
        );

        let content = std::fs::read_to_string(&path).unwrap();
        let entries: Vec<Value> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("retained audit JSONL must stay valid"))
            .collect();
        assert_eq!(entries.len(), KEEP_LINES + 1);
        assert!(entries
            .iter()
            .any(|entry| entry["marker"] == "snapshot-owner"));
        assert!(entries
            .iter()
            .any(|entry| entry["marker"] == "between-snapshot-and-replace"));
    }

    #[test]
    fn multiprocess_append_and_rotation_preserve_every_new_record() {
        const CHILDREN: usize = 4;
        const RECORDS_PER_CHILD: usize = 100;

        if let Some(child_id) = std::env::var_os("TOOLPORT_AUDIT_STRESS_CHILD") {
            let path = PathBuf::from(
                std::env::var_os("TOOLPORT_AUDIT_STRESS_PATH").expect("stress audit path"),
            );
            let ready = PathBuf::from(
                std::env::var_os("TOOLPORT_AUDIT_STRESS_READY").expect("stress ready path"),
            );
            let go = PathBuf::from(
                std::env::var_os("TOOLPORT_AUDIT_STRESS_GO").expect("stress go path"),
            );
            std::fs::write(&ready, "ready").expect("signal stress child ready");
            wait_for_path(&go, "multiprocess stress start signal");
            let child_id = child_id.to_string_lossy();
            for seq in 0..RECORDS_PER_CHILD {
                write_line_at(
                    &path,
                    &json!({"server":"stress","newId":format!("{child_id}-{seq}")}),
                );
            }
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "toolport-audit-multiprocess-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        let mut fixture = AuditProcessFixture::new(root.clone());
        let path = root.join("audit.jsonl");
        let go = root.join("go");
        std::fs::write(&path, oversized_audit_content()).unwrap();

        let executable = std::env::current_exe().unwrap();
        for child_id in 0..CHILDREN {
            let ready = root.join(format!("ready-{child_id}"));
            fixture.children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "audit::tests::multiprocess_append_and_rotation_preserve_every_new_record",
                        "--nocapture",
                    ])
                    .env("TOOLPORT_AUDIT_STRESS_CHILD", child_id.to_string())
                    .env("TOOLPORT_AUDIT_STRESS_PATH", &path)
                    .env("TOOLPORT_AUDIT_STRESS_READY", ready)
                    .env("TOOLPORT_AUDIT_STRESS_GO", &go)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn independent audit appender"),
            );
        }
        for child_id in 0..CHILDREN {
            wait_for_path(
                &root.join(format!("ready-{child_id}")),
                "audit stress child readiness",
            );
        }
        std::fs::write(&go, "go").unwrap();
        for (child_id, child) in fixture.children.iter_mut().enumerate() {
            let status = wait_for_child(child, "audit stress child");
            assert!(
                status.success(),
                "audit stress child {child_id} failed: {status}"
            );
        }

        let content = std::fs::read_to_string(&path).unwrap();
        let entries: Vec<Value> = content
            .lines()
            .map(|line| serde_json::from_str(line).expect("stress output must remain valid JSONL"))
            .collect();
        let new_ids: std::collections::HashSet<&str> = entries
            .iter()
            .filter_map(|entry| entry.get("newId").and_then(Value::as_str))
            .collect();
        assert_eq!(new_ids.len(), CHILDREN * RECORDS_PER_CHILD);
        for child_id in 0..CHILDREN {
            for seq in 0..RECORDS_PER_CHILD {
                let expected = format!("{child_id}-{seq}");
                assert!(new_ids.contains(expected.as_str()), "missing {expected}");
            }
        }
        assert!(entries.len() <= KEEP_LINES + CHILDREN * RECORDS_PER_CHILD);
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_AUDIT_BYTES);
    }

    #[test]
    fn decision_entry_records_outcome_and_never_stores_raw_args() {
        let e = decision_entry(
            "neon",
            "delete_branch",
            Some("claude"),
            "destructive",
            "unreachable",
            "deadbeef",
            Some(1234),
        );
        assert_eq!(e["kind"], "approval");
        assert_eq!(e["reason"], "destructive");
        assert_eq!(e["decision"], "unreachable");
        assert_eq!(e["argsHash"], "deadbeef");
        // The wait is `heldMs` (approval wait), not `durationMs` (downstream exec time).
        assert_eq!(e["heldMs"], 1234);
        assert!(e.get("durationMs").is_none());
        assert_eq!(e["client"], "claude");
        // Held (didn't run) but ok:true so it stays out of the error rate.
        assert_eq!(e["held"], true);
        assert_eq!(e["ok"], true);
        // The record is a hash + metadata only: raw arguments must never be present.
        assert!(e.get("arguments").is_none());

        // A distinct decision is distinguishable in the log - the whole point vs record_held.
        let denied = decision_entry("s", "t", None, "untrusted_source", "denied", "h", None);
        assert_eq!(denied["decision"], "denied");
        assert_eq!(denied["reason"], "untrusted_source");
        // Unattributed call omits the client field entirely.
        assert!(denied.get("client").is_none());
        // heldMs is optional.
        assert!(denied.get("heldMs").is_none());

        // An approved call RAN, so it is audited but not counted as held (still ok:true).
        let approved = decision_entry("s", "t", None, "destructive", "approved", "h", None);
        assert_eq!(approved["decision"], "approved");
        assert_eq!(approved["held"], false);
        assert_eq!(approved["ok"], true);
    }

    #[test]
    fn timed_entry_records_client_name_when_present() {
        let entry = timed_entry(
            "crm",
            "crm__lookup",
            true,
            Some(12),
            None,
            Some("http-client"),
            Some("claude"),
            Some("deadbeef"),
            None,
        );

        assert_eq!(entry["client"], "http-client");
        assert_eq!(entry["clientName"], "claude");
        assert_eq!(entry["argsHash"], "deadbeef");
    }

    #[test]
    fn timed_entry_omits_empty_or_missing_client_name() {
        let missing = timed_entry(
            "crm",
            "crm__lookup",
            true,
            Some(12),
            None,
            Some("http-client"),
            None,
            None,
            None,
        );

        assert!(missing.get("clientName").is_none());

        let empty = timed_entry(
            "crm",
            "crm__lookup",
            true,
            Some(12),
            None,
            Some("http-client"),
            Some(""),
            None,
            None,
        );

        assert!(empty.get("clientName").is_none());
    }

    #[test]
    fn timed_entry_records_the_pii_pass_as_a_count_and_flags_an_incomplete_one() {
        let pass = |replaced, complete| {
            timed_entry(
                "crm",
                "crm__lookup",
                true,
                Some(12),
                None,
                Some("claude"),
                None,
                Some("deadbeef"),
                Some(PiiPass { replaced, complete }),
            )
        };

        // A complete pass records the count only. `piiIncomplete` is written just for the
        // fail-open case, so its absence is the "fully applied" signal.
        let clean = pass(3, true);
        assert_eq!(clean["piiReplaced"], 3);
        assert!(clean.get("piiIncomplete").is_none());

        // The fail-open case is the one a reader must be able to see: values reached the
        // model in the clear even though redaction was on.
        let leaky = pass(2, false);
        assert_eq!(leaky["piiReplaced"], 2);
        assert_eq!(leaky["piiIncomplete"], true);

        // Redaction on but nothing detected is NOT the same fact as redaction off, so the
        // field is present and zero rather than absent.
        let nothing_found = pass(0, true);
        assert_eq!(nothing_found["piiReplaced"], 0);

        // Redaction off: no PII fields at all, so an untouched call can't be misread as a
        // clean redaction pass.
        let off = timed_entry(
            "crm",
            "crm__lookup",
            true,
            Some(12),
            None,
            Some("claude"),
            None,
            Some("deadbeef"),
            None,
        );
        assert!(off.get("piiReplaced").is_none());
        assert!(off.get("piiIncomplete").is_none());

        // Counts only: no field of this record may carry a detected value.
        let rendered = clean.to_string();
        assert!(!rendered.contains("@"), "no value may reach the audit log");
    }

    #[test]
    fn pii_counts_are_exported_to_csv() {
        // The CSV is the governance export; a count that only exists in the JSONL would be
        // invisible to the artifact an auditor actually receives.
        let entries = vec![timed_entry(
            "crm",
            "crm__lookup",
            true,
            Some(12),
            None,
            None,
            None,
            None,
            Some(PiiPass {
                replaced: 4,
                complete: false,
            }),
        )];
        let csv = to_csv(&entries);
        let (header, rows) = csv.split_once("\r\n").expect("a header and a row");
        let cols: Vec<&str> = header.split(',').collect();
        let cells: Vec<&str> = rows.trim_end().split(',').collect();
        let cell = |name: &str| {
            let at = cols
                .iter()
                .position(|c| *c == name)
                .expect("column present");
            cells[at].trim_matches('"').to_string()
        };
        assert_eq!(cell("piiReplaced"), "4");
        assert_eq!(cell("piiIncomplete"), "true");
    }

    #[test]
    fn args_hash_is_stable_across_key_order_and_binds_to_content() {
        // Key order must not change the hash (content-binding needs a canonical form).
        assert_eq!(
            args_hash(&json!({ "a": 1, "b": [2, 3], "c": { "x": 1, "y": 2 } })),
            args_hash(&json!({ "c": { "y": 2, "x": 1 }, "b": [2, 3], "a": 1 })),
        );
        // Different content -> different hash.
        assert_ne!(
            args_hash(&json!({ "table": "users" })),
            args_hash(&json!({ "table": "orders" })),
        );
        // Array order IS significant (it's part of the content).
        assert_ne!(args_hash(&json!([1, 2])), args_hash(&json!([2, 1])));
        // It's a SHA-256: 64 lowercase hex chars, and it never echoes the raw value.
        let h = args_hash(&json!({ "secret": "hunter2" }));
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!h.contains("hunter2"));
    }

    #[test]
    fn canonical_json_sorts_object_keys_recursively() {
        let value = json!({ "b": 1, "a": { "d": 4, "c": 3 } });
        assert_eq!(canonical_json(&value), r#"{"a":{"c":3,"d":4},"b":1}"#);
        // Regression guard: the streaming implementation must remain byte-for-byte
        // compatible with hashes persisted by previous Toolport versions.
        assert_eq!(
            args_hash(&value),
            "943d56ce0b02b80a8afcd12d849426226b68f2d8cd2840af8f6f93067f14c360"
        );
    }

    fn isolated_data_dir(label: &str) -> (crate::registry::DataDirOverride, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "toolport-audit-read-{label}-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch data dir");
        (crate::registry::DataDirOverride::set(&path), path)
    }

    /// A missing audit.jsonl is an empty log, not a load failure.
    #[test]
    fn read_recent_missing_file_is_ok_empty() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("missing");
        let path = audit_path().expect("audit path under override");
        assert!(!path.exists(), "fixture must not create the log");
        let entries = read_recent(10).expect("missing file is Ok empty");
        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A readable JSONL returns newest-first parsed rows.
    #[test]
    fn read_recent_readable_jsonl_is_newest_first() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("readable");
        let path = audit_path().expect("audit path under override");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "{\"i\":1}\n{\"i\":2}\n").unwrap();
        let entries = read_recent(10).expect("readable fixture");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["i"], 2);
        assert_eq!(entries[1]["i"], 1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// An existing but unreadable audit.jsonl must not look like "no tool calls"
    /// / Protection active (SBS-873).
    #[test]
    fn read_recent_unreadable_existing_path_is_err() {
        let _lock = crate::registry::data_dir_test_lock();
        let (_override, root) = isolated_data_dir("unreadable");
        let path = audit_path().expect("audit path under override");
        // IsADirectory: the log path exists but cannot be read as a file.
        std::fs::create_dir_all(&path).unwrap();
        let err = read_recent(10).expect_err("unreadable existing path must be Err");
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(root);
    }
}
