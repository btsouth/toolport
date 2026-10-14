import { useI18n, translate, Trans } from "@/lib/i18n";
import { useWindowVisible } from "@/lib/windowVisible";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Download,
  Fingerprint,
  History,
  ScrollText,
  Search,
  Share2,
  ShieldAlert,
  ShieldCheck,
  Sparkles,
  Trash2,
  X,
  XCircle,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { toast } from "sonner";
import {
  fmtBytes,
  fmtMs,
  fmtPercent,
  fmtTokens,
  fmtTs,
  stableListKeys,
} from "@/lib/utils";
import { toastError } from "@/lib/toast";
import { save } from "@tauri-apps/plugin-dialog";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  clearActivityLogs,
  exportAuditToPath,
  getAuditLog,
  getAuditStats,
  getInspectLog,
  getSavingsSummary,
  getSearchTraces,
  getSecurityEvents,
  getToolIdentities,
  type SecurityEvent,
} from "@/lib/api";
import type {
  AuditEntry,
  AuditStats,
  InspectEntry,
  Registry,
  SavingsSummary,
  SearchTrace,
  ServerStat,
  ToolIdentity,
} from "@/lib/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** A badge describing one security event by kind. */
function eventBadge(e: SecurityEvent): { label: string; cls: string } {
  if (e.type === "result_injection_blocked") {
    return {
      label: translate("injection blocked"),
      cls: "bg-destructive/15 text-destructive",
    };
  }
  if (e.type === "result_injection") {
    return {
      label: translate("injected result"),
      cls: "bg-destructive/15 text-destructive",
    };
  }
  if (e.type === "tool_poison_flag") {
    return {
      label: translate("suspicious content"),
      cls: "bg-destructive/15 text-destructive",
    };
  }
  if (e.type === "pins_load_failed") {
    return {
      label: translate("integrity baseline lost"),
      cls: "bg-destructive/15 text-destructive",
    };
  }
  if (e.change === "changed") {
    return { label: translate("changed"), cls: "bg-warning/15 text-warning" };
  }
  return { label: translate("new tool"), cls: "bg-owned/15 text-owned" };
}

const SECURITY_DISMISSED_KEY = "toolport.security.dismissed";
const SECURITY_DISMISSED_KEY_LEGACY = "conduit.security.dismissed";

/** High-signal, interrupting events vs benign, quiet-history churn. The backend now
 * tags a `severity`; for events written before that (no field) we classify by type:
 * poison / injected-result / lost-baseline are high, a plain tool_drift is benign. */
function eventSeverity(e: SecurityEvent): "high" | "info" {
  if (e.severity === "high" || e.severity === "info") return e.severity;
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

/** Durable per-event key for dismissal: identity (type, server, tool, change, severity),
 * NOT the timestamp. A benign drift that re-flags later (e.g. RevenueCat revising a beta
 * tool again) collapses to the same key, so dismissing it once keeps it dismissed instead
 * of returning as a brand-new, undismissable notice. Server + change are kept so tool-less
 * events (e.g. pins_load_failed) don't collide and dismiss each other. Severity is kept so
 * dismissing a tool's benign change never masks a later HIGH-severity change on that same
 * tool (e.g. it turns destructive) - that must still interrupt. */
function securityKey(e: SecurityEvent): string {
  return `${e.type}:${e.server ?? ""}:${e.tool ?? ""}:${e.change}:${eventSeverity(e)}`;
}

/** Unique React list key. `securityKey` is deliberately timestamp-free (so dismissal is
 * durable), but two un-collapsed instances of the same drift can be on screen at once,
 * so the render key still needs the timestamp to stay unique. */
function renderKey(e: SecurityEvent): string {
  return `${securityKey(e)}:${e.ts}`;
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
  for (const e of newestFirst) {
    const dupe = kept.some(
      (k) =>
        k.type === e.type &&
        k.server === e.server &&
        k.tool === e.tool &&
        k.change === e.change &&
        eventSeverity(k) === eventSeverity(e) &&
        Math.abs(k.ts - e.ts) <= WINDOW_MS,
    );
    if (!dupe) kept.push(e);
  }
  return kept;
}

/** Collapse the loud notices to one row per finding identity (securityKey), keeping the
 * newest occurrence and a count of how many times it has fired. `dedupeSecurity` only
 * merges a short window (to fold a per-client burst); this merges across ALL time, so a
 * finding that recurs hours apart - e.g. the same poison flag re-emitted after each
 * baseline reset - reads as one "×N, last seen …" row instead of a confusing stack of
 * identical notices. Dismissing the row clears the whole group (dismissal is by identity). */
function collapseByIdentity(
  events: SecurityEvent[],
): { e: SecurityEvent; count: number }[] {
  const groups = new Map<string, { e: SecurityEvent; count: number }>();
  for (const e of events) {
    const key = securityKey(e);
    const g = groups.get(key);
    if (!g) {
      groups.set(key, { e, count: 1 });
    } else {
      g.count += 1;
      // Keep the newest occurrence so the timestamp and any evidence excerpt are current.
      if (e.ts > g.e.ts) g.e = e;
    }
  }
  return [...groups.values()].sort((a, b) => b.e.ts - a.e.ts);
}

function loadDismissed(): Set<string> {
  try {
    const raw =
      localStorage.getItem(SECURITY_DISMISSED_KEY) ??
      localStorage.getItem(SECURITY_DISMISSED_KEY_LEGACY);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}

/** Upper bound on remembered dismissals, so the persisted set can't grow without
 * limit across sessions. Well above any realistic count of distinct findings. */
const MAX_DISMISSED = 500;

/** Add keys to the dismissed set, cap it to the most-recent MAX_DISMISSED (Set
 * preserves insertion order, so slicing the tail keeps the newest), and persist.
 * Returns the new set for use as React state. */
function addDismissed(prev: Set<string>, keys: string[]): Set<string> {
  const merged = new Set(prev);
  for (const k of keys) merged.add(k);
  const arr = [...merged];
  const next =
    arr.length > MAX_DISMISSED ? new Set(arr.slice(arr.length - MAX_DISMISSED)) : merged;
  try {
    localStorage.setItem(SECURITY_DISMISSED_KEY, JSON.stringify([...next]));
    localStorage.removeItem(SECURITY_DISMISSED_KEY_LEGACY);
  } catch {
    // ignore storage failures; the dismissal just won't persist
  }
  return next;
}

/** Calm, always-on "you're protected" state, shown whenever there are no live
 * security notices. A protection the user never sees builds no trust, so we make
 * the integrity + content-defense watch visible even when nothing is wrong. */
function SecurityResting() {
  const { t: tr } = useI18n();
  return (
    <div className="mb-4 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/30 px-4 py-2.5 text-xs text-muted-foreground">
      <ShieldCheck className="size-4 shrink-0 text-owned" />
      <span>
        <Trans
          message="{value1} Toolport is watching tool definitions and results. Nothing needs your attention right now."
          values={{
            value1: (
              <span className="font-medium text-foreground">
                {tr("Protection active.")}
              </span>
            ),
          }}
        />
      </span>
    </div>
  );
}

type LoadStatus = "loading" | "ready" | "error" | "stale";

/** Security data is part of the trust boundary: a failed read is unknown, never an
 * all-clear. Keep this state visible until a successful read verifies the current log. */
function SecurityLoadNotice({
  status,
  onRetry,
}: {
  status: Exclude<LoadStatus, "ready">;
  onRetry: () => void;
}) {
  const { t: tr } = useI18n();
  if (status === "loading") {
    return (
      <div
        role="status"
        className="mb-4 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/30 px-4 py-2.5 text-xs text-muted-foreground"
      >
        <ShieldCheck className="size-4 shrink-0" />
        <span>{tr("Checking protection status…")}</span>
      </div>
    );
  }

  const stale = status === "stale";
  return (
    <div
      role="alert"
      className="mb-4 flex items-center gap-3 rounded-lg border border-warning/40 bg-warning/5 px-4 py-2.5 text-xs"
    >
      <AlertTriangle className="size-4 shrink-0 text-warning" />
      <span className="text-muted-foreground">
        <span className="font-medium text-foreground">
          {stale
            ? tr("Security status may be out of date.")
            : tr("Couldn't verify protection status.")}
        </span>{" "}
        {stale
          ? tr(
              "Showing the last security events Toolport read successfully; the latest check failed.",
            )
          : tr("Toolport couldn't read security events, so this is not an all-clear.")}
      </span>
      <button
        type="button"
        onClick={onRetry}
        aria-label={tr("Retry protection status")}
        className="ml-auto shrink-0 rounded-md border px-2.5 py-1 font-medium text-foreground transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
      >
        {tr("Retry")}
      </button>
    </div>
  );
}

function AuditStaleNotice({ onRetry }: { onRetry: () => void }) {
  const { t: tr } = useI18n();
  return (
    <div
      role="alert"
      className="mb-4 flex items-center gap-3 rounded-lg border border-warning/40 bg-warning/5 px-4 py-2.5 text-xs"
    >
      <AlertTriangle className="size-4 shrink-0 text-warning" />
      <span className="text-muted-foreground">
        <Trans
          message="{value1}  Showing the last calls Toolport read successfully; the latest refresh failed."
          values={{
            value1: (
              <span className="font-medium text-foreground">
                {tr("Activity may be out of date.")}
              </span>
            ),
          }}
        />
      </span>
      <button
        type="button"
        onClick={onRetry}
        aria-label={tr("Retry activity log")}
        className="ml-auto shrink-0 rounded-md border px-2.5 py-1 font-medium text-foreground transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
      >
        {tr("Retry")}
      </button>
    </div>
  );
}

/** Surfaces tool security events: a tool you approved changed (rug-pull signal),
 * a known server added one, a tool definition contains injection-like content
 * (poisoning), or a tool returned data that looks like injected instructions
 * (agentjacking, which Toolport labels as data before the agent sees it).
 * Collapsible, and each notice can be dismissed once you've reviewed it. */
function SecurityNotices({
  events,
  onDismiss,
}: {
  events: SecurityEvent[];
  onDismiss: (e: SecurityEvent) => void;
}) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(true);
  // One row per finding, newest first, with a recurrence count. See collapseByIdentity.
  const collapsed = collapseByIdentity(events);
  return (
    <div className="mb-4 rounded-lg border border-warning/40 bg-warning/5 p-4">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left"
      >
        <ShieldAlert className="size-4 shrink-0 text-warning" />
        <h3 className="text-sm font-medium text-warning">
          {tr("Tool security notices")}
        </h3>
        <span className="rounded-full bg-warning/15 px-1.5 py-0.5 text-xs font-medium text-warning">
          {collapsed.length}
        </span>
        <ChevronRight
          className={`ml-auto size-4 text-warning/70 transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <>
          <p className="mt-2 mb-3 max-w-2xl text-xs text-muted-foreground">
            {tr(
              "A tool changed after you approved it, a tool's definition contains instruction-like content, or a tool returned data that looks like injected instructions. Usually benign, but it's how rug pulls, tool poisoning, and agentjacking work, so Toolport flags it (and labels suspicious tool output as data). Dismiss the ones you've reviewed.",
            )}
          </p>
          <ul className="space-y-1.5 text-xs">
            {collapsed.slice(0, 10).map(({ e, count }) => {
              const badge = eventBadge(e);
              return (
                <li key={securityKey(e)} className="flex flex-col gap-1">
                  <div className="flex items-center gap-2">
                    <span className={`rounded px-1.5 py-0.5 font-medium ${badge.cls}`}>
                      {badge.label}
                    </span>
                    <code className="font-mono text-foreground">
                      {e.tool || e.server || tr("integrity baseline")}
                    </code>
                    {e.signatures && e.signatures.length > 0 && (
                      <span className="text-muted-foreground">
                        ({e.signatures.join(", ")})
                      </span>
                    )}
                    {count > 1 && (
                      <span
                        className="rounded-full bg-warning/15 px-1.5 py-0.5 font-medium text-warning"
                        title={tr("Recurred in {value1} separate time windows", {
                          value1: count,
                        })}
                      >
                        ×{count}
                      </span>
                    )}
                    <span className="ml-auto text-muted-foreground">
                      {count > 1 ? tr("last ") : ""}
                      {fmtTs(e.ts)}
                    </span>
                    <button
                      onClick={() => onDismiss(e)}
                      aria-label={tr("Dismiss this notice")}
                      className="rounded p-0.5 text-muted-foreground/60 transition-colors hover:bg-warning/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-warning"
                    >
                      <X className="size-3.5" />
                    </button>
                  </div>
                  {e.evidence && (
                    <p className="ml-1 max-w-2xl border-l-2 border-warning/40 pl-2 font-mono text-[11px] leading-relaxed break-words text-muted-foreground">
                      <span className="mr-1 select-none font-sans text-muted-foreground/70">
                        {tr("matched:")}
                      </span>
                      {e.evidence}
                    </p>
                  )}
                </li>
              );
            })}
          </ul>
        </>
      )}
    </div>
  );
}

/** A quiet, non-interrupting history of benign tool-definition churn: a
 * non-destructive tool's description or schema was revised with its safety hints
 * intact (vendors routinely revise beta tools server-side). Collapsed by default,
 * muted styling, no badge or warning color, it's viewable for the record, not an
 * alert. The loud, actionable signal (poison, destructive change, safety-annotation
 * downgrade) stays in SecurityNotices. Dismissal is durable per drift identity, so
 * clearing a recurring benign change keeps it quiet when the vendor churns it again. */
function QuietDriftHistory({
  events,
  onDismiss,
  onDismissAll,
}: {
  events: SecurityEvent[];
  onDismiss: (e: SecurityEvent) => void;
  onDismissAll: () => void;
}) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  return (
    <div className="mb-4 rounded-lg border border-border/60 bg-muted/20 p-3">
      <div className="flex w-full items-center gap-2 text-xs text-muted-foreground">
        <button
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex flex-1 items-center gap-2 text-left"
        >
          <History className="size-3.5 shrink-0" />
          <span className="font-medium text-foreground/80">
            {tr("New & changed tools")}
          </span>
          <span className="rounded-full bg-muted px-1.5 py-0.5 font-medium">
            {events.length}
          </span>
          <ChevronRight
            className={`ml-auto size-3.5 transition-transform ${open ? "rotate-90" : ""}`}
          />
        </button>
        <button
          onClick={onDismissAll}
          className="shrink-0 rounded px-2 py-0.5 font-medium text-muted-foreground/70 transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border"
        >
          {tr("Dismiss all")}
        </button>
      </div>
      {open && (
        <>
          <p className="mt-2 mb-2 max-w-2xl text-xs text-muted-foreground">
            {tr(
              "New tools first seen, and benign, non-destructive changes to tools you've already approved (vendors revise beta tools server-side). Kept for the record, not flagged as a risk, the approval and destructive-tool gates still guard every call. Dismiss any you've reviewed.",
            )}
          </p>
          <ul className="space-y-1 text-xs">
            {events.slice(0, 20).map((e) => (
              <li key={renderKey(e)} className="flex items-center gap-2">
                <span className="rounded bg-muted px-1.5 py-0.5 text-muted-foreground">
                  {e.change === "added" ? tr("new tool") : tr("changed")}
                </span>
                <code className="font-mono text-muted-foreground">{e.tool}</code>
                <span className="ml-auto text-muted-foreground/70">{fmtTs(e.ts)}</span>
                <button
                  onClick={() => onDismiss(e)}
                  aria-label={tr("Dismiss this change")}
                  className="rounded p-0.5 text-muted-foreground/50 transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border"
                >
                  <X className="size-3.5" />
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

/** Catalog exposure: exact serialized bytes with a provider-independent estimate. */
function SavingsBanner({ savings }: { savings: SavingsSummary }) {
  const { t: tr, locale } = useI18n();
  const hasCatalog = savings.listLoads > 0;
  const measured = (savings.measuredLoads ?? 0) > 0;
  const discovered = (savings.discoveryCount ?? 0) > 0;
  const avoided = savings.avoidedSurfaceBytes ?? 0;
  const legacy =
    savings.legacyEstimatedTokensAvoided ?? (measured ? 0 : savings.tokensSaved);
  const since = savings.sinceTs > 0 ? fmtTs(savings.sinceTs, "monthDay") : null;
  const details = [
    hasCatalog
      ? savings.listLoads === 1
        ? tr("across {value1} tool-list load", {
            value1: savings.listLoads.toLocaleString(locale),
          })
        : tr("across {value1} tool-list loads", {
            value1: savings.listLoads.toLocaleString(locale),
          })
      : null,
    hasCatalog
      ? tr("≈{value1} per load", {
          value1: fmtTokens(Math.round(savings.tokensSaved / savings.listLoads)),
        })
      : null,
    hasCatalog && savings.peakCatalog > 4
      ? tr("peak catalog {value1} tools", {
          value1: savings.peakCatalog.toLocaleString(locale),
        })
      : null,
    since ? tr("since {value1}", { value1: since }) : null,
  ].filter(Boolean);

  const share = async () => {
    const text = hasCatalog
      ? tr(
          "Toolport kept ≈{value1} tokens of MCP tool definitions out of my agent's context across {value2} loads. Estimated from serialized size (UTF-8 bytes / 4), not model billing. toolport.app",
          {
            value1: fmtTokens(savings.tokensSaved),
            value2: savings.listLoads.toLocaleString(locale),
          },
        )
      : tr(
          "Toolport recorded {value1} discovery searches returning {value2} of text at its MCP boundary. toolport.app",
          {
            value1: savings.discoveryCount ?? 0,
            value2: fmtBytes(savings.discoveryResponseBytes ?? 0),
          },
        );
    try {
      await navigator.clipboard.writeText(text);
      toast.success(tr("Savings copied, paste them anywhere"));
    } catch {
      toastError(tr("Couldn't copy to clipboard"));
    }
  };

  return (
    <div className="mb-6 rounded-lg border border-success/30 bg-success/[0.06] p-4">
      <div className="flex items-center gap-2">
        <Sparkles className="size-4 text-success" />
        <span className="text-sm font-medium text-muted-foreground">
          {hasCatalog
            ? tr("Tool definitions kept out of your agent's context")
            : tr("Discovery payload returned")}
        </span>
      </div>
      <div className="mt-2 flex flex-wrap items-end gap-x-6 gap-y-1">
        <span className="text-3xl font-semibold tabular-nums text-success">
          {hasCatalog
            ? `≈ ${fmtTokens(savings.tokensSaved)}`
            : fmtBytes(savings.discoveryResponseBytes ?? 0)}{" "}
          <span className="text-base font-normal text-muted-foreground">
            {hasCatalog ? tr("tokens saved") : tr("discovery text")}
          </span>
        </span>
      </div>
      {(savings.latestCatalogTs ?? 0) > 0 && (
        <div className="mt-2 text-sm text-muted-foreground">
          {tr(
            "Latest load: {value1} /  {value2} tools full {value3} {value4} /  {value5} tools exposed",
            {
              value1: fmtBytes(savings.latestFullSurfaceBytes ?? 0),
              value2: (savings.latestFullToolCount ?? 0).toLocaleString(locale),
              value3: " → ",
              value4: fmtBytes(savings.latestExposedSurfaceBytes ?? 0),
              value5: (savings.latestExposedToolCount ?? 0).toLocaleString(locale),
            },
          )}
        </div>
      )}
      {measured && (
        <div
          className="mt-2 text-sm text-muted-foreground"
          title={tr("{value1} exact UTF-8 bytes avoided", {
            value1: avoided.toLocaleString(locale),
          })}
        >
          {tr(
            "{value1} full ·  {value2} exposed · {value3}  avoided across measured loads {value4} {value5}",
            {
              value1: fmtBytes(savings.fullSurfaceBytes ?? 0),
              value2: fmtBytes(savings.exposedSurfaceBytes ?? 0),
              value3: fmtBytes(avoided),
              value4: tr(" · {value1} full/load", {
                value1: fmtBytes(
                  Math.round(
                    (savings.fullSurfaceBytes ?? 0) / (savings.measuredLoads ?? 1),
                  ),
                ),
              }),
              value5:
                (savings.extraExposedSurfaceBytes ?? 0) > 0
                  ? tr(" · {value1} extra exposure on small catalogs", {
                      value1: fmtBytes(savings.extraExposedSurfaceBytes ?? 0),
                    })
                  : "",
            },
          )}
        </div>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button
          onClick={share}
          className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition hover:text-foreground"
        >
          <Trans
            message="{value1} Share"
            values={{ value1: <Share2 className="size-3.5" /> }}
          />
        </button>
      </div>
      <p className="mt-2.5 text-xs text-muted-foreground">
        {details.length > 0 ? `${details.join(" · ")}. ` : ""}
        {measured
          ? savings.measuredLoads === 1
            ? tr("{value1} measured load. ", {
                value1: (savings.measuredLoads ?? 0).toLocaleString(locale),
              })
            : tr("{value1} measured loads. ", {
                value1: (savings.measuredLoads ?? 0).toLocaleString(locale),
              })
          : ""}
        {discovered
          ? tr("{value1} searches returned {value2} of discovery text. ", {
              value1: (savings.discoveryCount ?? 0).toLocaleString(locale),
              value2: fmtBytes(savings.discoveryResponseBytes ?? 0),
            })
          : ""}
        {legacy > 0
          ? tr("Includes ≈{value1} from older estimated records. ", {
              value1: fmtTokens(legacy),
            })
          : ""}
        {hasCatalog
          ? tr(
              "Estimate: serialized UTF-8 bytes ÷ 4. Actual model usage depends on the client, model, and caching.",
            )
          : tr(
              "Exact text bytes at Toolport's MCP boundary; model token usage may differ.",
            )}
      </p>
    </div>
  );
}

function errCell(errors: number, errorRate: number) {
  return fmtPercent(errorRate, { floorNonZero: errors > 0 });
}

/** One server row that expands to reveal its per-tool breakdown. */
function ServerRow({ s }: { s: ServerStat }) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  const tools = s.tools ?? [];
  const expandable = tools.length > 0;
  return (
    <>
      <tr
        className={`border-b last:border-0 ${expandable ? "cursor-pointer hover:bg-muted/30" : ""}`}
        onClick={() => expandable && setOpen((o) => !o)}
      >
        <td className="px-3 py-2 font-medium">
          {expandable ? (
            // Keyboard-operable disclosure lives on a real <button> inside the cell so
            // the row keeps its table semantics; the row-level onClick stays for
            // mouse click-anywhere and this button stops propagation to avoid a
            // double toggle.
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setOpen((o) => !o);
              }}
              aria-expanded={open}
              className="flex min-w-0 items-center gap-1.5 rounded-sm text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              <ChevronRight
                aria-hidden="true"
                className={`size-3.5 text-muted-foreground transition-transform ${open ? "rotate-90" : ""}`}
              />
              <span className="truncate">{s.server}</span>
              <span className="sr-only">
                {tr("{value1} per-tool breakdown", {
                  value1: open ? tr("Collapse") : tr("Expand"),
                })}
              </span>
            </button>
          ) : (
            <span className="flex min-w-0 items-center gap-1.5">
              <span className="inline-block size-3.5" />
              <span className="truncate">{s.server}</span>
            </span>
          )}
        </td>
        <td className="px-3 py-2 text-right tabular-nums">{s.calls}</td>
        <td
          className={`px-3 py-2 text-right tabular-nums ${
            s.errors > 0 ? "text-destructive" : "text-muted-foreground"
          }`}
        >
          {errCell(s.errors, s.errorRate)}
        </td>
        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">
          {fmtMs(s.avgMs)}
        </td>
        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">
          {fmtMs(s.p95Ms)}
        </td>
      </tr>
      {open &&
        tools.map((t) => (
          <tr
            key={t.tool}
            className="border-b border-border/40 bg-muted/20 last:border-0"
          >
            <td className="py-1.5 pr-3 pl-9 font-mono text-xs text-muted-foreground">
              {t.tool}
            </td>
            <td className="px-3 py-1.5 text-right text-xs tabular-nums text-muted-foreground">
              {t.calls}
            </td>
            <td
              className={`px-3 py-1.5 text-right text-xs tabular-nums ${
                t.errors > 0 ? "text-destructive" : "text-muted-foreground"
              }`}
            >
              {errCell(t.errors, t.errorRate)}
            </td>
            <td className="px-3 py-1.5 text-right text-xs tabular-nums text-muted-foreground">
              {fmtMs(t.avgMs)}
            </td>
            <td className="px-3 py-1.5 text-right text-xs tabular-nums text-muted-foreground">
              {fmtMs(t.p95Ms)}
            </td>
          </tr>
        ))}
    </>
  );
}

/** One recent-call row. A failed call with a captured error message expands to
 * show why it failed; latency is shown inline. Args/results aren't recorded in
 * the audit log (it's an append-only governance record), so they're not shown. */
/** What the PII pass did on this call: a count, and a warning when it did not fully
 * apply (SBS-607).
 *
 * Renders nothing when redaction was off (no `piiReplaced`) AND when it ran but matched
 * nothing. The audit record keeps those two apart, and the export still does; the row
 * does not, because a badge on every call of a busy log is noise that buries the two
 * cases worth looking at. Whether the feature is on is a setting the user set; whether it
 * did something, or quietly failed to, is what a per-call row can tell them.
 *
 * The count is the whole payload. Pseudonymization exists to keep values away from the
 * model, so surfacing one here to explain the count would defeat it. */
function PiiBadge({ entry }: { entry: AuditEntry }) {
  const { t: tr } = useI18n();
  const replaced = entry.piiReplaced;
  if (replaced === undefined) return null;
  // Redaction fails OPEN: a full session map or an over-cap result leaves values in the
  // clear. That case gets warning styling because the honest reading of the row is "some
  // of this reached the model unredacted", not "N were handled".
  if (entry.piiIncomplete) {
    return (
      <span
        className="flex shrink-0 items-center gap-1 rounded bg-warning/15 px-1.5 py-0.5 text-[10px] text-warning"
        title={
          replaced === 1
            ? tr(
                "{value1} value pseudonymized, but the pass did not fully apply — some values reached the model in the clear (the session map was full, or the result exceeded the scan cap).",
                { value1: replaced },
              )
            : tr(
                "{value1} values pseudonymized, but the pass did not fully apply — some values reached the model in the clear (the session map was full, or the result exceeded the scan cap).",
                { value1: replaced },
              )
        }
      >
        <Trans
          message="{value1} {value2} pseudonymized, incomplete"
          values={{
            value1: <ShieldAlert aria-hidden="true" className="size-3" />,
            value2: replaced,
          }}
        />
      </span>
    );
  }
  if (replaced === 0) return null;
  return (
    <span
      className="flex shrink-0 items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground"
      title={
        replaced === 1
          ? tr(
              "{value1} value in this result were replaced with pseudonyms before the model saw them. The values themselves are never logged.",
              { value1: replaced },
            )
          : tr(
              "{value1} values in this result were replaced with pseudonyms before the model saw them. The values themselves are never logged.",
              { value1: replaced },
            )
      }
    >
      <Trans
        message="{value1} {value2} pseudonymized"
        values={{
          value1: <ShieldCheck aria-hidden="true" className="size-3" />,
          value2: replaced,
        }}
      />
    </span>
  );
}

function CallRow({ e }: { e: AuditEntry }) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  const hasDetail = !e.ok && !!e.error;
  return (
    <div className="rounded-md border border-border/50 text-sm">
      <div
        className={`flex items-center gap-3 rounded-md px-3 py-2 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none ${
          hasDetail ? "cursor-pointer hover:bg-muted/30" : ""
        }`}
        role={hasDetail ? "button" : undefined}
        tabIndex={hasDetail ? 0 : undefined}
        aria-expanded={hasDetail ? open : undefined}
        onClick={() => hasDetail && setOpen((o) => !o)}
        onKeyDown={
          hasDetail
            ? (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  setOpen((o) => !o);
                }
              }
            : undefined
        }
      >
        {hasDetail ? (
          <ChevronRight
            aria-hidden="true"
            className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
              open ? "rotate-90" : ""
            }`}
          />
        ) : (
          <span className="inline-block size-3.5 shrink-0" />
        )}
        {e.held ? (
          <ShieldAlert className="size-4 shrink-0 text-warning" />
        ) : e.ok ? (
          <CheckCircle2 className="size-4 shrink-0 text-success" />
        ) : (
          <XCircle className="size-4 shrink-0 text-destructive" />
        )}
        <span className="min-w-0 truncate font-medium">{e.server}</span>
        <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">
          {e.tool}
        </span>
        {(e.clientName || e.client) && (
          <span
            className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground"
            title={
              e.client
                ? tr("Client: {value1}", { value1: e.client })
                : tr("Client that made this call")
            }
          >
            {e.clientName ?? e.client}
          </span>
        )}
        <PiiBadge entry={e} />
        <span className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground">
          {fmtMs(e.durationMs ?? e.heldMs ?? null)}
        </span>
        <span className="shrink-0 text-xs text-muted-foreground">{fmtTs(e.ts)}</span>
      </div>
      {open && e.error && (
        <div className="border-t border-border/50 bg-destructive/5 px-3 py-2 pl-9">
          <p className="font-mono text-xs whitespace-pre-wrap break-words text-destructive">
            {e.error}
          </p>
        </div>
      )}
    </div>
  );
}

function StatsPanel({ stats }: { stats: AuditStats }) {
  const { t: tr } = useI18n();
  // The three summary cards are the glanceable health check and stay visible; the full
  // per-server table (can be 20+ rows) collapses by default so it stops being a wall
  // below the security lane. It's one tap when you actually want the breakdown.
  const [tableOpen, setTableOpen] = useState(false);
  if (stats.total === 0) return null;
  const errPct = fmtPercent(stats.errorRate, { floorNonZero: stats.errors > 0 });
  // Surface how many servers have errors on the collapsed toggle: the aggregate error % can
  // dilute a single fully-broken server among many healthy ones, so a per-server signal has
  // to be visible without expanding the table.
  const serversWithErrors = stats.servers.filter((s) => s.errors > 0).length;
  return (
    <div className="mb-6 flex flex-col gap-3">
      <div className="grid grid-cols-3 gap-3">
        <div className="rounded-lg border p-3">
          <div className="text-2xl font-semibold tabular-nums">{stats.total}</div>
          <div className="text-xs text-muted-foreground">{tr("calls logged")}</div>
        </div>
        <div
          className={`rounded-lg border p-3 ${stats.errors > 0 ? "border-destructive/40 bg-destructive/[0.04]" : ""}`}
        >
          <div
            className={`text-2xl font-semibold tabular-nums ${stats.errors > 0 ? "text-destructive" : ""}`}
          >
            {stats.errors}
          </div>
          <div className="text-xs text-muted-foreground">
            {tr("errors ({value1})", { value1: errPct })}
          </div>
        </div>
        <div className="rounded-lg border p-3">
          <div className="text-2xl font-semibold tabular-nums">
            {stats.servers.length}
          </div>
          <div className="text-xs text-muted-foreground">{tr("active servers")}</div>
        </div>
      </div>

      <button
        onClick={() => setTableOpen((v) => !v)}
        aria-expanded={tableOpen}
        className="flex w-fit items-center gap-2 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
      >
        <Trans
          message="{value1} Per-server breakdown {value2} {value3}"
          values={{
            value1: (
              <ChevronRight
                className={`size-4 transition-transform ${tableOpen ? "rotate-90" : ""}`}
              />
            ),
            value2: (
              <span className="text-xs font-normal text-muted-foreground/70">
                {stats.servers.length === 1
                  ? tr("{count} server", { count: stats.servers.length })
                  : tr("{count} servers", { count: stats.servers.length })}
              </span>
            ),
            value3: serversWithErrors > 0 && (
              <span className="rounded-full bg-destructive/10 px-1.5 py-0.5 text-xs font-medium text-destructive">
                {tr("{value1} with errors", { value1: serversWithErrors })}
              </span>
            ),
          }}
        />
      </button>

      {tableOpen && (
        <>
          <div className="overflow-hidden rounded-lg border">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b bg-muted/40 text-left text-xs text-muted-foreground">
                  <th className="px-3 py-2 font-medium">{tr("Server")}</th>
                  <th className="px-3 py-2 text-right font-medium">{tr("Calls")}</th>
                  <th className="px-3 py-2 text-right font-medium">{tr("Errors")}</th>
                  <th className="px-3 py-2 text-right font-medium">{tr("Avg")}</th>
                  <th className="px-3 py-2 text-right font-medium">{tr("p95")}</th>
                </tr>
              </thead>
              <tbody>
                {stats.servers.map((s) => (
                  <ServerRow key={s.server} s={s} />
                ))}
              </tbody>
            </table>
          </div>
          <p className="text-xs text-muted-foreground/70">
            {tr("Click a server to see its per-tool breakdown.")}
          </p>
        </>
      )}
    </div>
  );
}

/** Pretty-print a captured body. A truncation marker (a plain string like
 * "<truncated N bytes>") is shown as-is; everything else is JSON-formatted. */
function fmtBody(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

/** One captured call. Expands to show the pretty-printed request + response JSON. */
function InspectRow({ e }: { e: InspectEntry }) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-md border border-border/50 text-sm">
      <div
        className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/30"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            setOpen((o) => !o);
          }
        }}
        role="button"
        aria-expanded={open}
        tabIndex={0}
      >
        <ChevronRight
          className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
        {e.ok ? (
          <CheckCircle2 className="size-4 shrink-0 text-success" />
        ) : (
          <XCircle className="size-4 shrink-0 text-destructive" />
        )}
        <span className="min-w-0 truncate font-medium">{e.server}</span>
        <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">
          {e.tool}
        </span>
        {(e.clientName || e.client) && (
          <span
            className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground"
            title={
              e.client
                ? tr("Client: {value1}", { value1: e.client })
                : tr("Client that made this call")
            }
          >
            {e.clientName ?? e.client}
          </span>
        )}
        <span className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground">
          {fmtMs(e.durationMs ?? null)}
        </span>
        <span className="shrink-0 text-xs text-muted-foreground">
          {fmtTs(e.ts, "time")}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9">
          <div className="mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
            {tr("Request")}
          </div>
          <pre className="mb-3 overflow-x-auto rounded bg-background/60 p-2 font-mono text-xs whitespace-pre-wrap break-words">
            {fmtBody(e.request)}
          </pre>
          <div className="mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
            {tr("Response")}
          </div>
          <pre className="overflow-x-auto rounded bg-background/60 p-2 font-mono text-xs whitespace-pre-wrap break-words">
            {fmtBody(e.response)}
          </pre>
        </div>
      )}
    </div>
  );
}

/** One recorded search and its exact returned text size when available. */
function DiscoveryRow({ t }: { t: SearchTrace }) {
  const { t: tr, locale } = useI18n();
  const [open, setOpen] = useState(false);
  const hit = t.returned > 0;
  const fallbackCount = t.fallbacks ?? t.ranking?.filter((r) => r.fallback).length ?? 0;
  const resultSummary = fallbackCount
    ? tr("{value1} direct + {value2} fallback", {
        value1: t.total,
        value2: fallbackCount,
      })
    : hit
      ? `${t.returned} of ${t.total}`
      : tr("no match");
  return (
    <div className="rounded-md border border-border/50 text-sm">
      <div
        className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/30"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            setOpen((o) => !o);
          }
        }}
        role="button"
        aria-expanded={open}
        tabIndex={0}
      >
        <ChevronRight
          className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
        <Search className="size-4 shrink-0 text-owned" />
        <span className="min-w-0 truncate font-mono text-xs">
          {tr("“{value1}”", { value1: t.query || tr("(empty)") })}
        </span>
        {t.client && (
          <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
            {t.client}
          </span>
        )}
        {t.escalated && (
          <span className="shrink-0 rounded bg-warning/15 px-1.5 py-0.5 text-[11px] text-warning">
            {tr("loop-broken")}
          </span>
        )}
        {t.mode === "semantic" && (
          <span className="shrink-0 rounded bg-owned/10 px-1.5 py-0.5 text-[11px] text-owned">
            {tr("semantic")}
          </span>
        )}
        <span className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground">
          {resultSummary}
        </span>
        <span className="shrink-0 text-xs text-muted-foreground">
          {fmtTs(t.ts, "time")}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9 text-xs">
          {hit ? (
            t.ranking && t.ranking.length > 0 ? (
              <div className="mb-2 space-y-1">
                {t.ranking.map((r) => (
                  <div key={r.name} className="flex items-baseline gap-2">
                    <span className="w-5 shrink-0 text-right tabular-nums text-[11px] text-muted-foreground">
                      #{r.rank}
                    </span>
                    <span
                      className={`shrink-0 font-mono text-[11px] ${
                        r.name === t.top ? "text-owned" : "text-foreground"
                      }`}
                    >
                      {r.name}
                    </span>
                    <span className="min-w-0 truncate text-[11px] text-muted-foreground">
                      {r.pinned
                        ? tr("pinned prerequisite")
                        : r.fallback
                          ? tr("fallback candidate")
                          : r.matched.length > 0
                            ? tr("matched {value1}", { value1: r.matched.join(", ") })
                            : t.mode === "semantic"
                              ? tr("semantic match")
                              : "—"}
                    </span>
                  </div>
                ))}
              </div>
            ) : (
              <div className="mb-2 flex flex-wrap gap-1">
                {t.names.map((n) => (
                  <span
                    key={n}
                    className={`rounded px-1.5 py-0.5 font-mono text-[11px] ${
                      n === t.top
                        ? "bg-owned/15 text-owned"
                        : "bg-muted text-muted-foreground"
                    }`}
                  >
                    {n}
                  </span>
                ))}
              </div>
            )
          ) : (
            <div className="mb-2 text-muted-foreground">
              {tr("No tools matched this query.")}
            </div>
          )}
          {t.responseContentBytes !== undefined ? (
            <div
              className="text-muted-foreground"
              title={tr("{value1} exact UTF-8 text bytes", {
                value1: t.responseContentBytes.toLocaleString(locale),
              })}
            >
              {t.returned === 1
                ? tr(
                    "Returned {value1} of discovery content containing  {value2} matching schema. {value4}  ≈{value5} tokens (UTF-8 bytes ÷ 4).",
                    {
                      value1: fmtBytes(t.responseContentBytes),
                      value2: t.returned,
                      value4:
                        t.catalogSchemaBytes !== undefined
                          ? tr(" Full scoped catalog schemas: {value1}.", {
                              value1: fmtBytes(t.catalogSchemaBytes),
                            })
                          : "",
                      value5: fmtTokens(t.estimatedResponseTokens ?? 0),
                    },
                  )
                : tr(
                    "Returned {value1} of discovery content containing  {value2} matching schemas. {value4}  ≈{value5} tokens (UTF-8 bytes ÷ 4).",
                    {
                      value1: fmtBytes(t.responseContentBytes),
                      value2: t.returned,
                      value4:
                        t.catalogSchemaBytes !== undefined
                          ? tr(" Full scoped catalog schemas: {value1}.", {
                              value1: fmtBytes(t.catalogSchemaBytes),
                            })
                          : "",
                      value5: fmtTokens(t.estimatedResponseTokens ?? 0),
                    },
                  )}
            </div>
          ) : (
            <div className="text-muted-foreground">
              {tr(
                "Legacy schema-only estimates: ≈{value1} returned vs ≈ {value2} catalog. Search guidance text was not counted.",
                { value1: fmtTokens(t.returnedTokens), value2: fmtTokens(t.flatTokens) },
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Collapsible local discovery history. */
/** Loading placeholder for a self-contained Activity panel, shown only while its
 * first fetch is in flight (never once real rows exist — a live refresh should not
 * flash this over last-known data). */
function PanelLoadingNotice({ label, icon: Icon }: { label: string; icon: LucideIcon }) {
  const { t: tr } = useI18n();
  return (
    <div
      role="status"
      className="mb-6 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/20 p-4 text-xs text-muted-foreground"
    >
      <Icon className="size-4 shrink-0" />
      <span>{tr("Loading {value1}…", { value1: label.toLowerCase() })}</span>
    </div>
  );
}

/** A failed load for a self-contained Activity panel. Distinguishes "couldn't check"
 * from "nothing to show" with an explicit retry, so a backend failure never collapses
 * into the same wording as a genuinely empty panel (#728). */
function PanelErrorNotice({ label, onRetry }: { label: string; onRetry: () => void }) {
  const { t: tr } = useI18n();
  return (
    <div
      role="alert"
      className="mb-6 flex items-center gap-3 rounded-lg border border-warning/40 bg-warning/5 p-4 text-xs"
    >
      <AlertTriangle className="size-4 shrink-0 text-warning" />
      <span className="text-muted-foreground">
        <Trans
          message="{value1}  This isn't necessarily empty — Toolport couldn't check."
          values={{
            value1: (
              <span className="font-medium text-foreground">
                {tr("Couldn't load {value1}.", { value1: label.toLowerCase() })}
              </span>
            ),
          }}
        />
      </span>
      <button
        type="button"
        onClick={onRetry}
        aria-label={tr("Retry loading {value1}", { value1: label.toLowerCase() })}
        className="ml-auto shrink-0 rounded-md border px-2.5 py-1 font-medium text-foreground transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
      >
        {tr("Retry")}
      </button>
    </div>
  );
}

function DiscoveryTraces({ refreshKey }: { refreshKey: number }) {
  const { t: tr } = useI18n();
  const [entries, setEntries] = useState<SearchTrace[]>([]);
  // Collapsed by default: this is glanceable telemetry, not an alert, and the list can run
  // to 100 rows. Leading with it open was a big part of the Activity tab feeling busy.
  const [open, setOpen] = useState(false);
  // Distinguishes a load failure from a genuinely empty result (#728): both leave
  // `entries` empty, but only one should show "Nothing searched yet."
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [retryTick, setRetryTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setStatus((s) => (s === "error" ? "loading" : s));
    getSearchTraces(100)
      .then((e) => {
        if (!alive) return;
        setEntries(e);
        setStatus("ready");
      })
      .catch(() => {
        if (!alive) return;
        setStatus("error");
      });
    return () => {
      alive = false;
    };
  }, [refreshKey, retryTick]);

  if (status === "loading" && entries.length === 0) {
    return <PanelLoadingNotice label={tr("Discovery")} icon={Search} />;
  }

  if (status === "error") {
    return (
      <PanelErrorNotice
        label={tr("Discovery")}
        onRetry={() => setRetryTick((t) => t + 1)}
      />
    );
  }

  if (entries.length === 0)
    return (
      <div className="mb-6 rounded-lg border border-border/60 bg-muted/20 p-4 text-xs text-muted-foreground">
        <Trans
          message="{value1} With lazy discovery on, this shows every tool search your agents run, what matched, why it ranked, and the returned MCP payload size. Nothing searched yet."
          values={{
            value1: (
              <div className="mb-1 flex items-center gap-2">
                <Search className="size-4 shrink-0 text-owned" />
                <span className="font-medium text-foreground/80">{tr("Discovery")}</span>
              </div>
            ),
          }}
        />
      </div>
    );

  return (
    <div className="mb-6 rounded-lg border border-owned/30 bg-owned/[0.04] p-4">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left"
      >
        <Search className="size-4 shrink-0 text-owned" />
        <h3 className="text-sm font-medium">{tr("Discovery")}</h3>
        <span className="rounded-full bg-owned/15 px-1.5 py-0.5 text-xs font-medium text-owned">
          {entries.length}
        </span>
        <ChevronRight
          className={`ml-auto size-4 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <>
          <p className="mt-2 mb-3 max-w-2xl text-xs text-muted-foreground">
            {tr(
              "What the model searched for and what Toolport handed back. Each search returns matching tool schemas. Match counts and returned text bytes are exact at Toolport's MCP boundary; token figures are estimates. Local and bounded: tool names only, never arguments or results.",
            )}
          </p>
          <div className="flex flex-col gap-1">
            {stableListKeys(entries, (t) => `${t.ts}-${t.query}`).map((key, i) => (
              <DiscoveryRow key={key} t={entries[i]} />
            ))}
          </div>
        </>
      )}
    </div>
  );
}

/** One tool's identity card: the model-visible alias joined to its source server +
 * profiles, with the pinned definition fingerprint and first-seen / last-changed. */
function ToolIdentityRow({ t }: { t: ToolIdentity }) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  const fpShort = t.fingerprint.replace(/^v\d+:/, "").slice(0, 12) || "-";
  const fmtDate = (ms: number) => (ms > 0 ? fmtTs(ms, "date") : "-");
  return (
    <div className="rounded-md border border-border/50 text-sm">
      <div
        className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/30"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            setOpen((o) => !o);
          }
        }}
        role="button"
        aria-expanded={open}
        tabIndex={0}
      >
        <ChevronRight
          className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
        <span className="min-w-0 truncate font-mono text-xs">{t.alias}</span>
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
          {t.serverName || t.serverId || tr("unattributed")}
        </span>
        {t.quarantined && (
          <span className="shrink-0 rounded bg-warning/15 px-1.5 py-0.5 text-[11px] text-warning">
            {tr("quarantined")}
          </span>
        )}
        <span
          className="ml-auto shrink-0 font-mono text-[11px] text-muted-foreground"
          title={t.fingerprint}
        >
          {fpShort}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9 text-xs">
          <dl className="grid grid-cols-[7rem_1fr] gap-x-3 gap-y-1">
            <dt className="text-muted-foreground">{tr("Upstream tool")}</dt>
            <dd className="font-mono break-all">{t.upstream || "-"}</dd>
            <dt className="text-muted-foreground">{tr("Source server")}</dt>
            <dd>
              {t.serverName ? `${t.serverName} (${t.serverId})` : tr("unattributed")}
            </dd>
            <dt className="text-muted-foreground">{tr("Profiles")}</dt>
            <dd>{t.profiles.length ? t.profiles.join(", ") : "-"}</dd>
            <dt className="text-muted-foreground">{tr("Fingerprint")}</dt>
            <dd className="font-mono break-all">{t.fingerprint || "-"}</dd>
            {t.firstSeen > 0 ? (
              <>
                <dt className="text-muted-foreground">{tr("First seen")}</dt>
                <dd>{fmtDate(t.firstSeen)}</dd>
                <dt className="text-muted-foreground">{tr("Last changed")}</dt>
                <dd>{fmtDate(t.lastChanged)}</dd>
              </>
            ) : (
              <>
                <dt className="text-muted-foreground">{tr("History")}</dt>
                <dd className="text-muted-foreground/80">
                  {tr(
                    "This tool's baseline predates first-seen tracking, so it has no dates yet. Toolport fills them in automatically the next time a client lists tools through the gateway.",
                  )}
                </dd>
              </>
            )}
          </dl>
        </div>
      )}
    </div>
  );
}

/** One server's tools inside Tool identities: a collapsible header with a count and the
 * per-tool rows. Forced open while a search is active so matches are visible without a
 * click; otherwise collapsed by default so N servers read as N lines, not 378 rows. */
function IdentityServerGroup({
  label,
  items,
  forceOpen,
}: {
  label: string;
  items: ToolIdentity[];
  forceOpen: boolean;
}) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  const shown = forceOpen || open;
  const quarantined = items.filter((t) => t.quarantined).length;
  return (
    <div className="rounded-md border border-border/50">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={shown}
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-muted/30"
      >
        <ChevronRight
          className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
            shown ? "rotate-90" : ""
          }`}
        />
        <span className="min-w-0 truncate text-sm font-medium">{label}</span>
        <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[11px] font-medium text-muted-foreground">
          {items.length}
        </span>
        {quarantined > 0 && (
          <span className="shrink-0 rounded-full bg-warning/15 px-1.5 py-0.5 text-[11px] font-medium text-warning">
            {tr("{value1} quarantined", { value1: quarantined })}
          </span>
        )}
      </button>
      {shown && (
        <div className="flex flex-col gap-1 p-2 pt-0">
          {items.map((t) => (
            <ToolIdentityRow key={t.alias} t={t} />
          ))}
        </div>
      )}
    </div>
  );
}

/** Collapsible capability-provenance panel: every pinned tool's identity, so a human
 * can verify which server/profile an alias maps to and whether its definition drifted.
 * Grouped by server and searchable, because a busy setup pins hundreds of tools across
 * a dozen-plus servers and a flat list is unusable. Self-hides until the gateway has
 * pinned a baseline. */
function ToolIdentities({ refreshKey }: { refreshKey: number }) {
  const { t: tr } = useI18n();
  const [rows, setRows] = useState<ToolIdentity[]>([]);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  // Distinguishes a load failure from a genuinely empty result (#728): both leave
  // `rows` empty, but only one should hide the panel.
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [retryTick, setRetryTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setStatus((s) => (s === "error" ? "loading" : s));
    getToolIdentities()
      .then((r) => {
        if (!alive) return;
        setRows(r);
        setStatus("ready");
      })
      .catch(() => {
        if (!alive) return;
        setStatus("error");
      });
    return () => {
      alive = false;
    };
  }, [refreshKey, retryTick]);

  const q = query.trim().toLowerCase();

  // Group every pinned tool under its source server, servers sorted alphabetically.
  const groups = useMemo(() => {
    const m = new Map<string, ToolIdentity[]>();
    for (const t of rows) {
      const label = t.serverName || t.serverId || tr("unattributed");
      const arr = m.get(label);
      if (arr) arr.push(t);
      else m.set(label, [t]);
    }
    return [...m.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }, [rows, tr]);

  // A server-name match shows that server's whole block ("vercel" -> all Vercel tools);
  // otherwise match individual tools by alias or upstream name ("vercel__deploy" -> that
  // one tool). Empty groups drop out.
  const filtered = useMemo(() => {
    if (!q) return groups;
    return groups
      .map(([label, items]): [string, ToolIdentity[]] => {
        if (label.toLowerCase().includes(q)) return [label, items];
        return [
          label,
          items.filter(
            (t) =>
              t.alias.toLowerCase().includes(q) || t.upstream.toLowerCase().includes(q),
          ),
        ];
      })
      .filter(([, items]) => items.length > 0);
  }, [groups, q]);

  if (status === "loading" && rows.length === 0) {
    return <PanelLoadingNotice label={tr("Tool identities")} icon={Fingerprint} />;
  }

  if (status === "error") {
    return (
      <PanelErrorNotice
        label={tr("Tool identities")}
        onRetry={() => setRetryTick((t) => t + 1)}
      />
    );
  }

  if (rows.length === 0) return null;

  const shownTools = filtered.reduce((n, [, items]) => n + items.length, 0);

  return (
    <div className="mb-6 rounded-lg border border-border/60 bg-muted/[0.04] p-4">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left"
      >
        <Fingerprint className="size-4 shrink-0 text-muted-foreground" />
        <h3 className="text-sm font-medium">{tr("Tool identities")}</h3>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
          {rows.length}
        </span>
        <ChevronRight
          className={`ml-auto size-4 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <>
          <p className="mt-2 mb-3 max-w-2xl text-xs text-muted-foreground">
            {tr(
              "What each model-visible tool name actually maps to: its source server and profiles, the pinned definition fingerprint drift detection checks against, and when it was first seen or last changed. Prefixing helps the model pick a tool; this helps you verify what crossed the boundary.",
            )}
          </p>

          <div className="relative mb-3">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={tr("Filter by server or tool name…")}
              aria-label={tr("Filter tool identities")}
              className="h-8 pl-8 text-sm"
            />
          </div>

          {filtered.length === 0 ? (
            <p className="py-6 text-center text-xs text-muted-foreground">
              {tr('No server or tool matches "{value1}".', { value1: query })}
            </p>
          ) : (
            <>
              {q && (
                <p className="mb-2 text-xs text-muted-foreground/70">
                  {shownTools === 1
                    ? filtered.length === 1
                      ? tr("{value1} tool in {value3}  server", {
                          value1: shownTools,
                          value3: filtered.length,
                        })
                      : tr("{value1} tool in {value3}  servers", {
                          value1: shownTools,
                          value3: filtered.length,
                        })
                    : filtered.length === 1
                      ? tr("{value1} tools in {value3}  server", {
                          value1: shownTools,
                          value3: filtered.length,
                        })
                      : tr("{value1} tools in {value3}  servers", {
                          value1: shownTools,
                          value3: filtered.length,
                        })}
                </p>
              )}
              <div className="flex flex-col gap-1">
                {filtered.map(([label, items]) => (
                  <IdentityServerGroup
                    key={label}
                    label={label}
                    items={items}
                    forceOpen={q.length > 0}
                  />
                ))}
              </div>
            </>
          )}
        </>
      )}
    </div>
  );
}

/** Collapsible live inspector: a local "network tab" for MCP. Only rendered while
 * live inspection is on. Lists recent captured calls, each expandable to its raw
 * request + response JSON. Ephemeral, local, opt-in. */
function LiveInspector({ refreshKey }: { refreshKey: number }) {
  const { t: tr } = useI18n();
  const [entries, setEntries] = useState<InspectEntry[]>([]);
  const [open, setOpen] = useState(true);
  // Distinguishes a load failure from a genuinely empty buffer (#728): both leave
  // `entries` empty, but only one should say "run a tool and it'll show here."
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [retryTick, setRetryTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setStatus((s) => (s === "error" ? "loading" : s));
    getInspectLog(50)
      .then((e) => {
        if (!alive) return;
        setEntries(e);
        setStatus("ready");
      })
      .catch(() => {
        if (!alive) return;
        setStatus("error");
      });
    return () => {
      alive = false;
    };
  }, [refreshKey, retryTick]);

  return (
    <div className="mb-6 rounded-lg border border-info/30 bg-info/[0.04] p-4">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left"
      >
        <Activity className="size-4 shrink-0 text-info" />
        <h3 className="text-sm font-medium">{tr("Live inspector")}</h3>
        <span className="rounded-full bg-info/15 px-1.5 py-0.5 text-xs font-medium text-info">
          {entries.length}
        </span>
        <ChevronRight
          className={`ml-auto size-4 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <>
          <p className="mt-2 mb-3 max-w-2xl text-xs text-muted-foreground">
            {tr(
              "Ephemeral, local, opt-in. While live inspection is on, Toolport keeps the last 50 tool calls' arguments and results here so you can inspect them. This buffer is separate from the audit log, never leaves your machine, and clears when you turn inspection off or restart the gateway.",
            )}
          </p>
          {status === "loading" && entries.length === 0 ? (
            <p role="status" className="py-6 text-center text-sm text-muted-foreground">
              {tr("Checking for captured calls…")}
            </p>
          ) : status === "error" ? (
            <PanelErrorNotice
              label={tr("Live inspector")}
              onRetry={() => setRetryTick((t) => t + 1)}
            />
          ) : entries.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              {tr(
                "No calls captured yet. Run a tool through Toolport and it'll show here.",
              )}
            </p>
          ) : (
            <div className="flex flex-col gap-1">
              {stableListKeys(entries, (e) => `${e.ts}-${e.server}-${e.tool}`).map(
                (key, i) => (
                  <InspectRow key={key} e={entries[i]} />
                ),
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}

export function ActivityView({
  refreshKey,
  registry,
}: {
  refreshKey: number;
  registry: Registry | null;
}) {
  const { t: tr } = useI18n();
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [stats, setStats] = useState<AuditStats | null>(null);
  const [savings, setSavings] = useState<SavingsSummary | null>(null);
  const [savingsLoadStatus, setSavingsLoadStatus] = useState<LoadStatus>("loading");
  const [serverFilter, setServerFilter] = useState<string>("");
  // Show ALL recent calls by default, not a pre-filtered errors-only view. Defaulting the
  // filter on made a healthy log read as "everything is failing"; the StatsPanel already
  // surfaces the true error rate, and the toggle is right there for triage.
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [security, setSecurity] = useState<SecurityEvent[]>([]);
  const [securityLoadStatus, setSecurityLoadStatus] = useState<LoadStatus>("loading");
  const [dismissed, setDismissed] = useState<Set<string>>(loadDismissed);
  const [logOpen, setLogOpen] = useState(false);
  // Distinguish a load FAILURE (gateway unreachable / audit log unreadable) from a
  // genuinely empty log: both leave `entries` empty, but only one is an error. Without
  // this, a first-run backend failure renders the friendly "No tool calls yet" state
  // and hides that anything is wrong.
  const [auditLoadStatus, setAuditLoadStatus] = useState<LoadStatus>("loading");
  // Local reload trigger: bumped after a "Clear retained activity" so the panels
  // refetch (they'd otherwise keep showing the just-deleted rows until the parent's
  // refreshKey next changes).
  const [reloadTick, setReloadTick] = useState(0);
  // Invalidates audit reads that started BEFORE a successful clear: a live-tick
  // getAuditLog can resolve after the file was wiped (its effect cleanup only runs
  // at the next commit) and would restore the deleted rows — which a failed
  // follow-up reload then preserves as "stale". Bumped on clear; both completion
  // handlers ignore results from an older generation.
  const auditGeneration = useRef(0);

  function retryLoads() {
    // Keep last-known rows visible while retrying. Only a never-successful load returns
    // to the full loading state; stale data remains explicitly marked stale.
    setAuditLoadStatus((status) => (status === "error" ? "loading" : status));
    setSecurityLoadStatus((status) => (status === "error" ? "loading" : status));
    setReloadTick((tick) => tick + 1);
  }

  async function clearActivity() {
    try {
      await clearActivityLogs();
      // The next getAuditLog can fail (Windows file lock right after clear).
      // Do not keep the deleted rows as last-known after a successful wipe.
      auditGeneration.current += 1;
      setEntries([]);
      setAuditLoadStatus("ready");
      setSavings(null);
      setSavingsLoadStatus("loading");
      toast.success(tr("Cleared retained activity"));
      setReloadTick((t) => t + 1);
    } catch (e) {
      toastError(tr("Couldn't clear activity: {value1}", { value1: String(e) }));
    }
  }

  // Native visibility also covers a window hidden to the tray, where the
  // webview may keep reporting "visible". Pause file reads and rerenders while
  // the user is elsewhere; resume within one tick when they return.
  const windowVisible = useWindowVisible();
  const [liveTick, setLiveTick] = useState(0);
  useEffect(() => {
    if (!windowVisible) return;
    const id = setInterval(() => setLiveTick((t) => t + 1), 3000);
    return () => clearInterval(id);
  }, [windowVisible]);
  // Combined so panels refetch on either a registry change or a live tick. Both counters
  // only increase, so the sum always changes when either does (no collisions).
  const liveKey = refreshKey + liveTick;

  useEffect(() => {
    let alive = true;
    const generation = auditGeneration.current;
    getAuditLog(200)
      .then((e) => {
        if (!alive || generation !== auditGeneration.current) return;
        setEntries(e);
        setAuditLoadStatus("ready");
      })
      .catch(() => {
        if (!alive || generation !== auditGeneration.current) return;
        // Never replace a last-known audit trail with a false empty state.
        setAuditLoadStatus((status) =>
          status === "ready" || status === "stale" ? "stale" : "error",
        );
      });
    getAuditStats()
      .then((s) => alive && setStats(s))
      .catch(() => alive && setStats(null));
    getSavingsSummary()
      .then((s) => {
        if (!alive) return;
        setSavings(s);
        setSavingsLoadStatus("ready");
      })
      .catch(() => {
        if (!alive) return;
        setSavingsLoadStatus((status) =>
          status === "ready" || status === "stale" ? "stale" : "error",
        );
      });
    getSecurityEvents(50)
      .then((s) => {
        if (!alive) return;
        setSecurity(s);
        setSecurityLoadStatus("ready");
      })
      .catch(() => {
        if (!alive) return;
        // A failed refresh must preserve prior findings and must never produce the
        // reassuring verified-empty state.
        setSecurityLoadStatus((status) =>
          status === "ready" || status === "stale" ? "stale" : "error",
        );
      });
    return () => {
      alive = false;
    };
  }, [liveKey, reloadTick]);

  // Export the full retained audit log. The save dialog offers CSV and JSON; the
  // chosen extension picks the format. CSV is formula-injection-safe in the backend.
  async function exportLog() {
    try {
      const path = await save({
        title: tr("Export activity log"),
        defaultPath: "toolport-activity.csv",
        filters: [
          { name: "CSV", extensions: ["csv"] },
          { name: "JSON", extensions: ["json"] },
        ],
      });
      if (!path) return;
      const format = path.toLowerCase().endsWith(".json") ? "json" : "csv";
      await exportAuditToPath(path, format);
      toast.success(tr("Exported activity log"));
    } catch (e) {
      toastError(tr("Couldn't export the log: {value1}", { value1: String(e) }));
    }
  }

  const liveSecurity = dedupeSecurity(security).filter(
    (e) => !dismissed.has(securityKey(e)),
  );
  // Split loud/actionable signal from benign churn so vendor revisions don't bury a real
  // poison or privilege-escalation flag (the failure this whole surface exists to avoid).
  //
  // A tool FIRST APPEARING is inventory, not a rug-pull: you never approved it, so it
  // can't have changed under you, and the destructive-tool + approval gates still guard
  // the actual call. So "added" always goes to the quiet lane, even when the new tool is
  // destructive (e.g. a server exposing `delete_*`). That keeps a bulk first-baseline
  // from masquerading as a wall of alarms. The loud lane stays for what genuinely needs
  // a human: poison, injected results, and a tool you already approved CHANGING.
  const isNewTool = (e: SecurityEvent) =>
    e.change === "added" &&
    e.type !== "tool_poison_flag" &&
    e.type !== "result_injection" &&
    e.type !== "result_injection_blocked";
  const highSecurity = liveSecurity.filter(
    (e) => eventSeverity(e) === "high" && !isNewTool(e),
  );
  const infoSecurity = liveSecurity.filter(
    (e) => eventSeverity(e) !== "high" || isNewTool(e),
  );
  const dismissSecurity = (e: SecurityEvent) => {
    setDismissed((prev) => addDismissed(prev, [securityKey(e)]));
  };
  // Clear a whole batch at once (the quiet lane can hold dozens of first-sightings after
  // a re-baseline; making the user dismiss each would just recreate the noise problem).
  const dismissAllSecurity = (events: SecurityEvent[]) => {
    setDismissed((prev) => addDismissed(prev, events.map(securityKey)));
  };

  const banner = (
    <>
      {/* Loud lane: the only thing here that may need a decision. */}
      {securityLoadStatus !== "ready" ? (
        <SecurityLoadNotice status={securityLoadStatus} onRetry={retryLoads} />
      ) : null}
      {highSecurity.length > 0 ? (
        <SecurityNotices events={highSecurity} onDismiss={dismissSecurity} />
      ) : securityLoadStatus === "ready" ? (
        <SecurityResting />
      ) : null}
      {infoSecurity.length > 0 ? (
        <QuietDriftHistory
          events={infoSecurity}
          onDismiss={dismissSecurity}
          onDismissAll={() => dismissAllSecurity(infoSecurity)}
        />
      ) : null}
      {/* Calm lane: lead with the value stat people actually want to see, then keep the
          reference panels (discovery / identities / inspector) collapsed below it so they
          don't stack into a wall on first load. */}
      {(savingsLoadStatus === "error" || savingsLoadStatus === "stale") && (
        <p role="alert" className="mb-3 text-sm text-destructive">
          {savingsLoadStatus === "stale"
            ? tr("Catalog telemetry unavailable. Showing the last loaded measurements.")
            : tr("Catalog telemetry unavailable. Retry Activity to load measurements.")}
        </p>
      )}
      {savings && (savings.listLoads > 0 || (savings.discoveryCount ?? 0) > 0) ? (
        <SavingsBanner savings={savings} />
      ) : null}
      <DiscoveryTraces refreshKey={liveKey} />
      <ToolIdentities refreshKey={liveKey} />
      {registry?.liveInspect ? <LiveInspector refreshKey={liveKey} /> : null}
    </>
  );

  const servers = useMemo(
    () => [...new Set((entries ?? []).map((e) => e.server))].sort(),
    [entries],
  );

  const visible = (entries ?? []).filter(
    (e) => (!serverFilter || e.server === serverFilter) && (!errorsOnly || !e.ok),
  );

  if (entries === null && auditLoadStatus === "loading") {
    return (
      <div>
        {banner}
        <div className="flex items-center justify-center py-24 text-sm text-muted-foreground">
          {tr("Loading activity…")}
        </div>
      </div>
    );
  }

  if (entries === null && auditLoadStatus === "error") {
    return (
      <div>
        {banner}
        <div className="flex flex-col items-center justify-center gap-3 py-24 text-center">
          <AlertTriangle className="size-10 text-destructive/70" />
          <div>
            <p className="font-medium">{tr("Couldn't load activity")}</p>
            <p className="max-w-md text-sm text-muted-foreground">
              {tr(
                "Toolport couldn't reach the gateway or read the audit log. This is not an empty log, if the gateway isn't running, start it and refresh.",
              )}
            </p>
            <button
              type="button"
              onClick={retryLoads}
              aria-label={tr("Retry activity log")}
              className="mt-3 rounded-md border px-3 py-1.5 text-sm font-medium transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              {tr("Retry")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // Defensive fallback for an impossible state transition: unknown audit data must
  // still render as an error, never as a verified empty log.
  if (entries === null) {
    return (
      <div>
        {banner}
        <div className="flex items-center justify-center py-24 text-sm text-destructive">
          {tr("Activity status is unavailable. Retry the activity log.")}
        </div>
      </div>
    );
  }

  if (entries.length === 0 && auditLoadStatus === "stale") {
    return (
      <div>
        {banner}
        <AuditStaleNotice onRetry={retryLoads} />
        <div className="flex flex-col items-center justify-center gap-3 py-20 text-center">
          <AlertTriangle className="size-10 text-warning/70" />
          <div>
            <p className="font-medium">{tr("No current activity status")}</p>
            <p className="max-w-md text-sm text-muted-foreground">
              {tr(
                "The last successful read found no calls, but the latest refresh failed, so Toolport can't verify that the log is still empty.",
              )}
            </p>
          </div>
        </div>
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div>
        {banner}
        <div className="flex flex-col items-center justify-center gap-3 py-24 text-center">
          <ScrollText className="size-10 text-muted-foreground/50" />
          <div>
            <p className="font-medium">{tr("No tool calls yet")}</p>
            <p className="max-w-md text-sm text-muted-foreground">
              {tr(
                "Once a client runs a tool through Toolport, every call is recorded here, with per-server latency and error rates.",
              )}
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div>
      {banner}
      {auditLoadStatus === "stale" && <AuditStaleNotice onRetry={retryLoads} />}
      {stats && <StatsPanel stats={stats} />}

      <div className="mb-2 flex items-center gap-2">
        <button
          onClick={() => setLogOpen((v) => !v)}
          aria-expanded={logOpen}
          className="flex flex-1 items-center gap-2 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          <Trans
            message="{value1} Recent calls {value2}"
            values={{
              value1: (
                <ChevronRight
                  className={`size-4 transition-transform ${logOpen ? "rotate-90" : ""}`}
                />
              ),
              value2: (
                <span className="text-xs font-normal text-muted-foreground/70">
                  {entries.length === 1
                    ? tr("last {value1} call", { value1: entries.length })
                    : tr("last {value1} calls", { value1: entries.length })}
                </span>
              ),
            }}
          />
        </button>
        <button
          onClick={() => void exportLog()}
          title={tr("Export the full activity log to a file (CSV or JSON)")}
          className="flex shrink-0 items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
        >
          <Trans
            message="{value1} Export"
            values={{ value1: <Download className="size-3.5" aria-hidden="true" /> }}
          />
        </button>
        <ConfirmDialog
          trigger={
            <button
              title={tr("Delete all retained activity kept on this device")}
              className="flex shrink-0 items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              <Trans
                message="{value1} Clear"
                values={{ value1: <Trash2 className="size-3.5" aria-hidden="true" /> }}
              />
            </button>
          }
          title={tr("Clear retained activity?")}
          description={tr(
            "Permanently deletes everything Toolport keeps on this device: the audit log, discovery search traces, live-inspection captures, and the savings tally (including its running total). Nothing is sent anywhere; each log starts fresh on the next event. This can't be undone.",
          )}
          confirmLabel={tr("Clear activity")}
          destructive
          onConfirm={clearActivity}
        />
      </div>

      {logOpen && (
        <>
          <div className="mb-2 flex items-center gap-2">
            <Select
              value={serverFilter || "all"}
              onValueChange={(v) => setServerFilter(v === "all" ? "" : v)}
            >
              <SelectTrigger className="h-8 w-fit gap-1.5 text-sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{tr("All servers")}</SelectItem>
                {servers.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <button
              onClick={() => setErrorsOnly((v) => !v)}
              aria-pressed={errorsOnly}
              className={`h-8 rounded-md border px-2.5 text-sm transition-colors ${
                errorsOnly
                  ? "border-destructive/50 bg-destructive/10 text-destructive"
                  : "text-muted-foreground hover:bg-accent"
              }`}
            >
              {tr("Errors only")}
            </button>
            <span className="ml-auto text-xs text-muted-foreground">
              {tr("{value1} of {value2}", {
                value1: visible.length,
                value2: entries.length,
              })}
            </span>
          </div>

          <div className="flex flex-col gap-1">
            {visible.length === 0 ? (
              <div className="flex flex-col items-center gap-2 py-12 text-center">
                <p className="text-sm text-muted-foreground">
                  {errorsOnly && !serverFilter
                    ? tr("No errors in the last {value1} calls.", {
                        value1: entries.length,
                      })
                    : tr("No calls match this filter.")}
                </p>
                {(serverFilter || errorsOnly) && (
                  <button
                    onClick={() => {
                      setServerFilter("");
                      setErrorsOnly(false);
                    }}
                    className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  >
                    <Trans
                      message="{value1} Clear filter"
                      values={{ value1: <X className="size-3.5" /> }}
                    />
                  </button>
                )}
              </div>
            ) : (
              stableListKeys(visible, (e) => `${e.ts}-${e.server}-${e.tool}`).map(
                (key, i) => <CallRow key={key} e={visible[i]} />,
              )
            )}
          </div>
        </>
      )}
    </div>
  );
}
