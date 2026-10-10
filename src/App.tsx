import { secretReferenceReview } from "@/lib/secretRefs";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeft,
  CircleCheck,
  KeyRound,
  MoreHorizontal,
  Download,
  Plus,
  RefreshCw,
  Search,
  ServerOff,
  Store,
  TriangleAlert,
  WifiOff,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import {
  detectClients,
  getRegistry,
  takeRegistryRecoveryNotice,
  importServers,
  previewImportServers,
  probeServers,
  removeServer,
  setAllEnabled,
  setServerEnabled,
  type ClientNeedingRestart,
} from "@/lib/api";
import {
  importableServers,
  isEnabled,
  isGatewayServer,
  type DetectedClient,
  type ImportItem,
  type ProbeResult,
  type Registry,
  type ServerEntry,
  type View,
} from "@/lib/types";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { AccessUpgradeNotice } from "@/components/AccessUpgradeNotice";
import { RemovedFeaturesNotice } from "@/components/RemovedFeaturesNotice";
import { AppSidebar } from "@/components/AppSidebar";
import { ClientLogo } from "@/components/ClientLogo";
import { PendingApprovals } from "@/components/PendingApprovals";
import { TeamPairingDialog } from "@/components/TeamPairingDialog";
import { QuarantineAlert } from "@/components/QuarantineAlert";
import { RegistryServerRow } from "@/components/RegistryServerRow";
import { ServerDialog } from "@/components/ServerDialog";
import {
  ImportReviewDialog,
  needsTeamEnableReview,
  sameReviewedDefinition,
} from "@/components/ImportReviewDialog";

// Secondary destinations are code-split so the initial bundle only carries the
// default Servers view and the app chrome. Each mounts behind a Suspense
// fallback the first time it's opened. (Named exports, hence the .then wrap.)
const Onboarding = lazy(() =>
  import("@/components/Onboarding").then((m) => ({ default: m.Onboarding })),
);
const ClientDetail = lazy(() =>
  import("@/components/ClientDetail").then((m) => ({ default: m.ClientDetail })),
);
const ClientsView = lazy(() =>
  import("@/components/ClientsView").then((m) => ({ default: m.ClientsView })),
);
const ActivityView = lazy(() =>
  import("@/components/ActivityView").then((m) => ({ default: m.ActivityView })),
);
const CatalogView = lazy(() =>
  import("@/components/CatalogView").then((m) => ({ default: m.CatalogView })),
);
const TeamsView = lazy(() =>
  import("@/components/TeamsView").then((m) => ({ default: m.TeamsView })),
);
const SettingsView = lazy(() =>
  import("@/components/SettingsView").then((m) => ({ default: m.SettingsView })),
);
import { Button } from "@/components/ui/button";
import { Callout } from "@/components/Callout";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { GitHubStarPrompt, type StarSurface } from "@/components/GitHubStarPrompt";
import { serverNameOrder } from "@/lib/serverOrder";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Skeleton } from "@/components/ui/skeleton";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { useTheme } from "@/lib/theme";
import { fmtTs } from "@/lib/utils";
import { createSingleFlight } from "@/lib/singleFlight";
import { resolveShortcut, shortcutHelp } from "@/lib/shortcuts";
import { subscribeToTrayApprovals } from "@/lib/trayApprovals";

/** Above this many servers, "Disable all" asks for confirmation first. */
const BULK_DISABLE_CONFIRM_MIN = 3;

function App() {
  const { resolved: resolvedTheme } = useTheme();
  const [registry, setRegistry] = useState<Registry | null>(null);
  const registryRef = useRef<Registry | null>(null);
  const [clients, setClients] = useState<DetectedClient[]>([]);
  const [importPreview, setImportPreview] = useState<ImportItem[] | null>(null);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [togglingAll, setTogglingAll] = useState(false);
  // Gates the "Disable all" bulk action behind a confirm when it turns off more
  // than a couple of servers, so one menu click can't silently kill a big set.
  const [confirmDisableAll, setConfirmDisableAll] = useState(false);
  const [confirmEnableTeam, setConfirmEnableTeam] = useState<ServerEntry | null>(null);
  const [selectedClientId, setSelectedClientId] = useState<string | null>(null);
  const [toolsServerId, setToolsServerId] = useState<string | null>(null);
  const [view, setView] = useState<View>("servers");
  const [activityKey, setActivityKey] = useState(0);
  const [health, setHealth] = useState<Record<string, ProbeResult>>({});
  const [probing, setProbing] = useState(false);
  // Whether the app's Rust backend answered the last health probe. `probe_servers`
  // returns per-server failures as ok:false results; a *thrown* invoke instead means
  // the backend itself didn't respond, and without this the server badges would sit
  // on "Checking…" forever with no explanation. Optimistic default so the banner
  // only appears after a real failure.
  const [backendReachable, setBackendReachable] = useState(true);
  const [query, setQuery] = useState("");
  // Keyboard shortcuts (SBS-143). The app had none, which for a developer tool means
  // every interaction is mouse-only.
  const searchRef = useRef<HTMLInputElement>(null);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  // Ctrl+N mounts its own ServerDialog with `autoOpen` rather than reaching into the
  // trigger-based one, so the shortcut needs no changes to ServerDialog's API.
  const [addServerOpen, setAddServerOpen] = useState(false);
  const isMac =
    typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? "");
  const [onboarded, setOnboarded] = useState(
    () =>
      localStorage.getItem("toolport.onboarded") === "1" ||
      localStorage.getItem("conduit.onboarded") === "1",
  );
  const [showOnboarding, setShowOnboarding] = useState(false);
  // Step the wizard opens at (0 = Welcome). Set to the Connect step when resuming
  // after a catalog detour, so a browsing user still lands on the step that wires
  // Toolport into their tools.
  const [onboardingStep, setOnboardingStep] = useState(0);
  const [resumeAtConnect, setResumeAtConnect] = useState(false);
  const [justOnboarded, setJustOnboarded] = useState(false);
  const [starSurface, setStarSurface] = useState<StarSurface>(null);
  const lastProbeRef = useRef(0);
  const probeFlightRef = useRef(createSingleFlight<ProbeResult[]>());
  const loadedOnce = useRef(false);

  // Probe health quietly (no toast). Used on load and after authenticating, so
  // each server's status badge reflects reality without the user clicking around.
  const reprobe = useCallback((): Promise<ProbeResult[]> => {
    // A second caller receives the SAME promise, not an invented empty result.
    // This prevents onboarding, enablement, and refresh from treating in-flight
    // work as an authoritative "zero problems" response (SBS-720).
    return probeFlightRef.current.run(async () => {
      lastProbeRef.current = Date.now();
      setProbing(true);
      try {
        const results = await probeServers();
        setHealth(Object.fromEntries(results.map((r) => [r.serverId, r])));
        setBackendReachable(true);
        return results;
      } catch (error) {
        setBackendReachable(false);
        throw error;
      } finally {
        setProbing(false);
      }
    });
  }, []);

  // Registry/auth mutations need a probe that starts after any older snapshot.
  // Multiple mutations during one active probe share a single trailing run.
  const reprobeAfterMutation = useCallback(
    (): Promise<ProbeResult[]> =>
      probeFlightRef.current.runAfterCurrent(async () => {
        lastProbeRef.current = Date.now();
        setProbing(true);
        try {
          const results = await probeServers();
          setHealth(Object.fromEntries(results.map((r) => [r.serverId, r])));
          setBackendReachable(true);
          return results;
        } catch (error) {
          setBackendReachable(false);
          throw error;
        } finally {
          setProbing(false);
        }
      }),
    [],
  );

  const applyRegistryChange = useCallback(
    (next: Registry) => {
      const activeId = (value: Registry | null) =>
        value?.defaultAccessProfileId ?? value?.defaultAccessContextId;
      const enabledIds = (value: Registry | null) =>
        new Set(
          value?.servers.filter((server) => server.enabled).map((server) => server.id) ??
            [],
        );
      const previous = registryRef.current;
      const previousProfileId = activeId(previous);
      const nextProfileId = activeId(next);
      const previousEnabled = enabledIds(previous);
      const nextEnabled = enabledIds(next);
      const invalidate =
        previousProfileId !== nextProfileId
          ? nextEnabled
          : new Set([...nextEnabled].filter((id) => !previousEnabled.has(id)));

      if (invalidate.size > 0) {
        // A profile switch or enablement must not inherit the previous profile/set's
        // health. Clear those rows before the new registry lands, then probe the
        // authoritative backend state.
        setHealth((current) => {
          const fresh = { ...current };
          invalidate.forEach((id) => delete fresh[id]);
          return fresh;
        });
      }
      registryRef.current = next;
      setRegistry(next);
      if (invalidate.size > 0) void reprobeAfterMutation().catch(() => {});
    },
    [reprobeAfterMutation],
  );

  // A Teams connection that finished pairing lands on its Teams view.
  const openTeams = useCallback(() => {
    setSelectedClientId(null);
    setView("teams");
  }, []);

  useEffect(() => {
    const unlisten = listen("show-teams", () => setView("teams"));
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  // Refresh statuses when the user returns to the window, so a server that came
  // up (or went down) while they were away reflects reality without a manual
  // refresh. Guarded so rapid alt-tabbing doesn't re-spawn every server.
  useEffect(() => {
    const onFocus = () => {
      if (Date.now() - lastProbeRef.current > 20_000) void reprobe().catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [reprobe]);

  // `announce` is set by the manual Refresh button: it waits for the health probe
  // and reports a summary toast. The silent path (initial load, focus refresh)
  // fires the probe without blocking or toasting.
  const load = useCallback(
    async (announce = false) => {
      setLoading(true);
      setError(null);
      try {
        const [reg, dc, recovery] = await Promise.all([
          getRegistry(),
          detectClients(),
          takeRegistryRecoveryNotice(),
        ]);
        registryRef.current = reg;
        setRegistry(reg);
        setClients(dc);
        if (recovery) {
          const when = fmtTs(recovery.recoveredAtMs);
          const detail =
            recovery.reason === "corrupt"
              ? `The registry file was damaged. Restored from backup (${when}).`
              : recovery.reason === "missing"
                ? `The registry file was missing. Restored from backup (${when}).`
                : `${recovery.reason}. Loaded from backup (${when}).`;
          toast.warning("Registry recovered from backup", {
            description: recovery.quarantinePath
              ? `${detail} A copy of the bad file was saved for inspection.`
              : detail,
            duration: 12_000,
          });
        }
        loadedOnce.current = true;
        setActivityKey((k) => k + 1);
        if (announce) {
          try {
            const results = await reprobeAfterMutation();
            if (results.length > 0) {
              const up = results.filter((r) => r.ok).length;
              toast.success(`${up} of ${results.length} servers healthy`);
            } else {
              toast.success("Refreshed");
            }
          } catch {
            toast.warning("Refreshed, but couldn't check server health");
          }
        } else {
          void reprobe().catch(() => {});
        }
      } catch (e) {
        // After the first successful load, a refresh failure shouldn't blow away a
        // working list. Surface it as a toast and keep what's on screen.
        if (loadedOnce.current) {
          toastError(`Couldn't refresh: ${e}`);
        } else {
          setError(String(e));
        }
      } finally {
        setLoading(false);
      }
    },
    [reprobe, reprobeAfterMutation],
  );

  useEffect(() => {
    load();
  }, [load]);

  // An agent toggling a server through the gateway writes the registry; the backend
  // watches that file and emits this event, so the UI reflects the change live
  // without a manual reload.
  useEffect(() => {
    const unlisten = listen<Registry>("registry-changed", (e) => {
      applyRegistryChange(e.payload);
      setActivityKey((k) => k + 1);
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, [applyRegistryChange]);

  // The tray remains available while the window is hidden. Its approvals entry
  // should reveal the app at the exact place where the waiting calls can be
  // inspected, rather than merely opening whichever screen was last visible.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const openApprovals = () => {
      if (cancelled) return;
      setSelectedClientId(null);
      setView("activity");
      setActivityKey((key) => key + 1);
    };
    subscribeToTrayApprovals(openApprovals)
      .then((remove) => {
        if (cancelled) remove();
        else unlisten = remove;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // The backend signals an authoritative removal from a team (a 401/403 on the
  // membership heartbeat) so we can tell the member plainly rather than leaving them
  // to wonder why the team's servers vanished. The registry (team already cleared) is
  // pushed via the normal team_sync return / registry-changed path.
  useEffect(() => {
    const unlisten = listen("team-removed", () => {
      toast.warning(
        "You were removed from the team. Its shared servers have been removed from your setup.",
      );
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  // A refresh emits one `server-probed` event per server as its probe finishes, so
  // each row resolves the moment its own result is in - a slow npx cold-start no
  // longer holds the whole grid in "checking" until the slowest server returns
  // (issue #252). The batched probeServers() return still reconciles at the end.
  useEffect(() => {
    const unlisten = listen<ProbeResult>("server-probed", (e) => {
      setHealth((h) => ({ ...h, [e.payload.serverId]: e.payload }));
      setBackendReachable(true);
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  // The launch reaper stops obsolete gateways, but an app that was already running
  // cached the old spawn command and just launches it again. Only restarting that
  // app fixes it, and nothing else in the UI would say so (SOU-435). Settings keeps
  // the durable list; this is the nudge for someone who never opens it.
  useEffect(() => {
    const unlisten = listen<ClientNeedingRestart[]>("gateway-restart-needed", (e) => {
      const apps = e.payload;
      if (apps.length === 0) return;
      const names = [...new Set(apps.map((a) => a.client))].join(", ");
      toast.warning(
        apps.length === 1
          ? `${names} is still starting an old gateway`
          : `${names} are still starting an old gateway`,
        {
          description:
            "They cached the old path at startup, so restarting them is the only way to pick up the current gateway. Settings keeps the list.",
          duration: 10000,
        },
      );
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  // Required Teams work lives in Rust for the application's lifetime. The webview
  // observes results; hiding/minimizing it cannot stop config delivery or reporting.
  useEffect(() => {
    const unlisten = listen<Registry>("team-sync-registry", (event) => {
      applyRegistryChange(event.payload);
    });
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [applyRegistryChange]);

  function selectClient(id: string) {
    setSelectedClientId(id);
    setView("clients");
  }

  // Top-level destinations leave any selected client detail behind.
  function selectView(v: View) {
    setSelectedClientId(null);
    setView(v);
  }

  /** Focus the search box once it exists.
   *
   * Coming from another view the input is not mounted yet, and the view it replaces
   * may be lazy-loaded behind Suspense, so a single frame is not reliably enough.
   * Retry for a few frames, then give up rather than spin.
   */
  function focusSearchWhenMounted() {
    let frames = 0;
    const tick = () => {
      if (searchRef.current) {
        searchRef.current.select();
        return;
      }
      if (frames++ < 15) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }

  // One global keydown listener rather than per-control handlers, so a shortcut works
  // wherever focus happens to be. The decision of what a keystroke means lives in
  // `resolveShortcut` and is unit-tested there; this only performs the effect.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const action = resolveShortcut(e, e.target as HTMLElement | null);
      if (!action) return;
      // Only claim the key once we know it is ours. Ctrl+R in particular would
      // otherwise reload the webview and throw away in-flight state.
      switch (action.kind) {
        case "view":
          e.preventDefault();
          selectView(action.view);
          break;
        case "focusSearch":
          e.preventDefault();
          // Search only exists on the servers list; go there first so the shortcut
          // is not a silent no-op from another view.
          selectView("servers");
          focusSearchWhenMounted();
          break;
        case "addServer":
          e.preventDefault();
          setAddServerOpen(true);
          break;
        case "refresh":
          e.preventDefault();
          void load(true);
          break;
        case "help":
          e.preventDefault();
          setShortcutsOpen(true);
          break;
        case "closeHelp":
          // No preventDefault: Escape belongs to whatever dialog or menu is open, and
          // this must not stop it closing. Only acts when the sheet is actually up.
          setShortcutsOpen(false);
          break;
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [load]);

  const profileId = registry
    ? (registry.defaultAccessProfileId ?? registry.profiles[0]?.id)
    : undefined;
  // The gateway entry is Toolport itself, not a server it proxies - never list it.
  const servers = (registry?.servers ?? []).filter((s) => !isGatewayServer(s));
  const enabledCount = registry
    ? servers.filter((s) => isEnabled(registry, s.id)).length
    : 0;
  // Probe results can outlive a profile toggle, so only count servers that are
  // still enabled. Otherwise a newly disabled server makes the posture claim
  // more reachable "enabled servers" than the profile actually contains.
  const connectedCount = registry
    ? servers.filter((s) => isEnabled(registry, s.id) && health[s.id]?.ok).length
    : 0;

  // Bucket each server so the list can lead with what needs action. A server
  // needs attention when it's enabled but its probe failed (auth or error).
  type Group = "attention" | "checking" | "active" | "disabled";
  const groupOf = (s: ServerEntry): Group => {
    if (!registry || !isEnabled(registry, s.id)) return "disabled";
    const h = health[s.id];
    if (!h) return "checking";
    return h.ok ? "active" : "attention";
  };
  const attentionServers = servers.filter((s) => groupOf(s) === "attention");
  const attentionCount = attentionServers.length;
  const checkedCount = registry
    ? servers.filter((s) => isEnabled(registry, s.id) && health[s.id]).length
    : 0;

  const q = query.trim().toLowerCase();
  const matches = (s: ServerEntry) =>
    !q ||
    s.name.toLowerCase().includes(q) ||
    (s.url ?? "").toLowerCase().includes(q) ||
    (s.command ?? "").toLowerCase().includes(q);
  const visible = servers.filter(matches).sort(serverNameOrder);
  // The posture and next action summarize the profile, not the current search.
  // Keep these counts independent of `visible` so a filter cannot produce a
  // misleading "0 servers" action while hiding an affected row.
  const authAttention = attentionServers.filter((s) => health[s.id]?.authRequired);
  const errorAttention = attentionServers.filter((s) => !health[s.id]?.authRequired);

  // Count what would actually be imported: drop the gateway entry and anything
  // already in the registry, then dedupe by name across clients (the backend
  // import dedupes too). Using raw server counts here made the banner promise
  // imports that the importer then correctly skipped.
  const importable = new Set(
    clients.flatMap((c) =>
      importableServers(c, registry).map((s) => s.name.toLowerCase()),
    ),
  ).size;
  const selectedClient = selectedClientId
    ? clients.find((c) => c.id === selectedClientId)
    : undefined;

  // Show the first-run wizard once, only on a genuinely fresh setup: no servers
  // and no client connected yet. Latched in its own state so a mid-flow connect
  // (which flips gatewayInstalled) doesn't unmount the dialog. Existing users,
  // and anyone who has dismissed it, never see it.
  useEffect(() => {
    if (onboarded || showOnboarding || resumeAtConnect || loading || !registry) return;
    const fresh = servers.length === 0 && !clients.some((c) => c.gatewayInstalled);
    if (fresh) setShowOnboarding(true);
  }, [
    onboarded,
    showOnboarding,
    resumeAtConnect,
    loading,
    registry,
    servers.length,
    clients,
  ]);

  // The wizard hands off to the catalog mid-flow (Add-servers step). When the user
  // navigates back out of the catalog, resume the wizard at the Connect step rather
  // than abandoning onboarding, so they don't silently skip connecting a client.
  useEffect(() => {
    if (resumeAtConnect && view !== "catalog" && !onboarded) {
      setOnboardingStep(2);
      setShowOnboarding(true);
      setResumeAtConnect(false);
    }
  }, [resumeAtConnect, view, onboarded]);

  function finishOnboarding() {
    localStorage.setItem("toolport.onboarded", "1");
    // Drop the pre-rename key so brand remnants do not linger in DevTools.
    localStorage.removeItem("conduit.onboarded");
    setOnboarded(true);
    setJustOnboarded(true);
    setShowOnboarding(false);
    setResumeAtConnect(false);
    setOnboardingStep(0);
  }

  async function applyToggle(
    serverId: string,
    enabled: boolean,
    reviewed = false,
    reviewedDefinition?: ServerEntry,
  ) {
    if (!profileId) return;
    setBusyId(serverId);
    try {
      const next = reviewedDefinition
        ? await setServerEnabled(
            profileId,
            serverId,
            enabled,
            reviewed,
            reviewedDefinition,
          )
        : await setServerEnabled(profileId, serverId, enabled, reviewed);
      applyRegistryChange(next);
    } catch (e) {
      toastError(`Couldn't toggle: ${e}`);
    } finally {
      setBusyId(null);
    }
  }

  async function handleToggle(serverId: string, enabled: boolean) {
    if (enabled) {
      const server = servers.find((s) => s.id === serverId);
      if (server && needsTeamEnableReview(server)) {
        setConfirmEnableTeam(server);
        return;
      }
    }
    await applyToggle(serverId, enabled);
  }

  async function handleRemove(serverId: string, name: string) {
    setBusyId(serverId);
    try {
      applyRegistryChange(await removeServer(serverId));
      toast.success(`Removed "${name}"`);
    } catch (e) {
      toastError(`Couldn't remove: ${e}`);
    } finally {
      setBusyId(null);
    }
  }

  async function handleToggleAll() {
    if (!profileId || togglingAll) return;
    const enable = enabledCount < servers.length;
    const pendingReview =
      enable && registry
        ? servers.filter((s) => needsTeamEnableReview(s) && !isEnabled(registry, s.id))
            .length
        : 0;
    if (enable && pendingReview > 0 && pendingReview === servers.length - enabledCount) {
      toast.message(
        pendingReview === 1
          ? "That team server still needs review in Teams."
          : `${pendingReview} team servers still need review in Teams.`,
      );
      return;
    }
    setTogglingAll(true);
    try {
      applyRegistryChange(await setAllEnabled(profileId, enable));
      let message = enable ? "Enabled all servers" : "Disabled all servers";
      if (enable && pendingReview > 0) {
        message =
          pendingReview === 1
            ? "Enabled servers. 1 team server still needs review."
            : `Enabled servers. ${pendingReview} team servers still need review.`;
      }
      toast.success(message);
    } catch (e) {
      toastError(`Couldn't update servers: ${e}`);
    } finally {
      setTogglingAll(false);
    }
  }

  async function handleImport() {
    setImporting(true);
    try {
      const preview = await previewImportServers();
      if (preview.length === 0) {
        toast.success("Nothing new to import");
        return;
      }
      setImportPreview(preview);
    } catch (e) {
      toastError(`Couldn't prepare import: ${e}`);
    } finally {
      setImporting(false);
    }
  }

  async function confirmImport(selected: string[]) {
    setImporting(true);
    try {
      const before = registry?.servers.length ?? 0;
      const next = await importServers(selected);
      applyRegistryChange(next);
      const added = next.servers.length - before;
      toast.success(
        added > 0
          ? `Imported ${added} server${added === 1 ? "" : "s"}`
          : "Nothing new to import",
      );
      setImportPreview(null);
    } catch (e) {
      toastError(`Import failed: ${e}`);
    } finally {
      setImporting(false);
    }
  }

  const serverRow = (server: ServerEntry) => (
    <RegistryServerRow
      key={server.id}
      server={server}
      openTools={toolsServerId === server.id}
      registry={registry}
      enabled={registry ? isEnabled(registry, server.id) : false}
      busy={busyId === server.id}
      health={health[server.id]}
      onToggle={(en) => handleToggle(server.id, en)}
      onRemove={() => handleRemove(server.id, server.name)}
      onRegistryChange={applyRegistryChange}
      onReprobe={() => void reprobeAfterMutation().catch(() => {})}
    />
  );

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-screen overflow-hidden bg-background text-foreground">
        <AppSidebar
          registry={registry}
          onRegistryChange={applyRegistryChange}
          view={view}
          onSelectView={selectView}
          onShortcuts={() => setShortcutsOpen(true)}
          onReplayOnboarding={() => {
            setOnboardingStep(0);
            setShowOnboarding(true);
          }}
        />

        <main className="flex min-w-0 flex-1 flex-col">
          <RemovedFeaturesNotice
            registry={registry}
            onRegistryChange={applyRegistryChange}
          />
          <AccessUpgradeNotice
            registry={registry}
            onRegistryChange={applyRegistryChange}
          />
          <header className="app-header flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2 sm:px-6 sm:py-4">
            <div className="flex min-w-0 flex-1 items-center gap-3">
              {view === "clients" && selectedClient && (
                <>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="-ml-2 text-muted-foreground"
                    onClick={() => selectView("clients")}
                    aria-label="Back to clients"
                  >
                    <ArrowLeft className="size-4" />
                    Clients
                  </Button>
                  <div className="h-7 w-px bg-border" aria-hidden="true" />
                  <ClientLogo
                    id={selectedClient.id}
                    name={selectedClient.name}
                    size={32}
                  />
                </>
              )}
              <div className="min-w-0">
                <h1 className="truncate text-lg font-semibold tracking-tight">
                  {view === "activity"
                    ? "Activity"
                    : view === "catalog"
                      ? "Browse catalog"
                      : view === "teams"
                        ? "Teams"
                        : view === "settings"
                          ? "Settings"
                          : view === "clients"
                            ? (selectedClient?.name ?? "Clients")
                            : "Servers"}
                </h1>
                <p className="truncate text-sm text-muted-foreground">
                  {view === "activity"
                    ? "Tool calls routed through Toolport"
                    : view === "catalog"
                      ? "Add MCP servers from the registry"
                      : view === "teams"
                        ? "Share one MCP server set across your team"
                        : view === "settings"
                          ? "Global discovery and security policy"
                          : view === "clients"
                            ? selectedClient
                              ? "MCP client"
                              : "Manage Toolport in your installed AI tools"
                            : loading || !registry
                              ? "Loading…"
                              : "One gateway in front of every MCP server you run"}
                </p>
              </div>
            </div>
            <div className="flex min-w-0 flex-wrap items-center gap-2">
              {view === "servers" && (
                <>
                  <div className="relative">
                    <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      ref={searchRef}
                      value={query}
                      onChange={(e) => setQuery(e.target.value)}
                      placeholder="Search servers"
                      title={`Search servers (/ or ${isMac ? "⌘" : "Ctrl"}F)`}
                      className="h-9 w-32 pl-8 sm:w-44"
                    />
                  </div>
                  <ServerDialog
                    onSaved={setRegistry}
                    existingNames={servers.map((s) => s.name)}
                    trigger={
                      <Button
                        variant="outline"
                        size="sm"
                        title={`Add server (${isMac ? "⌘" : "Ctrl"}N)`}
                      >
                        <Plus className="size-4" />
                        Add server
                      </Button>
                    }
                  />
                  <Button
                    variant="outline"
                    size="sm"
                    title="Browse Toolport's curated server catalog"
                    onClick={() => selectView("catalog")}
                  >
                    <Store className="size-4" />
                    Browse catalog
                  </Button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label="More actions"
                        title="Import and server actions"
                      >
                        <MoreHorizontal className="size-4" />
                      </Button>
                    </DropdownMenuTrigger>

                    <DropdownMenuContent align="end" className="w-38">
                      <DropdownMenuItem onClick={handleImport}>
                        <Download className="mr-2 size-4" />
                        <span>Import</span>
                      </DropdownMenuItem>

                      {servers.length > 0 && (
                        <DropdownMenuItem
                          onClick={() => {
                            // "Disable all" only shows when every server is enabled,
                            // so it turns off `servers.length`. Confirm when that's
                            // more than a couple; "Enable all" and small sets go
                            // straight through.
                            const disabling = enabledCount >= servers.length;
                            if (disabling && servers.length > BULK_DISABLE_CONFIRM_MIN) {
                              setConfirmDisableAll(true);
                            } else {
                              void handleToggleAll();
                            }
                          }}
                          // Gate on the flag handleToggleAll actually sets (togglingAll),
                          // not just busyId, so it can't be re-fired mid-run. Disabled
                          // while a search is active: it acts on ALL servers, so it must
                          // not silently toggle ones hidden by the filter.
                          disabled={togglingAll || busyId !== null || query.trim() !== ""}
                          title={
                            query.trim() !== ""
                              ? "Clear the search to enable or disable all servers"
                              : undefined
                          }
                        >
                          <ServerOff className="mr-2 size-4" />
                          <span>
                            {enabledCount < servers.length ? "Enable all" : "Disable all"}
                          </span>
                        </DropdownMenuItem>
                      )}
                    </DropdownMenuContent>
                  </DropdownMenu>
                </>
              )}
              <Button
                variant="ghost"
                size="icon"
                className="size-8"
                aria-label="Refresh"
                title={`Reload servers, clients, and health (${isMac ? "⌘" : "Ctrl"}R)`}
                onClick={() => load(true)}
                disabled={loading}
              >
                <RefreshCw
                  className={`size-4 ${loading || probing ? "animate-spin" : ""}`}
                />
              </Button>
            </div>
          </header>

          {!backendReachable && (
            <Callout
              variant="warning"
              role="status"
              className="mx-6 mt-3 flex items-center gap-3"
            >
              <WifiOff className="size-4 shrink-0" aria-hidden="true" />
              <span className="min-w-0 flex-1">
                Toolport's backend didn't respond to the last health check. Some features
                may be unavailable, and server status may be stale.
              </span>
              <Button
                variant="outline"
                size="sm"
                className="shrink-0"
                onClick={() => void reprobe().catch(() => {})}
                disabled={probing}
              >
                Retry
              </Button>
            </Callout>
          )}

          <ScrollArea className="min-h-0 flex-1">
            <div className="p-3 sm:p-6">
              <ErrorBoundary
                resetKey={`${view}:${selectedClient?.id ?? ""}`}
                fallback={(err, retry) => <ViewCrash error={err} onRetry={retry} />}
              >
                <Suspense
                  fallback={
                    <div className="flex flex-col gap-2">
                      {Array.from({ length: 6 }).map((_, i) => (
                        <Skeleton key={i} className="h-11 w-full rounded-lg" />
                      ))}
                    </div>
                  }
                >
                  {view === "clients" ? (
                    selectedClient ? (
                      <ClientDetail
                        client={selectedClient}
                        registry={registry}
                        onChanged={load}
                        onRegistryChange={applyRegistryChange}
                      />
                    ) : (
                      <ClientsView
                        clients={clients}
                        registry={registry}
                        loading={loading}
                        onSelectClient={selectClient}
                      />
                    )
                  ) : view === "activity" ? (
                    <ActivityView refreshKey={activityKey} registry={registry} />
                  ) : view === "catalog" ? (
                    <CatalogView registry={registry} onAdded={applyRegistryChange} />
                  ) : view === "teams" ? (
                    <TeamsView
                      registry={registry}
                      onRegistryChange={applyRegistryChange}
                      health={health}
                      onReprobe={() => void reprobeAfterMutation().catch(() => {})}
                    />
                  ) : view === "settings" ? (
                    <SettingsView
                      registry={registry}
                      onRegistryChange={applyRegistryChange}
                    />
                  ) : loading && registry === null ? (
                    <div className="flex flex-col gap-2">
                      {Array.from({ length: 6 }).map((_, i) => (
                        <Skeleton key={i} className="h-11 w-full rounded-lg" />
                      ))}
                    </div>
                  ) : error ? (
                    <ErrorState message={error} />
                  ) : servers.length === 0 ? (
                    <EmptyState
                      importable={importable}
                      onImport={handleImport}
                      onBrowseCatalog={() => selectView("catalog")}
                    />
                  ) : visible.length === 0 ? (
                    <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed px-3 py-6 text-center">
                      <p className="text-sm text-muted-foreground">
                        No servers match "{query}".
                      </p>
                      <button
                        type="button"
                        onClick={() => setQuery("")}
                        className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                      >
                        <X className="size-3.5" />
                        Clear search
                      </button>
                    </div>
                  ) : (
                    <div className="flex flex-col gap-5">
                      <ServerPosture
                        backendReachable={backendReachable}
                        probing={probing}
                        enabled={enabledCount}
                        checked={checkedCount}
                        connected={connectedCount}
                        attention={attentionCount}
                        disabled={servers.length - enabledCount}
                      />
                      {backendReachable && attentionCount > 0 && (
                        <ServerNextAction
                          authServers={authAttention}
                          errorServers={errorAttention}
                        />
                      )}
                      <div className="overflow-hidden rounded-xl border border-border/60 bg-card/40">
                        {visible.map(serverRow)}
                      </div>
                    </div>
                  )}
                </Suspense>
              </ErrorBoundary>
            </div>
          </ScrollArea>
        </main>
      </div>
      <ImportReviewDialog
        open={importPreview !== null}
        items={importPreview ?? []}
        busy={importing}
        onOpenChange={(open) => {
          if (!open && !importing) setImportPreview(null);
        }}
        onConfirm={confirmImport}
      />
      {showOnboarding && registry && (
        <Suspense fallback={null}>
          <Onboarding
            key={onboardingStep}
            initialStep={onboardingStep}
            clients={clients}
            registry={registry}
            onRegistryChange={applyRegistryChange}
            onClientsRefresh={load}
            onBrowseCatalog={() => {
              setShowOnboarding(false);
              setResumeAtConnect(true);
              selectView("catalog");
            }}
            onProbe={reprobe}
            onOpenTools={() => {
              setShowOnboarding(false);
              setToolsServerId(
                registry?.servers.find((server) => isEnabled(registry, server.id))?.id ??
                  registry?.servers[0]?.id ??
                  null,
              );
              selectView("servers");
            }}
            onFinish={finishOnboarding}
          />
        </Suspense>
      )}
      <GitHubStarPrompt
        justOnboarded={justOnboarded}
        onboardingOpen={showOnboarding}
        enabledCount={enabledCount}
        refreshKey={activityKey}
        onVisibleChange={setStarSurface}
      />
      <PendingApprovals />
      <TeamPairingDialog onConnected={openTeams} />
      {/* Quarantine has no global signal otherwise: the first sign used to be an agent
          call failing, with the only fix buried in Settings (SOU-293). */}
      <QuarantineAlert onReview={() => selectView("settings")} />
      <ConfirmDialog
        open={confirmDisableAll}
        onOpenChange={setConfirmDisableAll}
        title="Disable all servers?"
        description={`This turns off all ${servers.length} servers for every client. Clients will lose their tools until you re-enable them.`}
        confirmLabel="Disable all"
        destructive
        onConfirm={handleToggleAll}
      />
      <ConfirmDialog
        open={confirmEnableTeam !== null}
        onOpenChange={(open) => {
          if (!open) setConfirmEnableTeam(null);
        }}
        title={
          confirmEnableTeam ? `Enable "${confirmEnableTeam.name}"?` : "Enable server?"
        }
        description={
          confirmEnableTeam
            ? secretReferenceReview(confirmEnableTeam).join("\n") +
              "\n" +
              (confirmEnableTeam.transport === "stdio" || confirmEnableTeam.command
                ? `This runs a local command on your machine: ${[confirmEnableTeam.command, ...(confirmEnableTeam.args ?? [])].join(" ")}. Only enable it if you recognize and trust this command.`
                : `This connects Toolport to ${confirmEnableTeam.url ?? ""}, using its saved authentication. Verify the destination before enabling it.`)
            : undefined
        }
        confirmLabel="Enable"
        onConfirm={() => {
          if (!confirmEnableTeam) return;
          // Re-check the definition against the one that was reviewed. Team sync runs
          // on a timer, so a push landing while this dialog is open would otherwise
          // enable a command or URL the member never saw - the confirmation carried
          // only the id. If it changed under them, re-open review on the new one
          // instead of enabling it.
          const live = registry?.servers.find((s) => s.id === confirmEnableTeam.id);
          if (!live) {
            setConfirmEnableTeam(null);
            toastError("That server is no longer in your registry.");
            return;
          }
          if (!sameReviewedDefinition(confirmEnableTeam, live)) {
            setConfirmEnableTeam(live);
            toastError(
              "This server changed while you were reviewing it. Check it again.",
            );
            // Reject so ConfirmDialog skips its setOpen(false) - a normal return
            // would close the dialog and onOpenChange(false) would null out the
            // `live` entry we just swapped in, so the re-review never appears.
            throw new Error("definition changed");
          }
          return applyToggle(confirmEnableTeam.id, true, true, confirmEnableTeam);
        }}
      />
      {/* Ctrl+N. Mounted only while open so `autoOpen` fires each time, and unmounted
          on close so the next press starts from a clean form. */}
      {addServerOpen && (
        <ServerDialog
          autoOpen
          onClose={() => setAddServerOpen(false)}
          onSaved={applyRegistryChange}
          existingNames={servers.map((s) => s.name)}
        />
      )}
      {/* `?` cheat sheet: shortcuts nobody can see are shortcuts nobody uses. */}
      <Dialog open={shortcutsOpen} onOpenChange={setShortcutsOpen}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>Keyboard shortcuts</DialogTitle>
          </DialogHeader>
          <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2 text-sm">
            {shortcutHelp(isMac).map((row) => (
              <div key={row.keys} className="contents">
                <dt>
                  <kbd className="rounded border border-border/60 bg-muted px-1.5 py-0.5 font-mono text-xs">
                    {row.keys}
                  </kbd>
                </dt>
                <dd className="text-muted-foreground">{row.what}</dd>
              </div>
            ))}
          </dl>
        </DialogContent>
      </Dialog>
      <Toaster
        theme={resolvedTheme}
        position="bottom-right"
        offset={
          starSurface === "chip"
            ? { bottom: "3.5rem" }
            : starSurface
              ? { bottom: "10rem" }
              : 16
        }
      />
    </TooltipProvider>
  );
}

/** A factual reachability baseline. Security posture remains in Settings; this only
 * summarizes the health probe and never presents a stale or partial check as healthy. */
export function serverPostureCopy({
  backendReachable,
  probing,
  enabled,
  checked,
  connected,
  attention,
  disabled,
}: {
  backendReachable: boolean;
  probing: boolean;
  enabled: number;
  checked: number;
  connected: number;
  attention: number;
  disabled: number;
}) {
  const complete = enabled > 0 && checked === enabled && !probing;
  const healthy = backendReachable && complete && attention === 0;
  const title = !backendReachable
    ? "Reachability status unavailable"
    : enabled === 0
      ? "No servers enabled"
      : probing
        ? "Checking server reachability"
        : checked < enabled
          ? "Reachability check incomplete"
          : attention === 0
            ? `${connected} enabled server${connected === 1 ? "" : "s"} reachable`
            : `${connected} of ${enabled} enabled servers reachable`;
  const detail = !backendReachable
    ? checked > 0
      ? attention > 0
        ? `Last known: ${connected} reachable; ${attention} need${attention === 1 ? "s" : ""} a quick check. Status may be out of date.`
        : `Last known: ${connected} reachable. Status may be out of date.`
      : "The last health check did not complete."
    : enabled === 0
      ? `${disabled} server${disabled === 1 ? "" : "s"} turned off.`
      : probing
        ? `${checked} of ${enabled} checked so far.`
        : checked < enabled
          ? `${checked} of ${enabled} checked. Refresh to try again.`
          : attention > 0
            ? `${attention} need${attention === 1 ? "s" : ""} a quick check.`
            : disabled > 0
              ? `${disabled} turned off.`
              : "Every enabled server is ready.";
  return { healthy, title, detail };
}

function ServerPosture({
  backendReachable,
  probing,
  enabled,
  checked,
  connected,
  attention,
  disabled,
}: {
  backendReachable: boolean;
  probing: boolean;
  enabled: number;
  checked: number;
  connected: number;
  attention: number;
  disabled: number;
}) {
  const { healthy, title, detail } = serverPostureCopy({
    backendReachable,
    probing,
    enabled,
    checked,
    connected,
    attention,
    disabled,
  });

  return (
    <div
      role="status"
      className={`flex items-center gap-3 rounded-xl border px-4 py-3 ${
        healthy ? "border-success/20 bg-success/5" : "border-border/70 bg-card/40"
      }`}
    >
      <div
        className={`grid size-8 shrink-0 place-items-center rounded-lg ${
          healthy
            ? "bg-success/10 text-success"
            : probing
              ? "bg-info/10 text-info"
              : "bg-muted text-muted-foreground"
        }`}
      >
        {healthy ? (
          <CircleCheck className="size-4" />
        ) : (
          <RefreshCw className={`size-4 ${probing ? "animate-spin" : ""}`} />
        )}
      </div>
      <div>
        <p className="text-sm font-semibold">{title}</p>
        <p className="text-xs text-muted-foreground">{detail}</p>
      </div>
    </div>
  );
}

/** One page-level owner for the next useful action. The rows below retain the
 * controls and evidence, but no longer compete with multiple warning summaries. */
function ServerNextAction({
  authServers,
  errorServers,
}: {
  authServers: ServerEntry[];
  errorServers: ServerEntry[];
}) {
  const authCount = authServers.length;
  const errorCount = errorServers.length;
  const title =
    authCount === 1 && errorCount === 0
      ? `Sign in to ${authServers[0].name}`
      : authCount > 0 && errorCount === 0
        ? `Sign in to ${authCount} servers`
        : errorCount === 1 && authCount === 0
          ? `${errorServers[0].name} couldn't start`
          : errorCount > 0 && authCount === 0
            ? `${errorCount} servers couldn't start`
            : `${authCount + errorCount} servers need a quick check`;
  const detail =
    authCount > 0 && errorCount === 0
      ? "Use the actions below to finish setup. Everything else stays available."
      : errorCount > 0 && authCount === 0
        ? "Open the affected rows below for the error and recovery details."
        : `${authCount} need sign-in; ${errorCount} couldn't start. The other servers stay available.`;

  return (
    <div className="flex items-start gap-3 rounded-xl border border-warning/25 bg-card/45 px-4 py-3">
      <div className="grid size-8 shrink-0 place-items-center rounded-lg bg-warning/10 text-warning">
        {authCount > 0 && errorCount === 0 ? (
          <KeyRound className="size-4" />
        ) : (
          <TriangleAlert className="size-4" />
        )}
      </div>
      <div>
        <p className="text-sm font-semibold">{title}</p>
        <p className="text-xs text-muted-foreground">{detail}</p>
      </div>
    </div>
  );
}

function EmptyState({
  importable,
  onImport,
  onBrowseCatalog,
}: {
  importable: number;
  onImport: () => void;
  onBrowseCatalog: () => void;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-4 py-24 text-center">
      <ServerOff className="size-10 text-muted-foreground/50" />
      <div>
        <p className="font-medium">No servers in Toolport yet</p>
        <p className="text-sm text-muted-foreground">
          {importable > 0
            ? `Found ${importable} server${importable === 1 ? "" : "s"} in your installed clients. Import them to get started.`
            : "Browse the catalog to add one, or import servers from a client."}
        </p>
      </div>
      {importable > 0 ? (
        <Button onClick={onImport}>
          <Download className="size-4" />
          Import {importable} from clients
        </Button>
      ) : (
        <Button onClick={onBrowseCatalog}>
          <Store className="size-4" />
          Browse catalog
        </Button>
      )}
    </div>
  );
}

function ViewCrash({ error, onRetry }: { error: Error; onRetry: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-24 text-center">
      <TriangleAlert className="size-10 text-warning" />
      <div>
        <p className="font-medium">Something went wrong in this view</p>
        <p className="max-w-md text-sm text-muted-foreground">
          The rest of Toolport is still running. Try again, or reload the window if it
          keeps happening.
        </p>
        <p className="mt-2 font-mono text-xs text-muted-foreground/70">{error.message}</p>
      </div>
      <div className="flex gap-2">
        <Button variant="outline" onClick={onRetry}>
          Try again
        </Button>
        <Button onClick={() => window.location.reload()}>Reload</Button>
      </div>
    </div>
  );
}

function ErrorState({ message }: { message: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-24 text-center">
      <TriangleAlert className="size-10 text-warning" />
      <div>
        <p className="font-medium">Couldn't reach the backend</p>
        <p className="max-w-md text-sm text-muted-foreground">
          {import.meta.env.DEV ? (
            <>
              Make sure you're running the desktop app with{" "}
              <code className="font-mono">npm run tauri dev</code>, not the browser-only
              dev server.
            </>
          ) : (
            <>Toolport's backend didn't start. Try quitting and reopening the app.</>
          )}
        </p>
        <p className="mt-2 font-mono text-xs text-muted-foreground/70">{message}</p>
      </div>
    </div>
  );
}

export default App;
