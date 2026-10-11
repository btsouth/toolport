import { useEffect, useState } from "react";
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
  X,
} from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import {
  importServers,
  previewImportServers,
  installGateway,
  setClientDiscovery,
  uninstallGateway,
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
import { ImportReviewDialog } from "@/components/ImportReviewDialog";
import { clientRestartHint, clientRestartHintAfterRemoval } from "@/lib/clientConnect";

interface Props {
  client: DetectedClient;
  registry: Registry | null;
  onChanged: () => void;
  onRegistryChange: (registry: Registry) => void;
}

/** Client-side rules can only target downstream names in Full mode. */
const DISCOVERY_HINT: Record<string, string> = {
  lazy: "Search, then call tools. Client per-tool permission rules need Full mode.",
  grouped:
    "Browse a server, then call tools. Client per-tool permission rules need Full mode.",
  full: "Full tool list. Client per-tool permission rules need Full mode.",
};

export function ClientDetail({ client, registry, onChanged, onRegistryChange }: Props) {
  const [busy, setBusy] = useState(false);
  // Snapshotted at dialog-open time so a registry-changed event mid-review can't
  // reshuffle `toImport` out from under the indices the user already confirmed.
  const [bulkImportServers, setBulkImportServers] = useState<ImportItem[] | null>(null);
  // Empty uses the default access; named access sets narrow enabled servers.
  const [profile, setProfile] = useState("");
  const [migrateOpen, setMigrateOpen] = useState(false);
  const [resetOpen, setResetOpen] = useState(false);
  const installed = client.gatewayInstalled;
  const customized = client.entryState === "customized";
  const legacyBearer = client.servers.some(hasLegacyBearerArgv);
  // Whether the client app is actually on this machine. We allow Disconnect even
  // when absent (to clean up a stale entry), but block a fresh Connect, writing a
  // config into a client that isn't installed just creates a file nothing reads.
  const present = client.appPresent;
  const profiles = registry?.profiles ?? [];
  // The access Toolport last connected this client with (empty uses the default). Keep the picker in sync with it as the selected client changes.
  const currentScope = registry?.clientScopes?.[client.id] ?? "";
  useEffect(() => {
    if (!currentScope) {
      setProfile("");
      return;
    }
    const match = profiles.find((p) => p.id === currentScope || p.name === currentScope);
    setProfile(match?.id ?? currentScope);
  }, [currentScope, client.id, registry?.profiles]);

  // SOU-317 follow-up (SBS-336): the restart advice is load-bearing — an MCP client
  // typically does not pick up a rewritten config until relaunch — but it only ever
  // appeared in a toast, which fades after a few seconds and is gone if the user was
  // looking elsewhere. Keep it in the panel until they dismiss it.
  // Keyed by client id rather than cleared in an effect: selecting another client must
  // not carry the previous one's advice across, and deriving that from the key avoids a
  // cascading-render setState-in-effect.
  const [restartNotice, setRestartNotice] = useState<{
    clientId: string;
    title: string;
    text: string;
  } | null>(null);
  const showRestartNotice = restartNotice?.clientId === client.id ? restartNotice : null;

  /** `applied` = Toolport was written into the config; `removed` = taken out of it.
   *
   * Both need a restart, for opposite reasons, and the connect wording ("so it loads
   * Toolport") reads as a failed disconnect on the removal path. */
  function noteRestartNeeded(kind: "applied" | "removed") {
    setRestartNotice({
      clientId: client.id,
      title: kind === "applied" ? "Not live yet" : "Still connected until restart",
      text:
        kind === "applied"
          ? clientRestartHint(client.name)
          : clientRestartHintAfterRemoval(client.name),
    });
  }

  // Absence is Auto; use the backend default rather than infer it from native search.
  const autoMode = client.discovery?.autoMode ?? "lazy";
  const storedMode = registry?.clientDiscovery?.[client.id]?.trim().toLowerCase();
  const clientMode = storedMode && DISCOVERY_HINT[storedMode] ? storedMode : "";
  const effectiveMode = clientMode || autoMode;

  /** Set or clear this client's discovery-mode override; applies live, no reconnect. */
  async function applyDiscovery(mode: string) {
    setBusy(true);
    try {
      const next = await setClientDiscovery(client.id, mode || null);
      onRegistryChange(next);
      toast.success(
        mode
          ? `${client.name} discovery set to "${mode}".`
          : `${client.name} now uses Auto discovery.`,
      );
    } catch (e) {
      toastError(`${e}`);
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
      // A set that is itself named "Default" would read "Default access (Default)".
      return name && name.trim().toLowerCase() !== "default"
        ? `Default access (${name})`
        : "Default access";
    }
    return scopeRef === "@all-enabled"
      ? "All enabled servers"
      : (profiles.find((p) => p.id === scopeRef || p.name === scopeRef)?.name ??
          scopeRef);
  }

  /** Re-apply a scope to an already-connected client (overwrites its gateway
   * entry's TOOLPORT_PROFILE in place, no disconnect needed). */
  async function applyScope() {
    if (customized) {
      // Must not silently overwrite a hand-edited entry (SOU-406).
      setResetOpen(true);
      return;
    }
    setBusy(true);
    try {
      const outcome = await installGateway(client.id, profile || undefined, false);
      // Rescope rewrites the client's MCP config the same way Connect does; without a
      // restart hint the change is invisible until the next cold start (SOU-317).
      toast.success(
        profile
          ? `${client.name} access set to "${accessLabel(profile)}".`
          : `${client.name} now uses the default access.`,
        {
          description: [clientRestartHint(client.name), ...(outcome.warnings ?? [])].join(
            " ",
          ),
        },
      );
      noteRestartNeeded("applied");
      onChanged();
    } catch (e) {
      toastError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  /** Overwrite a customized entry with the default gateway (after confirm). */
  async function resetToDefault() {
    setBusy(true);
    try {
      const outcome = await installGateway(client.id, profile || undefined, true);
      toast.success(
        legacyBearer
          ? `Migrated ${client.name} to stdio`
          : `Reset ${client.name} to the default Toolport gateway`,
        {
          description: [clientRestartHint(client.name), ...(outcome.warnings ?? [])].join(
            " ",
          ),
        },
      );
      noteRestartNeeded("applied");
      setResetOpen(false);
      onChanged();
    } catch (e) {
      toastError(`${e}`);
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
    (registry?.servers ?? []).map((s) => s.name.toLowerCase()),
  );
  const pluginNames = new Set(client.pluginServers.map((s) => s.name.toLowerCase()));

  // Every client-side server worth showing, deduped by name, minus Toolport's
  // own gateway entry. These exist here only as import candidates.
  const byName = new Map<string, McpServer>();
  for (const s of [...client.servers, ...client.pluginServers]) {
    if (isGatewayDetected(s)) continue;
    if (!byName.has(s.name.toLowerCase())) byName.set(s.name.toLowerCase(), s);
  }
  const allServers = [...byName.values()];
  const toImport = importableServers(client, registry);
  const bulkImportPreview = bulkImportServers;

  async function reviewServers(servers: McpServer[]) {
    try {
      const preview = await previewImportServers();
      const items = servers.map((server) =>
        preview.find(
          (item) =>
            item.name === server.name &&
            item.command === server.command &&
            item.url === server.url &&
            JSON.stringify(item.args) === JSON.stringify(server.args),
        ),
      );
      if (items.some((item) => !item?.key))
        throw new Error("Server changed. Refresh the client and review again.");
      setBulkImportServers(items as ImportItem[]);
    } catch (error) {
      toastError(String(error));
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
      toast.success("Imported selected servers. Check their status under Servers.");
      setBulkImportServers(null);
    } catch (error) {
      toastError(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function toggleInstall() {
    setBusy(true);
    try {
      if (installed) {
        const outcome = await uninstallGateway(client.id);
        const restored = outcome.restored?.length ?? 0;
        toast.success(
          restored > 0
            ? `Disconnected Toolport from ${client.name} and put back ${restored} server${restored === 1 ? "" : "s"}`
            : `Disconnected Toolport from ${client.name}`,
          {
            description: [
              clientRestartHintAfterRemoval(client.name),
              ...(outcome.warnings ?? []),
            ].join(" "),
          },
        );
        noteRestartNeeded("removed");
      }
      onChanged();
    } catch (e) {
      toastError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-5">
      {/* Connection: the one thing that actually matters in a client. */}
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          {customized ? (
            <Badge variant="warning" className="mb-1">
              <TriangleAlert className="size-3" />
              custom configuration
            </Badge>
          ) : installed ? (
            <Badge variant="success" className="mb-1">
              <Link2 className="size-3" />
              connected
            </Badge>
          ) : present ? (
            <Badge variant="info" className="mb-1">
              <Monitor className="size-3" />
              ready to connect
            </Badge>
          ) : (
            <Badge variant="outline" className="mb-1 text-muted-foreground">
              not installed
            </Badge>
          )}
          <p className="truncate font-mono text-xs text-muted-foreground">
            {client.configExists
              ? client.configPath
              : present
                ? "installed - no MCP config yet"
                : "not installed on this machine"}
          </p>
          {legacyBearer && (
            <p className="mt-1 text-xs text-warning">
              This older HTTP connection exposes its bearer credential in process
              arguments. Review migration to stdio below. Your existing connection is
              preserved until you confirm.
            </p>
          )}
          {customized && (
            <p className="mt-1 text-xs text-muted-foreground">
              Toolport is leaving your hand-edited gateway entry as-is. Reset it only if
              you want Toolport to manage the entry.
            </p>
          )}
          {installed && !customized && (
            <p className="mt-1 text-xs text-muted-foreground">
              Sees{" "}
              <span className="font-medium text-foreground">
                {accessLabel(currentScope)}
              </span>{" "}
              · {scopeServerCount(currentScope)} server
              {scopeServerCount(currentScope) === 1 ? "" : "s"}
            </p>
          )}
          {!present && !installed && (
            <p className="mt-1 text-xs text-muted-foreground">
              Install {client.name} on this machine before connecting it to Toolport.
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!customized && <span className="text-xs text-muted-foreground">Access</span>}
          {!customized && (
            <Select
              value={profile || "@default-access"}
              onValueChange={(value) =>
                setProfile(value === "@default-access" ? "" : value)
              }
            >
              <SelectTrigger aria-label="Access" size="sm" className="w-52">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="@default-access">{accessLabel("")}</SelectItem>
                <SelectItem value="@all-enabled">All enabled servers</SelectItem>
                {profiles.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {installed && !customized && profile !== currentScope && (
            <Button size="sm" onClick={applyScope} disabled={busy}>
              <Check className="size-4" />
              Apply access
            </Button>
          )}
          {(customized || legacyBearer) && (
            <ConfirmDialog
              open={resetOpen}
              onOpenChange={setResetOpen}
              trigger={
                <Button size="sm" variant="default" disabled={busy}>
                  <Check className="size-4" />
                  {legacyBearer ? "Review migration" : "Reset to default"}
                </Button>
              }
              title={
                legacyBearer
                  ? `Migrate ${client.name} to stdio?`
                  : `Reset ${client.name} to the default Toolport gateway?`
              }
              description={
                legacyBearer
                  ? "Toolport backs up the config before replacing the older HTTP connection with its standard stdio command. Other MCP servers stay in place. Any customized Toolport command, arguments and headers will be replaced; review those changes before confirming. Restart the client afterward."
                  : "This overwrites the hand-edited toolport entry with the standard stdio gateway command. Your other MCP servers are left alone."
              }
              confirmLabel={legacyBearer ? "Migrate to stdio" : "Reset to default"}
              onConfirm={resetToDefault}
            />
          )}
          {installed ? (
            <ConfirmDialog
              trigger={
                <Button size="sm" variant="outline" disabled={busy}>
                  <Plug className="size-4" />
                  Disconnect
                </Button>
              }
              title={`Disconnect Toolport from ${client.name}?`}
              description={
                customized
                  ? "This removes the custom toolport entry from the client's MCP config. You can reconnect anytime."
                  : "This rewrites the client's MCP config to remove the gateway and puts back any servers Toolport moved out of it. You can reconnect anytime."
              }
              confirmLabel="Disconnect"
              destructive
              onConfirm={toggleInstall}
            />
          ) : (
            <Button
              size="sm"
              variant="default"
              onClick={() => setMigrateOpen(true)}
              disabled={busy || !present}
            >
              <PlugZap className="size-4" />
              Connect to Toolport
            </Button>
          )}
        </div>
      </div>

      {client.error && (
        <div
          role="alert"
          className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-sm text-warning"
        >
          <TriangleAlert className="mt-0.5 size-4 shrink-0" />
          <div className="min-w-0">
            <p className="font-medium">Couldn't read this client's configuration</p>
            <p className="mt-0.5 break-words text-xs">{client.error}</p>
          </div>
        </div>
      )}

      {showRestartNotice && (
        <div
          role="status"
          className="flex items-start gap-2 rounded-lg border border-info/30 bg-info/10 px-3 py-2 text-sm text-info"
        >
          <RefreshCw className="mt-0.5 size-4 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="font-medium">{showRestartNotice.title}</p>
            <p className="mt-0.5 break-words text-xs">{showRestartNotice.text}</p>
          </div>
          <button
            type="button"
            onClick={() => setRestartNotice(null)}
            aria-label="Dismiss restart reminder"
            className="rounded p-0.5 text-info/70 transition-colors hover:bg-info/10 hover:text-info focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-info"
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      {!customized && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
          <div className="min-w-0">
            <div className="text-xs font-medium text-foreground">
              Discovery mode
              {!clientMode && (
                <span className="ml-1.5 font-normal text-muted-foreground">
                  Auto: {autoMode}
                </span>
              )}
            </div>
            <p className="mt-0.5 text-2xs text-muted-foreground">
              {DISCOVERY_HINT[effectiveMode] ?? DISCOVERY_HINT.lazy}
            </p>
          </div>
          <Select
            value={clientMode || "__auto__"}
            onValueChange={(v) => applyDiscovery(v === "__auto__" ? "" : v)}
          >
            <SelectTrigger
              size="sm"
              aria-label="Discovery mode"
              className="w-44 shrink-0"
              disabled={busy}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__auto__">Auto ({autoMode})</SelectItem>
              <SelectItem value="lazy">Lazy · search</SelectItem>
              <SelectItem value="grouped">Grouped · per-server</SelectItem>
              <SelectItem value="full">Full · every tool</SelectItem>
            </SelectContent>
          </Select>
        </div>
      )}

      {installed && !customized ? (
        <div>
          <div className="mb-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
            Servers it can reach
          </div>
          {scopeServers(currentScope).length === 0 ? (
            <p className="text-xs text-muted-foreground">
              No enabled servers in this access set yet. Turn servers on in Servers and
              include them under Settings &gt; Advanced.
            </p>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {scopeServers(currentScope).map((s) => (
                <span
                  key={s.id}
                  className="rounded-md border border-border/60 bg-muted/40 px-2 py-1 font-mono text-[11px] text-foreground/90"
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
            Connect {client.name} once and it reaches your{" "}
            <span className="font-medium text-foreground">
              {scopeServerCount(profile)} managed server
              {scopeServerCount(profile) === 1 ? "" : "s"}
            </span>{" "}
            through one gateway, no re-wiring per project. Your keys stay in your
            keychain.
          </p>
          <GatewayFlow
            clientName={client.name}
            servers={scopeServers(profile).map((s) => s.name)}
          />
        </div>
      ) : !customized ? (
        <p className="max-w-prose text-sm text-muted-foreground">
          Connect {client.name}, then enable servers under{" "}
          <span className="font-medium text-foreground">All servers</span> and
          they&apos;ll all route through Toolport, no per-client setup.
        </p>
      ) : null}

      {client.usesConnectors && (
        <Card className="gap-0 border-info/20 bg-info/5">
          <CardContent className="flex gap-3 p-4">
            <Puzzle className="mt-0.5 size-4 shrink-0 text-info" />
            <div className="text-sm">
              <p className="font-medium">{client.name} manages servers as connectors</p>
              <p className="mt-1 text-muted-foreground">
                Those live in {client.name}'s Customize → Connectors and sync to your
                account, outside the local config files Toolport reads. Connecting
                Toolport adds a local gateway entry so your Toolport-managed servers
                appear in {client.name} too.
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
            <span className="text-2xs font-semibold tracking-[0.09em] text-muted-foreground uppercase">
              Import into Toolport
            </span>
            <div className="flex items-center gap-1.5">
              {toImport.length > 0 && (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2 text-xs"
                  onClick={handleImportAll}
                  disabled={busy}
                >
                  <Download className="size-3" />
                  Import all ({toImport.length})
                </Button>
              )}
            </div>
          </div>
          <details className="mb-2">
            <summary className="cursor-pointer text-xs font-medium text-muted-foreground/80 hover:text-foreground">
              How importing works
            </summary>
            <ul className="mt-1.5 mb-1 space-y-0.5 text-xs text-muted-foreground">
              <li>
                <span className="font-medium text-foreground">Import</span> copies a
                server into Toolport; {client.name} keeps its own copy.
              </li>
              {movable.length > 0 && (
                <li>
                  <span className="font-medium text-foreground">Review and connect</span>{" "}
                  copies it, then removes it from {client.name}'s config so the gateway is
                  the only source (plugin servers stay). The cutover that actually saves
                  context.
                </li>
              )}
            </ul>
          </details>
          {installed && toImport.length > 0 && movable.length > 0 && (
            <p className="mb-3 -mt-1 inline-flex items-start gap-1.5 rounded-md bg-warning/10 px-2 py-1 text-xs text-warning">
              <TriangleAlert className="mt-0.5 size-3.5 shrink-0" />
              <span>
                {client.name} is already connected to Toolport. Import on its own leaves a
                copy here too, so these tools load twice, once directly and once through
                the gateway. Use <span className="font-medium">Review and connect</span>{" "}
                to avoid that.
              </span>
            </p>
          )}

          <div className="grid gap-2 sm:grid-cols-2">
            {allServers.map((server) => (
              <ServerMiniCard
                key={server.name}
                server={server}
                isPlugin={pluginNames.has(server.name.toLowerCase())}
                imported={importedNames.has(server.name.toLowerCase())}
                busy={busy}
                // Same review dialog as "Import all": a per-row import must show the
                // same shell/private-host warnings before anything is added.
                onImport={() => void reviewServers([server])}
              />
            ))}
          </div>

          {toImport.length === 0 && (
            <p className="mt-3 inline-flex items-center gap-1.5 text-xs text-success">
              <Check className="size-3.5" />
              Everything here is already in Toolport. Manage it under{" "}
              <span className="inline-flex items-center gap-0.5 font-medium">
                All servers <ArrowRight className="size-3" />
              </span>
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
            noteRestartNeeded("applied");
            onChanged();
          }}
        />
      )}
      <ImportReviewDialog
        open={bulkImportPreview !== null}
        items={bulkImportPreview ?? []}
        busy={busy}
        title={`Review ${client.name} servers`}
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
  const shown = servers.slice(0, 4);
  const extra = servers.length - shown.length;
  const link = "mb-7 h-px w-8 shrink-0";
  return (
    <div className="flex flex-wrap items-center justify-center gap-1 rounded-xl border border-border/60 bg-card/40 px-4 py-5">
      <div className="flex flex-col items-center gap-2 text-center">
        <div className="grid size-14 place-items-center rounded-xl border border-border bg-secondary text-xl">
          <Monitor className="size-6 text-muted-foreground" />
        </div>
        <div className="text-2xs font-semibold">{clientName}</div>
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
        <div className="text-2xs font-semibold">
          Toolport
          <span className="block font-normal text-muted-foreground">gateway</span>
        </div>
      </div>
      <div className={`${link} bg-border`} />
      <div className="flex flex-col gap-1">
        {shown.map((s) => (
          <span
            key={s}
            className="rounded-md border border-border/60 bg-card px-2 py-1 font-mono text-2xs text-muted-foreground"
          >
            {s}
          </span>
        ))}
        {extra > 0 && (
          <span className="px-2 font-mono text-2xs text-muted-foreground/60">
            +{extra} more
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
  busy,
  onImport,
}: {
  server: McpServer;
  isPlugin: boolean;
  imported: boolean;
  busy: boolean;
  onImport: () => void;
}) {
  return (
    <Card aria-disabled={imported} className={`gap-0 ${imported ? "opacity-70" : ""}`}>
      <CardContent className="flex flex-col gap-2 p-3">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-1.5">
            <span className="truncate text-sm font-medium">{server.name}</span>
            {isPlugin && (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                plugin
              </span>
            )}
          </div>
        </div>
        <code className="truncate font-mono text-xs text-muted-foreground">
          {server.command
            ? [server.command, ...server.args].join(" ")
            : (server.url ?? "")}
        </code>
        <div className="flex justify-end">
          {imported ? (
            <span className="inline-flex items-center gap-1 text-xs text-success">
              <Check className="size-3" />
              in Toolport
            </span>
          ) : (
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={onImport}
              disabled={busy}
            >
              <Download className="size-3" />
              Import
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
