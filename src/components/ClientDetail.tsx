import { useI18n, Trans } from "@/lib/i18n";
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
  Shuffle,
  TriangleAlert,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import {
  addServer,
  installGateway,
  migrateClient,
  setClientDiscovery,
  uninstallGateway,
} from "@/lib/api";
import {
  importableServers,
  isGatewayServer,
  isGatewayDetected,
  type DetectedClient,
  type ImportItem,
  type McpServer,
  type Registry,
  type ServerEntry,
} from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { TransportPill } from "@/components/TransportPill";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ImportReviewDialog } from "@/components/ImportReviewDialog";
import {
  clientRestartHint,
  clientRestartHintAfterRemoval,
  connectSuccessDescription,
} from "@/lib/clientConnect";

interface Props {
  client: DetectedClient;
  registry: Registry | null;
  onChanged: () => void;
  onRegistryChange: (registry: Registry) => void;
}

/** One-line explainer per discovery mode, shown under the per-client picker. */
const DISCOVERY_HINT: Record<string, string> = {
  lazy: "Advertises a few meta-tools; the client searches, then calls. Fewest tokens.",
  grouped: "One help tool per server; the client expands a server before calling it.",
  full: "Advertises every tool up front. Most tokens, no discovery step.",
};

// Local-model desktop apps: users often run small / quantized models here that stumble on
// lazy's multi-step search-then-call chain, yet get flooded by the full catalog. Grouped
// (one hop per server) is the middle ground. Every other client is a hosted-model or
// agentic client that handles lazy's savings fine. This drives a RECOMMENDATION only; it
// never auto-applies, so an explicit user choice is never silently overridden.
const LOCAL_MODEL_CLIENTS = new Set(["lm-studio", "jan", "anythingllm"]);

/** The mode we suggest for a client, with a one-line why. Advisory, not enforced. */
function recommendedMode(clientId: string): { mode: string; why: string } {
  return LOCAL_MODEL_CLIENTS.has(clientId)
    ? { mode: "grouped", why: "local models do better browsing one server at a time" }
    : {
        mode: "lazy",
        why: "this client handles the search-then-call step and saves the most tokens",
      };
}

export function ClientDetail({ client, registry, onChanged, onRegistryChange }: Props) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  // Snapshotted at dialog-open time so a registry-changed event mid-review can't
  // reshuffle `toImport` out from under the indices the user already confirmed.
  const [bulkImportServers, setBulkImportServers] = useState<McpServer[] | null>(null);
  // "" = follow the active profile; else scope to one.
  const [profile, setProfile] = useState("");
  // stdio (spawn per client) or sharedHttp (one bridge, SOU-407).
  const [transport, setTransport] = useState<"stdio" | "sharedHttp">("stdio");
  const [migrateOpen, setMigrateOpen] = useState(false);
  const [resetOpen, setResetOpen] = useState(false);
  const installed = client.gatewayInstalled;
  const customized = client.entryState === "customized";
  const managedTransport =
    registry?.clientManagedEntries?.[client.id]?.transport === "sharedHttp"
      ? "sharedHttp"
      : "stdio";
  // Whether the client app is actually on this machine. We allow Disconnect even
  // when absent (to clean up a stale entry), but block a fresh Connect, writing a
  // config into a client that isn't installed just creates a file nothing reads.
  const present = client.appPresent;
  const profiles = registry?.profiles ?? [];
  // The scope Toolport last connected this client with ("" = follow the active
  // profile). Keep the picker in sync with it as the selected client changes.
  const currentScope = registry?.clientScopes?.[client.id] ?? "";
  useEffect(() => {
    if (!currentScope) {
      setProfile("");
      return;
    }
    const match = profiles.find((p) => p.id === currentScope || p.name === currentScope);
    setProfile(match?.id ?? currentScope);
  }, [currentScope, client.id, registry?.profiles]);
  useEffect(() => {
    setTransport(managedTransport);
  }, [managedTransport, client.id]);

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

  // Discovery: the global mode this client falls back to, and its own override (if any).
  // The gateway resolves env > per-client > global, so an override here applies live.
  const globalMode =
    registry?.discoveryMode?.toLowerCase() ||
    ((registry?.lazyDiscovery ?? true) ? "lazy" : "full");
  const clientMode = registry?.clientDiscovery?.[client.id] ?? "";
  const effectiveMode = clientMode || globalMode;
  const recommended = recommendedMode(client.id);

  /** Set or clear this client's discovery-mode override; applies live, no reconnect. */
  async function applyDiscovery(mode: string) {
    setBusy(true);
    try {
      const next = await setClientDiscovery(client.id, mode || null);
      onRegistryChange(next);
      toast.success(
        mode
          ? t('{value1} discovery set to "{value2}".', {
              value1: client.name,
              value2: mode,
            })
          : t("{value1} now inherits the global discovery mode.", {
              value1: client.name,
            }),
      );
    } catch (e) {
      toastError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  /** How many servers a given scope ("" = active profile, else a named profile)
   * resolves to, for the "scoped to X · N servers" summary. */
  function scopeServerCount(scopeRef: string): number {
    const target = scopeRef
      ? profiles.find(
          (p) => p.id === scopeRef || p.name.toLowerCase() === scopeRef.toLowerCase(),
        )
      : (profiles.find((p) => p.id === registry?.activeProfileId) ?? profiles[0]);
    if (!target) return 0;
    // Exclude Toolport's own gateway entry so the count matches the Servers list (which
    // filters it out) instead of over-counting by one.
    const ids = new Set(
      (registry?.servers ?? []).filter((s) => !isGatewayServer(s)).map((s) => s.id),
    );
    return target.enabledServerIds.filter((id) => ids.has(id)).length;
  }

  /** The actual servers a scope resolves to, so a connected client shows WHAT it can
   * reach, not just a count. */
  function scopeServers(scopeRef: string): { id: string; name: string }[] {
    const target = scopeRef
      ? profiles.find(
          (p) => p.id === scopeRef || p.name.toLowerCase() === scopeRef.toLowerCase(),
        )
      : (profiles.find((p) => p.id === registry?.activeProfileId) ?? profiles[0]);
    if (!target) return [];
    const enabled = new Set(target.enabledServerIds);
    // Never surface the gateway's own entry as a reachable server.
    return (registry?.servers ?? [])
      .filter((s) => enabled.has(s.id) && !isGatewayServer(s))
      .map((s) => ({ id: s.id, name: s.name }));
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
      // Pass live transport so Apply scope does not rewrite Shared HTTP → stdio (WS3-2).
      await installGateway(client.id, profile || undefined, false, transport);
      // Rescope rewrites the client's MCP config the same way Connect does; without a
      // restart hint the change is invisible until the next cold start (SOU-317).
      const scopeName = profiles.find((p) => p.id === profile)?.name ?? profile;
      toast.success(
        profile
          ? t('{value1} scoped to "{value2}".', {
              value1: client.name,
              value2: scopeName,
            })
          : t("{value1} now follows the active profile.", { value1: client.name }),
        { description: clientRestartHint(client.name) },
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
      await installGateway(client.id, profile || undefined, true, transport);
      toast.success(
        t("Reset {value1} to the default Toolport gateway", { value1: client.name }),
        {
          description: clientRestartHint(client.name),
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

  async function migrate() {
    setBusy(true);
    try {
      // Migrate rewrites the whole gateway slot; force only after the user confirmed
      // the migrate dialog when the entry is customized (SOU-406).
      const result = await migrateClient(
        client.id,
        profile || undefined,
        customized || undefined,
        transport,
      );
      onRegistryChange(result.registry);
      toast.success(
        t(
          result.moved.length === 1
            ? "Moved {value1} server into Toolport"
            : "Moved {value1} servers into Toolport",
          { value1: result.moved.length },
        ),
        {
          // Migrate rewrites the whole gateway slot, so it needs the restart line for
          // the same reason Connect does.
          description: t(
            "{value1} now uses only the Toolport gateway. Config backed up. {value2}",
            { value1: client.name, value2: clientRestartHint(client.name) },
          ),
        },
      );
      noteRestartNeeded("applied");
      setMigrateOpen(false);
      onChanged();
    } catch (e) {
      toastError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

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
  const bulkImportPreview: ImportItem[] | null =
    bulkImportServers?.map((server, index) => ({
      key: String(index),
      name: server.name,
      transport: server.transport,
      command: server.command,
      args: server.args,
      url: server.url,
      isNew: true,
    })) ?? null;

  async function importOne(server: McpServer) {
    const isPlugin = pluginNames.has(server.name.toLowerCase());
    const entry: ServerEntry = {
      id: "",
      name: server.name,
      transport: server.transport,
      command: server.command,
      args: server.args,
      env: server.envKeys.map((key) => ({ key, value: null, secret: true })),
      url: server.url,
      source: `imported:${client.id}${isPlugin ? "-plugin" : ""}`,
    };
    const next = await addServer(entry);
    onRegistryChange(next);
    return next;
  }

  async function handleImportAll() {
    setBulkImportServers(toImport);
  }

  async function confirmImportAll(selected: string[]) {
    const servers = bulkImportServers ?? [];
    setBusy(true);
    let ok = 0;
    const failed: string[] = [];
    const succeeded = new Set<string>();
    for (const key of selected) {
      const s = servers[Number(key)];
      if (!s) continue;
      try {
        await importOne(s);
        ok += 1;
        succeeded.add(key);
      } catch {
        failed.push(s.name);
      }
    }
    setBusy(false);
    if (failed.length === 0) {
      toast.success(
        t(
          ok === 1
            ? "Imported {value1} server into Toolport"
            : "Imported {value1} servers into Toolport",
          { value1: ok },
        ),
      );
      setBulkImportServers(null);
    } else {
      // Some imports failed. Drop ONLY the rows that succeeded so a re-confirm can't
      // re-import them; the failures (and any rows the user didn't select this time)
      // stay in the dialog to retry or import next.
      if (ok > 0) {
        toast.warning(
          t("Imported {value1}, couldn't import {value2}", {
            value1: ok,
            value2: failed.join(", "),
          }),
        );
      } else {
        toastError(t("Couldn't import {value1}", { value1: failed.join(", ") }));
      }
      setBulkImportServers(servers.filter((_, i) => !succeeded.has(String(i))));
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
            ? t(
                restored === 1
                  ? "Disconnected Toolport from {value1} and put back {value2} server"
                  : "Disconnected Toolport from {value1} and put back {value2} servers",
                { value1: client.name, value2: restored },
              )
            : t("Disconnected Toolport from {value1}", { value1: client.name }),
          { description: clientRestartHintAfterRemoval(client.name) },
        );
        noteRestartNeeded("removed");
      } else {
        const outcome = await installGateway(
          client.id,
          profile || undefined,
          false,
          transport,
        );
        // Restart is the load-bearing line (SOU-317): MCP clients typically do not
        // pick up a new gateway entry until relaunch. Scope/backup are secondary.
        toast.success(t("Connected Toolport to {value1}", { value1: client.name }), {
          description: connectSuccessDescription(client.name, [
            transport === "sharedHttp"
              ? t("Uses the shared HTTP bridge (one gateway process).")
              : null,
            profile
              ? t('Scoped to the "{value1}" profile.', {
                  value1: profiles.find((p) => p.id === profile)?.name ?? profile,
                })
              : null,
            !profile && outcome.backup ? t("Previous config backed up.") : null,
          ]),
        });
        noteRestartNeeded("applied");
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
              <Trans
                message={"{value1}custom configuration"}
                values={{ value1: <TriangleAlert className="size-3" /> }}
              />
            </Badge>
          ) : installed ? (
            <Badge variant="success" className="mb-1">
              <Trans
                message={"{value1}connected"}
                values={{ value1: <Link2 className="size-3" /> }}
              />
            </Badge>
          ) : present ? (
            <Badge variant="info" className="mb-1">
              <Trans
                message={"{value1}ready to connect"}
                values={{ value1: <Monitor className="size-3" /> }}
              />
            </Badge>
          ) : (
            <Badge variant="outline" className="mb-1 text-muted-foreground">
              {t("not installed ")}
            </Badge>
          )}
          <p className="truncate font-mono text-xs text-muted-foreground">
            {client.configExists
              ? client.configPath
              : present
                ? t("installed - no MCP config yet")
                : t("not installed on this machine")}
          </p>
          {customized && (
            <p className="mt-1 text-xs text-muted-foreground">
              {t(
                "Toolport is leaving your hand-edited gateway entry as-is. Reset it only if you want Toolport to manage the entry. ",
              )}
            </p>
          )}
          {installed && !customized && (
            <p className="mt-1 text-xs text-muted-foreground">
              <Trans
                message={
                  scopeServerCount(currentScope) === 1
                    ? "Sees{value1}{value2}{value3}\n              · {value4} server{value6}"
                    : "Sees{value1}{value2}{value3}\n              · {value4} servers{value6}"
                }
                values={{
                  value1: " ",
                  value2: (
                    <span className="font-medium text-foreground">
                      {currentScope
                        ? t('the "{value1}" profile', { value1: currentScope })
                        : t("the active profile")}
                    </span>
                  ),
                  value3: " ",
                  value4: scopeServerCount(currentScope),
                  value6: managedTransport === "sharedHttp" ? t(" · shared HTTP") : "",
                }}
              />
            </p>
          )}
          {!present && !installed && (
            <p className="mt-1 text-xs text-muted-foreground">
              <Trans
                message={
                  "Install {value1} on this machine before connecting it to Toolport."
                }
                values={{ value1: client.name }}
              />
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!installed && present && (
            <Select
              value={transport}
              onValueChange={(v) =>
                setTransport(v === "sharedHttp" ? "sharedHttp" : "stdio")
              }
            >
              <SelectTrigger size="sm" className="w-44">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="stdio">{t("Spawn (stdio)")}</SelectItem>
                <SelectItem value="sharedHttp">{t("Shared HTTP")}</SelectItem>
              </SelectContent>
            </Select>
          )}
          {profiles.length > 1 && !customized && (
            <Select
              value={profile || "__all__"}
              onValueChange={(v) => setProfile(v === "__all__" ? "" : v)}
            >
              <SelectTrigger size="sm" className="w-52">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__all__">{t("Follow active profile")}</SelectItem>
                {profiles.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    <Trans message={"Only: {value1}"} values={{ value1: p.name }} />
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {installed && !customized && profile !== currentScope && (
            <Button size="sm" onClick={applyScope} disabled={busy}>
              <Trans
                message={"{value1}Apply scope"}
                values={{ value1: <Check className="size-4" /> }}
              />
            </Button>
          )}
          {customized && (
            <ConfirmDialog
              open={resetOpen}
              onOpenChange={setResetOpen}
              trigger={
                <Button size="sm" variant="default" disabled={busy}>
                  <Trans
                    message={"{value1}Reset to default"}
                    values={{ value1: <Check className="size-4" /> }}
                  />
                </Button>
              }
              title={t("Reset {value1} to the default Toolport gateway?", {
                value1: client.name,
              })}
              description={t(
                "This overwrites the hand-edited toolport entry with the standard stdio gateway command. Your other MCP servers are left alone.",
              )}
              confirmLabel={t("Reset to default")}
              onConfirm={resetToDefault}
            />
          )}
          {installed ? (
            <ConfirmDialog
              trigger={
                <Button size="sm" variant="outline" disabled={busy}>
                  <Trans
                    message={"{value1}Disconnect"}
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
              onClick={toggleInstall}
              disabled={busy || !present}
            >
              <Trans
                message={"{value1}Connect to Toolport"}
                values={{ value1: <PlugZap className="size-4" /> }}
              />
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
            <p className="font-medium">
              {t("Couldn't read this client's configuration")}
            </p>
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
            <p className="font-medium">{t(showRestartNotice.title)}</p>
            <p className="mt-0.5 break-words text-xs">
              {showRestartNotice.title === "Not live yet"
                ? clientRestartHint(client.name)
                : clientRestartHintAfterRemoval(client.name)}
            </p>
          </div>
          <button
            type="button"
            onClick={() => setRestartNotice(null)}
            aria-label={t("Dismiss restart reminder")}
            className="rounded p-0.5 text-info/70 transition-colors hover:bg-info/10 hover:text-info focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-info"
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      {installed && !customized && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
          <div className="min-w-0">
            <div className="text-xs font-medium text-foreground">
              <Trans
                message={"Discovery mode {value1}"}
                values={{
                  value1: !clientMode && (
                    <span className="ml-1.5 font-normal text-muted-foreground">
                      <Trans
                        message={"inheriting {value1}"}
                        values={{ value1: globalMode }}
                      />
                    </span>
                  ),
                }}
              />
            </div>
            <p className="mt-0.5 text-2xs text-muted-foreground">
              {t(DISCOVERY_HINT[effectiveMode] ?? DISCOVERY_HINT.lazy)}
            </p>
            {effectiveMode === recommended.mode ? (
              <p className="mt-0.5 text-2xs text-success">
                <Trans
                  message={"Recommended ({value1}), {value2}."}
                  values={{ value1: recommended.mode, value2: t(recommended.why) }}
                />
              </p>
            ) : (
              <p className="mt-0.5 text-2xs text-muted-foreground/80">
                <Trans
                  message={"Recommended:{value1}{value2},{value3}{value4}."}
                  values={{
                    value1: " ",
                    value2: (
                      <span className="font-medium text-foreground">
                        {recommended.mode}
                      </span>
                    ),
                    value3: " ",
                    value4: t(recommended.why),
                  }}
                />
              </p>
            )}
          </div>
          <Select
            value={clientMode || "__inherit__"}
            onValueChange={(v) => applyDiscovery(v === "__inherit__" ? "" : v)}
          >
            <SelectTrigger size="sm" className="w-44 shrink-0" disabled={busy}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__inherit__">
                <Trans message={"Inherit ({value1})"} values={{ value1: globalMode }} />
              </SelectItem>
              <SelectItem value="lazy">{t("Lazy · fewest tokens")}</SelectItem>
              <SelectItem value="grouped">{t("Grouped · per-server")}</SelectItem>
              <SelectItem value="full">{t("Full · every tool")}</SelectItem>
            </SelectContent>
          </Select>
        </div>
      )}

      {installed && !customized ? (
        <div>
          <div className="mb-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
            {t("Servers it can reach ")}
          </div>
          {scopeServers(currentScope).length === 0 ? (
            <p className="text-xs text-muted-foreground">
              {t("No enabled servers in this scope yet. Enable some under All servers. ")}
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
            <Trans
              message={
                "Connect {value1} once and it reaches your{value2}{value3}{value4}through one gateway, no re-wiring per project. Your keys stay in your keychain."
              }
              values={{
                value1: client.name,
                value2: " ",
                value3: (
                  <span className="font-medium text-foreground">
                    <Trans
                      message={
                        scopeServerCount(profile) === 1
                          ? "{value1} managed server"
                          : "{value1} managed servers"
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
            message={
              "Connect {value1}, then enable servers under{value2}{value3} and they'll all route through Toolport, no per-client setup."
            }
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
                  message={"{value1} manages servers as connectors"}
                  values={{ value1: client.name }}
                />
              </p>
              <p className="mt-1 text-muted-foreground">
                <Trans
                  message={
                    "Those live in {value1}'s Customize → Connectors and sync to your account, outside the local config files Toolport reads. Connecting Toolport adds a local gateway entry so your Toolport-managed servers appear in {value2} too."
                  }
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
            <span className="text-2xs font-semibold tracking-[0.09em] text-muted-foreground uppercase">
              {t("Import into Toolport ")}
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
                  <Trans
                    message={"{value1}Import all ({value2})"}
                    values={{
                      value1: <Download className="size-3" />,
                      value2: toImport.length,
                    }}
                  />
                </Button>
              )}
              {movable.length > 0 && (
                <Button
                  size="sm"
                  variant="default"
                  className="h-7 px-2 text-xs"
                  onClick={() => setMigrateOpen(true)}
                  disabled={busy}
                >
                  <Trans
                    message={"{value1}Move into gateway ({value2})"}
                    values={{
                      value1: <Shuffle className="size-3" />,
                      value2: movable.length,
                    }}
                  />
                </Button>
              )}
            </div>
          </div>
          <details className="mb-2">
            <summary className="cursor-pointer text-xs font-medium text-muted-foreground/80 hover:text-foreground">
              {t("How importing works ")}
            </summary>
            <ul className="mt-1.5 mb-1 space-y-0.5 text-xs text-muted-foreground">
              <li>
                <Trans
                  message={
                    "{value1} copies a server into Toolport; {value2} keeps its own copy."
                  }
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
                    message={
                      "{value1}{value2}copies it, then removes it from {value3}'s config so the gateway is the only source (plugin servers stay). The cutover that actually saves context."
                    }
                    values={{
                      value1: (
                        <span className="font-medium text-foreground">
                          {t("Move into gateway")}
                        </span>
                      ),
                      value2: " ",
                      value3: client.name,
                    }}
                  />
                </li>
              )}
            </ul>
          </details>
          {installed && toImport.length > 0 && movable.length > 0 && (
            <p className="mb-3 -mt-1 inline-flex items-start gap-1.5 rounded-md bg-warning/10 px-2 py-1 text-xs text-warning">
              <TriangleAlert className="mt-0.5 size-3.5 shrink-0" />
              <span>
                <Trans
                  message={
                    "{value1} is already connected to Toolport. Import on its own leaves a copy here too, so these tools load twice, once directly and once through the gateway. Use {value2} to avoid that."
                  }
                  values={{
                    value1: client.name,
                    value2: <span className="font-medium">{t("Move into gateway")}</span>,
                  }}
                />
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
                onImport={() => setBulkImportServers([server])}
              />
            ))}
          </div>

          {toImport.length === 0 && (
            <p className="mt-3 inline-flex items-center gap-1.5 text-xs text-success">
              <Trans
                message={
                  "{value1}Everything here is already in Toolport. Manage it under{value2}{value3}"
                }
                values={{
                  value1: <Check className="size-3.5" />,
                  value2: " ",
                  value3: (
                    <span className="inline-flex items-center gap-0.5 font-medium">
                      <Trans
                        message={"All servers {value1}"}
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

      <Dialog open={migrateOpen} onOpenChange={setMigrateOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>
              <Trans
                message={"Move {value1} onto Toolport"}
                values={{ value1: client.name }}
              />
            </DialogTitle>
          </DialogHeader>
          <div className="flex flex-col gap-3 py-1 text-sm">
            <p className="text-muted-foreground">
              <Trans
                message={
                  "This imports{value1}{value2}{value3}from {value4} into Toolport and turns them on, then rewrites{value5}{value6}'s config so it uses{value7}{value8}. The original config is backed up first, and Disconnect puts these servers back."
                }
                values={{
                  value1: " ",
                  value2: (
                    <span className="font-medium text-foreground">
                      <Trans
                        message={
                          movable.length === 1 ? "{value1} server" : "{value1} servers"
                        }
                        values={{ value1: movable.length }}
                      />
                    </span>
                  ),
                  value3: " ",
                  value4: client.name,
                  value5: " ",
                  value6: client.name,
                  value7: " ",
                  value8: (
                    <span className="font-medium text-foreground">
                      {t("only the Toolport gateway ")}
                    </span>
                  ),
                }}
              />
            </p>
            <p className="rounded-md bg-warning/10 p-2 text-xs text-warning">
              {t(
                "Secret values (API keys, tokens) aren't carried over, they stay only in the backed-up config. After migrating, re-enter them under each server's secrets so the gateway can connect. ",
              )}
            </p>
            {customized && (
              <p className="rounded-md bg-warning/10 p-2 text-xs text-warning">
                {t(
                  "This client has a custom Toolport entry. Migrating replaces it with the default gateway command. ",
                )}
              </p>
            )}
            <div className="rounded-md bg-muted/40 p-2 font-mono text-xs text-muted-foreground">
              {movable.map((s) => s.name).join(", ")}
            </div>
            {client.pluginServers.length > 0 && (
              <p className="text-xs text-muted-foreground">
                <Trans
                  message={
                    client.pluginServers.length === 1
                      ? "Note: {value1} server managed by {value3}'s plugins or extensions can't be moved, only {value4} controls those. They stay where they are (you can still import a copy above)."
                      : "Note: {value1} servers managed by {value3}'s plugins or extensions can't be moved, only {value4} controls those. They stay where they are (you can still import a copy above)."
                  }
                  values={{
                    value1: client.pluginServers.length,
                    value3: client.name,
                    value4: client.name,
                  }}
                />
              </p>
            )}
            {profiles.length > 1 && (
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs text-muted-foreground">
                  {t("Scope this client to ")}
                </span>
                <Select
                  value={profile || "__all__"}
                  onValueChange={(v) => setProfile(v === "__all__" ? "" : v)}
                >
                  <SelectTrigger size="sm" className="w-52">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">{t("Follow active profile")}</SelectItem>
                    {profiles.map((p) => (
                      <SelectItem key={p.id} value={p.id}>
                        <Trans message={"Only: {value1}"} values={{ value1: p.name }} />
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setMigrateOpen(false)}
              disabled={busy}
            >
              {t("Cancel ")}
            </Button>
            <Button onClick={migrate} disabled={busy}>
              <Trans
                message={"{value1}Move {value2} into Toolport"}
                values={{
                  value1: <Shuffle className="size-4" />,
                  value2: movable.length,
                }}
              />
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ImportReviewDialog
        open={bulkImportPreview !== null}
        items={bulkImportPreview ?? []}
        busy={busy}
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
          <Trans
            message={"Toolport {value1}"}
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
            className="rounded-md border border-border/60 bg-card px-2 py-1 font-mono text-2xs text-muted-foreground"
          >
            {s}
          </span>
        ))}
        {extra > 0 && (
          <span className="px-2 font-mono text-2xs text-muted-foreground/60">
            <Trans message={"+{value1} more"} values={{ value1: extra }} />
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
  const { t } = useI18n();
  return (
    <Card aria-disabled={imported} className={`gap-0 ${imported ? "opacity-70" : ""}`}>
      <CardContent className="flex flex-col gap-2 p-3">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-1.5">
            <span className="truncate text-sm font-medium">{server.name}</span>
            {isPlugin && (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                {t("plugin ")}
              </span>
            )}
          </div>
          <TransportPill transport={server.transport} />
        </div>
        <code className="truncate font-mono text-xs text-muted-foreground">
          {server.command
            ? [server.command, ...server.args].join(" ")
            : (server.url ?? "")}
        </code>
        <div className="flex justify-end">
          {imported ? (
            <span className="inline-flex items-center gap-1 text-xs text-success">
              <Trans
                message={"{value1}in Toolport"}
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
              <Trans
                message={"{value1}Import"}
                values={{ value1: <Download className="size-3" /> }}
              />
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
