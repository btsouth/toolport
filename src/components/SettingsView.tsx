import { getLocale, translate } from "@/lib/i18n";
import { LANGUAGES, resolveLocale, type LanguageChoice, useI18n } from "@/lib/i18n";
import { type SettingsSubpage, SETTINGS_PAGES } from "@/lib/settingsPages";
import { toast } from "sonner";
import { useCallback, useEffect, useMemo, useState, useRef, type ReactNode } from "react";
import {
  Activity,
  Braces,
  Check,
  ChevronRight,
  Copy,
  Eye,
  EyeOff,
  FolderOpen,
  Globe,
  Layers,
  Monitor,
  Moon,
  Pin,
  Power,
  RefreshCw,
  ShieldX,
  Sun,
  Trash2,
  UserCheck,
  X,
} from "lucide-react";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { toastError } from "@/lib/toast";
import { QuarantineDefinition } from "@/components/QuarantineDefinition";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorCallout } from "@/components/ui/status-card";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { ServerLogo } from "@/components/ServerLogo";
import {
  gatherDiagnostics,
  openDataDir,
  addHttpClient,
  disconnectAllClients,
  dataRemovalPlan,
  removeToolportData,
  getRegistry,
  getBaselineNotice,
  getUiLanguageSuggestion,
  type DisconnectResult,
  disableAutostart,
  enableAutostart,
  isAutostartEnabled,
  clearInspectLog,
  httpBridgeStatus,
  listAllowedTools,
  listQuarantined,
  listServerTools,
  setProfileServerTools,
  releaseQuarantine,
  revokeAllowedTool,
  removeHttpClient,
  setCodeMode,
  setSafetyLevel,
  setLazyDiscovery,
  setFolderProfiles,
  setDefaultAccess,
  setAccessServer,
  createProfile,
  deleteProfile,
  setLiveInspect,
  setPiiRedaction,
  setToolPinned,
  startHttpBridge,
  stopHttpBridge,
  stopStaleGateways,
  clientsNeedingRestart,
  type ClientNeedingRestart,
  type HttpBridgeStatus,
  type QuarantinedTool,
  type ReapOutcome,
} from "@/lib/api";
import type { AllowedTool, FolderProfile, Profile, Registry } from "@/lib/types";
import { isGatewayServer } from "@/lib/types";
import { keptSafetySummary } from "@/lib/keptSafety";
import { useTheme, type Theme } from "@/lib/theme";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** The set of tools pinned as lazy-discovery prerequisites, with one-click unpin.
 * Pinning happens contextually (a tool's card in the server’s Tools tab); this is where you see
 * and manage the whole set. Reads `registry.pinnedTools` (server id -> tool names). */
function PinnedPrerequisites({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = useState<string | null>(null);
  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const s of registry?.servers ?? []) m.set(s.id, s.name);
    return m;
  }, [registry?.servers]);
  const pins = useMemo(() => {
    const out: { serverId: string; server: string; tool: string }[] = [];
    for (const [serverId, tools] of Object.entries(registry?.pinnedTools ?? {})) {
      for (const tool of tools) {
        out.push({ serverId, server: nameOf.get(serverId) ?? serverId, tool });
      }
    }
    return out.sort((a, b) =>
      `${a.server}${a.tool}`.localeCompare(`${b.server}${b.tool}`),
    );
  }, [registry?.pinnedTools, nameOf]);

  async function unpin(serverId: string, tool: string) {
    setBusy(`${serverId}:${tool}`);
    try {
      onRegistryChange(await setToolPinned(serverId, tool, false));
    } catch (e) {
      toastError(translate("Couldn't unpin the tool: {value1}", { value1: String(e) }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="rounded-lg border border-border/60 bg-muted/20 p-3">
      <div className="flex items-center gap-2 text-meta">
        <Pin className="size-3.5 shrink-0 text-info" />
        <span className="font-medium">{translate("Tools always included")}</span>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-muted-foreground">
          {pins.length}
        </span>
        <span className="ml-auto text-muted-foreground">
          {translate("included in every tool search")}
        </span>
      </div>
      {pins.length === 0 ? (
        <EmptyState
          title={translate("Nothing here yet")}
          framed={false}
          description={translate(
            "None yet. In Servers, open a server's Tools tab and pin tools your agent needs every time, such as sign-in or a required lookup. They will be included even when they do not match the search.",
          )}
        />
      ) : (
        <ul className="mt-2 space-y-1">
          {pins.map((p) => (
            <li
              key={`${p.serverId}:${p.tool}`}
              className="flex items-center gap-2 text-meta"
            >
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">
                {p.tool}
              </code>
              <span className="text-muted-foreground">{p.server}</span>
              <button
                onClick={() => unpin(p.serverId, p.tool)}
                disabled={busy === `${p.serverId}:${p.tool}`}
                aria-label={t("Unpin {value1}", { value1: p.tool })}
                className="ml-auto rounded p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border disabled:opacity-50"
              >
                <X className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Folder -> profile auto-routing (SOU-188): map a project folder to a profile so a client
 * opened in that folder auto-scopes to it, no manual profile switch. Reads/writes
 * `registry.folderProfiles`; the longest matching path wins at match time. stdio clients. */
function FolderRouting({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const { t } = useI18n();
  const mappings = registry?.folderProfiles ?? [];
  const profiles = registry?.profiles ?? [];
  const [path, setPath] = useState("");
  const [profile, setProfile] = useState("");
  const [busy, setBusy] = useState(false);

  async function save(next: FolderProfile[]) {
    setBusy(true);
    try {
      onRegistryChange(await setFolderProfiles(next));
    } catch (e) {
      toastError(
        translate("Couldn't save folder routing: {value1}", { value1: String(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  async function browse() {
    try {
      const picked = await openFolderDialog({
        directory: true,
        multiple: false,
        title: translate("Choose a project folder"),
      });
      if (typeof picked === "string") setPath(picked);
    } catch (e) {
      toastError(
        translate("Couldn't open the folder picker: {value1}", { value1: String(e) }),
      );
    }
  }

  async function add() {
    const p = path.trim();
    if (!p || !profile) return;
    // Replace any existing mapping for the same path rather than duplicating it.
    const next = [...mappings.filter((m) => m.path.trim() !== p), { path: p, profile }];
    await save(next);
    setPath("");
    setProfile("");
  }

  // A mapping's profile is stored as an id or name; show the profile's display name.
  const label = (ref: string) =>
    profiles.find((p) => p.id === ref || p.name === ref)?.name ?? ref;

  return (
    <section className="mt-2 flex flex-col gap-3">
      <div>
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {translate("Project folder routing")}
        </h2>
        <p className="text-meta text-muted-foreground">
          {translate(
            "Automatically use the matching access set when an MCP client reports a project root. The longest matching folder wins.",
          )}
        </p>
      </div>
      <div className="toolport-list">
        {profiles.length === 0 ? (
          <EmptyState
            framed={false}
            title={translate(
              "Create an access set before adding project folder routing.",
            )}
          />
        ) : (
          <>
            {mappings.length === 0 && (
              <EmptyState
                framed={false}
                title={translate("No project folders are mapped yet.")}
              />
            )}
            {mappings.map((m) => (
              <div
                key={m.path}
                className="flex items-center gap-3 border-b border-border px-4 py-3"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-sm font-semibold" title={m.path}>
                    {m.path}
                  </p>
                  <p className="text-meta text-muted-foreground">
                    {translate("Uses {profile_name}", { profile_name: label(m.profile) })}
                  </p>
                </div>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => save(mappings.filter((x) => x.path !== m.path))}
                  disabled={busy}
                  aria-label={t("Remove routing for {value1}", { value1: m.path })}
                  title={translate("Remove folder mapping")}
                >
                  <Trash2 />
                </Button>
              </div>
            ))}
            <div className="flex flex-wrap items-center gap-2 px-4 py-3">
              <input
                value={path}
                onChange={(e) => setPath(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void add();
                }}
                placeholder={translate("/path/to/project")}
                className="h-8 min-w-48 flex-1 rounded-md border border-input bg-transparent px-2 font-mono text-meta focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none"
              />
              <Button
                variant="outline"
                onClick={() => void browse()}
                disabled={busy}
                aria-label={t("Browse for a project folder")}
                title={t("Browse…")}
              >
                <FolderOpen /> {t("Browse")}
              </Button>
              <Select value={profile} onValueChange={setProfile}>
                <SelectTrigger className="w-40">
                  <SelectValue placeholder={translate("Access set")} />
                </SelectTrigger>
                <SelectContent>
                  {profiles.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Button
                variant="outline"
                onClick={() => void add()}
                disabled={busy || !path.trim() || !profile}
              >
                {translate("Add folder mapping")}
              </Button>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

function AccessSetCreator({
  onRegistryChange,
}: {
  onRegistryChange: (r: Registry) => void;
}) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <form
      className="flex gap-2"
      onSubmit={async (event) => {
        event.preventDefault();
        setBusy(true);
        try {
          onRegistryChange(await createProfile(name));
          setName("");
        } catch (e) {
          toastError(`${e}`);
        } finally {
          setBusy(false);
        }
      }}
    >
      <input
        aria-label={translate("Access set name")}
        placeholder={translate("Access set name")}
        value={name}
        onChange={(e) => setName(e.target.value)}
        className="h-8 w-56 min-w-0 rounded-md border border-input bg-transparent px-2 text-sm focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none"
      />
      <Button variant="outline" disabled={busy || !name.trim()} type="submit">
        {translate("Create access set")}
      </Button>
    </form>
  );
}

interface Props {
  page?: SettingsSubpage;
  pendingCount?: number;
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}

type SettingKey =
  "autostart" | "lazy-discovery" | "code-mode" | "pii-redaction" | "live-inspect";

type RegistrySettingKey = Exclude<SettingKey, "autostart">;

const REGISTRY_FIELD_BY_SETTING = {
  "lazy-discovery": "lazyDiscovery",
  "code-mode": "codeMode",
  "pii-redaction": "piiRedaction",
  "live-inspect": "liveInspect",
} as const satisfies Record<RegistrySettingKey, keyof Registry>;

/** Tool-granular scope for one profile (SOU-189): per enabled server, open an editor to pick exactly
 * which tools the profile exposes. All-checked = the whole server (no narrowing); unchecking
 * writes an allow-list that tools/list, search, and the call guard all honor. Tools load
 * lazily when the editor opens. stdio clients (the per-profile router); the HTTP bridge is a follow-up. */
function ProfileToolScope({
  profile,
  registry,
  onRegistryChange,
}: {
  profile: Profile;
  registry: Registry;
  onRegistryChange: (r: Registry) => void;
}) {
  const { t } = useI18n();
  const serverName = useMemo(() => {
    const m = new Map<string, string>();
    // Toolport's own gateway is infrastructure, not a scopable server, so keep it out.
    for (const s of registry.servers) if (!isGatewayServer(s)) m.set(s.id, s.name);
    return m;
  }, [registry.servers]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [toolsByServer, setToolsByServer] = useState<Record<string, string[]>>({});
  const [errorByServer, setErrorByServer] = useState<Record<string, boolean>>({});
  const [loadingByServer, setLoadingByServer] = useState<Record<string, boolean>>({});
  const requestIdByServer = useRef<Record<string, number>>({});
  const [busy, setBusy] = useState(false);
  const scope = profile.toolScope ?? {};

  async function loadTools(serverId: string) {
    const requestId = (requestIdByServer.current[serverId] ?? 0) + 1;
    requestIdByServer.current[serverId] = requestId;
    setLoadingByServer((m) => ({
      ...m,
      [serverId]: true,
    }));
    try {
      const tools = await listServerTools(serverId);
      if (requestIdByServer.current[serverId] !== requestId) {
        return;
      }
      setToolsByServer((m) => ({ ...m, [serverId]: tools.map((t) => t.name) }));
      setErrorByServer((m) => {
        const next = { ...m };
        delete next[serverId];
        return next;
      });
    } catch (e) {
      // Ignore stale failures (a newer load for this server is in flight or done).
      if (requestIdByServer.current[serverId] !== requestId) {
        return;
      }
      toastError(
        translate("Couldn't load {value1} tools: {value2}", {
          value1: serverName.get(serverId) ?? serverId,
          value2: String(e),
        }),
      );
      setErrorByServer((m) => ({ ...m, [serverId]: true }));
    } finally {
      if (requestIdByServer.current[serverId] === requestId) {
        setLoadingByServer((m) => ({
          ...m,
          [serverId]: false,
        }));
      }
    }
  }

  async function expand(serverId: string) {
    if (expanded === serverId) {
      setExpanded(null);
      return;
    }
    setExpanded(serverId);
    if (!toolsByServer[serverId]) {
      await loadTools(serverId);
    }
  }

  async function toggleTool(serverId: string, tool: string, allTools: string[]) {
    // Current in-scope set is the allow-list if present, else every tool.
    const current = new Set(scope[serverId] ?? allTools);
    if (current.has(tool)) current.delete(tool);
    else current.add(tool);
    // Every real tool selected -> clear the scope (whole server). Otherwise persist the
    // checked subset (which may be empty = expose no tools). `.every` (not a size compare)
    // so a stale entry from a since-removed tool can't fake an "all selected" state.
    const next = allTools.every((t) => current.has(t))
      ? null
      : allTools.filter((t) => current.has(t));
    setBusy(true);
    try {
      onRegistryChange(await setProfileServerTools(profile.id, serverId, next));
    } catch (e) {
      toastError(
        translate("Couldn't update tool scope: {value1}", { value1: String(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  async function include(serverId: string, included: boolean) {
    try {
      onRegistryChange(await setAccessServer(profile.id, serverId, included));
    } catch (e) {
      toastError(`${e}`);
    }
  }

  return (
    <>
      {registry.servers
        .filter((server) => !isGatewayServer(server))
        .map((server) => {
          const serverId = server.id;
          const included = profile.enabledServerIds.includes(serverId);
          const allTools = toolsByServer[serverId];
          const scoped = scope[serverId];
          const badge = scoped
            ? allTools
              ? t("{value1} of {value2} tools", {
                  value1: scoped.length,
                  value2: allTools.length,
                })
              : scoped.length === 1
                ? t("1 tool")
                : t("{value1} tools", { value1: scoped.length })
            : t("all tools");
          const open = expanded === serverId;
          return (
            <div
              key={serverId}
              className="flex items-center gap-3 border-b border-border px-4 py-2.5 last:border-b-0"
            >
              <ServerLogo name={server.name} transport={server.transport} />
              <span className="min-w-0 truncate text-sm" title={server.name}>
                {server.name}
              </span>
              {!server.enabled && <Badge variant="off">{translate("Off")}</Badge>}
              <span className="flex-1" />
              {included && (
                <Dialog
                  open={open}
                  onOpenChange={(value) => {
                    if (value) void expand(serverId);
                    else setExpanded(null);
                  }}
                >
                  <DialogTrigger asChild>
                    <button
                      type="button"
                      aria-haspopup="dialog"
                      aria-label={translate("Choose tools for {value1}", {
                        value1: String(serverName.get(serverId)),
                      })}
                      title={translate("Choose tools for {value1}", {
                        value1: String(serverName.get(serverId)),
                      })}
                      className={cn(
                        "flex h-7 shrink-0 items-center gap-1 rounded-md px-2 text-meta font-medium hover:bg-muted focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                        scoped ? "text-primary" : "text-muted-foreground",
                      )}
                    >
                      {badge}
                      <ChevronRight className="size-3.5" />
                    </button>
                  </DialogTrigger>
                  <DialogContent>
                    <DialogHeader>
                      <DialogTitle>
                        {t("Choose tools for {server}", {
                          server: serverName.get(serverId) ?? serverId,
                        })}
                      </DialogTitle>
                      <DialogDescription>
                        {translate("Select which tools this access set exposes.")}
                      </DialogDescription>
                    </DialogHeader>
                    <div className="max-h-[60vh] overflow-y-auto">
                      {loadingByServer[serverId] ? (
                        <SkeletonRows label={translate("Loading tools…")} />
                      ) : errorByServer[serverId] ? (
                        <ErrorCallout
                          title={translate("Couldn't load tools")}
                          onRetry={() => loadTools(serverId)}
                        />
                      ) : allTools && allTools.length > 0 ? (
                        <div className="flex flex-col gap-1">
                          {allTools.map((tool) => (
                            <label
                              key={tool}
                              className="flex items-center gap-2 text-meta"
                            >
                              <input
                                type="checkbox"
                                checked={scoped ? scoped.includes(tool) : true}
                                disabled={busy}
                                onChange={() => toggleTool(serverId, tool, allTools)}
                                className="size-3.5"
                              />
                              <code className="font-mono text-foreground">{tool}</code>
                            </label>
                          ))}
                        </div>
                      ) : (
                        <p className="text-meta text-muted-foreground">
                          {translate("This server exposes no tools.")}
                        </p>
                      )}
                    </div>
                  </DialogContent>
                </Dialog>
              )}
              <Switch
                checked={included}
                onCheckedChange={(value) => void include(serverId, value)}
                aria-label={server.name}
              />
            </div>
          );
        })}
    </>
  );
}

/** Anonymous discovery defaults and global security policy. Identified clients
 * choose discovery in Clients. */
export function SettingsView({
  registry,
  onRegistryChange,
  page = "general",
  pendingCount = 0,
}: Props) {
  const {
    t,
    language,
    setLanguage,
    loading: languageLoading,
    error: languageError,
    saveError,
  } = useI18n();
  const [languageSuggestionPending, setLanguageSuggestionPending] = useState(false);
  const [languageSaving, setLanguageSaving] = useState(false);
  const languageSuggestionRequest = useRef(0);
  const suggestedLocale = resolveLocale(
    navigator.languages?.length ? navigator.languages : [navigator.language],
  );
  const suggestedLanguage = LANGUAGES.find(({ code }) => code === suggestedLocale)!;
  useEffect(() => {
    const request = ++languageSuggestionRequest.current;
    let active = true;
    void getUiLanguageSuggestion().then(
      (pending) => {
        if (active && request === languageSuggestionRequest.current)
          setLanguageSuggestionPending(pending);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, []);
  const saveLanguage = async (choice: LanguageChoice) => {
    const request = ++languageSuggestionRequest.current;
    setLanguageSaving(true);
    try {
      await setLanguage(choice);
      // setLanguage reports save errors through the store, so consult the
      // authoritative flag before removing the offer, including same-choice saves.
      const pending = await getUiLanguageSuggestion();
      if (request === languageSuggestionRequest.current)
        setLanguageSuggestionPending(pending);
    } catch {
      // Retain the offer when persistence or the status request fails.
    } finally {
      if (request === languageSuggestionRequest.current) setLanguageSaving(false);
    }
  };
  const [purgePlan, setPurgePlan] = useState<import("@/lib/api").DataRemovalPlan | null>(
    null,
  );
  const [purgeBusy, setPurgeBusy] = useState(false);
  const preparePurge = async () => {
    try {
      setPurgePlan(await dataRemovalPlan());
    } catch (error) {
      toastError(
        translate("Could not prepare data removal: {value1}", { value1: String(error) }),
      );
    }
  };
  const purge = async () => {
    if (!purgePlan) return;
    setPurgeBusy(true);
    try {
      await removeToolportData(purgePlan.reportPath);
    } catch (error) {
      toastError(
        translate("Could not start data removal: {value1}", { value1: String(error) }),
      );
      setPurgeBusy(false);
    }
  };
  const { theme, setTheme } = useTheme();
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removeBusy, setRemoveBusy] = useState(false);
  const [removeResults, setRemoveResults] = useState<DisconnectResult[] | null>(null);
  const removeAll = async () => {
    setRemoveBusy(true);
    try {
      setRemoveResults(await disconnectAllClients());
      setRemoveOpen(false);
      try {
        onRegistryChange(await getRegistry());
      } catch (error) {
        toastError(
          translate("Client removal finished, but could not refresh settings: {value1}", {
            value1: String(error),
          }),
        );
      }
    } catch (error) {
      toastError(
        translate("Could not remove client connections: {value1}", {
          value1: String(error),
        }),
      );
    } finally {
      setRemoveBusy(false);
    }
  };
  const lazyDiscovery = registry?.lazyDiscovery ?? true;
  // Match the registry default when the field is absent or still loading.
  const codeMode = registry?.codeMode ?? false;
  const piiRedaction = registry?.piiRedaction ?? false;
  const liveInspect = registry?.liveInspect ?? false;
  const safetyLevels = ["off", "ask", "strict"] as const;
  const teamFloor =
    safetyLevels[
      Math.max(
        safetyLevels.indexOf(registry?.teamMinSafetyLevel ?? "off"),
        registry?.teamForcedHumanApproval ? 1 : 0,
      )
    ];
  const memberLevel =
    registry?.safetyLevel ??
    (registry && registry.version >= 2
      ? "ask"
      : registry?.humanApproval || registry?.confirmDestructive
        ? "ask"
        : "off");
  const effectiveLevel =
    safetyLevels[
      Math.max(safetyLevels.indexOf(memberLevel), safetyLevels.indexOf(teamFloor))
    ];
  const [safetyBusy, setSafetyBusy] = useState(false);
  const [busySettings, setBusySettings] = useState<ReadonlySet<SettingKey>>(
    () => new Set(),
  );
  const latestRegistry = useRef(registry);
  useEffect(() => {
    latestRegistry.current = registry;
  }, [registry]);
  const keptSummary = keptSafetySummary(registry?.keptV1Safety);
  const levelLabel = { off: t("Off"), ask: t("Ask"), strict: t("Strict") } as const;
  // Picking a level, even the current one, also drops protections kept from 1.x.
  async function chooseSafety(level: "off" | "ask" | "strict") {
    setSafetyBusy(true);
    try {
      const updated = await setSafetyLevel(level);
      const reconciled = latestRegistry.current
        ? {
            ...latestRegistry.current,
            safetyLevel: updated.safetyLevel,
            keptV1Safety: updated.keptV1Safety,
          }
        : updated;
      latestRegistry.current = reconciled;
      onRegistryChange(reconciled);
    } catch (error) {
      toastError(
        translate("Couldn't update safety: {value1}", { value1: String(error) }),
      );
    } finally {
      setSafetyBusy(false);
    }
  }
  const [quarantined, setQuarantined] = useState<QuarantinedTool[]>([]);
  const [quarantineError, setQuarantineError] = useState(false);
  const baselineContext = JSON.stringify([
    registry?.activeProfileId,
    effectiveLevel,
    registry?.quarantineOnDrift,
    registry?.teamForcedQuarantineOnDrift,
  ]);
  const [baselineNotice, setBaselineNotice] = useState<{
    context: string;
    ts: number;
    detail: string;
  } | null>(null);
  const [allowedTools, setAllowedTools] = useState<AllowedTool[]>([]);
  const [allowedError, setAllowedError] = useState(false);
  const [bridge, setBridge] = useState<HttpBridgeStatus | null>(null);
  const [bridgeError, setBridgeError] = useState(false);
  // Mask the bridge bearer token by default (grants any local process access to all
  // the user's tools); reveal on demand.
  const [showToken, setShowToken] = useState(false);
  const [bridgeBusy, setBridgeBusy] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [newLabel, setNewLabel] = useState("");
  const [newProfile, setNewProfile] = useState("");
  const [newToken, setNewToken] = useState<string | null>(null);
  const [clientBusy, setClientBusy] = useState(false);
  const [reapBusy, setReapBusy] = useState(false);
  const [reapOutcome, setReapResult] = useState<ReapOutcome | null>(null);
  // Apps still launching an obsolete gateway. Seeded from stored state because the
  // launch reaper already found them before this view existed; refreshed from the
  // button's own outcome, which merges rather than replaces (SOU-435).
  const [needsRestart, setNeedsRestart] = useState<ClientNeedingRestart[]>([]);
  const [restartCheckFailed, setRestartCheckFailed] = useState(false);
  const loadNeedsRestart = useCallback(async () => {
    try {
      setNeedsRestart(await clientsNeedingRestart());
      setRestartCheckFailed(false);
    } catch {
      setRestartCheckFailed(true);
      toastError(translate("Couldn't check for apps using an old gateway"));
    }
  }, [setNeedsRestart, setRestartCheckFailed]);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Start the restart-status request on mount.
    void loadNeedsRestart();
  }, [loadNeedsRestart]);
  // Launch-at-login is OS-level state owned by the autostart plugin. Model
  // loading/ready/error explicitly so the switch is never presented as a
  // verified Off while the real OS value is unknown or unreadable (#694).
  const [autostart, setAutostart] = useState<
    { status: "loading" } | { status: "error" } | { status: "ready"; enabled: boolean }
  >({ status: "loading" });

  const httpClients = registry?.httpClients ?? [];
  const profiles = registry?.profiles ?? [];
  const serverName = useMemo(() => {
    const m = new Map<string, string>();
    // Exclude Toolport's own gateway; it's infrastructure, not a profile-scopable server.
    for (const s of registry?.servers ?? []) if (!isGatewayServer(s)) m.set(s.id, s.name);
    return m;
  }, [registry?.servers]);

  // Launch-at-login is OS-level (managed by the autostart plugin), not registry state.
  // A failed read must not be rendered as "Off": keep the switch disabled and offer
  // a retry until the OS value is actually known.
  const loadAutostart = useCallback(async () => {
    setAutostart({ status: "loading" });
    try {
      setAutostart({ status: "ready", enabled: await isAutostartEnabled() });
    } catch {
      setAutostart({ status: "error" });
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Start the OS autostart request on mount.
    void loadAutostart();
  }, [loadAutostart]);

  const toggleAutostart = async (on: boolean) => {
    setBusySettings((current) => new Set(current).add("autostart"));
    try {
      if (on) await enableAutostart();
      else await disableAutostart();
      setAutostart({ status: "ready", enabled: on });
    } catch (e) {
      toastError(
        on
          ? t("Couldn't enable launch at login: {value2}", { value2: String(e) })
          : t("Couldn't disable launch at login: {value2}", { value2: String(e) }),
      );
    } finally {
      setBusySettings((current) => {
        const next = new Set(current);
        next.delete("autostart");
        return next;
      });
    }
  };

  async function addClient() {
    if (!newLabel.trim()) return;
    setClientBusy(true);
    try {
      const res = await addHttpClient(newLabel.trim(), newProfile || undefined);
      onRegistryChange(res.registry);
      setNewToken(res.token);
      setNewLabel("");
    } catch (e) {
      toastError(translate("Couldn't add client: {value1}", { value1: String(e) }));
    } finally {
      setClientBusy(false);
    }
  }

  async function removeClient(id: string) {
    try {
      onRegistryChange(await removeHttpClient(id));
    } catch (e) {
      toastError(translate("Couldn't remove client: {value1}", { value1: String(e) }));
    }
  }

  // One-shot (not polled): the status only changes when the user toggles it here.
  // Exposed as a callback so a failed initial read can be retried explicitly, rather
  // than leaving the panel wedged until an app restart.
  const loadBridge = useCallback(() => {
    httpBridgeStatus()
      .then((s) => {
        setBridge(s);
        setBridgeError(false);
      })
      .catch(() => setBridgeError(true));
  }, []);
  useEffect(() => {
    loadBridge();
  }, [loadBridge]);

  useEffect(() => {
    let current = true;
    const load = () => {
      void getBaselineNotice()
        .then((notice) => {
          if (!current) return;
          setBaselineNotice(
            notice
              ? {
                  context: baselineContext,
                  ts: notice.ts,
                  detail: notice.detail,
                }
              : null,
          );
        })
        .catch(() => {});
      return (
        listQuarantined()
          .then((q) => {
            setQuarantined(q);
            setQuarantineError(false);
          })
          // A failed poll must not read as "nothing quarantined": keep any prior
          // list and flag the error so the panel can say the status is stale.
          .catch(() => setQuarantineError(true))
      );
    };
    load();
    // Quarantine happens asynchronously in the gateway, so poll to keep the list fresh.
    const id = setInterval(load, 15000);
    return () => {
      current = false;
      clearInterval(id);
    };
  }, [baselineContext]);

  const reapprove = async (q: QuarantinedTool) => {
    try {
      await releaseQuarantine(q.profile, q.tool, undefined, q.record_id);
      setQuarantined(await listQuarantined());
    } catch (e) {
      toastError(t("Couldn't re-approve: {value1}", { value1: String(e) }));
    }
  };

  // Tools the user allowed to skip human approval. Polled because "always allow" is chosen
  // from the approval overlay (a different component), so this keeps the list in sync.
  useEffect(() => {
    const load = () =>
      listAllowedTools()
        .then((t) => {
          setAllowedTools(t);
          setAllowedError(false);
        })
        .catch(() => setAllowedError(true));
    load();
    const id = setInterval(load, 10000);
    return () => clearInterval(id);
  }, []);

  const revokeAllowed = async (key: string) => {
    try {
      await revokeAllowedTool(key);
      setAllowedTools(await listAllowedTools());
    } catch (e) {
      toastError(t("Couldn't revoke: {value1}", { value1: String(e) }));
    }
  };

  const toggleBridge = async (on: boolean) => {
    setBridgeBusy(true);
    try {
      setBridge(on ? await startHttpBridge() : await stopHttpBridge());
      setBridgeError(false);
    } catch (e) {
      toastError(
        on
          ? t("Couldn't start the HTTP endpoint: {value2}", { value2: String(e) })
          : t("Couldn't stop the HTTP endpoint: {value2}", { value2: String(e) }),
      );
    } finally {
      setBridgeBusy(false);
    }
  };

  const copy = (text: string, which: string) => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(which);
        setTimeout(() => setCopied(null), 1500);
      },
      () => {},
    );
  };

  const publishRegistrySetting = (key: RegistrySettingKey, updated: Registry) => {
    const field = REGISTRY_FIELD_BY_SETTING[key];
    const reconciled: Registry = latestRegistry.current
      ? { ...latestRegistry.current, [field]: updated[field] }
      : updated;
    latestRegistry.current = reconciled;
    onRegistryChange(reconciled);
  };

  const apply =
    (key: RegistrySettingKey, fn: (v: boolean) => Promise<Registry>) =>
    async (v: boolean) => {
      setBusySettings((current) => new Set(current).add(key));
      try {
        // eslint-disable-next-line react-hooks/refs -- This callback reads the latest registry after IPC, not during render.
        publishRegistrySetting(key, await fn(v));
      } catch (e) {
        toastError(t("Couldn't update the setting: {value1}", { value1: String(e) }));
      } finally {
        setBusySettings((current) => {
          const next = new Set(current);
          next.delete(key);
          return next;
        });
      }
    };

  // Live inspection needs a step the plain `apply` helper doesn't: when turned OFF,
  // clear the ephemeral capture ring so nothing lingers on disk.
  const applyLiveInspect = async (on: boolean) => {
    setBusySettings((current) => new Set(current).add("live-inspect"));
    try {
      const reg = await setLiveInspect(on);
      if (!on) await clearInspectLog();
      publishRegistrySetting("live-inspect", reg);
    } catch (e) {
      toastError(t("Couldn't update the setting: {value1}", { value1: String(e) }));
    } finally {
      setBusySettings((current) => {
        const next = new Set(current);
        next.delete("live-inspect");
        return next;
      });
    }
  };

  const toggle = (
    Icon: typeof Layers,
    on: boolean,
    accent: string,
    title: string,
    desc: string,
    onChange: (v: boolean) => void,
    settingKey: SettingKey,
    extra?: {
      switchDisabled?: boolean;
      tooltip?: string;
      hint?: string;
      hintTone?: "muted" | "destructive";
      onRetry?: () => void;
    },
  ) => (
    <label
      title={extra?.tooltip}
      className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm"
    >
      <Icon
        className={`size-4 shrink-0 ${String(on ? accent : "text-muted-foreground")}`}
      />
      <span className="flex min-w-0 flex-1 flex-col leading-tight">
        <span className="font-medium">{title}</span>
        <span className="text-meta text-muted-foreground">{desc}</span>
        {extra?.hint && (
          <span
            className={
              extra.hintTone === "destructive"
                ? "text-meta text-destructive"
                : "text-meta text-muted-foreground"
            }
          >
            {extra.hint}
          </span>
        )}
      </span>
      <Switch
        checked={on}
        onCheckedChange={onChange}
        disabled={busySettings.has(settingKey) || extra?.switchDisabled}
      />
      {extra?.onRetry && (
        <Button size="sm" variant="ghost" className="gap-1.5" onClick={extra.onRetry}>
          <RefreshCw className="size-3.5" /> {t("Retry")}{" "}
        </Button>
      )}
    </label>
  );

  // Localization follows the current language.
  const reapResultParts: string[] = [];
  if (reapOutcome?.killed.length) {
    reapResultParts.push(
      t("Stopped {value1}: {value2}", {
        value1: reapOutcome.killed.length,
        value2: reapOutcome.killed.join("; "),
      }),
    );
  }
  if (reapOutcome?.failed.length) {
    reapResultParts.push(
      t("Could not stop {value1}: {value2}", {
        value1: reapOutcome.failed.length,
        value2: reapOutcome.failed.join("; "),
      }),
    );
  }
  const reapResult = !reapOutcome
    ? null
    : reapResultParts.length > 0
      ? reapResultParts.join(". ")
      : reapOutcome.needsRestart.length > 0
        ? // Derive localized feedback from the latest result.
          t(
            "Nothing to stop: these apps spawn the old gateway on their next tool call, so there is no process running between calls.",
          )
        : t("No old gateway processes found.");

  return (
    <div className="flex flex-col gap-8">
      <SectionHeading title={t(SETTINGS_PAGES.find((item) => item.id === page)!.label)}>
        {t(SETTINGS_PAGES.find((item) => item.id === page)!.description)}
      </SectionHeading>
      {page === "general" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("general")}
        >
          {languageSuggestionPending && suggestedLocale !== "en" && (
            <div
              data-testid="language-suggestion"
              className="flex flex-wrap items-center gap-3 rounded-md border px-3 py-2.5 text-sm"
            >
              <p className="min-w-0 flex-1">
                {t("Toolport is now available in {language}.", {
                  language: suggestedLanguage.label,
                })}
              </p>
              <Button
                size="sm"
                disabled={languageSaving}
                onClick={() => void saveLanguage(suggestedLocale)}
              >
                {t("Switch to {language}", { language: suggestedLanguage.label })}
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={languageSaving}
                onClick={() => void saveLanguage("en")}
              >
                {t("Keep English")}
              </Button>
            </div>
          )}
          <div className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm">
            <Monitor className="size-4 shrink-0 text-info" />
            <label
              htmlFor="interface-language"
              className="flex min-w-0 flex-1 flex-col leading-tight"
            >
              <span className="font-medium">{t("Language")}</span>
              <span className="text-meta text-muted-foreground">
                {t("Choose the language used in Toolport.")}
              </span>
            </label>
            <select
              id="interface-language"
              value={language}
              onChange={(event) =>
                void saveLanguage(event.target.value as LanguageChoice)
              }
              disabled={languageSaving}
              aria-busy={languageLoading}
              className="max-w-48 rounded-md border bg-background px-2 py-1.5 text-sm"
            >
              <option value="system">{t("System language")}</option>
              {LANGUAGES.map(({ code, label }) => (
                <option key={code} value={code}>
                  {label}
                </option>
              ))}
            </select>
          </div>
          {languageLoading && (
            <p role="status" className="text-meta text-muted-foreground">
              {t("Loading language…")}
            </p>
          )}
          {(languageError || saveError) && (
            <p role="alert" className="text-meta text-destructive">
              {saveError
                ? t("Couldn't save the language setting. Try again.")
                : t("Couldn't load this language. Reload Toolport to retry.")}
            </p>
          )}
          {toggle(
            Power,
            autostart.status === "ready" && autostart.enabled,
            "text-info",
            translate("Launch at login"),
            translate(
              "Start Toolport in the tray when you sign in, so it can hold tool calls for approval even before you open it",
            ),
            toggleAutostart,
            "autostart",
            {
              switchDisabled: autostart.status !== "ready",
              hint:
                autostart.status === "loading"
                  ? translate("Checking the current OS setting…")
                  : autostart.status === "error"
                    ? translate(
                        "Couldn't read the OS setting — launch-at-login state is unknown.",
                      )
                    : undefined,
              hintTone: autostart.status === "error" ? "destructive" : "muted",
              onRetry:
                autostart.status === "error" ? () => void loadAutostart() : undefined,
            },
          )}
          <div className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm">
            <Sun className="size-4 shrink-0 text-info" />
            <span className="flex min-w-0 flex-1 flex-col leading-tight">
              <span className="font-medium">{translate("Appearance")}</span>
              <span className="text-meta text-muted-foreground">
                {translate("Light, dark, or follow your system setting.")}
              </span>
            </span>
            <div className="flex shrink-0 gap-0.5 rounded-md border bg-background p-0.5">
              {(
                [
                  ["light", Sun, t("Light")],
                  ["system", Monitor, t("System")],
                  ["dark", Moon, t("Dark")],
                ] as [Theme, typeof Sun, string][]
              ).map(([value, Icon, label]) => (
                <button
                  key={value}
                  onClick={() => setTheme(value)}
                  aria-pressed={theme === value}
                  title={label}
                  className={`flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors ${String(
                    theme === value
                      ? "bg-muted font-medium text-foreground"
                      : "text-muted-foreground hover:text-foreground",
                  )}`}
                >
                  <Icon className="size-3.5" />
                  {label}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between gap-3 rounded-md border p-3 text-sm">
            <div>
              <p className="font-medium">{translate("Updates")}</p>
              <p className="text-meta text-muted-foreground">
                {translate("Check for a newer version of Toolport.")}
              </p>
            </div>
            <Button
              variant="outline"
              onClick={() => window.dispatchEvent(new Event("toolport-check-updates"))}
            >
              {translate("Check for updates")}
            </Button>
          </div>
        </section>
      )}
      {page === "tools" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("tools")}
        >
          <div className="settings-switches">
            {toggle(
              Layers,
              lazyDiscovery,
              "text-info",
              translate("Find tools as needed"),
              translate(
                "Agents search for the tools they need instead of loading the full list. This is the default for connections without saved client settings. Choose a different behavior in Clients.",
              ),
              // eslint-disable-next-line react-hooks/refs -- Render passes an event callback; its registry ref is read only after IPC.
              apply("lazy-discovery", setLazyDiscovery),
              "lazy-discovery",
            )}
            {toggle(
              Braces,
              codeMode,
              "text-info",
              translate("Code mode"),
              translate(
                "Let agents combine several tool calls in one script to reduce back-and-forth. Each call follows your access and approval settings. Scripts run in a restricted environment, but this does not replace those settings.",
              ),
              // eslint-disable-next-line react-hooks/refs -- Render passes an event callback; its registry ref is read only after IPC.
              apply("code-mode", setCodeMode),
              "code-mode",
              {
                tooltip:
                  "A gateway started with TOOLPORT_CODE_MODE=1 can keep scripts available even when this setting is off.",
              },
            )}
          </div>
          {/* Tools always included is a refinement of lazy discovery (the tools it must never
            hide), not a peer feature, so nest it under the Lazy discovery toggle with an
            indent + left rail. It has no meaning when lazy discovery is off, so it collapses
            away entirely then. */}
          {lazyDiscovery ? (
            <div className="rounded-md border p-3">
              <PinnedPrerequisites
                registry={registry}
                onRegistryChange={onRegistryChange}
              />
            </div>
          ) : null}
        </section>
      )}
      {page === "safety" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("safety")}
        >
          <fieldset
            className="grid grid-cols-1 gap-3 sm:grid-cols-3"
            aria-label={translate("Safety")}
          >
            <legend className="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
              {translate("Safety level")}
            </legend>
            {safetyLevels.map((level) => (
              <label
                key={level}
                className={`flex cursor-pointer flex-col gap-2 rounded-lg border p-4 ${String(effectiveLevel === level ? "border-primary bg-primary/5" : "bg-card")}`}
              >
                <span className="flex items-center justify-between font-medium">
                  {levelLabel[level]}
                  <input
                    type="radio"
                    name="safety"
                    aria-label={levelLabel[level]}
                    checked={effectiveLevel === level}
                    disabled={
                      safetyBusy ||
                      safetyLevels.indexOf(level) < safetyLevels.indexOf(teamFloor)
                    }
                    onChange={() => void chooseSafety(level)}
                    className="accent-primary"
                  />
                </span>
                <span className="text-meta text-muted-foreground">
                  {level === "off"
                    ? translate("No extra checks before destructive calls.")
                    : level === "ask"
                      ? translate("Pause destructive calls for your approval.")
                      : translate(
                          "Hide destructive tools and ask before untrusted calls.",
                        )}
                </span>
              </label>
            ))}
          </fieldset>
          <p className="text-meta text-muted-foreground">
            {translate(
              "Ask pauses destructive calls for your approval. Strict also hides destructive tools, pauses risky tool changes, blocks high-confidence injection and asks before untrusted calls. Tool labeling and change history stay on at every level.",
            )}
          </p>
          <p
            role="status"
            className="toolport-alert rounded-md border bg-muted/20 px-3 py-2 text-meta text-muted-foreground"
          >
            {effectiveLevel === "off"
              ? translate(
                  "Safety is set to Off. Toolport does not ask before destructive calls. Server sign-in and client permissions may still ask for approval.",
                )
              : effectiveLevel === "ask"
                ? translate(
                    "Safety is set to Ask. Destructive calls need your approval before they run.",
                  )
                : translate(
                    "Safety is set to Strict. Destructive tools are hidden and untrusted calls need your approval.",
                  )}
          </p>
          {keptSummary && (
            <div className="flex flex-col items-start gap-2 rounded-md border px-3 py-2 text-meta">
              <p>
                {keptSummary}{" "}
                {translate(
                  "Choosing a level replaces these with that level's protections.",
                )}
              </p>
              <Button
                size="sm"
                variant="outline"
                disabled={safetyBusy}
                onClick={() => chooseSafety(effectiveLevel)}
              >
                {t("Use standard {level}", { level: levelLabel[effectiveLevel] })}
              </Button>
            </div>
          )}
          {teamFloor !== "off" && (
            <p className="text-meta">
              {t(
                "Team minimum safety level: {level}. Choices below this floor are unavailable.",
                { level: teamFloor === "ask" ? t("Ask") : t("Strict") },
              )}
            </p>
          )}
          {(registry?.teamForcedQuarantineOnDrift ||
            registry?.teamForcedBlockOnInjection) && (
            <p className="text-meta">
              {translate("Team also enforces:")}{" "}
              {[
                registry.teamForcedQuarantineOnDrift && "quarantine on drift",
                registry.teamForcedBlockOnInjection && "block on injection",
              ]
                .filter(Boolean)
                .join(", ")}
              {translate(". These protections do not raise your safety level.")}
            </p>
          )}
          {baselineNotice && baselineNotice.context === baselineContext && (
            <p
              role="status"
              className="rounded-md border border-border bg-muted/20 px-3 py-2 text-meta text-muted-foreground"
            >
              {t("Last baseline warning: {time}: {detail}", {
                time: new Date(baselineNotice.ts).toLocaleString(getLocale()),
                detail: baselineNotice.detail,
              })}
            </p>
          )}
          {quarantined.length === 0 && quarantineError && (
            <ErrorCallout
              title={translate("Quarantined tools")}
              cause={translate("Couldn't read quarantine status. Retrying every 15s.")}
              onRetry={() => {
                void listQuarantined()
                  .then((rows) => {
                    setQuarantined(rows);
                    setQuarantineError(false);
                  })
                  .catch(() => setQuarantineError(true));
              }}
            />
          )}
          {quarantined.length > 0 && (
            <div className="toolport-alert flex flex-col gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <ShieldX className="size-4 shrink-0 text-destructive" />
                <span className="text-sm font-medium">
                  {translate("Quarantined tools")}
                </span>
                <span className="text-meta text-muted-foreground">
                  {quarantineError
                    ? translate("status may be stale")
                    : translate("kept until you re-approve")}
                </span>
              </div>
              <ul className="flex flex-col gap-1.5">
                {quarantined.map((q) => (
                  <li
                    key={`${q.profile}:${q.tool}`}
                    className="flex items-start gap-2 text-meta"
                  >
                    <div className="min-w-0 flex-1">
                      <p className="break-words font-mono">{q.tool}</p>
                      <p className="text-muted-foreground">{q.detail || q.reason}</p>
                      <QuarantineDefinition tool={q} />
                    </div>
                    <button
                      type="button"
                      onClick={() => reapprove(q)}
                      className="ml-auto shrink-0 rounded-md border bg-background px-2 py-0.5 text-xs font-medium hover:bg-accent"
                    >
                      {translate("Re-approve")}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {allowedTools.length === 0 && allowedError && (
            <ErrorCallout
              title={translate("Allowed tools")}
              cause={translate(
                "Couldn't read the allowed-tools list. Retrying every 10s.",
              )}
              onRetry={() => {
                void listAllowedTools()
                  .then((rows) => {
                    setAllowedTools(rows);
                    setAllowedError(false);
                  })
                  .catch(() => setAllowedError(true));
              }}
            />
          )}
          {allowedTools.length > 0 && (
            <div className="flex flex-col gap-2 rounded-md border border-border bg-muted/20 px-3 py-2.5">
              <div className="flex items-center gap-2">
                <UserCheck className="size-4 shrink-0 text-info" />
                <span className="text-sm font-medium">{translate("Allowed tools")}</span>
                <span className="text-meta text-muted-foreground">
                  {allowedError
                    ? translate("list may be stale")
                    : translate("skip human approval")}
                </span>
              </div>
              <ul className="flex flex-col gap-1.5">
                {allowedTools.map((t) => (
                  <li key={t.key} className="flex items-center gap-2 text-meta">
                    <span className="min-w-0 truncate font-mono">
                      {t.server}/{t.tool}
                    </span>
                    <span className="shrink-0 text-muted-foreground">
                      {t.persistent ? translate("always") : translate("this session")}
                    </span>
                    <button
                      type="button"
                      onClick={() => void revokeAllowed(t.key)}
                      className="ml-auto shrink-0 rounded-md border bg-background px-2 py-0.5 text-xs font-medium hover:bg-accent"
                    >
                      {translate("Revoke")}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="settings-switches">
            {toggle(
              EyeOff,
              piiRedaction,
              "text-info",
              translate("Hide personal data from the model"),
              translate(
                "Replace emails, phone numbers, card numbers and API keys in tool results with placeholders before the model sees them, then put the real values back when it calls a tool. A value only goes back to the server it came from, so a call that would send one server's data to another is refused. Real data stays on this machine and is forgotten when the conversation ends. Off by default; a value no detector recognises still passes through, so this reduces what reaches the model rather than guaranteeing it",
              ),
              // eslint-disable-next-line react-hooks/refs -- Render passes an event callback; its registry ref is read only after IPC.
              apply("pii-redaction", setPiiRedaction),
              "pii-redaction",
            )}
            {toggle(
              Activity,
              liveInspect,
              "text-info",
              translate("Live request/response inspection"),
              translate(
                "Off by default. While on, Toolport captures each tool call's arguments and results to a small local, ephemeral buffer (the last 50 calls) so you can inspect them in Activity. This is separate from the audit log, never leaves your machine, and is cleared when you turn it off or restart the gateway.",
              ),
              // eslint-disable-next-line react-hooks/refs -- Render passes an event callback; its registry ref is read only after IPC.
              applyLiveInspect,
              "live-inspect",
            )}
          </div>
          <p className="text-sm" aria-live="polite">
            {translate("Pending approvals:")}
            {pendingCount}
            {translate(". Waiting calls appear in the approval queue.")}
          </p>
        </section>
      )}
      {page === "access" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("access")}
        >
          <div className="mt-3 flex flex-col gap-2">
            {profiles.length > 0 && (
              <section className="flex flex-col gap-2">
                <div className="flex flex-wrap items-end justify-between gap-3">
                  <div className="min-w-60 flex-1">
                    <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
                      {translate("Access sets")}
                    </h2>
                    <p className="text-meta text-muted-foreground">
                      {translate(
                        "Access sets narrow enabled servers and, for stdio clients, tools. Servers that are off stay hidden from every client.",
                      )}
                    </p>
                  </div>
                  <AccessSetCreator onRegistryChange={onRegistryChange} />
                </div>
                <div className="toolport-list">
                  <div className="flex items-center gap-3 border-b border-border px-4 py-3 last:border-b-0">
                    <div className="min-w-0 flex-1">
                      <p className="text-sm font-semibold">
                        {translate("Default access")}
                      </p>
                      <p className="text-meta text-muted-foreground">
                        {translate(
                          "Default access for clients without an explicit access set",
                        )}
                      </p>
                    </div>
                    <Select
                      value={registry?.defaultAccessProfileId || "@all-enabled"}
                      onValueChange={async (id) => {
                        try {
                          onRegistryChange(
                            await setDefaultAccess(id === "@all-enabled" ? null : id),
                          );
                        } catch (e) {
                          toastError(`${e}`);
                        }
                      }}
                    >
                      <SelectTrigger aria-label={translate("Default access")}>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
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
                  </div>
                  {registry?.defaultAccessLegacyPolicy &&
                    !registry.defaultAccessProfileId && (
                      <div className="flex items-center gap-3 px-4 py-3">
                        <p className="min-w-0 flex-1 text-meta text-muted-foreground">
                          {translate(
                            "The upgrade retained the default access set's tool restrictions and instructions.",
                          )}
                        </p>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={async () => {
                            try {
                              onRegistryChange(await setDefaultAccess(null));
                            } catch (e) {
                              toastError(`${e}`);
                            }
                          }}
                        >
                          {translate("Use All enabled servers")}
                        </Button>
                      </div>
                    )}
                </div>
                {profiles.map((p) => {
                  const count = p.enabledServerIds.filter((id) =>
                    serverName.has(id),
                  ).length;
                  const active = p.id === registry?.defaultAccessProfileId;
                  return (
                    <div key={p.id} className="toolport-list">
                      <div className="flex items-center gap-2 border-b border-border px-4 py-3 last:border-b-0">
                        <span className="truncate text-sm font-semibold">{p.name}</span>
                        {active && <Badge variant="info">{translate("default")}</Badge>}
                        <span className="flex-1 text-meta text-muted-foreground">
                          {count}{" "}
                          {count === 1 ? translate("server") : translate("servers")}
                        </span>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={translate("Delete access set")}
                          title={translate("Delete access set")}
                          disabled={
                            profiles.length <= 1 ||
                            p.id === registry?.defaultAccessProfileId ||
                            p.id === registry?.defaultAccessContextId
                          }
                          onClick={async () => {
                            try {
                              onRegistryChange(await deleteProfile(p.id));
                            } catch (e) {
                              toastError(`${e}`);
                            }
                          }}
                        >
                          <Trash2 />
                        </Button>
                      </div>
                      {registry && (
                        <ProfileToolScope
                          profile={p}
                          registry={registry}
                          onRegistryChange={onRegistryChange}
                        />
                      )}
                    </div>
                  );
                })}
                <FolderRouting registry={registry} onRegistryChange={onRegistryChange} />
              </section>
            )}
          </div>

          <div className="mt-2">
            <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
              {translate("Scoped HTTP clients")}
            </h2>
            <p className="text-meta text-muted-foreground">
              {translate(
                "Each gets its own bearer token and server scope, so one endpoint can serve several clients.",
              )}
            </p>
          </div>
          {bridge?.running && bridge.url ? (
            <div className="toolport-list flex flex-col gap-3 px-4 py-3">
              {httpClients.length > 0 && (
                <ul className="flex flex-col divide-y divide-border">
                  {httpClients.map((c) => (
                    <li key={c.id} className="flex items-center gap-2 py-2 text-sm">
                      <span className="truncate font-medium">
                        {c.label || translate("(unnamed)")}
                      </span>
                      <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                        {c.profile === "@all-enabled"
                          ? translate("All enabled servers")
                          : profiles.find(
                              (p) => p.id === c.profile || p.name === c.profile,
                            )?.name ||
                            c.profile ||
                            translate("Full connected set")}
                      </span>
                      <button
                        type="button"
                        onClick={() => removeClient(c.id)}
                        aria-label={translate("Revoke {value1}", {
                          value1: String(c.label),
                        })}
                        className="ml-auto shrink-0 rounded p-1 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                      >
                        <Trash2 className="size-3.5" />
                      </button>
                    </li>
                  ))}
                </ul>
              )}

              {newToken && (
                <>
                  <div className="flex items-center gap-2 rounded border border-success/30 bg-success/5 px-2 py-1.5">
                    <span className="shrink-0 text-meta font-medium text-success">
                      {translate("New token")}
                    </span>
                    <code className="min-w-0 flex-1 truncate text-meta">{newToken}</code>
                    <button
                      type="button"
                      onClick={() => copy(newToken, "newtoken")}
                      title={translate("Copy token")}
                      className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                    >
                      {copied === "newtoken" ? (
                        <Check className="size-3.5 text-success" />
                      ) : (
                        <Copy className="size-3.5" />
                      )}
                    </button>
                    <button
                      type="button"
                      onClick={() => setNewToken(null)}
                      aria-label={translate("Dismiss")}
                      className="shrink-0 rounded p-1 text-muted-foreground hover:text-foreground"
                    >
                      <X className="size-3.5" />
                    </button>
                  </div>
                  <p className="text-meta text-warning">
                    {translate("Copy this token now, it won't be shown again.")}
                  </p>
                </>
              )}

              <div className="flex items-center gap-2">
                <input
                  value={newLabel}
                  onChange={(e) => setNewLabel(e.target.value)}
                  placeholder={translate("Client name (e.g. Open WebUI)")}
                  className="h-8 min-w-0 flex-1 rounded-md border border-input bg-transparent px-2 text-meta focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none"
                />
                {profiles.length > 0 && (
                  <Select
                    value={newProfile || "__default__"}
                    onValueChange={(v) => setNewProfile(v === "__default__" ? "" : v)}
                  >
                    <SelectTrigger
                      size="sm"
                      aria-label={translate("Access")}
                      className="h-8 w-32 shrink-0 text-meta"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__default__">
                        {translate("Full connected set")}
                      </SelectItem>
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
                <button
                  type="button"
                  onClick={addClient}
                  disabled={clientBusy || !newLabel.trim()}
                  className="h-8 shrink-0 rounded-md border bg-background px-2.5 text-meta font-medium hover:bg-accent disabled:opacity-50"
                >
                  {translate("Add")}
                </button>
              </div>
            </div>
          ) : (
            <p className="toolport-list px-4 py-3 text-meta text-muted-foreground">
              {translate(
                "Start the HTTP endpoint in Connections to manage scoped clients.",
              )}
            </p>
          )}
        </section>
      )}
      {page === "connections" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("connections")}
        >
          <div className="flex flex-col gap-2 rounded-md border px-3 py-2.5">
            <label className="flex items-center gap-2.5 text-sm">
              <Globe
                className={`size-4 shrink-0 ${String(bridge?.running ? "text-success" : "text-muted-foreground")}`}
              />
              <span className="flex min-w-0 flex-1 flex-col leading-tight">
                <span className="font-medium">
                  {translate("Open WebUI / HTTP endpoint")}
                </span>
                <span className="text-meta text-muted-foreground">
                  {translate(
                    "Serve your tools over HTTP/OpenAPI for Open WebUI and any OpenAPI client",
                  )}
                </span>
              </span>
              <Switch
                checked={!!bridge?.running}
                onCheckedChange={toggleBridge}
                disabled={bridgeBusy}
              />
            </label>
            {bridgeError && bridge === null && (
              <p className="flex items-center gap-1.5 text-meta text-muted-foreground">
                <span>
                  {translate(
                    "Couldn't read the HTTP endpoint status. The gateway may be starting up.",
                  )}
                </span>
                <button
                  type="button"
                  onClick={loadBridge}
                  className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-primary"
                >
                  {translate("Retry")}
                </button>
              </p>
            )}
            {bridge?.running && bridge.url && (
              <>
                <div className="flex items-center gap-2 rounded border bg-muted/40 px-2 py-1.5">
                  <span className="shrink-0 text-meta font-medium text-muted-foreground">
                    {translate("URL")}
                  </span>
                  <code className="min-w-0 flex-1 truncate text-meta">{bridge.url}</code>
                  <button
                    type="button"
                    onClick={() => copy(bridge.url!, "url")}
                    title={translate("Copy URL")}
                    className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    {copied === "url" ? (
                      <Check className="size-3.5 text-success" />
                    ) : (
                      <Copy className="size-3.5" />
                    )}
                  </button>
                </div>
                {bridge.token && (
                  <div className="flex items-center gap-2 rounded border bg-muted/40 px-2 py-1.5">
                    <span className="shrink-0 text-meta font-medium text-muted-foreground">
                      {translate("Token")}
                    </span>
                    <code className="min-w-0 flex-1 truncate text-meta">
                      {showToken ? bridge.token : "•".repeat(24)}
                    </code>
                    <button
                      type="button"
                      onClick={() => setShowToken((s) => !s)}
                      title={
                        showToken ? translate("Hide token") : translate("Reveal token")
                      }
                      aria-label={
                        showToken ? translate("Hide token") : translate("Reveal token")
                      }
                      className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                    >
                      {showToken ? (
                        <EyeOff className="size-3.5" />
                      ) : (
                        <Eye className="size-3.5" />
                      )}
                    </button>
                    <button
                      type="button"
                      onClick={() => copy(bridge.token!, "token")}
                      title={translate("Copy token")}
                      className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                    >
                      {copied === "token" ? (
                        <Check className="size-3.5 text-success" />
                      ) : (
                        <Copy className="size-3.5" />
                      )}
                    </button>
                  </div>
                )}
                <p className="text-meta text-muted-foreground">
                  {translate(
                    "In Open WebUI: Settings → Tools → add the URL as an OpenAPI server and paste the token as its API key (Bearer auth), then set Function Calling to Native (per chat). The token stops other local apps from calling your tools.",
                  )}
                </p>
              </>
            )}
          </div>
          <div className="flex flex-col gap-2 rounded-md border px-3 py-2.5">
            <div className="flex items-center gap-2.5 text-sm">
              <RefreshCw
                className={`size-4 shrink-0 text-muted-foreground ${String(reapBusy ? "animate-spin" : "")}`}
              />
              <span className="flex min-w-0 flex-1 flex-col leading-tight">
                <span className="font-medium">{translate("Stop old gateways")}</span>
                <span className="text-meta text-muted-foreground">
                  {translate(
                    "End leftover gateway processes from earlier installs. Where the gateway path stays the same across upgrades, a host that respawns MCP picks up the new binary on its own. Where the filename carries its version, or the old one sits at a path an upgrade never touches, an app that was already running keeps launching it until you restart that app.",
                  )}
                </span>
              </span>
              <button
                type="button"
                disabled={reapBusy}
                onClick={async () => {
                  setReapBusy(true);
                  setReapResult(null);
                  try {
                    const outcome = await stopStaleGateways();
                    // Always from the outcome: it carries the merged advice, so
                    // clicking Run before a client has respawned cannot blank the
                    // panel below (SOU-435).
                    setNeedsRestart(outcome.needsRestart);
                    // This outcome IS a fresh answer to "which apps need a
                    // restart", so a stale "couldn't check" panel must go with it.
                    // Leaving it up would state the check failed directly above
                    // the check's own result, which is the same contradiction the
                    // panel exists to prevent (#730).
                    setRestartCheckFailed(false);
                    setReapResult(outcome);
                  } catch (e) {
                    toastError(`Couldn't stop old gateways: ${String(e)}`);
                  } finally {
                    setReapBusy(false);
                  }
                }}
                className="h-8 shrink-0 rounded-md border bg-background px-2.5 text-meta font-medium hover:bg-accent disabled:opacity-50"
              >
                {reapBusy ? translate("Working…") : translate("Run")}
              </button>
            </div>
            {reapResult && (
              <p className="text-meta text-muted-foreground">{reapResult}</p>
            )}
            {restartCheckFailed && (
              <p className="flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-meta text-muted-foreground">
                {translate("Couldn't check for apps using an old gateway.")}
                <button
                  type="button"
                  aria-label={translate("Retry checking for old gateway")}
                  onClick={() => void loadNeedsRestart()}
                  className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-primary"
                >
                  {translate("Retry")}
                </button>
              </p>
            )}
            {needsRestart.length > 0 && (
              <div className="rounded-md border border-warning/40 bg-warning/5 p-3">
                <p className="text-meta font-medium text-warning">
                  {needsRestart.length === 1
                    ? translate("1 app is still launching an old gateway")
                    : translate("{value1} apps are still launching an old gateway", {
                        value1: String(needsRestart.length),
                      })}
                </p>
                <ul className="mt-1.5 space-y-1">
                  {needsRestart.map((c) => (
                    <li key={c.clientPid} className="text-meta text-muted-foreground">
                      <span className="font-medium text-foreground">{c.client}</span>
                      {/* The pid is the only thing separating two rows for the same app.
                        Without it, several copies of one editor render as identical
                        lines that read as a duplicate-row bug, and "restart each one"
                        cannot be followed when a dozen are running. */}
                      <span className="ml-1 font-mono text-muted-foreground">
                        {translate("(pid")}
                        {c.clientPid})
                      </span>{" "}
                      {translate("keeps starting")}
                      <code className="font-mono">{c.gateway}</code>
                    </li>
                  ))}
                </ul>
                <p className="mt-2 text-meta text-muted-foreground">
                  {translate(
                    "These apps cached the old gateway path when they started, so stopping the process only makes them launch it again — and they only launch it when they next use a tool, which is why none may be running this moment. Restart each one to pick up the current gateway. This list clears itself as you do.",
                  )}
                </p>
              </div>
            )}
          </div>
        </section>
      )}
      {page === "help" && (
        <section
          className="settings-page flex flex-col gap-4"
          aria-label={translate("help")}
        >
          {/* One row group: each action sits in a row that says what it does. */}
          <div className="toolport-list">
            <div className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-3">
              <div className="min-w-60 flex-1">
                <p className="text-sm font-semibold">
                  {translate("Support and local data")}
                </p>
                <p className="text-meta text-muted-foreground">
                  {translate(
                    "Diagnostics redact secrets. The data folder contains your registry, audit, and gateway logs.",
                  )}
                </p>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  onClick={async () => {
                    try {
                      await navigator.clipboard.writeText(await gatherDiagnostics());
                      toast.success(
                        translate("Diagnostics copied, paste them into your bug report"),
                      );
                    } catch {
                      toastError(translate("Couldn't copy diagnostics"));
                    }
                  }}
                >
                  {translate("Copy diagnostics")}
                </Button>
                <Button
                  variant="outline"
                  onClick={() =>
                    openDataDir().catch(() =>
                      toastError(translate("Couldn't open data folder")),
                    )
                  }
                >
                  {translate("Open data folder")}
                </Button>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-3">
              <div className="min-w-60 flex-1">
                <p className="text-sm font-semibold">
                  {translate("Remove Toolport from all clients")}
                </p>
                <p className="text-meta text-muted-foreground">
                  {translate(
                    "Unchanged configs return to their original bytes. Your edits are preserved and moved entries are restored. Each client result is reported here.",
                  )}
                </p>
              </div>
              <Button
                variant="destructive"
                aria-label={translate("Remove Toolport from all clients")}
                disabled={removeBusy}
                onClick={() => setRemoveOpen(true)}
              >
                {translate("Remove")}
              </Button>
            </div>
            <div className="flex flex-wrap items-center gap-3 px-4 py-3">
              <div className="min-w-60 flex-1">
                <p className="text-sm font-semibold">
                  {translate("Remove Toolport data")}
                </p>
                <p className="text-meta text-muted-foreground">
                  {translate(
                    "Normal uninstall keeps your data. Remove it only when you no longer need Toolport.",
                  )}
                </p>
              </div>
              <Button
                variant="destructive"
                aria-label={translate("Remove Toolport data")}
                disabled={purgeBusy}
                onClick={() => void preparePurge()}
              >
                {translate("Remove")}
              </Button>
            </div>
          </div>
          <Dialog
            open={purgePlan !== null}
            onOpenChange={(open) => {
              if (!open && !purgeBusy) setPurgePlan(null);
            }}
          >
            <DialogContent>
              <DialogHeader>
                <DialogTitle>{translate("Remove Toolport data?")}</DialogTitle>
                <DialogDescription>
                  {translate(
                    "Toolport will close, restore and disconnect its clients, then permanently remove the resources below. Client files and native servers stay. If restoration fails or sessions remain open, data stays for recovery.",
                  )}
                </DialogDescription>
              </DialogHeader>
              <ul className="list-disc space-y-2 pl-5 text-sm break-words">
                {purgePlan?.resources.map((resource) => (
                  <li key={resource}>{resource}</li>
                ))}
              </ul>
              <p className="text-sm break-words">
                {translate("Results and exact leftovers will be saved to")}{" "}
                <strong>{purgePlan?.reportPath}</strong>
                {translate(". This cannot be undone.")}
              </p>
              <DialogFooter>
                <Button
                  variant="ghost"
                  disabled={purgeBusy}
                  onClick={() => setPurgePlan(null)}
                >
                  {translate("Cancel")}
                </Button>
                <Button
                  variant="destructive"
                  data-confirm=""
                  disabled={purgeBusy}
                  onClick={() => void purge()}
                >
                  {purgeBusy
                    ? translate("Closing Toolport…")
                    : translate("Close and remove data")}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
          {removeResults && (
            <ul className="text-sm" aria-live="polite">
              {removeResults.length === 0 ? (
                <li>{translate("No client connections to remove.")}</li>
              ) : (
                removeResults.map((result) => (
                  <li
                    key={`${result.clientId}:${result.path}`}
                    className={
                      result.error ? "text-destructive" : "text-muted-foreground"
                    }
                  >
                    {result.clientId}:{" "}
                    {result.error ?? translate("Client configuration restored")}
                    {result.warnings?.length ? `; ${result.warnings.join("; ")}` : ""}
                  </li>
                ))
              )}
            </ul>
          )}
          <Dialog
            open={removeOpen}
            onOpenChange={(open) => {
              if (!removeBusy) setRemoveOpen(open);
            }}
          >
            <DialogContent>
              <DialogHeader>
                <DialogTitle>
                  {translate("Remove Toolport from all clients?")}
                </DialogTitle>
                <DialogDescription>
                  {translate(
                    "Unchanged configs return to their original bytes. If you edited a config, Toolport preserves your edits and restores entries it moved. Each client result is reported here.",
                  )}
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button
                  variant="ghost"
                  disabled={removeBusy}
                  onClick={() => setRemoveOpen(false)}
                >
                  {translate("Cancel")}
                </Button>
                <Button
                  variant="destructive"
                  data-confirm=""
                  disabled={removeBusy}
                  onClick={() => void removeAll()}
                >
                  {removeBusy
                    ? translate("Removing…")
                    : translate("Remove from all clients")}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </section>
      )}
    </div>
  );
}

function SectionHeading({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5">
      <h2 className="text-[17px] font-semibold">{title}</h2>
      <p className="text-meta text-muted-foreground">{children}</p>
    </div>
  );
}
