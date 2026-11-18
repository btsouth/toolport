import type { SecurityEvent } from "./api";

/** High-signal, interrupting events vs benign, quiet-history churn. The backend now
 * tags a `severity`; for events written before that (no field) we classify by type:
 * poison / injected-result / lost-baseline are high, a plain tool_drift is benign.
 * `warn` (SEC-01 definition-content drift) rides the actionable tier for display. */
export function eventSeverity(e: SecurityEvent): "high" | "info" {
  if (e.severity === "high" || e.severity === "warn") return "high";
  if (e.severity === "info") return "info";
  if (
    e.type === "tool_poison_flag" ||
    e.type === "result_injection" ||
    e.type === "result_injection_blocked" ||
    e.type === "pins_load_failed"
  ) {
    return "high";
  }
  return "info";
}

/** The identity severity: the recorded tier when present, else the display tier. Keeps
 * a `warn` definition-content drift distinct from a later `high` event on the same tool. */
function severityIdentity(e: SecurityEvent): string {
  return e.severity ?? eventSeverity(e);
}

/** Durable per-event key for dismissal: identity (type, server, tool, change, severity),
 * NOT the timestamp. A benign drift that re-flags later (e.g. RevenueCat revising a beta
 * tool again) collapses to the same key, so dismissing it once keeps it dismissed instead
 * of returning as a brand-new, undismissable notice. Server + change are kept so tool-less
 * events (e.g. pins_load_failed) don't collide and dismiss each other. The RECORDED
 * severity is kept (not the display tier) so dismissing a tool's benign change never
 * masks a later HIGH-severity change on that same tool (e.g. it turns destructive) -
 * that must still interrupt. */
export function securityKey(e: SecurityEvent): string {
  const severity = severityIdentity(e);
  const identity = `${e.type}:${e.server ?? ""}:${e.tool ?? ""}:${e.change}:${severity}`;
  return e.profile ? `${identity}:profile=${e.profile}` : identity;
}

/** Review changes and actionable findings through their occurrence timestamp.
 * A later definition must reappear regardless of its severity. */
export function dismissalKey(e: SecurityEvent): string {
  const identity = securityKey(e);
  return e.type === "tool_drift" || eventSeverity(e) === "high"
    ? `${identity}@${e.ts}`
    : identity;
}

/** An acceptance marker covers this occurrence and earlier duplicate reports.
 * Later tool changes require a new review. */
export function dismissalIndex(dismissed: Set<string>): Map<string, number> {
  const index = new Map<string, number>();
  for (const marker of dismissed) {
    const separator = marker.lastIndexOf("@");
    if (separator < 0) continue;
    const through = Number(marker.slice(separator + 1));
    if (!Number.isFinite(through)) continue;
    const identity = marker.slice(0, separator);
    index.set(identity, Math.max(index.get(identity) ?? -Infinity, through));
  }
  return index;
}

function isDismissed(
  e: SecurityEvent,
  dismissed: Set<string>,
  index: Map<string, number>,
): boolean {
  const identity = securityKey(e);
  // Another profile can join a shared change after it was reviewed.
  if (e.type === "tool_drift" && e.profiles && e.blocked !== false) return false;
  if (
    e.type === "tool_drift" &&
    !e.new_fp &&
    e.blocked === false &&
    dismissed.has(identity)
  )
    return true;
  if (e.type !== "tool_drift" && eventSeverity(e) !== "high")
    return dismissed.has(identity);
  return (index.get(identity) ?? -Infinity) >= e.ts;
}

/** Each connected client runs its own gateway process, so a single server tool change
 * is flagged once PER client against the shared baseline, producing identical notices.
 * Collapse those: keep only the newest of any (type, server, tool, change, severity) seen
 * within a short window, so genuinely-separate changes over time are preserved. Severity
 * is part of the identity so a benign `info` revision can never collapse (and hide) a
 * later `high` one on the same tool - that loud signal must survive the dedupe. */
function dedupeSecurity(events: SecurityEvent[]): SecurityEvent[] {
  const WINDOW_MS = 10 * 60 * 1000;
  const newestFirst = [...events].sort((a, b) => b.ts - a.ts);
  const kept: SecurityEvent[] = [];
  const newestByIdentity = new Map<string, number>();
  for (const e of newestFirst) {
    const key = JSON.stringify([
      e.type,
      e.server,
      e.profile || "",
      e.tool,
      e.change,
      severityIdentity(e),
      e.type === "tool_drift" ? e.new_fp : null,
    ]);
    const newest = newestByIdentity.get(key);
    if (newest !== undefined && newest - e.ts <= WINDOW_MS) continue;
    newestByIdentity.set(key, e.ts);
    kept.push(e);
  }
  return kept;
}

export function selectSecurityEvents(
  events: SecurityEvent[],
  dismissed: Set<string>,
  index = dismissalIndex(dismissed),
): SecurityEvent[] {
  return dedupeSecurity(events).filter((event) => !isDismissed(event, dismissed, index));
}

/** Same rule as `integrity::tool_change_asks_review`: with reviews off only a change
 * verified unblocked, with no pending definition review, becomes plain history.
 * Blocked, unknown (`blocked` missing or null) and review-required changes still ask. */
export function toolChangeAsksReview(e: SecurityEvent, reviewsEnabled: boolean): boolean {
  return reviewsEnabled || e.blocked !== false || e.review_required === true;
}
