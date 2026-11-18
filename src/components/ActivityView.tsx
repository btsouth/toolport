import { translate as tr, Trans, useI18n, msg, type SourceMessage } from "@/lib/i18n";
import {
  dismissalIndex,
  dismissalKey,
  eventSeverity,
  securityKey,
  selectSecurityEvents,
  toolChangeAsksReview,
} from "@/lib/securityReview";
import { ToolChanges } from "./ToolChanges";
import { activityClientSegments, clientIdentityTooltip } from "@/lib/clientIdentity";
import { hostedClientName } from "@/lib/hostAttribution";
import { HostThreadChip } from "@/components/HostThreadChip";
import { useWindowVisible } from "@/lib/windowVisible";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Download,
  Fingerprint,
  History,
  RefreshCw,
  ScrollText,
  Search,
  Share2,
  ShieldAlert,
  ShieldCheck,
  Ban,
  Check,
  ShieldX,
  Clock,
  Undo2,
  Sparkles,
  Wrench,
  Trash2,
  X,
  XCircle,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import {
  fmtAgo,
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
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { PageHeaderActions } from "@/components/PageHeader";
import {
  clearActivityLogs,
  exportAuditToPath,
  getActivityRevision,
  releaseActivityRevision,
  getActivityHealth,
  getAuditLog,
  getAuditStats,
  getInspectLog,
  getSavingsSummary,
  getSearchTraces,
  getSecurityEvents,
  getActivitySafety,
  getToolIdentities,
  openGatewayLog,
  type ActivitySafety,
  type SecurityEvent,
} from "@/lib/api";
import { EmptyState } from "@/components/ui/empty-state";
import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorCallout, StatusCard } from "@/components/ui/status-card";
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/** A badge describing one security event by kind. */
function eventBadge(e: SecurityEvent): { label: string; cls: string } {
  const cls =
    e.blocked || e.type === "result_injection_blocked"
      ? "bg-destructive/15 text-destructive"
      : "bg-muted text-muted-foreground";
  if (e.type === "sync_change_refused") {
    return {
      label: tr("synced change blocked"),
      cls: "bg-destructive/15 text-destructive",
    };
  }
  if (e.type === "result_injection_blocked") {
    return {
      label: tr("injection blocked"),
      cls,
    };
  }
  if (e.type === "result_injection") {
    return {
      label: tr("injected result"),
      cls,
    };
  }
  if (e.type === "tool_poison_flag") {
    return {
      label: tr("suspicious content"),
      cls,
    };
  }
  if (e.type === "pins_load_failed") {
    return {
      label: tr("integrity baseline lost"),
      cls,
    };
  }
  if (e.change === "changed") {
    return { label: tr("changed"), cls: "bg-muted text-muted-foreground" };
  }
  return { label: tr("new tool"), cls: "bg-owned/15 text-owned" };
}

const SECURITY_DISMISSED_KEY = "toolport.security.dismissed";
const SECURITY_DISMISSED_KEY_LEGACY = "conduit.security.dismissed";

/** Unique React list key. `securityKey` is deliberately timestamp-free (so dismissal is
 * durable), but two un-collapsed instances of the same drift can be on screen at once,
 * so the render key still needs the timestamp to stay unique. */
function renderKey(e: SecurityEvent): string {
  return `${securityKey(e)}:${e.ts}`;
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
 * limit across sessions. Covers the complete bounded security retention window. */
const MAX_DISMISSED = 2000;

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
    <StatusCard
      variant="success"
      icon={<Check />}
      title={tr("Protection active.")}
      detail={tr(
        "Toolport is watching tool definitions and results. Nothing needs your attention right now.",
      )}
      className="mb-4"
    />
  );
}

/** Safety Off with nothing blocked or held: say the state, never claim protection.
 * Spec §4: Off is `ban`. */
function SafetyOffNotice() {
  const { t: tr } = useI18n();
  return (
    <StatusCard
      icon={<Ban />}
      title={tr("Safety is Off.")}
      detail={tr("Toolport records tool changes but does not block them.")}
      className="mb-4"
    />
  );
}

/** A collapsed list of tool-change records, the same bordered panel GTK draws. */
function ToolChangeRecords({ label, children }: { label: string; children: ReactNode }) {
  return (
    <details className="group mb-4 rounded-xl border border-border bg-card px-4 py-3">
      <summary className="flex cursor-pointer list-none items-center gap-2 text-sm [&::-webkit-details-marker]:hidden">
        <ChevronRight className="size-4 shrink-0 text-muted-foreground transition-transform group-open:rotate-90" />
        {label}
      </summary>
      <div className="mt-3">{children}</div>
    </details>
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
        className="mb-4 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/30 px-4 py-2.5 text-meta text-muted-foreground"
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
      className="mb-4 flex items-center gap-3 toolport-alert border-warning bg-warning/8 px-4 py-2.5 text-meta"
    >
      <AlertTriangle className="size-4 shrink-0 text-muted-foreground" />
      <span className="text-muted-foreground">
        <span className="block font-semibold text-foreground">
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
    <StatusCard
      role="alert"
      variant="warning"
      icon={<AlertTriangle />}
      title={tr("Activity may be out of date.")}
      detail={tr(
        "Showing the last calls Toolport read successfully; the latest refresh failed.",
      )}
      actions={
        <Button
          variant="outline"
          size="sm"
          onClick={onRetry}
          aria-label={tr("Retry activity log")}
        >
          {tr("Retry")}
        </Button>
      }
      className="mb-4"
    />
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
  const blocked = events.some(
    (event) => event.blocked || event.type === "result_injection_blocked",
  );
  return (
    <div
      className={`mb-4 toolport-alert p-4 ${blocked ? "border-destructive/40 bg-destructive/5" : "border-border bg-muted/20"}`}
    >
      <button
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 text-left"
      >
        <ShieldAlert className="size-4 shrink-0 text-muted-foreground" />
        <h3 className="text-sm font-medium text-muted-foreground">
          {tr("Tool security notices")}
        </h3>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
          {collapsed.length}
        </span>
        <ChevronRight
          className={`ml-auto size-4 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <>
          <p className="mt-2 mb-3 max-w-2xl text-meta text-muted-foreground">
            {tr(
              "A tool changed after you approved it, a tool's definition contains instruction-like content, or a tool returned data that looks like injected instructions. Usually benign, but it's how rug pulls, tool poisoning, and agentjacking work, so Toolport flags it (and labels suspicious tool output as data). Dismiss the ones you've reviewed.",
            )}
          </p>
          <ul className="space-y-1.5 text-meta">
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
                        className="rounded-full bg-muted px-1.5 py-0.5 font-medium text-muted-foreground"
                        title={tr("Recurred in {count} separate time windows", { count })}
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
                      className="rounded p-0.5 text-muted-foreground transition-colors hover:bg-warning/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-warning"
                    >
                      <X className="size-3.5" />
                    </button>
                  </div>
                  {e.detail && (
                    <p className="ml-1 max-w-2xl text-meta leading-relaxed break-words text-muted-foreground">
                      {e.detail}
                    </p>
                  )}
                  {e.evidence && (
                    <p className="ml-1 max-w-2xl border-l-2 border-warning/40 pl-2 font-mono text-meta leading-relaxed break-words text-muted-foreground">
                      <span className="mr-1 select-none font-sans text-muted-foreground">
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
      <div className="flex w-full items-center gap-2 text-meta text-muted-foreground">
        <button
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex flex-1 items-center gap-2 text-left"
        >
          <History className="size-3.5 shrink-0" />
          <span className="font-medium text-muted-foreground">
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
          className="shrink-0 rounded px-2 py-0.5 font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border"
        >
          {tr("Dismiss all")}
        </button>
      </div>
      {open && (
        <>
          <p className="mt-2 mb-2 max-w-2xl text-meta text-muted-foreground">
            {tr(
              "New tools first seen, and benign, non-destructive changes to tools you've already approved (vendors revise beta tools server-side). Kept for the record, not flagged as a risk, the approval and destructive-tool gates still guard every call. Dismiss any you've reviewed.",
            )}
          </p>
          <ul className="space-y-1 text-meta">
            {events.slice(0, 20).map((e) => (
              <li key={renderKey(e)} className="flex items-center gap-2">
                <span className="rounded bg-muted px-1.5 py-0.5 text-muted-foreground">
                  {e.change === "added" ? tr("new tool") : tr("changed")}
                </span>
                <code className="font-mono text-muted-foreground">{e.tool}</code>
                <span className="ml-auto text-muted-foreground">{fmtTs(e.ts)}</span>
                <button
                  onClick={() => onDismiss(e)}
                  aria-label={tr("Dismiss this change")}
                  className="rounded p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border"
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

/** One session baseline minus every catalog response and search response. */
function SavingsBanner({ savings }: { savings: SavingsSummary }) {
  const { t: tr, locale } = useI18n();
  const hasCatalog = (savings.tokenizedLoads ?? 0) > 0;
  const discovered = (savings.discoveryCount ?? 0) > 0;
  // The signed formula stays exact; a negative result is labeled as a change, not savings.
  const negative = savings.tokensSaved < 0;
  const method = tr(
    "The baseline is the full-mode tools array for the same allowed servers and policies at the first tools/list in each session. We count compact JSON and search response text with cl100k_base, then subtract every tools/list and search response. Sessionless HTTP, initial catalogs above known client limits, and older records are excluded. Client caching and formatting may change model usage; this is not a billing figure.",
  );
  const exact = (value: number | undefined) => (value ?? 0).toLocaleString(locale);
  const share = async () => {
    try {
      await navigator.clipboard.writeText(
        tr(
          negative
            ? "Toolport measured a net token change of {tokens} across {sessions} client sessions. Full catalog baselines minus all catalog and search responses, counted locally with cl100k_base; not a billing figure. toolport.app"
            : "Toolport measured {tokens} net tokens saved across {sessions} client sessions. Full catalog baselines minus all catalog and search responses, counted locally with cl100k_base; not a billing figure. toolport.app",
          {
            tokens: fmtTokens(savings.tokensSaved),
            sessions: savings.tokenizedLoads ?? 0,
          },
        ),
      );
      toast.success(tr("Savings copied, paste them anywhere"));
    } catch {
      toastError(tr("Couldn't copy to clipboard"));
    }
  };
  // Green only for a real saving; a net cost reads neutral, as in GTK.
  const cost = hasCatalog && negative;
  return (
    <div
      className={`mb-6 toolport-alert p-4 ${cost ? "border-border bg-muted/20" : "border-success bg-success/8"}`}
    >
      <div className="flex items-center gap-2">
        <Sparkles
          className={`size-4 ${cost ? "text-muted-foreground" : "text-success"}`}
        />
        <span className="text-sm font-medium text-muted-foreground">
          {hasCatalog
            ? tr("Net tool description savings")
            : discovered
              ? tr("Discovery payload returned")
              : tr("Savings history")}
        </span>
      </div>
      <div className="mt-2 flex flex-wrap items-end gap-x-6 gap-y-1">
        <span
          className={`text-3xl font-semibold tabular-nums ${cost ? "text-foreground" : "text-success"}`}
          title={method}
        >
          {hasCatalog
            ? fmtTokens(savings.tokensSaved)
            : discovered
              ? fmtBytes(savings.discoveryResponseBytes ?? 0)
              : tr("Not measured yet")}{" "}
          <span className="text-base font-normal text-muted-foreground">
            {hasCatalog
              ? negative
                ? tr("net token change")
                : tr("net tokens saved")
              : discovered
                ? tr("discovery text")
                : ""}
          </span>
        </span>
      </div>
      <p className="mt-2 text-meta text-muted-foreground">
        {hasCatalog
          ? tr(
              "Full catalog baselines minus all catalog and search responses. Counted locally; not a billing figure.",
            )
          : tr(
              "Older records remain available below. Comparable session measurements start with this version.",
            )}
      </p>
      {(savings.previousReportedTokensSaved ?? 0) !== 0 && (
        <div className="mt-3 border-t border-success/20 pt-3 text-meta text-muted-foreground">
          <p className="font-medium">
            {tr("Old estimate: {tokens} tokens", {
              tokens: exact(savings.previousReportedTokensSaved),
            })}
          </p>
          <p className="mt-1">
            {tr("From previous versions; excluded from new savings.")}
          </p>
        </div>
      )}
      {hasCatalog && (
        <button
          onClick={share}
          className="mt-3 inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition hover:text-foreground"
        >
          <Trans
            message="{value1} Share"
            values={{ value1: <Share2 className="size-3.5" /> }}
          />
        </button>
      )}
      <details className="mt-3 text-meta text-muted-foreground">
        <summary className="cursor-pointer">{tr("How this is measured")}</summary>
        <div className="mt-2 space-y-2">
          <p>{method}</p>
          {hasCatalog && (
            <p>
              {tr(
                negative
                  ? "{baseline} baseline tokens − {catalog} catalog response tokens − {search} search response tokens = {net} net token change. A negative change means responses used more tokens than the baseline."
                  : "{baseline} baseline tokens − {catalog} catalog response tokens − {search} search response tokens = {net} net tokens saved.",
                {
                  baseline: exact(savings.fullTokens),
                  catalog: exact(savings.exposedTokens),
                  search: exact(savings.discoveryTokens),
                  net: exact(savings.tokensSaved),
                },
              )}
            </p>
          )}
          {(hasCatalog || discovered) && (
            <>
              <p>
                {exact(savings.tokenizedLoads)} {tr("client sessions;")}{" "}
                {exact(savings.listLoads)} {tr("catalog responses;")}{" "}
                {exact(savings.discoveryCount)} {tr("search responses.")}
              </p>
              <p>
                {fmtBytes(savings.fullSurfaceBytes ?? 0)} {tr("baseline catalog text;")}{" "}
                {fmtBytes(savings.exposedSurfaceBytes ?? 0)} {tr("catalog text sent;")}{" "}
                {fmtBytes(savings.discoveryResponseBytes ?? 0)} {tr("search text sent.")}
              </p>
            </>
          )}
          {(savings.historicalListLoads ?? 0) > 0 && (
            <p>
              {tr(
                "Excluded history: {records} catalog records ({measured} with byte measurements). Session identity and repeat loads cannot be verified.",
                {
                  records: exact(savings.historicalListLoads),
                  measured: exact(savings.historicalMeasuredLoads),
                },
              )}
            </p>
          )}
          {((savings.legacyEstimatedTokensAvoided ?? 0) > 0 ||
            (savings.estimatedTokensAvoided ?? 0) > 0) && (
            <p>
              {tr(
                "Older estimated records: {legacy} legacy tokens and {estimated} size-based tokens, excluded.",
                {
                  legacy: exact(savings.legacyEstimatedTokensAvoided),
                  estimated: exact(savings.estimatedTokensAvoided),
                },
              )}
            </p>
          )}
        </div>
      </details>
    </div>
  );
}

function errCell(errors: number, errorRate: number) {
  return fmtPercent(errorRate, { floorNonZero: errors > 0 });
}

/** One server row that expands to reveal its per-tool breakdown. */
function ServerRow({ s }: { s: ServerStat }) {
  const { t: tr, locale } = useI18n();
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
              title={s.server}
              // Bounded so a long name truncates instead of pushing the numbers
              // out of a narrow window; the title keeps it whole.
              className="flex max-w-[25vw] min-w-0 items-center gap-1.5 rounded-sm text-left focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              <ChevronRight
                aria-hidden="true"
                className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${open ? "rotate-90" : ""}`}
              />
              <span className="truncate">{s.server}</span>
              <span className="sr-only">
                {tr("{value1} per-tool breakdown", {
                  value1: open ? tr("Collapse") : tr("Expand"),
                })}
              </span>
            </button>
          ) : (
            <span
              title={s.server}
              className="flex max-w-[25vw] min-w-0 items-center gap-1.5"
            >
              <span className="inline-block size-3.5 shrink-0" />
              <span className="truncate">{s.server}</span>
            </span>
          )}
        </td>
        <td className="px-3 py-2 text-right tabular-nums">
          {s.calls.toLocaleString(locale)}
        </td>
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
            <td className="py-1.5 pr-3 pl-9 font-mono text-meta text-muted-foreground">
              {t.tool}
            </td>
            <td className="px-3 py-1.5 text-right text-xs tabular-nums text-muted-foreground">
              {t.calls.toLocaleString(locale)}
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
        className="flex shrink-0 items-center gap-1 rounded bg-warning/15 px-1.5 py-0.5 text-xs text-warning"
        title={tr(
          "{count} values masked, but the pass did not fully apply: some values reached the model in the clear (the session map was full, or the result exceeded the scan cap).",
          { count: replaced },
        )}
      >
        <ShieldAlert aria-hidden="true" className="size-3" />
        {replaced} {replaced === 1 ? tr("value") : tr("values")}{" "}
        {tr("masked, incomplete")}
      </span>
    );
  }
  if (replaced === 0) return null;
  return (
    <span
      className="flex shrink-0 items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground"
      title={tr(
        "Personal values in this result were replaced before reaching the model. The values themselves are never logged.",
      )}
    >
      <ShieldCheck aria-hidden="true" className="size-3" />
      {replaced} {replaced === 1 ? tr("value") : tr("values")} {tr("masked")}
    </span>
  );
}

function internalActivityLabel(tool: string): string {
  return tr(
    (
      {
        search: msg("Searched tools"),
        describe: msg("Looked up a tool"),
        status: msg("Checked Toolport status"),
        fetch: msg("Retrieved a tool result"),
      } as Record<string, SourceMessage>
    )[tool] ?? msg("Used Toolport"),
  );
}

function CallRow({ e }: { e: AuditEntry }) {
  const { t: tr } = useI18n();
  const [open, setOpen] = useState(false);
  const approvalOutcome =
    e.kind === "approval"
      ? (APPROVAL_OUTCOME[e.decision ?? ""] ?? UNKNOWN_APPROVAL)
      : null;
  // The icon says what kind of call this was, in a neutral color; the status
  // chip is the row's only status color, as in the GTK shell.
  const RowIcon = approvalOutcome?.Icon ?? (e.kind === "internal" ? Search : Wrench);
  const status: { label: SourceMessage; variant: StatusVariant } = approvalOutcome
    ? approvalOutcome
    : e.held
      ? { label: msg("Held"), variant: "warning" }
      : e.ok
        ? { label: msg("Succeeded"), variant: "success" }
        : { label: msg("Failed"), variant: "destructive" };
  const duration = approvalOutcome ? e.heldMs : e.durationMs;
  // The client name truncates on its own so the time and duration stay visible.
  const clientName = activityClientSegments(e, (name) => hostedClientName(name, e)).join(
    " · ",
  );
  const when = [
    fmtAgo(e.ts),
    ...(e.failureKind ? [e.failureKind.replace(/_/g, " ")] : []),
    ...(duration == null
      ? []
      : [
          approvalOutcome
            ? tr("waited {duration}", { duration: fmtMs(duration) })
            : fmtMs(duration),
        ]),
  ].join(" · ");
  const meta = [clientName, when].filter(Boolean).join(" · ");
  const hasDetail = !approvalOutcome && !e.ok && (!!e.error || !!e.runId);
  return (
    <div
      className={`border-b border-border text-sm last:border-b-0 ${e.kind === "internal" ? "bg-muted/30" : ""}`}
    >
      <div
        className={`@container flex min-h-14 items-center gap-3 px-4 py-3 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring focus-visible:outline-none ${
          hasDetail ? "cursor-pointer hover:bg-foreground/[0.04]" : ""
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
        <span className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <RowIcon aria-hidden="true" className="size-4" />
        </span>
        {/* Narrow rows put the chips under the title and let the detail line wrap, so
            neither squeezes the title. */}
        <div className="flex min-w-0 flex-1 flex-col gap-1.5 @xl:flex-row @xl:items-center @xl:gap-3">
          <div className="min-w-0 @xl:flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <span className="min-w-0 truncate font-medium">
                {e.kind === "internal" ? internalActivityLabel(e.tool) : e.server}
              </span>
              {e.kind !== "internal" && (
                <span className="min-w-0 truncate font-mono text-meta text-muted-foreground">
                  {e.tool}
                </span>
              )}
            </div>
            <div
              className="mt-1 break-words text-meta text-muted-foreground @xl:flex @xl:min-w-0 @xl:overflow-hidden @xl:whitespace-nowrap"
              title={[
                meta,
                clientIdentityTooltip(e),
                e.client && tr("Client: {value}", { value: e.client }),
                e.sessionId && tr("Session: {value}", { value: e.sessionId }),
                e.runId && tr("Run: {value}", { value: e.runId }),
                e.cold != null && (e.cold ? tr("cold catalog") : tr("warm catalog")),
                e.dispatchMs != null &&
                  tr("dispatch {duration}", { duration: fmtMs(e.dispatchMs) }),
                e.kind === "internal" && tr("Toolport: {tool}", { tool: e.tool }),
              ]
                .filter(Boolean)
                .join(" · ")}
            >
              <span className="@xl:min-w-0 @xl:truncate" dir="auto">
                {clientName}
              </span>
              <span className="@xl:shrink-0 @xl:whitespace-pre">
                {clientName ? ` · ${when}` : when}
              </span>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2 @xl:shrink-0 @xl:flex-nowrap">
            <HostThreadChip row={e} />
            <PiiBadge entry={e} />
            <Badge variant={status.variant}>{tr(status.label)}</Badge>
          </div>
        </div>
      </div>
      {open && hasDetail && (
        <div className="border-t border-border bg-destructive/5 py-2 pr-4 pl-[86px]">
          <p className="font-mono text-meta whitespace-pre-wrap break-words text-destructive">
            {e.error ?? e.failureKind?.replace(/_/g, " ")}
          </p>
          {e.runId && (
            <p className="mt-1 font-mono text-meta">
              {tr("Run: {run}", { run: e.runId })}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

type StatusVariant = "success" | "warning" | "destructive" | "off";

const APPROVAL_OUTCOME: Record<
  string,
  { label: SourceMessage; Icon: LucideIcon; variant: StatusVariant }
> = {
  approved: { label: msg("Approved"), Icon: ShieldCheck, variant: "success" },
  denied: { label: msg("Denied"), Icon: ShieldX, variant: "destructive" },
  no_response: { label: msg("No answer"), Icon: Clock, variant: "warning" },
  withdrawn: { label: msg("Withdrawn"), Icon: Undo2, variant: "off" },
  stale_state: {
    label: msg("Changed after approval"),
    Icon: AlertTriangle,
    variant: "warning",
  },
  unreachable: {
    label: msg("No approver available"),
    Icon: AlertTriangle,
    variant: "warning",
  },
};
const UNKNOWN_APPROVAL = {
  label: msg("Approval event"),
  Icon: ShieldAlert,
  variant: "off" as const,
};

export function TelemetryNotice({
  stats,
}: {
  stats: Pick<AuditStats, "telemetry" | "gatewayNotes"> | null;
}) {
  const { t: tr } = useI18n();
  const health = stats?.telemetry;
  const messages = [...(stats?.gatewayNotes ?? [])];
  if (
    health &&
    (health.queueDropped ||
      health.writeFailedRecords ||
      health.writeFailures ||
      health.incompleteFlushes)
  ) {
    messages.push(
      tr(
        "Activity, savings, Teams reporting and diagnostics may be incomplete: {dropped} records dropped, {unconfirmed} records with unconfirmed writes, {failures} write failures, {flushes} incomplete flushes since gateway start.",
        {
          dropped: health.queueDropped,
          unconfirmed: health.writeFailedRecords,
          failures: health.writeFailures,
          flushes: health.incompleteFlushes,
        },
      ),
    );
  }
  if (health?.retainedDropped) {
    messages.push(
      tr(
        "{count} dropped telemetry records are recorded in retained history. Activity and savings may be incomplete.",
        { count: health.retainedDropped },
      ),
    );
  }
  if (health?.unavailable) {
    messages.push(
      tr(
        "Gateway telemetry health is unavailable. Activity and savings may be incomplete.",
      ),
    );
  }
  if (messages.length === 0) return null;
  return (
    <div
      role="status"
      className="mb-4 toolport-alert border-warning bg-warning/8 px-4 py-2.5 text-meta"
    >
      {messages.map((message) => (
        <p key={message}>{message}</p>
      ))}
    </div>
  );
}

function StatsPanel({ stats }: { stats: AuditStats }) {
  const { t: tr, locale } = useI18n();
  // The per-server table is part of the page's main content, so it starts open (as in
  // GTK); one tap folds it away.
  const [tableOpen, setTableOpen] = useState(true);
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
          <div className="text-2xl font-semibold tabular-nums">
            {stats.total.toLocaleString(locale)}
          </div>
          <div className="text-meta text-muted-foreground">{tr("calls recorded")}</div>
        </div>
        <div
          className={`rounded-lg border p-3 ${stats.errors > 0 ? "border-destructive/40 bg-destructive/[0.04]" : ""}`}
        >
          <div
            className={`text-2xl font-semibold tabular-nums ${stats.errors > 0 ? "text-destructive" : ""}`}
          >
            {stats.errors.toLocaleString(locale)}
          </div>
          <div className="text-meta text-muted-foreground">
            {tr("errors ({value1})", { value1: errPct })}
          </div>
        </div>
        <div className="rounded-lg border p-3">
          <div className="text-2xl font-semibold tabular-nums">
            {stats.servers.length.toLocaleString(locale)}
          </div>
          <div className="text-meta text-muted-foreground">{tr("servers used")}</div>
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
              <span className="text-meta font-normal text-muted-foreground">
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
          <p className="text-meta text-muted-foreground">
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
    <div className="border-b border-border text-sm">
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
        <span className="min-w-0 truncate font-mono text-meta text-muted-foreground">
          {e.tool}
        </span>
        {(e.clientName || e.client) && (
          <span
            className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground"
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
        <span className="shrink-0 text-meta text-muted-foreground">
          {fmtTs(e.ts, "time")}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9">
          <div className="mb-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">
            {tr("Request")}
          </div>
          <pre className="mb-3 overflow-x-auto rounded bg-background/60 p-2 font-mono text-meta whitespace-pre-wrap break-words">
            {fmtBody(e.request)}
          </pre>
          <div className="mb-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">
            {tr("Response")}
          </div>
          <pre className="overflow-x-auto rounded bg-background/60 p-2 font-mono text-meta whitespace-pre-wrap break-words">
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
      ? tr("{returned} of {total}", { returned: t.returned, total: t.total })
      : tr("no match");
  return (
    <div className="border-b border-border text-sm">
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
        <span className="min-w-0 truncate font-mono text-meta">
          {tr("“{value1}”", { value1: t.query || tr("(empty)") })}
        </span>
        {t.client && (
          <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
            {t.client}
          </span>
        )}
        {t.escalated && (
          <span className="shrink-0 rounded bg-warning/15 px-1.5 py-0.5 text-xs text-warning">
            {tr("loop-broken")}
          </span>
        )}
        {t.mode === "semantic" && (
          <span className="shrink-0 rounded bg-owned/10 px-1.5 py-0.5 text-xs text-owned">
            {tr("semantic")}
          </span>
        )}
        <span className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground">
          {resultSummary}
        </span>
        <span className="shrink-0 text-meta text-muted-foreground">
          {fmtTs(t.ts, "time")}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9 text-meta">
          {hit ? (
            t.ranking && t.ranking.length > 0 ? (
              <div className="mb-2 space-y-1">
                {t.ranking.map((r) => (
                  <div key={r.name} className="flex items-baseline gap-2">
                    <span className="w-5 shrink-0 text-right tabular-nums text-xs text-muted-foreground">
                      #{r.rank}
                    </span>
                    <span
                      className={`shrink-0 font-mono text-meta ${
                        r.name === t.top ? "text-owned" : "text-foreground"
                      }`}
                    >
                      {r.name}
                    </span>
                    <span className="min-w-0 truncate text-meta text-muted-foreground">
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
                    className={`rounded px-1.5 py-0.5 font-mono text-meta ${
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
function PanelLoadingNotice({ label }: { label: string; icon?: LucideIcon }) {
  const { t: tr } = useI18n();
  return (
    <SkeletonRows
      className="mb-6"
      label={tr("Loading {value1}…", { value1: label.toLowerCase() })}
    />
  );
}

/** A failed load for a self-contained Activity panel. Distinguishes "couldn't check"
 * from "nothing to show" with an explicit retry, so a backend failure never collapses
 * into the same wording as a genuinely empty panel (#728). */
function PanelErrorNotice({ label, onRetry }: { label: string; onRetry: () => void }) {
  const { t: tr } = useI18n();
  return (
    <ErrorCallout
      className="mb-6"
      title={tr("Couldn't load {label}.", { label: label.toLocaleLowerCase() })}
      cause={tr("This isn't necessarily empty — Toolport couldn't check.")}
      onRetry={onRetry}
      retryAriaLabel={tr("Retry loading {value1}", { value1: label.toLowerCase() })}
    />
  );
}

function DiscoveryTraces({
  refreshKey,
  searches,
}: {
  refreshKey: number;
  searches: number;
}) {
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Clear the retry error before the new backend request starts.
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
      <div className="mb-6 rounded-lg border border-border/60 bg-muted/20 p-4 text-meta text-muted-foreground">
        <div className="mb-1 flex items-center gap-2">
          <Search className="size-4 shrink-0 text-owned" />
          <span className="font-medium text-muted-foreground">{tr("Discovery")}</span>
        </div>
        {searches > 0
          ? tr(
              "{count} searches in recent activity; detailed search records are not available.",
              { count: searches },
            )
          : tr("Nothing searched yet.")}
      </div>
    );

  return (
    <div className="mb-6 toolport-alert border-owned bg-owned/8 p-4">
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
          <p className="mt-2 mb-3 max-w-2xl text-meta text-muted-foreground">
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
    <div className="border-b border-border text-sm">
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
        <span className="min-w-0 truncate font-mono text-meta">{t.alias}</span>
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
          {t.serverName || t.serverId || tr("unattributed")}
        </span>
        {t.quarantined && (
          <span className="shrink-0 rounded bg-warning/15 px-1.5 py-0.5 text-xs text-warning">
            {tr("quarantined")}
          </span>
        )}
        <span
          className="ml-auto shrink-0 font-mono text-meta text-muted-foreground"
          title={t.fingerprint}
        >
          {fpShort}
        </span>
      </div>
      {open && (
        <div className="border-t border-border/50 bg-muted/20 px-3 py-2 pl-9 text-meta">
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
                <dd className="text-muted-foreground">
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
    <div className="border-b border-border">
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
        <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
          {items.length}
        </span>
        {quarantined > 0 && (
          <span className="shrink-0 rounded-full bg-warning/15 px-1.5 py-0.5 text-xs font-medium text-warning">
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Clear the retry error before the new backend request starts.
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
          <p className="mt-2 mb-3 max-w-2xl text-meta text-muted-foreground">
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
            <p className="py-6 text-center text-meta text-muted-foreground">
              {tr('No server or tool matches "{value1}".', { value1: query })}
            </p>
          ) : (
            <>
              {q && (
                <p className="mb-2 text-meta text-muted-foreground">
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Clear the retry error before the new backend request starts.
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
    <div className="mb-6 toolport-alert border-info bg-info/8 p-4">
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
          <p className="mt-2 mb-3 max-w-2xl text-meta text-muted-foreground">
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
  onReviewQuarantine,
}: {
  refreshKey: number;
  registry: Registry | null;
  onReviewQuarantine?: () => void;
}) {
  const { t: tr, locale } = useI18n();
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
  // Unknown keeps reviews on and claims neither Safety Off nor active protection.
  const [safety, setSafety] = useState<ActivitySafety>({
    toolChangeReviews: true,
    blocksNothing: false,
    reviewsConfirmed: false,
  });
  const reviewsEnabled = safety.toolChangeReviews;
  const [dismissed, setDismissed] = useState<Set<string>>(loadDismissed);
  // The call list is the page's main content: open by default, as in GTK.
  const [logOpen, setLogOpen] = useState(true);
  // Distinguish a load FAILURE (gateway unreachable / audit log unreadable) from a
  // genuinely empty log: both leave `entries` empty, but only one is an error. Without
  // this, a first-run backend failure renders the friendly "No tool calls yet" state
  // and hides that anything is wrong.
  const [auditLoadStatus, setAuditLoadStatus] = useState<LoadStatus>("loading");
  // The real cause of a failed audit read, shown as the error's cause line.
  const [auditError, setAuditError] = useState<string | null>(null);
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

  // Poll a cached content revision. Idle pages keep rows, stats and open panels;
  // native visibility also pauses this cheap check while hidden to the tray.
  const windowVisible = useWindowVisible();
  const [liveTick, setLiveTick] = useState(0);
  const [revisionUnavailable, setRevisionUnavailable] = useState(false);
  const revisionChecking = useRef(false);
  useEffect(() => {
    if (!windowVisible) return;
    let alive = true;
    let previous: string | undefined;
    async function checkRevision() {
      if (revisionChecking.current) return;
      revisionChecking.current = true;
      try {
        const revision = await getActivityRevision();
        if (!alive) return;
        setRevisionUnavailable(false);
        // Initial panel reads can precede a delayed digest. Reconcile once after
        // the first successful baseline, then leave genuinely idle rows alone.
        if (previous !== revision) setLiveTick((t) => t + 1);
        previous = revision;
      } catch {
        // Let panel readers surface an unreadable store rather than masking it
        // with a successful old revision.
        if (alive) {
          setRevisionUnavailable(true);
          setLiveTick((t) => t + 1);
        }
      } finally {
        revisionChecking.current = false;
      }
    }
    void checkRevision();
    const id = setInterval(() => void checkRevision(), 3000);
    return () => {
      alive = false;
      clearInterval(id);
      void releaseActivityRevision().catch(() => {});
    };
  }, [windowVisible]);
  const [liveHealth, setLiveHealth] = useState<Pick<
    AuditStats,
    "telemetry" | "gatewayNotes"
  > | null>(null);
  const [healthUnavailable, setHealthUnavailable] = useState(false);
  const healthChecking = useRef(false);
  useEffect(() => {
    if (!windowVisible) return;
    let alive = true;
    async function checkHealth() {
      if (healthChecking.current) return;
      healthChecking.current = true;
      try {
        const health = await getActivityHealth();
        if (alive) {
          setLiveHealth(health);
          setHealthUnavailable(false);
        }
      } catch {
        if (alive) setHealthUnavailable(true);
      } finally {
        healthChecking.current = false;
      }
    }
    const id = setInterval(() => void checkHealth(), 15_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [windowVisible]);
  const noticeHealth = liveHealth ?? stats;
  const noticeStats = {
    gatewayNotes: noticeHealth?.gatewayNotes,
    telemetry: {
      queueDropped: 0,
      writeFailedRecords: 0,
      writeFailures: 0,
      incompleteFlushes: 0,
      ...noticeHealth?.telemetry,
      retainedDropped: stats?.telemetry?.retainedDropped,
      ...(healthUnavailable ? { unavailable: true } : {}),
    },
  };
  // Combined so panels refetch on either a registry change or a live tick. Both counters
  // only increase, so the sum always changes when either does (no collisions).
  const liveKey = refreshKey + liveTick;

  useEffect(() => {
    if (!windowVisible) return;
    let alive = true;
    const generation = auditGeneration.current;
    getAuditLog(200)
      .then((e) => {
        if (!alive || generation !== auditGeneration.current) return;
        setEntries(e);
        setAuditLoadStatus("ready");
        setAuditError(null);
      })
      .catch((error: unknown) => {
        if (!alive || generation !== auditGeneration.current) return;
        // Tauri rejects with the command's message; an Error carries it in `message`.
        setAuditError(error instanceof Error ? error.message : String(error));
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
    // Safety and security events land in one render: the Off card hides when a
    // tool is held, so it must never pair fresh events with an older Safety
    // answer. Rust decides from the registry on disk; any failure keeps reviews on.
    void Promise.allSettled([getActivitySafety(), getSecurityEvents(2000)]).then(
      ([safetyResult, eventsResult]) => {
        if (!alive) return;
        const answer = safetyResult.status === "fulfilled" ? safetyResult.value : null;
        setSafety({
          toolChangeReviews: answer?.toolChangeReviews !== false,
          blocksNothing:
            answer?.toolChangeReviews === false && answer.blocksNothing === true,
          reviewsConfirmed:
            answer?.toolChangeReviews === true && answer.reviewsConfirmed === true,
        });
        if (eventsResult.status === "fulfilled") {
          const s = eventsResult.value;
          setSecurity((previous) =>
            JSON.stringify(previous) === JSON.stringify(s) ? previous : s,
          );
          setSecurityLoadStatus("ready");
        } else {
          // A failed refresh must preserve prior findings and must never produce
          // the reassuring verified-empty state.
          setSecurityLoadStatus((status) =>
            status === "ready" || status === "stale" ? "stale" : "error",
          );
        }
      },
    );
    return () => {
      alive = false;
    };
  }, [liveKey, reloadTick, windowVisible]);

  // Export the full retained audit log in the format picked from the Export menu;
  // the save dialog offers that format only. CSV is formula-injection-safe in the
  // backend.
  async function exportLog(format: "csv" | "json") {
    try {
      const path = await save({
        title: tr("Export activity log"),
        defaultPath: `toolport-activity.${format}`,
        filters: [{ name: format.toUpperCase(), extensions: [format] }],
      });
      if (!path) return;
      await exportAuditToPath(path, format);
      toast.success(tr("Exported activity log"));
    } catch (e) {
      toastError(tr("Couldn't export the log: {value1}", { value1: String(e) }));
    }
  }

  const reviewedIndex = useMemo(() => dismissalIndex(dismissed), [dismissed]);
  const liveSecurity = useMemo(
    () => selectSecurityEvents(security, dismissed, reviewedIndex),
    [security, dismissed, reviewedIndex],
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
  // With nothing able to block, verified-unblocked tool changes are plain history.
  const toolChanges = useMemo(
    () =>
      liveSecurity.filter(
        (e) =>
          e.type === "tool_drift" &&
          !e.historical &&
          toolChangeAsksReview(e, reviewsEnabled),
      ),
    [liveSecurity, reviewsEnabled],
  );
  const olderToolChanges = useMemo(
    () =>
      liveSecurity.filter(
        (e) =>
          e.type === "tool_drift" &&
          e.historical &&
          toolChangeAsksReview(e, reviewsEnabled),
      ),
    [liveSecurity, reviewsEnabled],
  );
  const recordedToolChanges = useMemo(
    () =>
      liveSecurity.filter(
        (e) => e.type === "tool_drift" && !toolChangeAsksReview(e, reviewsEnabled),
      ),
    [liveSecurity, reviewsEnabled],
  );
  const highSecurity = liveSecurity.filter(
    (e) => e.type !== "tool_drift" && eventSeverity(e) === "high" && !isNewTool(e),
  );
  const infoSecurity = liveSecurity.filter(
    (e) => e.type !== "tool_drift" && (eventSeverity(e) !== "high" || isNewTool(e)),
  );
  const dismissSecurity = (e: SecurityEvent) => {
    setDismissed((prev) => addDismissed(prev, [dismissalKey(e)]));
  };
  // Clear a whole batch at once (the quiet lane can hold dozens of first-sightings after
  // a re-baseline; making the user dismiss each would just recreate the noise problem).
  const dismissAllSecurity = (events: SecurityEvent[]) => {
    setDismissed((prev) => addDismissed(prev, events.map(dismissalKey)));
  };

  const banner = (
    <>
      <TelemetryNotice stats={noticeStats} />
      {revisionUnavailable ? (
        <p role="status" className="mb-4 text-sm text-destructive">
          {tr("Activity change checks are unavailable. Displayed activity may be stale.")}
        </p>
      ) : null}
      {/* Loud lane: the only thing here that may need a decision. */}
      {securityLoadStatus !== "ready" ? (
        <SecurityLoadNotice status={securityLoadStatus} onRetry={retryLoads} />
      ) : null}
      {/* Safety Off says what state you are in first, and only while nothing on this
          screen or in the stores is blocked or held. Notices block nothing at Off,
          so they follow the card. */}
      {!reviewsEnabled &&
      safety.blocksNothing &&
      securityLoadStatus === "ready" &&
      toolChanges.length === 0 &&
      olderToolChanges.length === 0 ? (
        <SafetyOffNotice />
      ) : null}
      {toolChanges.length > 0 ? (
        <ToolChanges
          events={toolChanges}
          registry={registry}
          onAccept={dismissAllSecurity}
          onReview={onReviewQuarantine}
        />
      ) : null}
      {olderToolChanges.length > 0 ? (
        <ToolChangeRecords
          label={`${olderToolChanges.length} ${tr("older tool-change records. Open history to review them.")}`}
        >
          <ToolChanges
            events={olderToolChanges}
            registry={registry}
            onAccept={dismissAllSecurity}
            onReview={onReviewQuarantine}
          />
        </ToolChangeRecords>
      ) : null}
      {recordedToolChanges.length > 0 ? (
        <ToolChangeRecords
          label={tr("{value0} tool-change records", {
            value0: recordedToolChanges.length,
          })}
        >
          <ToolChanges
            events={recordedToolChanges}
            registry={registry}
            onAccept={dismissAllSecurity}
            history
          />
        </ToolChangeRecords>
      ) : null}
      {highSecurity.length > 0 ? (
        <SecurityNotices events={highSecurity} onDismiss={dismissSecurity} />
      ) : safety.reviewsConfirmed &&
        securityLoadStatus === "ready" &&
        toolChanges.length === 0 ? (
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
      {savings &&
      ((savings.tokenizedLoads ?? 0) > 0 ||
        (savings.discoveryCount ?? 0) > 0 ||
        (savings.historicalListLoads ?? 0) > 0 ||
        (savings.previousReportedTokensSaved ?? 0) !== 0) ? (
        <SavingsBanner savings={savings} />
      ) : null}
      {/* Both count reloadTick, so Clear empties them too (both keys only grow). */}
      <DiscoveryTraces
        refreshKey={liveKey + reloadTick}
        searches={
          (entries ?? []).filter((e) => e.kind === "internal" && e.tool === "search")
            .length
        }
      />
      <ToolIdentities refreshKey={liveKey} />
      {registry?.liveInspect ? <LiveInspector refreshKey={liveKey + reloadTick} /> : null}
    </>
  );

  const servers = useMemo(
    () => [...new Set((entries ?? []).map((e) => e.server))].sort(),
    [entries],
  );

  // Export and Clear stay in the page header on every Activity state, as in GTK.
  const headerActions = (
    <PageHeaderActions>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="outline"
            title={tr("Export the full activity log to a file (CSV or JSON)")}
          >
            <Download className="size-4" aria-hidden="true" />
            {tr("Export")}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onSelect={() => void exportLog("json")}>
            {tr("Export JSON")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void exportLog("csv")}>
            {tr("Export CSV")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <ConfirmDialog
        trigger={
          <Button
            variant="destructive"
            title={tr("Delete all retained activity kept on this device")}
          >
            <Trash2 className="size-4" aria-hidden="true" />
            {tr("Clear")}
          </Button>
        }
        title={tr("Clear retained activity?")}
        description={tr(
          "Permanently deletes everything Toolport keeps on this device: the audit log, discovery search traces, live-inspection captures, and the savings tally (including its running total). Nothing is sent anywhere; each log starts fresh on the next event. This can't be undone.",
        )}
        confirmLabel={tr("Clear activity")}
        destructive
        onConfirm={clearActivity}
      />
    </PageHeaderActions>
  );

  const visible = (entries ?? []).filter(
    (e) =>
      (!serverFilter || e.server === serverFilter) &&
      (!errorsOnly || (e.kind !== "approval" && !e.ok)),
  );

  if (entries === null && auditLoadStatus === "loading") {
    return (
      <div>
        {headerActions}
        {banner}
        <SkeletonRows label={tr("Loading activity…")} />
      </div>
    );
  }

  if (entries === null && auditLoadStatus === "error") {
    return (
      <div>
        {headerActions}
        {banner}
        <ErrorCallout
          title={tr("Couldn't load activity")}
          cause={auditError ?? undefined}
          onRetry={retryLoads}
          retryAriaLabel={tr("Retry activity log")}
          onViewLog={() =>
            void openGatewayLog().catch((e: unknown) => toastError(String(e)))
          }
        />
      </div>
    );
  }

  // Defensive fallback for an impossible state transition: unknown audit data must
  // still render as an error, never as a verified empty log.
  if (entries === null) {
    return (
      <div>
        {headerActions}
        {banner}
        <ErrorCallout
          title={tr("Activity status is unavailable. Retry the activity log.")}
          onRetry={retryLoads}
          retryAriaLabel={tr("Retry activity log")}
        />
      </div>
    );
  }

  if (entries.length === 0 && auditLoadStatus === "stale") {
    // One card: the stale notice already says the refresh failed and offers Retry.
    return (
      <div>
        {headerActions}
        {banner}
        <StatusCard
          role="alert"
          variant="warning"
          icon={<AlertTriangle />}
          title={tr("No current activity status")}
          detail={tr(
            "The last successful read found no calls, but the latest refresh failed, so Toolport can't verify that the log is still empty.",
          )}
          actions={
            <Button
              variant="outline"
              size="sm"
              onClick={retryLoads}
              aria-label={tr("Retry activity log")}
            >
              {tr("Retry")}
            </Button>
          }
        />
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div>
        {headerActions}
        {banner}
        <EmptyState
          icon={<ScrollText />}
          title={tr("No activity yet")}
          description={tr(
            "Calls and approval outcomes appear here. Call totals and error rates count tools that ran.",
          )}
          action={
            <Button variant="outline" onClick={retryLoads}>
              <RefreshCw /> {tr("Refresh")}
            </Button>
          }
        />
      </div>
    );
  }

  return (
    <div>
      {headerActions}
      {banner}
      {auditLoadStatus === "stale" && <AuditStaleNotice onRetry={retryLoads} />}
      {stats && <StatsPanel stats={stats} />}

      <div className="mb-2 flex items-center gap-2">
        <button
          onClick={() => setLogOpen((v) => !v)}
          aria-expanded={logOpen}
          className="flex flex-1 items-center gap-2 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          <ChevronRight
            className={`size-4 transition-transform ${logOpen ? "rotate-90" : ""}`}
          />
          {tr("Recent calls and approvals")}
          <span className="text-meta font-normal text-muted-foreground">
            {tr("latest {count}", { count: entries.length })}
          </span>
        </button>
      </div>

      <p className="mb-2 text-meta text-muted-foreground">
        {/* The total only when the log holds more calls than the list has entries,
            so the two numbers never contradict (as in GTK). */}
        {stats && stats.total > entries.length
          ? tr(
              stats.total === 1
                ? "{count} call recorded on this computer. "
                : "{count} calls recorded on this computer. ",
              { count: stats.total.toLocaleString(locale) },
            )
          : ""}
        {logOpen
          ? visible.length === entries.length
            ? entries.length === 1
              ? tr("Showing the latest entry.")
              : tr("Showing the latest {count} entries.", {
                  count: visible.length.toLocaleString(locale),
                })
            : visible.length === 1
              ? tr("Showing 1 matching entry from the latest {total}.", {
                  total: entries.length.toLocaleString(locale),
                })
              : tr("Showing {matching} matching entries from the latest {total}.", {
                  matching: visible.length.toLocaleString(locale),
                  total: entries.length.toLocaleString(locale),
                })
          : entries.length === 1
            ? tr("Latest entry available.")
            : tr("Latest {count} entries available.", {
                count: entries.length.toLocaleString(locale),
              })}
      </p>

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
            <span className="ml-auto text-meta text-muted-foreground">
              {serverFilter || errorsOnly ? tr("Filtered") : ""}
            </span>
          </div>

          <div className={visible.length === 0 ? "flex flex-col" : "toolport-list"}>
            {visible.length === 0 ? (
              <EmptyState
                icon={<Search />}
                title={
                  errorsOnly && !serverFilter
                    ? entries.length === 1
                      ? tr("No errors in the latest entry.")
                      : tr("No errors in the latest {count} entries.", {
                          count: entries.length.toLocaleString(locale),
                        })
                    : tr("No calls match this filter.")
                }
                action={
                  (serverFilter || errorsOnly) && (
                    <Button
                      variant="outline"
                      onClick={() => {
                        setServerFilter("");
                        setErrorsOnly(false);
                      }}
                    >
                      <Trans message="{value1} Clear filter" values={{ value1: <X /> }} />
                    </Button>
                  )
                }
              />
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
