import { ErrorCallout, StatusCard } from "@/components/ui/status-card";
import { dismissClientNotice, useClientNotices } from "@/lib/clientNotice";
import { msg, Trans, translate, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { trustedClientName, clientIdentityTooltip } from "@/lib/clientIdentity";
import { getClientSessions, t3CodeOtherInstances, t3CodeRevision } from "@/lib/api";
import type { ClientActivity, T3CodeOther } from "@/lib/types";
import { fmtAgo } from "@/lib/utils";
import { type RefObject, useEffect, useId, useRef, useState } from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Download,
  Info,
  Layers,
  MonitorCog,
  Puzzle,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";
import { ClientRecoveryBanners } from "@/components/ClientRecoveryBanners";
import { ConnectReviewDialog } from "@/components/ConnectReviewDialog";
import { requestClientRecoveryScan } from "@/lib/clientRecoveryEvents";
import { ClientLogo } from "@/components/ClientLogo";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useWindowVisible } from "@/lib/windowVisible";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { SectionHeader } from "@/components/ui/section-header";
import { SkeletonRows } from "@/components/ui/skeleton";
import { clientDiscovery } from "@/lib/clientDiscovery";
import {
  T3_CODE_NAME,
  compareClientNames,
  connectAllOutcome,
  connectAllSummary,
  t3CodeActivitySegment,
  t3CodeBadge,
  t3CodeGapLines,
  t3CodeLeftOut,
  t3CodeLeftOutLines,
  t3CodeAnyEnabled,
  t3CodeEnabledNames,
  t3CodeInstanceCount,
  t3CodeStatus,
  t3CodeTargets,
  t3CodeUnconnectedCount,
} from "@/lib/hostAttribution";
import {
  importableServers,
  isGatewayDetected,
  type DetectedClient,
  type Registry,
} from "@/lib/types";

type ClientStatus = "connected" | "ready" | "customized" | "error" | "missing";

/** A chip whose text wraps onto more lines instead of hiding names behind an ellipsis;
 * a smaller radius keeps the corners of a tall chip from cutting into its text. */
const WRAPPING_CHIP =
  "h-auto min-w-0 rounded-lg max-w-full justify-start whitespace-normal text-left font-normal [overflow-wrap:anywhere]";

function statusOf(client: DetectedClient): ClientStatus {
  if (client.error) return "error";
  if (client.entryState === "customized") return "customized";
  if (client.gatewayInstalled) return "connected";
  if (client.appPresent) return "ready";
  return "missing";
}

function sortClients(clients: DetectedClient[]): DetectedClient[] {
  const rank = (client: DetectedClient) => {
    switch (statusOf(client)) {
      case "connected":
        return 0;
      case "ready":
        return 1;
      case "customized":
        return 2;
      case "error":
        return 3;
      case "missing":
        return 4;
    }
  };
  return [...clients].sort((a, b) => rank(a) - rank(b) || compareClientNames(a, b));
}

function StatusBadge({ status }: { status: ClientStatus }) {
  const { t } = useI18n();
  switch (status) {
    case "connected":
      return <Badge variant="success">{t("Connected")}</Badge>;
    case "ready":
      return <Badge variant="info">{t("Ready to connect")}</Badge>;
    case "customized":
      return <Badge variant="warning">{t("Custom configuration")}</Badge>;
    case "error":
      return <Badge variant="warning">{t("Couldn't read config")}</Badge>;
    case "missing":
      return <Badge variant="off">{t("Not installed")}</Badge>;
  }
}

/** The short discovery summary for a connected client, and the full sentence. */
function discoverySummary(client: DetectedClient, registry: Registry | null) {
  const { autoFullMax, clientMode, effectiveMode } = clientDiscovery(client, registry);
  if (!clientMode && autoFullMax) {
    const text = translate("Auto: {mode}", {
      mode: translate("full up to {count} tools, then lazy", { count: autoFullMax }),
    });
    return { text, full: text };
  }
  switch (effectiveMode) {
    case "full":
      return {
        text: translate("Full tool list"),
        full: translate(
          "Full tool list. Client per-tool permission rules need Full mode.",
        ),
      };
    case "grouped":
      return {
        text: translate("Browse a server, then call tools"),
        full: translate(
          "Browse a server, then call tools. Client per-tool permission rules need Full mode.",
        ),
      };
    default:
      return {
        text: translate("Search, then call tools"),
        full: translate(
          "Search, then call tools. Client per-tool permission rules need Full mode.",
        ),
      };
  }
}

/** The one-line detail under a client's name, and the fuller text for its tooltip.
 * Matches the GTK Clients page (linux_native/client_rows.rs). */
function clientDetail(
  client: DetectedClient,
  registry: Registry | null,
  asProfile: boolean,
): { text: string; full: string } {
  const status = statusOf(client);
  if (status === "missing")
    return {
      text: translate("Install this client to configure it with Toolport"),
      full: translate("Install this client to configure it with Toolport"),
    };
  if (status === "error") {
    const text = translate("Configuration could not be read safely");
    return { text, full: client.error ? `${text}\n${client.error}` : text };
  }
  const parts: string[] = [];
  const profile = asProfile ? client.claudeProfile : undefined;
  if (profile) {
    // A profile without a known command is already named by its folder.
    if (profile.command) parts.push(profile.folder);
    if (profile.lastUsedMs)
      parts.push(
        translate("Last active {value0}", { value0: fmtAgo(profile.lastUsedMs) }),
      );
  }
  let discovery: string | null = null;
  if (status === "connected") {
    const others =
      client.servers.filter((server) => !isGatewayDetected(server)).length +
      client.pluginServers.length;
    if (!client.usesConnectors && others > 0)
      parts.push(
        translate(
          others === 1 ? msg("{value0} other server") : msg("{value0} other servers"),
          { value0: others },
        ),
      );
    const summary = discoverySummary(client, registry);
    parts.push(summary.text);
    discovery = summary.full;
  } else if (client.usesConnectors) {
    parts.push(translate("Uses account connectors"));
  } else if (!profile) {
    const count = client.servers.length + client.pluginServers.length;
    parts.push(
      count === 0
        ? translate("No local MCP servers")
        : translate(
            count === 1
              ? msg("{value0} local MCP server")
              : msg("{value0} local MCP servers"),
            { value0: count },
          ),
    );
  }
  const text = parts.join(" · ");
  const full = [text, client.configPath, discovery].filter(Boolean).join("\n");
  return { text, full };
}

function ClientRow({
  client,
  registry,
  importCount,
  onSelect,
  variant = "line",
}: {
  client: DetectedClient;
  registry: Registry | null;
  importCount: number;
  onSelect: () => void;
  /** A row in a section's row group, or one profile in a Claude Code or Codex group. */
  variant?: "line" | "profile";
}) {
  const { t } = useI18n();
  const status = statusOf(client);
  const profile = variant === "profile" ? client.claudeProfile : undefined;
  const detail = clientDetail(client, registry, variant === "profile");
  const t3Code = t3CodeBadge(client.t3Code ?? []);

  return (
    <button
      type="button"
      onClick={onSelect}
      data-client-id={client.id}
      className={`grid min-h-14 w-full items-center gap-3 border-b border-border py-3 text-left transition-colors last:border-b-0 hover:bg-foreground/[0.04] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring ${
        profile
          ? "grid-cols-[minmax(0,1fr)_16px] pr-4 pl-14 sm:grid-cols-[minmax(0,1fr)_auto_16px]"
          : "grid-cols-[28px_minmax(0,1fr)_16px] px-4 sm:grid-cols-[28px_minmax(0,1fr)_auto_16px]"
      }`}
    >
      {!profile && <ClientLogo id={client.id} name={client.name} size={28} />}
      <span className="min-w-0">
        {/* The title line wraps: the name keeps its width and chips move to the next line
            when they don't fit. Each wraps when it alone is wider than the row. */}
        <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1">
          {profile ? (
            <span className="max-w-full font-mono text-sm font-medium [overflow-wrap:anywhere]">
              {profile.command ?? profile.folder}
            </span>
          ) : (
            <span className="max-w-full text-sm font-medium [overflow-wrap:anywhere]">
              {client.name}
            </span>
          )}
          {/* An unreadable profile has no account to show, signed in or not. Codex
              keeps its account next to its tokens, so it is never read. */}
          {profile && status !== "error" && profileProduct(client) === "claude-code" && (
            <Badge variant="secondary" className={WRAPPING_CHIP}>
              {profile.account ?? t("No claude.ai sign-in")}
            </Badge>
          )}
          {t3Code && (
            // Every name shows in the chip itself; the tooltip adds whether each is on,
            // and stays inside the window.
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Badge
                    variant="secondary"
                    className={WRAPPING_CHIP}
                    data-t3-code-badge=""
                  >
                    {t3Code.text}
                  </Badge>
                </TooltipTrigger>
                <TooltipContent
                  collisionPadding={8}
                  className="max-w-[min(20rem,calc(100vw-1rem))] whitespace-pre-line"
                >
                  {t3Code.title}
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          )}
          {client.usesConnectors && (
            <Puzzle
              className="size-3.5 shrink-0 text-info"
              aria-label={t("Uses connectors")}
            />
          )}
        </span>
        <span
          className="block text-meta text-muted-foreground [overflow-wrap:anywhere]"
          title={detail.full}
        >
          {detail.text}
        </span>
      </span>
      <span
        className={`${profile ? "col-start-1" : "col-start-2"} row-start-2 flex min-w-0 flex-wrap items-center gap-2 sm:col-auto sm:row-auto`}
      >
        {importCount > 0 && status !== "error" && status !== "missing" && (
          <Badge variant="owned">
            <Trans
              message={msg("{value1}{value2} to import")}
              values={{ value1: <Download />, value2: importCount }}
            />
          </Badge>
        )}
        <StatusBadge status={status} />
      </span>
      <ChevronRight
        className={`${profile ? "col-start-2" : "col-start-3"} row-start-1 size-4 text-muted-foreground sm:col-auto sm:row-auto`}
        aria-hidden="true"
      />
    </button>
  );
}

/** Clients that list one row per profile under a shared entry. */
type ProfileProduct = "claude-code" | "codex";

function profileProduct(client: DetectedClient): ProfileProduct {
  return client.id === "codex" || client.id.startsWith("codex@")
    ? "codex"
    : "claude-code";
}

/** Product names, not translated. */
const PRODUCT_NAMES: Record<ProfileProduct, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
};

/** Whether each group is open and its copies shown. Kept across visits to a client's
 * detail page and moves between sections, as the GTK page keeps it. */
const profileGroupView: Record<ProfileProduct, { open: boolean; copiesShown: boolean }> =
  {
    "claude-code": { open: true, copiesShown: false },
    codex: { open: true, copiesShown: false },
  };

/** A client's profiles under one entry: Claude Code profiles or Codex homes. Copies of
 * the default Claude Code profile (same account in `~/.claude`, read only through
 * CLAUDE_CONFIG_DIR) sit behind a note. */
function ProfileGroup({
  product,
  profiles,
  registry,
  onSelectClient,
}: {
  product: ProfileProduct;
  profiles: DetectedClient[];
  registry: Registry | null;
  onSelectClient: (id: string) => void;
}) {
  const { t } = useI18n();
  const view = profileGroupView[product];
  const [open, setOpenState] = useState(() => view.open);
  const [copiesShown, setCopiesShownState] = useState(() => view.copiesShown);
  const setOpen = (value: boolean) => {
    view.open = value;
    setOpenState(value);
  };
  const setCopiesShown = (value: boolean) => {
    view.copiesShown = value;
    setCopiesShownState(value);
  };
  const summaryId = useId();
  const copies = profiles.filter((client) => client.claudeProfile?.sameAs);
  // Profiles started by a command first, by command; the rest by folder.
  const shown = profiles
    .filter((client) => !client.claudeProfile?.sameAs)
    .sort((a, b) => {
      const left = a.claudeProfile;
      const right = b.claudeProfile;
      // By the same rule as the rows, which GTK's `profile_order` also uses.
      return (
        Number(!left?.command) - Number(!right?.command) ||
        compareClientNames(
          { name: left?.command ?? left?.folder ?? "", id: a.id },
          { name: right?.command ?? right?.folder ?? "", id: b.id },
        )
      );
    });
  const connected = shown.filter((client) => statusOf(client) === "connected").length;
  const copyOf = copies
    .map((client) => shown.find((other) => other.id === client.claudeProfile?.sameAs))
    .find(Boolean)?.claudeProfile?.command;
  const row = (client: DetectedClient) => (
    <ClientRow
      key={client.id}
      client={client}
      registry={registry}
      importCount={importableServers(client, registry).length}
      onSelect={() => onSelectClient(client.id)}
      variant="profile"
    />
  );
  const copiesNote = copies.length > 0 && (
    <div className="border-t border-border">
      <div className="flex items-center gap-3 py-3 pr-4 pl-14">
        <p className="min-w-0 flex-1 text-meta text-muted-foreground">
          {t(
            "Also found {folder}: the same account as {command}. Claude Code only reads it when CLAUDE_CONFIG_DIR points there, as some background tools do.",
            {
              folder: copies.map((client) => client.claudeProfile?.folder).join(", "),
              command: copyOf ?? "claude",
            },
          )}
        </p>
        <Button
          variant="ghost"
          size="sm"
          aria-expanded={copiesShown}
          onClick={() => setCopiesShown(!copiesShown)}
        >
          {copiesShown ? t("Hide") : t("Show")}
        </Button>
      </div>
      {copiesShown && <div className="border-t border-border">{copies.map(row)}</div>}
    </div>
  );
  if (shown.length === 1) {
    // One real profile and its copies: an ordinary row, with the note under it.
    return (
      <div className="border-b border-border last:border-b-0">
        <ClientRow
          client={shown[0]}
          registry={registry}
          importCount={importableServers(shown[0], registry).length}
          onSelect={() => onSelectClient(shown[0].id)}
        />
        {copiesNote && <div className="bg-muted/20">{copiesNote}</div>}
      </div>
    );
  }
  return (
    <div className="border-b border-border last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-label={t(
          product === "codex" ? msg("Codex profiles") : msg("Claude Code profiles"),
        )}
        aria-describedby={summaryId}
        className="grid min-h-14 w-full grid-cols-[28px_minmax(0,1fr)_16px] items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-foreground/[0.04] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring"
      >
        <ClientLogo id={product} name={PRODUCT_NAMES[product]} size={28} />
        <span className="min-w-0">
          <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1">
            <span className="max-w-full truncate text-sm font-medium">
              {PRODUCT_NAMES[product]}
            </span>
            <Badge variant="secondary" className="min-w-0 max-w-full font-normal">
              {t(
                shown.length === 1 ? msg("{value0} profile") : msg("{value0} profiles"),
                {
                  value0: shown.length,
                },
              )}
            </Badge>
          </span>
          <span id={summaryId} className="block truncate text-meta text-muted-foreground">
            {t("{connected} of {total} connected", {
              connected,
              total: shown.length,
            })}
          </span>
        </span>
        <ChevronDown
          className={`size-4 text-muted-foreground transition-transform ${open ? "" : "-rotate-90"}`}
          aria-hidden="true"
        />
      </button>
      {open && (
        <div className="border-t border-border bg-muted/20">
          {shown.map(row)}
          {copiesNote}
        </div>
      )}
    </div>
  );
}

/** The last T3 Code settings revision the Clients page saw, `null` before the first check.
 * The page rescans whenever it sees a new one, so this matches the scanned clients. */
export type T3CodeSeen = { value: string | null } | null;
/** Connect all's summary card, kept until dismissed. */
export type ConnectAllCard = {
  title: string;
  detail: string;
  tone: "success" | "warning" | "info";
};

export function ClientsView({
  clients,
  registry,
  loading = false,
  error,
  onSelectClient,
  onChanged,
  t3Seen: keptT3Seen,
  connectAllCard: keptCard,
  onConnectAllCard,
}: {
  clients: DetectedClient[];
  registry: Registry | null;
  loading?: boolean;
  error?: string | null;
  onSelectClient: (id: string) => void;
  onChanged?: () => Promise<void>;
  /** Kept with the scanned clients, so a change made while the page is unmounted (a
   * client's details or another view open) rescans on return. */
  t3Seen?: RefObject<T3CodeSeen>;
  /** Connect all's summary, kept with the page data so opening a client's details and
   * coming back does not drop it; only Dismiss clears it. */
  connectAllCard?: ConnectAllCard | null;
  onConnectAllCard?: (card: ConnectAllCard | null) => void;
}) {
  const { t, locale } = useI18n();
  const [showMissing, setShowMissing] = useState(false);
  // The restart reminder from a connect, shared with the client's details page.
  const restartNotice = useClientNotices().list;
  const [refreshing, setRefreshing] = useState(false);
  const [sessionRetry, setSessionRetry] = useState(0);
  const refresh = async () => {
    setSessionRetry((value) => value + 1);
    setRefreshing(true);
    try {
      await requestClientRecoveryScan();
      await onChanged?.();
    } finally {
      setRefreshing(false);
    }
  };
  const [showRecent, setShowRecent] = useState(false);
  const [sessions, setSessions] = useState<ClientActivity[]>([]);
  const [sessionError, setSessionError] = useState(false);
  useEffect(() => {
    let current = true;
    getClientSessions()
      .then((rows) => {
        if (current) {
          setSessions(rows);
          setSessionError(false);
        }
      })
      .catch(() => {
        if (current) setSessionError(true);
      });
    return () => {
      current = false;
    };
  }, [clients, sessionRetry]);
  // T3 Code: rescan when its settings change while this page is on screen, so the row
  // badges follow instances added, renamed or turned off. The page unmounts when another
  // view or a client's details are shown; the last revision seen is kept with the scanned
  // clients (`t3Seen`), so a change made meanwhile, or while the window is hidden, still
  // rescans on return. Without T3 Code its settings are checked (one
  // `stat`) only when the page opens and when the window gets focus; every five seconds
  // only while they exist. GTK does the same.
  const onChangedRef = useRef(onChanged);
  useEffect(() => {
    onChangedRef.current = onChanged;
  }, [onChanged]);
  const windowVisible = useWindowVisible();
  const ownT3Seen = useRef<T3CodeSeen>(null);
  const t3Seen = keptT3Seen ?? ownT3Seen;
  // Instances Toolport cannot use, for the card: read when the page opens, when T3 Code's
  // settings change and after each scan (a home folder may have appeared meanwhile).
  const [t3Others, setT3Others] = useState<T3CodeOther[]>([]);
  const t3Read = useRef(0);
  // A read that failed is tried again on the next check, even with no settings change.
  const t3ReadFailed = useRef(false);
  const readT3Others = () => {
    const read = ++t3Read.current;
    void t3CodeOtherInstances()
      .then((others) => {
        if (read !== t3Read.current) return;
        t3ReadFailed.current = false;
        setT3Others(others);
      })
      .catch(() => {
        if (read === t3Read.current) t3ReadFailed.current = true;
      });
  };
  const readT3OthersRef = useRef(readT3Others);
  useEffect(() => {
    readT3OthersRef.current = readT3Others;
  });
  // Without T3 Code there is no timer, so while it is absent each scan (Refresh, or after
  // a connect) checks again whether it has appeared; GTK reads the card's instances on
  // every scan too.
  const t3Tick = useRef<(() => Promise<void>) | null>(null);
  useEffect(() => {
    if (t3Seen.current?.value != null) readT3OthersRef.current();
    else if (t3Seen.current) void t3Tick.current?.();
  }, [clients, t3Seen]);
  useEffect(() => {
    if (!windowVisible) return;
    let active = true;
    let timer: number | undefined;
    const check = async () => {
      let revision: string | null;
      try {
        revision = await t3CodeRevision();
      } catch {
        return;
      }
      if (!active) return;
      const previous = t3Seen.current;
      t3Seen.current = { value: revision };
      if (previous && previous.value === revision) {
        if (revision !== null && t3ReadFailed.current) readT3OthersRef.current();
        return;
      }
      if (revision === null) {
        t3Read.current += 1;
        setT3Others([]);
      } else readT3OthersRef.current();
      if (!previous) return;
      try {
        await onChangedRef.current?.();
      } catch {
        // The page shows scan errors itself.
      }
    };
    const tick = async () => {
      await check();
      if (active && t3Seen.current?.value != null) {
        timer = window.setTimeout(() => void tick(), 5_000);
      }
    };
    t3Tick.current = tick;
    void tick();
    return () => {
      active = false;
      t3Tick.current = null;
      window.clearTimeout(timer);
    };
  }, [windowVisible, t3Seen]);
  const sorted = sortClients(clients);
  // The T3 Code card: how many enabled instances are not connected, the rows they use
  // that need attention first, then the instances Toolport can't use, each with why and
  // what to do. GTK's `t3_code_card` shows the same.
  const hasT3Rows = clients.some((client) => (client.t3Code?.length ?? 0) > 0);
  const t3Gaps = t3CodeGapLines(t3Others);
  const t3LeftOut = t3CodeLeftOut(clients);
  const t3Lines = [...t3CodeLeftOutLines(t3LeftOut), ...t3Gaps];
  // Connect all connects these rows, the ones the count includes that can be connected
  // as they are; instances Toolport can't use at all have no row to connect.
  const t3Targets = t3CodeTargets(sorted);
  const t3Count = t3CodeInstanceCount(t3Targets);
  const t3Unconnected = t3CodeUnconnectedCount(t3Targets, t3LeftOut, t3Others);
  const t3Card = useRef<HTMLDivElement>(null);
  // Connect all went away (everything it could connect is connected, say after a batch)
  // and took focus with it: the card takes focus instead of the page start.
  const lastT3Count = useRef(t3Count);
  useEffect(() => {
    const was = lastT3Count.current;
    lastT3Count.current = t3Count;
    if (was === 0 || t3Count > 0) return;
    if (!document.activeElement || document.activeElement === document.body)
      t3Card.current?.focus();
  }, [t3Count]);
  const [batch, setBatch] = useState<{
    targets: DetectedClient[];
    index: number;
    /** Ids of the rows connected so far. */
    connectedIds: string[];
    /** Ids of the rows skipped so far, in step order. */
    notConnectedIds: string[];
    /** A step failed with the keyring still locked: later steps don't ask to unlock it
     * (the backend's `connect_step`; GTK keeps the same flag). */
    keyringQuiet: boolean;
  } | null>(null);
  // A batch that just ended, waiting for the rescan after it: the summary is worked out
  // from the rows that rescan finds, so an instance turned off or removed meanwhile is
  // neither counted nor named. GTK builds it from its rescan the same way.
  const [summaryDue, setSummaryDue] = useState<{
    connectedIds: string[];
    notConnectedIds: string[];
    stopped: boolean;
  } | null>(null);
  const summaryShown = useRef<typeof summaryDue>(null);
  const [ownSummary, setOwnSummary] = useState<ConnectAllCard | null>(null);
  const allSummary = onConnectAllCard ? (keptCard ?? null) : ownSummary;
  const setAllSummary = onConnectAllCard ?? setOwnSummary;
  useEffect(() => {
    if (!summaryDue || summaryShown.current === summaryDue) return;
    summaryShown.current = summaryDue;
    const { title, detail, tone } = connectAllSummary({
      ...connectAllOutcome(clients, summaryDue.connectedIds, summaryDue.notConnectedIds),
      stopped: summaryDue.stopped,
    });
    // On the page until dismissed, as GTK shows it and as Clients keeps its restart
    // reminder: a toast fades before a long list of what was not connected is read.
    setAllSummary({ title, detail, tone });
  }, [summaryDue, clients, setAllSummary]);
  // When the batch closes, focus goes back to Connect all, or to the T3 Code card once
  // the button is gone, never to the page start.
  const connectAllButton = useRef<HTMLButtonElement>(null);
  const focusAfterBatch = useRef(false);
  const finishBatch = (
    connectedIds: string[],
    notConnectedIds: string[],
    stopped: boolean,
  ) => {
    setBatch(null);
    focusAfterBatch.current = true;
    // The rescan sets the new rows before it resolves; a failed one keeps the old rows.
    void (onChangedRef.current?.() ?? Promise.resolve())
      .catch(() => {})
      .then(() => setSummaryDue({ connectedIds, notConnectedIds, stopped }));
  };
  const onBatchDialogClosed = (event: Event) => {
    if (!focusAfterBatch.current) return;
    focusAfterBatch.current = false;
    event.preventDefault();
    const button = connectAllButton.current;
    if (button && !button.disabled) button.focus();
    else t3Card.current?.focus();
  };
  const batchTarget = batch ? batch.targets[batch.index] : undefined;
  // The step names the instances on its row as they were when Connect all started, the
  // batch the user agreed to; GTK fixes them the same way.
  const batchNames = t3CodeEnabledNames(batchTarget);
  const present = sorted.filter((client) => statusOf(client) !== "missing");
  const missing = sorted.filter((client) => statusOf(client) === "missing");
  const connected = present.filter((client) => statusOf(client) === "connected");
  const available = present.filter((client) => statusOf(client) !== "connected");
  // Several Claude Code profiles, or several Codex homes, share one entry; it sits with
  // the connected clients when any of its profiles is connected.
  const groups = (["claude-code", "codex"] as const)
    .map((product) => {
      const profiles = present.filter(
        (client) => client.claudeProfile && profileProduct(client) === product,
      );
      const shown = profiles.filter((client) => !client.claudeProfile?.sameAs);
      return {
        product,
        shown: shown.length,
        connected: shown.some((client) => statusOf(client) === "connected"),
        element: (
          <ProfileGroup
            key={product}
            product={product}
            profiles={profiles}
            registry={registry}
            onSelectClient={onSelectClient}
          />
        ),
      };
    })
    .filter((group) => group.shown > 0);
  const connectedGroups = groups.filter((group) => group.connected);
  const availableGroups = groups.filter((group) => !group.connected);
  const connectedRows = connected.filter((client) => !client.claudeProfile);
  const availableRows = available.filter((client) => !client.claudeProfile);
  const availableCount =
    availableRows.length + availableGroups.reduce((sum, group) => sum + group.shown, 0);

  const refreshAction = onChanged && (
    <Button variant="outline" disabled={refreshing} onClick={() => void refresh()}>
      <RefreshCw className={refreshing ? "animate-spin" : undefined} /> {t("Refresh")}
    </Button>
  );

  if (loading && clients.length === 0) {
    return <SkeletonRows label={t("Loading AI clients")} />;
  }

  if (error)
    return (
      <ErrorCallout title={t("Clients")} cause={error} onRetry={() => void refresh()} />
    );

  if (!onChanged && clients.length === 0 && sessions.length === 0 && !sessionError) {
    return (
      <EmptyState
        icon={<MonitorCog />}
        title={t("No AI clients detected")}
        description={t(
          "Install Claude Desktop, Cursor, VS Code, or another supported client, then refresh.",
        )}
        action={refreshAction}
      />
    );
  }

  return (
    <div className="flex w-full min-w-0 flex-col gap-6">
      <ClientRecoveryBanners
        refreshKey={clients}
        onRecovered={() => void onChanged?.()}
      />
      {allSummary && (
        // Connect all's summary, above the card as GTK places it: a spec §10 card that
        // stays until dismissed. A long name breaks anywhere instead of running off a
        // narrow window.
        <StatusCard
          role="status"
          testId="connect-all-summary"
          variant={allSummary.tone}
          icon={
            allSummary.tone === "warning" ? (
              <TriangleAlert />
            ) : allSummary.tone === "success" ? (
              <Check />
            ) : (
              <Info />
            )
          }
          title={allSummary.title}
          detail={
            allSummary.detail && (
              <span className="select-text [overflow-wrap:anywhere]">
                {allSummary.detail}
              </span>
            )
          }
          actions={
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setAllSummary(null);
                // Focus stays on the page, on Connect all or the card, as in GTK.
                const button = connectAllButton.current;
                if (button && !button.disabled) button.focus();
                else t3Card.current?.focus();
              }}
            >
              {t("Dismiss")}
            </Button>
          }
        />
      )}
      {restartNotice && (
        <StatusCard
          role="status"
          variant="info"
          className="select-text"
          icon={<RefreshCw />}
          title={restartNotice.title}
          detail={restartNotice.text}
          actions={
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => dismissClientNotice(restartNotice)}
            >
              {t("Dismiss")}
            </Button>
          }
        />
      )}
      {sessionError && (
        <ErrorCallout
          title={translate("Client session history could not be read.")}
          onRetry={() => void refresh()}
        />
      )}
      {present.length === 0 && (
        <EmptyState
          icon={<MonitorCog />}
          title={t("No supported clients installed")}
          description={t(
            "Install Claude Desktop, Cursor, VS Code, or another supported client, then refresh.",
          )}
          action={refreshAction}
        />
      )}
      {(hasT3Rows || t3Gaps.length > 0) && (
        <div
          data-t3-code-card=""
          ref={t3Card}
          tabIndex={-1}
          role="group"
          aria-label={T3_CODE_NAME}
          className="rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          <StatusCard
            icon={<Layers />}
            title={T3_CODE_NAME}
            detail={
              <>
                <p>
                  {t(
                    "T3 Code runs these clients as providers. Each reads its own MCP config, so Toolport connects to them, not to T3 Code.",
                  )}
                </p>
                {hasT3Rows && t3CodeAnyEnabled(clients, t3Unconnected) && (
                  <p className="mt-1">{t3CodeStatus(t3Unconnected)}</p>
                )}
                {t3Lines.map((line) => (
                  <p key={line} className="mt-1">
                    {line}
                  </p>
                ))}
              </>
            }
            actions={
              t3Count > 0 && (
                <Button
                  ref={connectAllButton}
                  size="sm"
                  disabled={batch !== null}
                  onClick={() =>
                    setBatch({
                      targets: t3Targets,
                      index: 0,
                      connectedIds: [],
                      notConnectedIds: [],
                      keyringQuiet: false,
                    })
                  }
                >
                  {t("Connect all")}
                </Button>
              )
            }
          />
        </div>
      )}
      {batch && batchTarget && (
        <ConnectReviewDialog
          key={`${batch.index}:${batchTarget.id}`}
          clientId={batchTarget.id}
          clientName={batchTarget.name}
          // The access saved for the row, as its own Connect keeps it.
          profile={registry?.clientScopes?.[batchTarget.id] || undefined}
          step={t("Step {index} of {total}: {names}", {
            index: batch.index + 1,
            total: batch.targets.length,
            names: batchNames.join(", "),
          })}
          hasNext={batch.index + 1 < batch.targets.length}
          onCloseAutoFocus={onBatchDialogClosed}
          keyringQuiet={batch.keyringQuiet}
          onKeyringLocked={() =>
            setBatch((current) => current && { ...current, keyringQuiet: true })
          }
          onSkip={() => {
            const notConnectedIds = [...batch.notConnectedIds, batchTarget.id];
            const next = batch.index + 1;
            if (next >= batch.targets.length)
              finishBatch(batch.connectedIds, notConnectedIds, false);
            else setBatch({ ...batch, index: next, notConnectedIds });
          }}
          onClose={(done, advance) => {
            const next = batch.index + 1;
            const connectedIds = done
              ? [...batch.connectedIds, batchTarget.id]
              : batch.connectedIds;
            const rest = batch.targets.slice(done ? next : batch.index);
            if (!advance && rest.length > 0) {
              // Cancel, close or Escape, on a review or a completion screen, stops the
              // batch; the rows not connected yet stay unconnected.
              finishBatch(
                connectedIds,
                [...batch.notConnectedIds, ...rest.map((target) => target.id)],
                true,
              );
              return;
            }
            if (next >= batch.targets.length)
              finishBatch(connectedIds, batch.notConnectedIds, false);
            else setBatch({ ...batch, index: next, connectedIds });
          }}
          onConnected={() => {}}
        />
      )}

      {connected.length > 0 && (
        <section>
          <SectionHeader count={connected.length}>
            {translate("Connected to Toolport")}
          </SectionHeader>
          <div className="toolport-list">
            {connectedGroups.map((group) => group.element)}
            {connectedRows.map((client) => (
              <ClientRow
                key={client.id}
                client={client}
                registry={registry}
                importCount={importableServers(client, registry).length}
                onSelect={() => onSelectClient(client.id)}
              />
            ))}
          </div>
        </section>
      )}

      {availableCount > 0 && (
        <section>
          <SectionHeader count={availableCount}>
            {t("Available to connect")}
          </SectionHeader>
          <p className="mb-2 text-meta text-muted-foreground">
            {t("Select an installed client to connect it or review its configuration.")}
          </p>
          <div className="toolport-list">
            {availableGroups.map((group) => group.element)}
            {availableRows.map((client) => (
              <ClientRow
                key={client.id}
                client={client}
                registry={registry}
                importCount={importableServers(client, registry).length}
                onSelect={() => onSelectClient(client.id)}
              />
            ))}
          </div>
        </section>
      )}

      {missing.length > 0 && (
        <section>
          <button
            type="button"
            onClick={() => setShowMissing((value) => !value)}
            aria-expanded={showMissing}
            className="flex w-full items-center gap-2 rounded-md px-1 py-1.5 text-left text-meta text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          >
            <ChevronRight
              className={`size-3.5 transition-transform ${showMissing ? "rotate-90" : ""}`}
              aria-hidden="true"
            />
            <span className="font-medium">{t("Not installed")}</span>
            <span>{missing.length}</span>
          </button>
          {showMissing && (
            <div className="mt-2 toolport-list">
              {missing.map((client) => (
                <ClientRow
                  key={client.id}
                  client={client}
                  registry={registry}
                  importCount={0}
                  onSelect={() => onSelectClient(client.id)}
                />
              ))}
            </div>
          )}
        </section>
      )}
      {sessions.length > 0 && (
        <section aria-label={translate("Recent client activity")}>
          <button
            type="button"
            onClick={() => setShowRecent((value) => !value)}
            aria-expanded={showRecent}
            className="flex w-full items-center gap-2 rounded-md px-1 py-1.5 text-left text-meta text-muted-foreground hover:text-foreground"
          >
            <ChevronRight
              className={`size-3.5 ${showRecent ? "rotate-90" : ""}`}
              aria-hidden="true"
            />
            {translate("Recent client activity")}
            <span>{sessions.length}</span>
          </button>
          {showRecent && (
            <div className="mt-2 toolport-list">
              {sessions.map((client) => (
                <div
                  key={trustedClientName(client)}
                  className="border-b border-border px-4 py-3 last:border-b-0"
                >
                  <p
                    className="truncate text-sm font-medium"
                    title={
                      clientIdentityTooltip(client) ??
                      [
                        client.clientLabel &&
                          translate("Reports itself as: {value0}", {
                            value0: client.clientLabel,
                          }),
                        translate("{value0} recorded sessions", {
                          value0: client.sessionCount ?? 1,
                        }),
                      ]
                        .filter(Boolean)
                        .join(" · ")
                    }
                  >
                    {trustedClientName(client)}
                  </p>
                  <p
                    className="text-meta text-muted-foreground"
                    title={translate(
                      "Calls today counts retained tool calls since local midnight. Older calls may no longer be retained.",
                    )}
                  >
                    {client.lastActiveMs
                      ? translate("Last active {value0}", {
                          value0: fmtAgo(client.lastActiveMs),
                        })
                      : translate("Last active time unavailable")}
                    {client.callsToday != null &&
                      translate(
                        client.callsToday === 1
                          ? msg(" · {count} call today")
                          : msg(" · {count} calls today"),
                        { count: client.callsToday.toLocaleString(locale) },
                      )}
                    {client.firstCatalogSize != null &&
                      translate(" · Last saw {value0} tools", {
                        value0: client.firstCatalogSize.toLocaleString(locale),
                      })}
                    {t3CodeActivitySegment(client) &&
                      ` · ${t3CodeActivitySegment(client)}`}
                  </p>
                </div>
              ))}
            </div>
          )}
        </section>
      )}
    </div>
  );
}
