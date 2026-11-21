import { msg, Trans, translate, useI18n, type SourceMessage } from "@/lib/i18n";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  ArrowRight,
  Check,
  Download,
  Link2,
  Monitor,
  Plug,
  PlugZap,
  Puzzle,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";
import {
  importServers,
  previewImportServers,
  installGateway,
  previewGatewaySettings,
  setClientDiscovery,
  uninstallGateway,
  removeClientLocation,
  listClientRecoveries,
} from "@/lib/api";
import {
  importableServers,
  hasLegacyBearerArgv,
  isGatewayServer,
  isGatewayDetected,
  type DetectedClient,
  type ImportItem,
  type McpServer,
  type Registry,
} from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ErrorCallout, StatusCard } from "@/components/ui/status-card";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ConnectReviewDialog } from "@/components/ConnectReviewDialog";
import { GatewaySettingsNotice } from "@/components/GatewaySettingsNotice";
import { DisclosureSummary, ImportReviewDialog } from "@/components/ImportReviewDialog";
import { clientRestartHint, clientRestartHintAfterRemoval } from "@/lib/clientConnect";
import { ClientRecoveryBanners } from "@/components/ClientRecoveryBanners";
import { clientDiscovery } from "@/lib/clientDiscovery";
import {
  clearListNotice,
  dismissClientNotice,
  noticeFor,
  releaseDetailNotice,
  setConnectNotice,
  setDetailNotice,
  useClientNotices,
} from "@/lib/clientNotice";

interface Props {
  client: DetectedClient;
  registry: Registry | null;
  onChanged: () => void;
  onRegistryChange: (registry: Registry) => void;
}

/** Client-side rules can only target downstream names in Full mode. */
const DISCOVERY_HINT: Record<string, SourceMessage> = {
  lazy: msg("Search, then call tools. Client per-tool permission rules need Full mode."),
  grouped: msg(
    "Browse a server, then call tools. Client per-tool permission rules need Full mode.",
  ),
  full: msg("Full tool list. Client per-tool permission rules need Full mode."),
};

export function ClientDetail({ client, registry, onChanged, onRegistryChange }: Props) {
  const { t } = useI18n();
  const keptReasonId = useId();
  const keptReason = t(
    "T3 Code still uses this folder. Remove it from T3 Code's provider settings first.",
  );
  const [busy, setBusy] = useState(false);
  // Bumped after Disconnect so the recovery card it may leave shows at once,
  // as in GTK, without waiting for a rescan.
  const [recoveryEpoch, setRecoveryEpoch] = useState(0);
  const recoveryKey = useMemo(() => ({ client, recoveryEpoch }), [client, recoveryEpoch]);
  // Snapshotted at dialog-open time so a registry-changed event mid-review can't
  // reshuffle `toImport` out from under the indices the user already confirmed.
  const [bulkImportServers, setBulkImportServers] = useState<ImportItem[] | null>(null);
  // A refused import shows in the review that is still open, not behind it.
  const [bulkImportError, setBulkImportError] = useState<string>();
  // Empty uses the default access; named access sets narrow enabled servers.
  const [profile, setProfile] = useState("");
  const [migrateOpen, setMigrateOpen] = useState(false);
  const [resetOpen, setResetOpen] = useState(false);
  // Each opening of the reset confirmation reads the settings it would add, so the
  // confirmation lists them as the connect review does.
  const [resetRequest, setResetRequest] = useState(0);
  const [resetSettings, setResetSettings] = useState<{
    request: number;
    lines: string[];
  } | null>(null);
  const resetSettingsReady = resetSettings?.request === resetRequest;
  function changeResetOpen(open: boolean) {
    if (open && !resetOpen) setResetRequest((request) => request + 1);
    setResetOpen(open);
  }
  useEffect(() => {
    if (!resetRequest) return;
    let active = true;
    previewGatewaySettings(client.id)
      .then((lines) => {
        if (active) setResetSettings({ request: resetRequest, lines });
      })
      .catch(() => {
        // Never block the reset on the list; the reset itself reports errors.
        if (active) setResetSettings({ request: resetRequest, lines: [] });
      });
    return () => {
      active = false;
    };
  }, [resetRequest, client.id]);
  const installed = client.gatewayInstalled;
  const customized = client.entryState === "customized";
  const legacyBearer = client.servers.some(hasLegacyBearerArgv);
  // Whether the client app is actually on this machine. We allow Disconnect even
  // when absent (to clean up a stale entry), but block a fresh Connect, writing a
  // config into a client that isn't installed just creates a file nothing reads.
  const present = client.appPresent;
  const profiles = useMemo(() => registry?.profiles ?? [], [registry?.profiles]);
  // The access Toolport last connected this client with (empty uses the default). Keep the picker in sync with it as the selected client changes.
  const currentScope = registry?.clientScopes?.[client.id] ?? "";
  useEffect(() => {
    if (!currentScope) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Reset the editable access picker when the selected client or saved scope changes.
      setProfile("");
      return;
    }
    const match = profiles.find((p) => p.id === currentScope || p.name === currentScope);
    setProfile(match?.id ?? currentScope);
  }, [currentScope, client.id, profiles]);

  // SOU-317 follow-up (SBS-336): the restart advice is load-bearing — an MCP client
  // typically does not pick up a rewritten config until relaunch — but it only ever
  // appeared in a toast, which fades after a few seconds and is gone if the user was
  // looking elsewhere. Keep it in the panel until they dismiss it.
  // Keyed by client id rather than cleared in an effect: selecting another client must
  // not carry the previous one's advice across, and deriving that from the key avoids a
  // cascading-render setState-in-effect.
  // The notice lives in a small store: the connect reminder also shows on the Clients
  // list, and dismissing it in either place clears both (as GTK does).
  const showRestartNotice = noticeFor(useClientNotices(), client.id);
  const setRestartNotice = setDetailNotice;
  // Feedback that arrives after this page closed is dropped: it would replace another
  // client's notice, or this client's list reminder, in the shared store.
  const pageOpen = useRef(true);
  useEffect(() => {
    pageOpen.current = true;
    return () => {
      pageOpen.current = false;
      releaseDetailNotice();
    };
  }, []);
  const [actionError, setActionError] = useState<{
    clientId: string;
    text: string;
  } | null>(null);
  const shownActionError = actionError?.clientId === client.id ? actionError.text : null;

  /** `applied` = Toolport was written into the config; `removed` = taken out of it.
   *
   * Both need a restart, for opposite reasons, and the connect wording ("so it loads
   * Toolport") reads as a failed disconnect on the removal path. */
  function showFeedback(title: string, text = "") {
    if (!pageOpen.current) return;
    setRestartNotice({ clientId: client.id, title, text });
  }

  function noteRestartNeeded(
    kind: "applied" | "removed",
    warnings: string[] = [],
    connected = false,
  ) {
    (connected ? setConnectNotice : setRestartNotice)({
      clientId: client.id,
      title:
        kind === "applied"
          ? translate("Not live yet")
          : translate("Still connected until restart"),
      text: [
        kind === "applied"
          ? clientRestartHint(client.name)
          : clientRestartHintAfterRemoval(client.name),
        ...warnings,
      ].join(" "),
    });
  }

  const { autoMode, autoFullMax, clientMode, effectiveMode } = clientDiscovery(
    client,
    registry,
  );
  const autoLabel = autoFullMax
    ? translate("full up to {count} tools, then lazy", { count: autoFullMax })
    : autoMode;

  /** Set or clear this client's discovery-mode override; applies live, no reconnect. */
  async function applyDiscovery(mode: string) {
    setBusy(true);
    try {
      const next = await setClientDiscovery(client.id, mode || null);
      onRegistryChange(next);
      showFeedback(
        mode
          ? translate('{value0} discovery set to "{value1}".', {
              value0: client.name,
              value1: mode,
            })
          : translate("{value0} now uses Auto discovery.", { value0: client.name }),
      );
    } catch (e) {
      showFeedback(String(e));
    } finally {
      setBusy(false);
    }
  }

  function scopeServers(scopeRef: string): { id: string; name: string }[] {
    const ref = scopeRef || registry?.defaultAccessProfileId || "@all-enabled";
    const target = profiles.find(
      (p) => p.id === ref || p.name.toLowerCase() === ref.toLowerCase(),
    );
    return (registry?.servers ?? [])
      .filter(
        (s) =>
          s.enabled &&
          !isGatewayServer(s) &&
          (ref === "@all-enabled" || target?.enabledServerIds.includes(s.id)),
      )
      .map((s) => ({ id: s.id, name: s.name }));
  }

  function scopeServerCount(scopeRef: string): number {
    return scopeServers(scopeRef).length;
  }
  function accessLabel(scopeRef: string): string {
    if (!scopeRef) {
      const defaultId =
        registry?.defaultAccessProfileId ||
        (registry?.defaultAccessLegacyPolicy ? registry.defaultAccessContextId : null);
      const name = profiles.find((p) => p.id === defaultId)?.name;
      // A set that is itself named "Default" would read "Default access (Default)",
      // and plain "Default access" next to it reads as the same choice.
      return name && name.trim().toLowerCase() !== "default"
        ? translate("Default access ({name})", { name })
        : translate("Default access (from Settings)");
    }
    return scopeRef === "@all-enabled"
      ? translate("All enabled servers")
      : (profiles.find((p) => p.id === scopeRef || p.name === scopeRef)?.name ??
          scopeRef);
  }

  /** Re-apply a scope to an already-connected client (overwrites its gateway
   * entry's TOOLPORT_PROFILE in place, no disconnect needed). */
  async function applyScope() {
    if (customized) {
      // Must not silently overwrite a hand-edited entry (SOU-406).
      changeResetOpen(true);
      return;
    }
    setBusy(true);
    try {
      const outcome = await installGateway(client.id, profile || undefined, false);
      // Rescope rewrites the client's MCP config the same way Connect does; without a
      // restart hint the change is invisible until the next cold start (SOU-317).
      noteRestartNeeded("applied", [
        profile
          ? translate('{value0} access set to "{value1}".', {
              value0: client.name,
              value1: accessLabel(profile),
            })
          : translate("{value0} now uses the default access.", { value0: client.name }),
        ...(outcome.warnings ?? []),
      ]);
      onChanged();
    } catch (e) {
      showFeedback(String(e));
    } finally {
      setBusy(false);
    }
  }

  /** Overwrite a customized entry with the default gateway (after confirm). */
  async function resetToDefault() {
    setBusy(true);
    try {
      const outcome = await installGateway(client.id, profile || undefined, true);
      noteRestartNeeded("applied", [
        legacyBearer
          ? translate("Migrated {value0} to stdio", { value0: client.name })
          : translate("Reset {value0} to the default Toolport gateway", {
              value0: client.name,
            }),
        ...(outcome.warnings ?? []),
      ]);
      setResetOpen(false);
      onChanged();
    } catch (e) {
      showFeedback(String(e));
      // Rethrow so ConfirmDialog stays open for retry (SOU-406 / CodeRabbit).
      throw e;
    } finally {
      setBusy(false);
    }
  }
  // Servers configured directly in the client (not the gateway) that migrate
  // would move into Toolport and strip from the client's config.

  const movable = client.servers.filter((s) => !isGatewayDetected(s));
  const importedNames = new Set(
    client.managedServerNames ??
      (registry?.servers ?? []).map((s) => s.name.toLowerCase()),
  );
  // Import candidates can carry a saved name (`github (2)`), so a plugin is
  // recognized by its connection, as the GTK shell does.
  const sameConnection = (a: McpServer, b: McpServer) =>
    a.transport === b.transport &&
    a.command === b.command &&
    a.url === b.url &&
    JSON.stringify(a.args) === JSON.stringify(b.args);
  const isPluginServer = (server: McpServer) =>
    !client.servers.some((s) => sameConnection(s, server)) &&
    client.pluginServers.some((s) => sameConnection(s, server));

  // Every client-side server worth showing, deduped by name, minus Toolport's
  // own gateway entry. These exist here only as import candidates.
  const byName = new Map<string, McpServer>();
  for (const s of [...client.servers, ...client.pluginServers]) {
    if (isGatewayDetected(s)) continue;
    if (!byName.has(s.name.toLowerCase())) byName.set(s.name.toLowerCase(), s);
  }
  const toImport = importableServers(client, registry);
  // Only review can check these: listed, never imported blind or called in Toolport.
  const unconfirmed = client.unconfirmedServers ?? [];
  // What Import would add, what review must check, then what is already in Toolport.
  const allServers = client.importableServers
    ? [
        ...toImport,
        ...unconfirmed,
        ...[...byName.values()].filter((s) => importedNames.has(s.name.toLowerCase())),
      ]
    : [...byName.values()];
  const bulkImportPreview = bulkImportServers;

  async function reviewServers(servers: McpServer[]) {
    try {
      const preview = await previewImportServers(client.id);
      const items = servers.map((server) => {
        // By the backend's connection id: the saved name can differ between the list
        // and the review, and two servers whose secrets differ can look alike here.
        const connection = unconfirmed.includes(server)
          ? client.unconfirmedKeys?.[unconfirmed.indexOf(server)]
          : client.importableKeys?.[toImport.indexOf(server)];
        return preview.find((item) =>
          connection
            ? item.connection === connection
            : item.name === server.name &&
              item.command === server.command &&
              item.url === server.url &&
              JSON.stringify(item.args) === JSON.stringify(server.args),
        );
      });
      // Review lists only what Import would change; an unconfirmed server it leaves
      // out is already in Toolport.
      const inToolport = servers.filter(
        (server, index) => !items[index] && unconfirmed.includes(server),
      );
      if (inToolport.length && inToolport.length === servers.length) {
        showFeedback(
          translate("{value0} is already in Toolport.", {
            value0: inToolport.map((server) => server.name).join(", "),
          }),
        );
        return;
      }
      if (items.some((item) => !item?.key))
        throw new Error(
          translate("Server changed. Refresh the client and review again."),
        );
      setBulkImportError(undefined);
      setBulkImportServers(items as ImportItem[]);
    } catch (error) {
      showFeedback(String(error));
    }
  }

  async function handleImportAll() {
    await reviewServers(toImport);
  }

  async function confirmImportAll(
    selected: string[],
    choices?: Record<string, Record<string, boolean>>,
    inputs?: Record<string, Record<string, string>>,
  ) {
    setBusy(true);
    try {
      const next = inputs
        ? await importServers(selected, choices, inputs)
        : choices
          ? await importServers(selected, choices)
          : await importServers(selected);
      onRegistryChange(next);
      showFeedback(
        translate("Imported selected servers. Check their status under Servers."),
      );
      setBulkImportServers(null);
      // Detection decides what is in Toolport now; scan again.
      onChanged();
    } catch (error) {
      setBulkImportError(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function toggleInstall() {
    setBusy(true);
    setActionError(null);
    try {
      if (installed) {
        let outcome: Awaited<ReturnType<typeof uninstallGateway>>;
        try {
          outcome = await uninstallGateway(client.id);
        } catch (refused) {
          // Same inline error line and wording as the GTK shell.
          setActionError({
            clientId: client.id,
            text: `${client.name}: ${String(refused)}`,
          });
          return;
        }
        // The disconnect already committed, even if loading notices later fails.
        clearListNotice(client.id);
        onChanged();
        setRecoveryEpoch((value) => value + 1);
        const restored = outcome.restored?.length ?? 0;
        // Recovery owns its own persistent notice. Do not also announce it in
        // an ordinary success notice or in a transient toast, but keep any
        // other warning, such as a keychain token that could not be removed.
        const pending = (await listClientRecoveries()).filter(
          (row) => row.clientId === client.id,
        );
        const warnings = (outcome.warnings ?? []).filter(
          (warning) => !pending.some((row) => warning.startsWith(row.message)),
        );
        if (pending.length && !warnings.length) {
          setRestartNotice(null);
        } else {
          noteRestartNeeded("removed", [
            restored > 0
              ? translate(
                  restored === 1
                    ? msg(
                        "Disconnected Toolport from {client} and put back {count} server.",
                      )
                    : msg(
                        "Disconnected Toolport from {client} and put back {count} servers.",
                      ),
                  { client: client.name, count: restored },
                )
              : translate("Disconnected Toolport from {value0}.", {
                  value0: client.name,
                }),
            ...warnings,
          ]);
        }
      }
      if (!installed) onChanged();
    } catch (e) {
      showFeedback(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-w-0 flex-col gap-5">
      <ClientRecoveryBanners
        clientId={client.id}
        refreshKey={recoveryKey}
        onRecovered={() => {
          // The recovery that blocked Disconnect changed: its error is stale.
          setActionError(null);
          onChanged();
        }}
      />
      {/* Connection: the one thing that actually matters in a client. */}
      {/* The title keeps at least 240px; when the actions don't fit beside it they wrap below. */}
      <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
        <div className="min-w-0 flex-1 basis-60">
          {/* The same words, case and colors as the Clients list. */}
          {customized ? (
            <Badge variant="warning" className="mb-1">
              <TriangleAlert aria-hidden="true" />
              {t("Custom configuration")}
            </Badge>
          ) : installed ? (
            <Badge variant="success" className="mb-1">
              <Link2 aria-hidden="true" />
              {t("Connected")}
            </Badge>
          ) : present ? (
            <Badge variant="info" className="mb-1">
              <Monitor aria-hidden="true" />
              {t("Ready to connect")}
            </Badge>
          ) : (
            <Badge variant="off" className="mb-1">
              {t("Not installed")}
            </Badge>
          )}
          <p className="truncate font-mono text-meta text-muted-foreground">
            {client.configExists
              ? client.configPath
              : present
                ? t("installed - no MCP config yet")
                : t("not installed on this machine")}
          </p>
          {legacyBearer && (
            <p className="mt-1 text-meta text-warning">
              {translate(
                "This older HTTP connection exposes its bearer credential in process arguments. Review migration to stdio below. Your existing connection is preserved until you confirm.",
              )}
            </p>
          )}
          {customized && (
            <p className="mt-1 text-meta text-muted-foreground">
              {t(
                "Toolport is leaving your hand-edited gateway entry as-is. Reset it only if you want Toolport to manage the entry.",
              )}
            </p>
          )}
          {installed && !customized && (
            <p className="mt-1 text-meta text-muted-foreground">
              <Trans
                message="Sees {access}"
                values={{
                  access: (
                    <span className="font-medium text-foreground">
                      {accessLabel(currentScope)}
                    </span>
                  ),
                }}
              />{" "}
              ·{" "}
              {t(
                scopeServerCount(currentScope) === 1
                  ? msg("{count} server")
                  : msg("{count} servers"),
                { count: scopeServerCount(currentScope) },
              )}
            </p>
          )}
          {!present && !installed && (
            <p className="mt-1 text-meta text-muted-foreground">
              <Trans
                message={msg(
                  "Install {value1} on this machine before connecting it to Toolport.",
                )}
                values={{ value1: client.name }}
              />
            </p>
          )}
        </div>
        <div className="flex max-w-full flex-wrap items-center gap-2">
          {client.locationKeptByT3Code && !client.locationRemovable && (
            <>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span
                    // A disabled button gets no pointer or focus events, so the wrapper
                    // shows why, to keyboard users too.
                    // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
                    tabIndex={0}
                    className="inline-flex rounded-md focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
                  >
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled
                      aria-describedby={keptReasonId}
                    >
                      {translate("Remove location")}
                    </Button>
                  </span>
                </TooltipTrigger>
                <TooltipContent className="max-w-64">{keptReason}</TooltipContent>
              </Tooltip>
              <span id={keptReasonId} className="sr-only">
                {keptReason}
              </span>
            </>
          )}
          {client.locationRemovable && (
            <ConfirmDialog
              trigger={
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy || installed || !!client.error}
                >
                  {translate("Remove location")}
                </Button>
              }
              title={translate("Remove {value0}?", { value0: client.name })}
              description={translate(
                "This forgets the added location. Your client config and backups are kept. Auto-detected Claude folders still appear in Clients.",
              )}
              confirmLabel={translate("Remove location")}
              onConfirm={async () => {
                await removeClientLocation(client.id);
                onChanged();
              }}
            />
          )}
          {!customized && (
            <span className="text-meta text-muted-foreground">{translate("Access")}</span>
          )}
          {!customized && (
            <Select
              value={profile || "@default-access"}
              onValueChange={(value) =>
                setProfile(value === "@default-access" ? "" : value)
              }
            >
              <SelectTrigger
                aria-label={translate("Access")}
                disabled={busy || !!client.error}
                size="sm"
                className="w-auto max-w-full min-w-[min(13rem,100%)]"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="@default-access">{accessLabel("")}</SelectItem>
                <SelectItem value="@all-enabled">
                  {translate("All enabled servers")}
                </SelectItem>
                {profiles.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {installed && !customized && profile !== currentScope && (
            <Button size="sm" onClick={applyScope} disabled={busy || !!client.error}>
              <Check className="size-4" />
              {translate("Apply access")}
            </Button>
          )}
          {(customized || legacyBearer) && (
            <ConfirmDialog
              open={resetOpen}
              onOpenChange={changeResetOpen}
              trigger={
                <Button size="sm" variant="default" disabled={busy || !!client.error}>
                  <Check className="size-4" />
                  {legacyBearer
                    ? translate(msg("Review migration"))
                    : translate("Reset to default")}
                </Button>
              }
              title={
                legacyBearer
                  ? translate("Migrate {value0} to stdio?", { value0: client.name })
                  : translate("Reset {value0} to the default Toolport gateway?", {
                      value0: client.name,
                    })
              }
              description={
                <>
                  <p>
                    {legacyBearer
                      ? translate(
                          "Toolport backs up the config before replacing the older HTTP connection with its standard stdio command. Other MCP servers stay in place. Any customized Toolport command, arguments and headers will be replaced; review those changes before confirming. Restart the client afterward.",
                        )
                      : translate(
                          "This overwrites the hand-edited toolport entry with the standard stdio gateway command. Your other MCP servers are left alone.",
                        )}
                  </p>
                  {resetSettingsReady && (
                    <div className="mt-3 text-popover-foreground">
                      <GatewaySettingsNotice
                        clientName={client.name}
                        lines={resetSettings.lines}
                      />
                    </div>
                  )}
                </>
              }
              confirmDisabled={!resetSettingsReady}
              confirmLabel={
                legacyBearer
                  ? translate(msg("Migrate to stdio"))
                  : translate("Reset to default")
              }
              onConfirm={resetToDefault}
            />
          )}
          {installed ? (
            <ConfirmDialog
              trigger={
                <Button size="sm" variant="destructive" disabled={busy || !!client.error}>
                  <Trans
                    message={msg("{value1}Disconnect")}
                    values={{ value1: <Plug className="size-4" /> }}
                  />
                </Button>
              }
              title={t("Disconnect Toolport from {value1}?", { value1: client.name })}
              description={
                customized
                  ? t(
                      "This removes the custom toolport entry from the client's MCP config. You can reconnect anytime.",
                    )
                  : t(
                      "This rewrites the client's MCP config to remove the gateway and puts back any servers Toolport moved out of it. You can reconnect anytime.",
                    )
              }
              confirmLabel={t("Disconnect")}
              destructive
              onConfirm={toggleInstall}
            />
          ) : (
            <Button
              size="sm"
              variant="default"
              onClick={() => setMigrateOpen(true)}
              disabled={busy || !present || !!client.error}
            >
              <Trans
                message={msg("{value1}Connect to Toolport")}
                values={{ value1: <PlugZap className="size-4" /> }}
              />
            </Button>
          )}
        </div>
      </div>

      {client.error && (
        <ErrorCallout
          title={translate("Couldn't read this client's configuration")}
          cause={client.error}
          onRetry={onChanged}
        />
      )}

      {shownActionError && (
        <p
          role="alert"
          className="select-text break-words toolport-alert border-destructive bg-destructive/8 px-3 py-2 text-sm text-destructive"
        >
          {shownActionError}
        </p>
      )}

      {showRestartNotice && (
        <StatusCard
          role="status"
          variant="info"
          className="select-text"
          icon={<RefreshCw />}
          title={showRestartNotice.title}
          detail={showRestartNotice.text}
          actions={
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => dismissClientNotice(showRestartNotice)}
            >
              {t("Dismiss")}
            </Button>
          }
        />
      )}

      {!customized && (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
          <div className="min-w-[min(15rem,100%)] flex-1">
            <div className="text-meta font-medium text-foreground">
              {translate("Discovery mode")}
              {!clientMode && (
                <span className="ml-1.5 font-normal text-muted-foreground">
                  {t("Auto: {mode}", { mode: autoLabel })}
                </span>
              )}
            </div>
            <p className="mt-0.5 text-meta text-muted-foreground">
              {t(DISCOVERY_HINT[effectiveMode] ?? DISCOVERY_HINT.lazy)}
            </p>
          </div>
          <Select
            value={clientMode || "__auto__"}
            onValueChange={(v) => applyDiscovery(v === "__auto__" ? "" : v)}
          >
            <SelectTrigger
              size="sm"
              aria-label={translate("Discovery mode")}
              className="w-auto max-w-full"
              disabled={busy || !!client.error}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__auto__">
                {t("Auto ({mode})", { mode: autoLabel })}
              </SelectItem>
              <SelectItem value="lazy">{translate("Lazy · search")}</SelectItem>
              <SelectItem value="grouped">{translate("Grouped · per-server")}</SelectItem>
              <SelectItem value="full">{translate("Full · every tool")}</SelectItem>
            </SelectContent>
          </Select>
        </div>
      )}

      {installed && !customized ? (
        <div>
          <div className="mb-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
            {t("Servers it can reach")}
          </div>
          {scopeServers(currentScope).length === 0 ? (
            <p className="text-meta text-muted-foreground">
              {translate(
                "No enabled servers in this access set yet. Turn servers on in Servers and include them under Settings > Advanced.",
              )}
            </p>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {scopeServers(currentScope).map((s) => (
                <span
                  key={s.id}
                  className="rounded-md border border-border/60 bg-muted/40 px-2 py-1 font-mono text-meta text-foreground"
                >
                  {s.name}
                </span>
              ))}
            </div>
          )}
        </div>
      ) : !customized && scopeServerCount(profile) > 0 ? (
        <div className="flex flex-col gap-4">
          <p className="max-w-prose text-sm text-muted-foreground">
            <Trans
              message={msg(
                "Connect {value1} once and it reaches your{value2}{value3}{value4}through one gateway, no re-wiring per project. Your keys stay in your keychain.",
              )}
              values={{
                value1: client.name,
                value2: " ",
                value3: (
                  <span className="font-medium text-foreground">
                    <Trans
                      message={
                        scopeServerCount(profile) === 1
                          ? msg("{value1} managed server")
                          : msg("{value1} managed servers")
                      }
                      values={{ value1: scopeServerCount(profile) }}
                    />
                  </span>
                ),
                value4: " ",
              }}
            />
          </p>
          <GatewayFlow
            clientName={client.name}
            servers={scopeServers(profile).map((s) => s.name)}
          />
        </div>
      ) : !customized ? (
        <p className="max-w-prose text-sm text-muted-foreground">
          <Trans
            message={msg(
              "Connect {value1}, then enable servers under{value2}{value3} and they'll all route through Toolport, no per-client setup.",
            )}
            values={{
              value1: client.name,
              value2: " ",
              value3: (
                <span className="font-medium text-foreground">{t("All servers")}</span>
              ),
            }}
          />
        </p>
      ) : null}

      {client.usesConnectors && (
        <Card className="gap-0 border-info/20 bg-info/5">
          <CardContent className="flex gap-3 p-4">
            <Puzzle className="mt-0.5 size-4 shrink-0 text-info" />
            <div className="text-sm">
              <p className="font-medium">
                <Trans
                  message={msg("{value1} manages servers as connectors")}
                  values={{ value1: client.name }}
                />
              </p>
              <p className="mt-1 text-muted-foreground">
                <Trans
                  message={msg(
                    "Those live in {value1}'s Customize → Connectors and sync to your account, outside the local config files Toolport reads. Connecting Toolport adds a local gateway entry so your Toolport-managed servers appear in {value2} too.",
                  )}
                  values={{ value1: client.name, value2: client.name }}
                />
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Import: client servers are sources to pull into Toolport. The WHOLE block only
          renders when the client actually has servers of its own to import - otherwise it
          used to show a header + a "how importing works" explainer describing Import / Move
          buttons that weren't on screen (an AI-agent client with no own servers). */}
      {allServers.length > 0 && (
        <div>
          <div className="mb-1 flex items-center justify-between gap-2">
            <span className="text-xs font-semibold tracking-[0.09em] text-muted-foreground uppercase">
              {t("Import into Toolport")}
            </span>
            <div className="flex items-center gap-1.5">
              {toImport.length > 0 && (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2 text-xs"
                  onClick={handleImportAll}
                  disabled={busy || !!client.error}
                >
                  <Trans
                    message={msg("{value1}Import all ({value2})")}
                    values={{
                      value1: <Download className="size-3" />,
                      value2: toImport.length,
                    }}
                  />
                </Button>
              )}
            </div>
          </div>
          <details className="mb-2">
            <DisclosureSummary className="text-meta font-medium text-muted-foreground hover:text-foreground">
              {t("How importing works")}
            </DisclosureSummary>
            <ul className="mt-1.5 mb-1 space-y-0.5 text-meta text-muted-foreground">
              <li>
                <Trans
                  message={msg(
                    "{value1} copies a server into Toolport; {value2} keeps its own copy.",
                  )}
                  values={{
                    value1: (
                      <span className="font-medium text-foreground">{t("Import")}</span>
                    ),
                    value2: client.name,
                  }}
                />
              </li>
              {movable.length > 0 && (
                <li>
                  <Trans
                    message="{review} copies it, then removes it from {client}'s config so the gateway is the only source (plugin servers stay). The cutover that actually saves context."
                    values={{
                      review: (
                        <span className="font-medium text-foreground">
                          {t("Review and connect")}
                        </span>
                      ),
                      client: client.name,
                    }}
                  />
                </li>
              )}
            </ul>
          </details>
          {installed && toImport.length > 0 && movable.length > 0 && (
            <p className="mb-3 -mt-1 inline-flex items-start gap-1.5 rounded-md bg-warning/10 px-2 py-1 text-meta text-warning">
              <TriangleAlert className="mt-0.5 size-3.5 shrink-0" />
              <span>
                <Trans
                  message="{client} is already connected to Toolport. Import on its own leaves a copy here too, so these tools load twice, once directly and once through the gateway. Use {review} to avoid that."
                  values={{
                    client: client.name,
                    review: (
                      <span className="font-medium">{t("Review and connect")}</span>
                    ),
                  }}
                />
              </span>
            </p>
          )}

          <div className="grid gap-2 sm:grid-cols-2">
            {allServers.map((server, index) => (
              <ServerMiniCard
                key={`${index}-${server.name}`}
                server={server}
                isPlugin={isPluginServer(server)}
                imported={
                  client.importableServers
                    ? index >= toImport.length + unconfirmed.length
                    : importedNames.has(server.name.toLowerCase())
                }
                unconfirmed={unconfirmed.includes(server)}
                busy={busy || !!client.error}
                // Same review dialog as "Import all": a per-row import must show the
                // same shell/private-host warnings before anything is added.
                onImport={() => void reviewServers([server])}
              />
            ))}
          </div>

          {toImport.length === 0 && unconfirmed.length === 0 && (
            <p className="mt-3 inline-flex items-center gap-1.5 text-meta text-success">
              <Trans
                message={msg(
                  "{value1}Everything here is already in Toolport. Manage it under{value2}{value3}",
                )}
                values={{
                  value1: <Check className="size-3.5" />,
                  value2: " ",
                  value3: (
                    <span className="inline-flex items-center gap-0.5 font-medium">
                      <Trans
                        message={msg("All servers {value1}")}
                        values={{ value1: <ArrowRight className="size-3" /> }}
                      />
                    </span>
                  ),
                }}
              />
            </p>
          )}
        </div>
      )}

      {migrateOpen && (
        <ConnectReviewDialog
          clientId={client.id}
          clientName={client.name}
          profile={profile || undefined}
          force={customized}
          onClose={() => setMigrateOpen(false)}
          onConnected={(next) => {
            onRegistryChange(next);
            noteRestartNeeded("applied", [], true);
            onChanged();
          }}
        />
      )}
      <ImportReviewDialog
        open={bulkImportPreview !== null}
        items={bulkImportPreview ?? []}
        busy={busy}
        error={bulkImportError}
        title={t("Review {value1} servers", { value1: client.name })}
        onOpenChange={(open) => {
          if (!open && !busy) setBulkImportServers(null);
        }}
        onConfirm={confirmImportAll}
      />
    </div>
  );
}

/** The product in one glance: client -> Toolport gateway -> the servers it reaches. Shows
 * the pitch concretely instead of describing it in prose. */
function GatewayFlow({ clientName, servers }: { clientName: string; servers: string[] }) {
  const { t } = useI18n();
  const shown = servers.slice(0, 4);
  const extra = servers.length - shown.length;
  const link = "mb-7 h-px w-8 shrink-0";
  return (
    <div className="flex flex-wrap items-center justify-center gap-1 rounded-xl border border-border/60 bg-card px-4 py-5">
      <div className="flex flex-col items-center gap-2 text-center">
        <div className="grid size-14 place-items-center rounded-xl border border-border bg-secondary text-xl">
          <Monitor className="size-6 text-muted-foreground" />
        </div>
        <div className="text-meta font-semibold">{clientName}</div>
      </div>
      <div className={`${link} bg-border`} />
      <div className="flex flex-col items-center gap-2 text-center">
        <div className="grid size-16 place-items-center rounded-xl border border-primary/35 bg-primary/10">
          <svg width="30" height="30" viewBox="0 0 32 32" aria-hidden="true">
            <circle
              cx="16"
              cy="16"
              r="13"
              fill="none"
              stroke="var(--brand)"
              strokeWidth="2.5"
            />
            <circle cx="16" cy="16" r="5" fill="var(--brand)" />
          </svg>
        </div>
        <div className="text-meta font-semibold">
          <Trans
            message={msg("Toolport {value1}")}
            values={{
              value1: (
                <span className="block font-normal text-muted-foreground">
                  {t("gateway")}
                </span>
              ),
            }}
          />
        </div>
      </div>
      <div className={`${link} bg-border`} />
      <div className="flex flex-col gap-1">
        {shown.map((s) => (
          <span
            key={s}
            className="rounded-md border border-border/60 bg-card px-2 py-1 font-mono text-xs text-muted-foreground"
          >
            {s}
          </span>
        ))}
        {extra > 0 && (
          <span className="px-2 font-mono text-meta text-muted-foreground">
            <Trans message={msg("+{value1} more")} values={{ value1: extra }} />
          </span>
        )}
      </div>
    </div>
  );
}

function ServerMiniCard({
  server,
  isPlugin,
  imported,
  unconfirmed,
  busy,
  onImport,
}: {
  server: McpServer;
  isPlugin: boolean;
  imported: boolean;
  unconfirmed: boolean;
  busy: boolean;
  onImport: () => void;
}) {
  const { t } = useI18n();
  return (
    <Card aria-disabled={imported} className={`gap-0 ${imported ? "bg-muted" : ""}`}>
      <CardContent className="flex flex-col gap-2 p-3">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-1.5">
            <span className="truncate text-sm font-medium">{server.name}</span>
            {isPlugin && (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                {t("plugin")}
              </span>
            )}
          </div>
        </div>
        <code className="truncate font-mono text-meta text-muted-foreground">
          {server.command
            ? [server.command, ...server.args].join(" ")
            : (server.url ?? "")}
        </code>
        {unconfirmed && (
          <p className="text-meta text-muted-foreground">
            {t("Open the review to check whether this is already in Toolport.")}
          </p>
        )}
        <div className="flex justify-end">
          {imported ? (
            <span className="inline-flex items-center gap-1 text-meta text-success">
              <Trans
                message={msg("{value1}in Toolport")}
                values={{ value1: <Check className="size-3" /> }}
              />
            </span>
          ) : (
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={onImport}
              disabled={busy}
            >
              {unconfirmed ? (
                t("Review")
              ) : (
                <Trans
                  message={msg("{value1}Import")}
                  values={{ value1: <Download className="size-3" /> }}
                />
              )}
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
