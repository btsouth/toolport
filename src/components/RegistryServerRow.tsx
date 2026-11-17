import { msg, translate, useI18n, type SourceMessage } from "@/lib/i18n";
import { referenceProvider } from "@/lib/secretRefs";
import { toastError } from "@/lib/toast";
import { isPersonalSync } from "@/lib/personalSync";
import { invoke } from "@tauri-apps/api/core";
import {
  lazy,
  Suspense,
  useEffect,
  useCallback,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import {
  Copy,
  RefreshCw,
  KeyRound,
  LogIn,
  MoreHorizontal,
  Pencil,
  Trash2,
  Users,
} from "lucide-react";
import { isDownloadLauncher } from "@/lib/launcher";
import { serverFailureLabel } from "@/lib/serverHealth";
import { errorHeadline, shortenUrls } from "@/lib/errors";
import { toast } from "sonner";
import type { ProbeResult, Registry, ServerEntry } from "@/lib/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { LazySecretsDialog as SecretsDialog } from "@/components/LazySecretsDialog";
import { LazyServerDialog as ServerDialog } from "@/components/LazyServerDialog";
import { LaunchSetupDialog } from "@/components/LaunchSetupDialog";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ServerLogo } from "@/components/ServerLogo";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { probeRegistered } from "@/lib/api";
import { needsTeamEnableReview } from "@/lib/importReview";

// Opened from the row menu only, so it loads in its own chunk.
const SyncSettingsDialog = lazy(() =>
  import("@/components/SyncSettingsDialog").then((m) => ({
    default: m.SyncSettingsDialog,
  })),
);
const ServerToolsPanel = lazy(() =>
  import("@/components/ServerToolsPanel").then((m) => ({ default: m.ServerToolsPanel })),
);

interface Props {
  openTools?: boolean;
  detail?: boolean;
  onOpen?: (tab: "overview" | "tools") => void;
  server: ServerEntry;
  registry: Registry | null;
  enabled: boolean;
  busy?: boolean;
  health?: ProbeResult;
  onToggle: (enabled: boolean) => void;
  onSwitchRef?: (id: string, node: HTMLButtonElement | null) => void;
  onRemove: () => void;
  onRegistryChange: (registry: Registry) => void;
  /** Re-run the health probe (e.g. after authenticating). */
  onReprobe?: () => void;
}

type Status = "disabled" | "checking" | "connected" | "needs-auth" | "locked" | "error";

function statusOf(enabled: boolean, health?: ProbeResult): Status {
  if (!enabled) return "disabled";
  // No probe result yet (loading, or queued behind an in-flight probe): checking.
  if (!health) return "checking";
  if (health.ok) return "connected";
  if (health.keyringLocked) return "locked";
  if (health.authRequired) return "needs-auth";
  return "error";
}

/** The status chip variant: one status color per row, on the chip only. */
const STATUS_BADGE: Record<Status, "off" | "success" | "warning" | "destructive"> = {
  disabled: "off",
  checking: "off",
  connected: "success",
  "needs-auth": "warning",
  // Waits on the user, like a sign-in: not a failure.
  locked: "warning",
  error: "destructive",
};

/** The shared transport names; the GTK rows use the same words. */
const TRANSPORT_LABELS: Record<string, SourceMessage> = {
  stdio: msg("Local stdio"),
  http: msg("Remote HTTP"),
  sse: msg("Remote SSE"),
};

const STATUS_ARIA_LABEL: Record<Status, SourceMessage> = {
  disabled: msg("Server disabled"),
  checking: msg("Checking connection"),
  connected: msg("Connected"),
  "needs-auth": msg("Authentication required"),
  locked: msg("Keyring locked"),
  error: msg("Connection error"),
};

const DETAIL_TABS = ["overview", "tools"] as const;
type DetailTab = (typeof DETAIL_TABS)[number];

const PANEL =
  "flex flex-col gap-2.5 rounded-md focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";

/** One segment of a segmented control: the selected one is raised (spec 13). */
const SEGMENT =
  "inline-flex h-7 items-center rounded-md border px-3 text-xs transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";

const ACTION =
  "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";

export function RegistryServerRow({
  registry,
  server,
  enabled,
  busy,
  health,
  onToggle,
  onSwitchRef,
  onRemove,
  onRegistryChange,
  onReprobe,
  openTools,
  detail = false,
  onOpen,
}: Props) {
  const { t } = useI18n();
  const switchRef = useCallback(
    (node: HTMLButtonElement | null) => onSwitchRef?.(server.id, node),
    [onSwitchRef, server.id],
  );
  const [expanded, setExpanded] = useState(detail);
  const [detailTab, setDetailTab] = useState<DetailTab>("overview");
  // Tab and panel IDs are per row, so two expanded rows never share them.
  const detailId = useId();
  const tabId = (tab: DetailTab) => `${detailId}-${tab}-tab`;
  const panelId = (tab: DetailTab) => `${detailId}-${tab}-panel`;
  const tabRefs = useRef<Partial<Record<DetailTab, HTMLButtonElement | null>>>({});
  // One tab stop for the tablist: arrows move between tabs (wrapping), Home and
  // End jump to the ends, and selection follows focus.
  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, tab: DetailTab) => {
    const count = DETAIL_TABS.length;
    const index = DETAIL_TABS.indexOf(tab);
    const targets: Partial<Record<string, number>> = {
      ArrowRight: (index + 1) % count,
      ArrowLeft: (index - 1 + count) % count,
      Home: 0,
      End: count - 1,
    };
    const nextIndex = targets[event.key];
    if (nextIndex === undefined) return;
    event.preventDefault();
    const next = DETAIL_TABS[nextIndex];
    setDetailTab(next);
    tabRefs.current[next]?.focus();
  };
  useEffect(() => {
    if (openTools) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Open the requested tools tab when navigation selects this row.
      setExpanded(true);
      setDetailTab("tools");
    }
  }, [openTools]);

  const target =
    server.command !== null
      ? [server.command, ...server.args].join(" ")
      : (server.url ?? "");
  const providers = [
    ...new Set(
      [...server.env, ...(server.headerKeys ?? []), ...(server.launch?.inputs ?? [])]
        .map((e) => referenceProvider(e.source?.ref)?.name)
        .filter(Boolean),
    ),
  ];
  const secretCount = server.env.filter((e) => e.secret).length;
  const status = statusOf(enabled, health);
  const requiredLaunch = server.launch?.inputs.filter((input) => input.required) ?? [];
  const missingPlainLaunch = requiredLaunch.filter(
    (input) => !input.secret && !input.value?.trim(),
  );
  // A probe that sits in "checking" past a few seconds is still starting. Name
  // that wait so a slow initialize reads as progress rather than a missing route.
  // Download launchers keep the more specific "Installing…" label.
  const launcher = isDownloadLauncher(server.command, server.args);
  const [initializing, setInitializing] = useState(false);
  // Reset during render (not in the effect) so a later re-check starts back at
  // "Checking…" instead of flashing a stale "Installing…".
  if (initializing && status !== "checking") setInitializing(false);
  useEffect(() => {
    if (status !== "checking") return;
    const t = setTimeout(() => setInitializing(true), 4000);
    return () => clearTimeout(t);
  }, [status]);
  // Team-synced servers are tagged `team:<id>`. An admin manages them centrally, so a
  // member sees a Team badge and can't edit/remove them locally (a local change would
  // just re-sync away), but still authenticates them (keys stay on their own machine).
  const personalSync = isPersonalSync(registry);
  const isTeam = (server.source?.startsWith("team:") ?? false) && !personalSync;

  const label =
    status === "needs-auth"
      ? serverFailureLabel(health!)
      : status === "connected"
        ? translate(
            health?.toolCount === 1
              ? msg("Ready · {count} tool")
              : msg("Ready · {count} tools"),
            { count: health?.toolCount ?? 0 },
          )
        : status === "error" || status === "locked"
          ? serverFailureLabel(health!)
          : status === "checking"
            ? initializing
              ? launcher
                ? translate("Installing…")
                : translate("Initializing…")
              : translate("Checking…")
            : requiredLaunch.length
              ? missingPlainLaunch.length
                ? translate("Setup required: {value0}", {
                    value0: missingPlainLaunch.map((input) => input.label).join(", "),
                  })
                : translate("Disabled · check launch setup")
              : translate("Off");

  // Next free "Name (N)" for the duplicate-for-another-account action.
  const existingNames = new Set(registry?.servers.map((s) => s.name.toLowerCase()) ?? []);
  const baseName = server.name.replace(/\s\(\d+\)$/, "");
  let duplicateName = `${baseName} (2)`;
  let index = 2;
  while (existingNames.has(duplicateName.toLowerCase())) {
    index++;
    duplicateName = `${baseName} (${index})`;
  }

  const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();
  // The row menu's dialogs, opened one at a time (as the GTK row's menu).
  const [dialog, setDialog] = useState<
    null | "edit" | "duplicate" | "sync" | "auth" | "credentials" | "remove"
  >(null);
  const closeDialog = () => setDialog(null);
  const editable = !isTeam && ["stdio", "http", "sse"].includes(server.transport);
  const open = () => (onOpen ? onOpen("overview") : setExpanded((v) => !v));

  // Test connection: the GTK row menu's check of the saved server, with its messages.
  async function testConnection() {
    const id = toast.loading(t("Testing {value0}…", { value0: server.name }));
    try {
      const probe = await probeRegistered(server.id);
      if (probe.ok)
        toast.success(
          probe.toolCount === 1
            ? t("{value0} connected and exposed {value1} tool", {
                value0: server.name,
                value1: probe.toolCount,
              })
            : t("{value0} connected and exposed {value1} tools", {
                value0: server.name,
                value1: probe.toolCount,
              }),
          { id },
        );
      else if (probe.authRequired)
        toast.error(
          t("{value0} needs credentials before it can connect", { value0: server.name }),
          { id },
        );
      else
        toast.error(
          t("Could not connect to {value0}: {value1}", {
            value0: server.name,
            value1: probe.error ?? "unknown connection error",
          }),
          { id },
        );
    } catch (error) {
      toast.error(
        t("Could not test {value0}: {value1}", {
          value0: server.name,
          value1: String(error),
        }),
        { id },
      );
    }
  }

  // Where it comes from, how it runs and where its keys live: the row's second line.
  const origin = isTeam
    ? translate("Team")
    : `${
        personalSync
          ? server.syncLocalOnly
            ? translate("This machine only")
            : registry?.team?.personalSyncState?.conflicts?.[
                  server.teamOriginalId ?? server.id
                ]
              ? translate("Sync conflict")
              : server.teamEnableReview === true
                ? translate("Needs review")
                : translate("Synced")
          : translate("Personal")
      }${
        server.source?.startsWith("imported:")
          ? ` · ${t("from {client}", { client: server.source.slice("imported:".length) })}`
          : ""
      }`;
  const meta = [
    origin,
    TRANSPORT_LABELS[server.transport]
      ? translate(TRANSPORT_LABELS[server.transport])
      : server.transport,
    providers.length > 0 &&
      t("Keys from {providers}", { providers: providers.join(", ") }),
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      data-server-id={server.id}
      className={
        detail
          ? "rounded-xl border border-border bg-card"
          : "border-b border-border last:border-b-0"
      }
    >
      {/* Row navigation stays separate from the switch and authentication controls. */}
      <div
        onClick={detail ? undefined : open}
        className={`flex min-h-14 cursor-pointer items-center gap-3 px-4 py-3 transition-colors hover:bg-foreground/[0.04] ${detail ? "" : "flex-wrap gap-y-2"}`}
      >
        {detail && (
          <span className="mr-auto text-sm font-semibold">{translate("Connection")}</span>
        )}
        {!detail && (
          <ServerLogo name={server.name} transport={server.transport} size={28} />
        )}

        {!detail && (
          // The name keeps 220px, as in the GTK app; past that the status and
          // actions wrap onto the next line.
          <span className="min-w-0 flex-1 basis-[220px]">
            <span title={server.name} className="flex min-w-0 items-center gap-1.5">
              {/* The name opens the details, as in the GTK row. */}
              <button
                type="button"
                data-detail-open={server.id}
                aria-expanded={onOpen ? undefined : expanded}
                aria-label={
                  expanded
                    ? t("Hide {value1} details", { value1: server.name })
                    : t("Show {value1} details", { value1: server.name })
                }
                onClick={(e) => {
                  stop(e);
                  open();
                }}
                className="min-w-0 truncate rounded-sm text-left text-sm font-semibold focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              >
                {server.name}
              </button>
              {isTeam && (
                <Users
                  className="size-3.5 shrink-0 text-muted-foreground"
                  aria-hidden="true"
                />
              )}
            </span>
            <span title={meta} className="block truncate text-meta text-muted-foreground">
              {meta}
            </span>
          </span>
        )}

        <span className="ml-auto flex min-w-0 flex-wrap items-center gap-2">
          <span
            role="status"
            aria-label={
              initializing
                ? launcher
                  ? translate("Installing the server package")
                  : translate("Server initializing")
                : status === "needs-auth" || status === "error" || status === "locked"
                  ? label
                  : status === "connected"
                    ? label.replace(" · ", ", ")
                    : t(STATUS_ARIA_LABEL[status])
            }
            className="sr-only"
          />
          {status === "needs-auth" ? (
            <>
              <StatusLabel status={status} label={label} error={health?.error ?? null} />
              <SecretsDialog
                server={server}
                onSaved={onRegistryChange}
                onChanged={onReprobe}
                trigger={
                  <Button variant="outline" size="sm" onClick={stop}>
                    <LogIn />
                    {health?.authTarget === "service_credential"
                      ? translate("Edit service key")
                      : translate("Sign in")}
                  </Button>
                }
              />
            </>
          ) : (
            <StatusLabel status={status} label={label} error={health?.error ?? null} />
          )}

          {/* A locked keyring has no error to show, but Retry may unlock it. */}
          {health && !health.ok && (health.error || status === "locked") && (
            <span className="flex gap-1">
              {(status === "error" || status === "locked") && onReprobe && (
                <button
                  type="button"
                  title={translate("Retry {value0} connection", { value0: server.name })}
                  className={ACTION}
                  disabled={busy}
                  onClick={(e) => {
                    stop(e);
                    onReprobe();
                  }}
                >
                  <RefreshCw className="size-3.5" aria-hidden="true" />
                  {translate("Retry")}
                </button>
              )}
              {health.error && (
                <button
                  type="button"
                  title={translate("View the connection error and server output")}
                  className={ACTION}
                  onClick={(e) => {
                    stop(e);
                    if (onOpen && !detail) onOpen("overview");
                    else {
                      setExpanded(true);
                      setDetailTab("overview");
                    }
                  }}
                >
                  {translate("View log")}
                </button>
              )}
            </span>
          )}
          <span className="flex items-center" onClick={stop}>
            <Switch
              ref={switchRef}
              checked={enabled}
              // Keep the pending control focusable so Tab can leave it and a
              // successful move can restore it. Block repeat requests here.
              aria-disabled={busy || undefined}
              data-disabled={busy ? "" : undefined}
              onCheckedChange={(next) => {
                if (!busy) onToggle(next);
              }}
              // Names what the switch does now, as the GTK switch does.
              aria-label={
                enabled
                  ? t("Disable {server}", { server: server.name })
                  : t("Enable {server}", { server: server.name })
              }
            />
          </span>
          {!detail && (
            // The row's actions (spec 9 and the mockup: chip, switch, then ⋯),
            // the same as the GTK row menu.
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-7 text-muted-foreground"
                  aria-label={t("Actions for {value0}", { value0: server.name })}
                  title={t("Actions for {value0}", { value0: server.name })}
                  onClick={stop}
                >
                  <MoreHorizontal className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              {/* As wide as its longest item: no item wraps onto two lines. */}
              <DropdownMenuContent align="end" className="w-max" onClick={stop}>
                {/* A team server awaiting review starts nothing until the user consents. */}
                <DropdownMenuItem
                  disabled={!enabled && needsTeamEnableReview(server)}
                  onSelect={() => void testConnection()}
                >
                  {t("Test connection")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => (onOpen ? onOpen("tools") : setExpanded(true))}
                >
                  {t("Server details and tools")}
                </DropdownMenuItem>
                <DropdownMenuItem disabled={!editable} onSelect={() => setDialog("edit")}>
                  {t("Edit")}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setDialog("sync")}>
                  {t("Sync settings")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={!editable}
                  title={t(
                    "Add another account: a new entry with the same connection, named separately",
                  )}
                  onSelect={() => setDialog("duplicate")}
                >
                  {t("Duplicate")}
                </DropdownMenuItem>
                {(server.transport === "http" || server.transport === "sse") && (
                  <DropdownMenuItem onSelect={() => setDialog("auth")}>
                    {t("Authentication")}
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem onSelect={() => setDialog("credentials")}>
                  {t("Credentials")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  variant="destructive"
                  disabled={busy || isTeam}
                  onSelect={() => setDialog("remove")}
                >
                  {t("Remove")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </span>
      </div>

      {(dialog === "edit" || dialog === "duplicate") && (
        <ServerDialog
          autoOpen
          onClose={closeDialog}
          onSaved={onRegistryChange}
          editId={dialog === "edit" ? server.id : undefined}
          initial={dialog === "edit" ? server : { ...server, name: duplicateName }}
          existingNames={registry?.servers.map((s) => s.name) ?? []}
        />
      )}
      {dialog === "sync" && (
        <Suspense fallback={null}>
          <SyncSettingsDialog
            server={server}
            registry={registry}
            onSaved={onRegistryChange}
            onClose={closeDialog}
          />
        </Suspense>
      )}
      {(dialog === "auth" || dialog === "credentials") && (
        <SecretsDialog
          autoOpen
          onClose={closeDialog}
          server={server}
          onSaved={onRegistryChange}
          onChanged={onReprobe}
        />
      )}
      {dialog === "remove" && (
        <ConfirmDialog
          open
          onOpenChange={(next) => !next && closeDialog()}
          title={translate("Remove {value0}?", { value0: server.name })}
          description={
            personalSync && !server.syncLocalOnly
              ? translate(
                  "This removes the server from Toolport and your other machines. Any saved secrets stay in your keychain.",
                )
              : translate(
                  "This deletes the server from Toolport on this machine. Any saved secrets stay in your keychain.",
                )
          }
          confirmLabel={translate("Remove")}
          destructive
          onConfirm={onRemove}
        />
      )}

      {!enabled && server.teamRouteRemoved && (
        <p className="pr-4 pb-2.5 pl-14 text-meta text-muted-foreground">
          {translate("Removed or disabled by the team. Your personal server stays off.")}
        </p>
      )}

      {expanded && (
        <div
          className={
            detail
              ? "flex flex-col gap-6 p-4"
              : "flex flex-col gap-2.5 pt-0.5 pr-4 pb-3 pl-14"
          }
        >
          <div
            role="tablist"
            aria-label={translate("{value0} details", { value0: server.name })}
            className="flex w-fit gap-0.5 rounded-md border bg-muted/40 p-0.5"
          >
            {DETAIL_TABS.map((tab) => (
              <button
                key={tab}
                ref={(node) => {
                  tabRefs.current[tab] = node;
                }}
                role="tab"
                id={tabId(tab)}
                aria-selected={detailTab === tab}
                aria-controls={panelId(tab)}
                tabIndex={detailTab === tab ? 0 : -1}
                className={`${SEGMENT} ${
                  detailTab === tab
                    ? "border-border bg-background font-semibold text-foreground shadow-sm"
                    : "border-transparent text-muted-foreground hover:text-foreground"
                }`}
                onClick={() => setDetailTab(tab)}
                onKeyDown={(event) => onTabKeyDown(event, tab)}
              >
                {tab === "overview" ? translate("Overview") : translate("Tools")}
              </button>
            ))}
          </div>
          {/* The inactive panel stays as an empty, hidden placeholder so every
              tab's aria-controls resolves. Its content mounts only when selected. */}
          {DETAIL_TABS.filter((tab) => tab !== detailTab).map((tab) => (
            <div
              key={tab}
              role="tabpanel"
              id={panelId(tab)}
              aria-labelledby={tabId(tab)}
              hidden
            />
          ))}
          {detailTab === "tools" ? (
            <div
              role="tabpanel"
              id={panelId("tools")}
              aria-labelledby={tabId("tools")}
              tabIndex={0}
              className={PANEL}
            >
              <Suspense fallback={<p>{translate("Loading tools…")}</p>}>
                <ServerToolsPanel
                  serverId={server.id}
                  registry={registry}
                  onRegistryChange={onRegistryChange}
                />
              </Suspense>
            </div>
          ) : (
            <div
              role="tabpanel"
              id={panelId("overview")}
              aria-labelledby={tabId("overview")}
              tabIndex={0}
              className={PANEL}
            >
              {detail && server.cwd && (
                <code className="block break-all font-mono text-meta text-muted-foreground">
                  {server.cwd}
                </code>
              )}
              {detail &&
                server.env
                  .filter((entry) => !entry.secret && !entry.source)
                  .map((entry) => (
                    <code
                      key={entry.key}
                      className="block break-all font-mono text-meta text-muted-foreground"
                    >
                      {entry.key}={entry.value}
                    </code>
                  ))}
              {!!requiredLaunch.length && (
                <p className="text-meta text-muted-foreground">
                  {t(
                    "Launch setup: {inputs}. Open Launch setup to add or review these values before enabling.",
                    { inputs: requiredLaunch.map((input) => input.label).join(", ") },
                  )}
                </p>
              )}
              {target && (
                <code className="block rounded-md bg-muted px-2 py-1.5 font-mono text-meta break-all text-muted-foreground">
                  {target}
                </code>
              )}
              {health && !health.ok && health.error && (
                <div className="flex flex-col gap-1">
                  {/* Lead with a readable headline so the useful signal (exit status,
                  EADDRINUSE, a 401) isn't buried under a stack trace + a giant
                  OAuth URL; the full output stays below, bounded and scrollable
                  with long URLs shortened. */}
                  <div className="flex items-start justify-between gap-2">
                    <p className="text-meta font-medium text-warning">
                      {errorHeadline(health.error)}
                    </p>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        void navigator.clipboard.writeText(health?.error ?? "");
                        toast.success(translate("Error copied"));
                      }}
                      title={translate("Copy the full error")}
                      className="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                    >
                      <Copy className="size-3" />
                      {translate("Copy")}
                    </button>
                  </div>
                  <p className="max-h-32 overflow-y-auto font-mono text-meta break-words whitespace-pre-wrap text-muted-foreground">
                    {shortenUrls(health.error)}
                  </p>
                </div>
              )}

              <div className="flex flex-wrap items-center gap-1">
                <SecretsDialog
                  server={server}
                  onSaved={onRegistryChange}
                  onChanged={onReprobe}
                  trigger={
                    <button className={ACTION}>
                      <KeyRound className="size-3.5" />
                      {translate("Secrets")}
                      {secretCount > 0 ? ` (${secretCount})` : ""}
                    </button>
                  }
                />

                {!!server.launch?.inputs.length && (
                  <LaunchSetupDialog
                    server={server}
                    onSaved={onRegistryChange}
                    onChanged={onReprobe}
                    trigger={
                      <button className={ACTION}>{translate("Launch setup")}</button>
                    }
                  />
                )}

                {personalSync && (
                  <label className="inline-flex items-center gap-2 text-meta">
                    <input
                      type="checkbox"
                      checked={server.syncLocalOnly === true}
                      onChange={async (e) => {
                        try {
                          const r = await invoke<Registry>("personal_sync_local_only", {
                            serverId: server.id,
                            localOnly: e.target.checked,
                          });
                          onRegistryChange(r);
                        } catch (error) {
                          toastError(String(error));
                        }
                      }}
                    />{" "}
                    {translate("Keep on this machine only")}
                    <span className="text-meta text-muted-foreground">
                      {translate("Copies already on your other machines stay there.")}
                    </span>
                  </label>
                )}
                {personalSync && (
                  <span className="text-meta text-muted-foreground">
                    {server.syncLocalOnly
                      ? translate("This machine only")
                      : translate("Synced")}
                  </span>
                )}
                {isTeam ? (
                  <span className="inline-flex items-center gap-1.5 px-2 py-1 text-meta text-muted-foreground">
                    <Users className="size-3.5" aria-hidden="true" />
                    {translate(
                      "Managed by your team. Ask an admin to change or remove it.",
                    )}
                  </span>
                ) : (
                  <>
                    <ServerDialog
                      onSaved={onRegistryChange}
                      initial={{ ...server, name: duplicateName }}
                      existingNames={registry?.servers.map((s) => s.name) ?? []}
                      trigger={
                        <button
                          className={ACTION}
                          title={translate("Add another account")}
                        >
                          <Copy className="size-3.5" />
                          {translate("Duplicate")}
                        </button>
                      }
                    />

                    <ServerDialog
                      onSaved={onRegistryChange}
                      editId={server.id}
                      initial={server}
                      existingNames={registry?.servers.map((s) => s.name) ?? []}
                      trigger={
                        <button className={ACTION}>
                          <Pencil className="size-3.5" />
                          {translate("Edit")}
                        </button>
                      }
                    />

                    <ConfirmDialog
                      trigger={
                        <button
                          disabled={busy}
                          className={`${ACTION} hover:bg-destructive/10 hover:text-destructive`}
                        >
                          <Trash2 className="size-3.5" />
                          {translate("Remove")}
                        </button>
                      }
                      title={translate("Remove {value0}?", { value0: server.name })}
                      description={
                        personalSync && !server.syncLocalOnly
                          ? translate(
                              "This removes the server from Toolport and your other machines. Any saved secrets stay in your keychain.",
                            )
                          : translate(
                              "This deletes the server from Toolport on this machine. Any saved secrets stay in your keychain.",
                            )
                      }
                      confirmLabel={translate("Remove")}
                      destructive
                      onConfirm={onRemove}
                    />
                  </>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function StatusLabel({
  status,
  label,
  error,
}: {
  status: Status;
  label: string;
  error: string | null;
}) {
  const text = (
    <Badge
      aria-hidden="true"
      variant={STATUS_BADGE[status]}
      className="max-w-56 font-medium"
    >
      <span className="size-2 shrink-0 rounded-full bg-current" />
      <span className="truncate">{label}</span>
    </Badge>
  );
  if (status === "error" && error) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{text}</TooltipTrigger>
        <TooltipContent side="top" className="max-w-xs">
          <p className="text-meta text-warning">{errorHeadline(error)}</p>
        </TooltipContent>
      </Tooltip>
    );
  }
  return text;
}
