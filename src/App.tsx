import { technicalText } from "@/lib/i18n";
import { translate } from "@/lib/i18n";
import { useI18n } from "@/lib/i18n";
import { needsTeamEnableReview, sameReviewedDefinition } from "@/lib/importReview";
import { serverPostureCopy } from "@/lib/serverPosture";
import { type SettingsSubpage } from "@/lib/settingsPages";
import { visibleExecutionText } from "@/lib/visibleExecutionText";
import { isPersonalSync } from "@/lib/personalSync";
import { requestClientRecoveryScan } from "@/lib/clientRecoveryEvents";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import {
  CircleCheck,
  KeyRound,
  MoreHorizontal,
  Download,
  FolderPlus,
  Plus,
  RefreshCw,
  Search,
  Server,
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
  openGatewayLog,
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
import { AddClientLocationDialog } from "@/components/AddClientLocationDialog";
import { PageHeader, PageHeaderActionsSlot } from "@/components/PageHeader";
import { PendingApprovals } from "@/components/PendingApprovals";
import { TeamPairingDialog } from "@/components/TeamPairingDialog";
import { QuarantineAlert } from "@/components/QuarantineAlert";
import { ServerLogo } from "@/components/ServerLogo";
import { RegistryServerRow } from "@/components/RegistryServerRow";
import { SectionHeader } from "@/components/ui/section-header";
import { LazyServerDialog as ServerDialog } from "@/components/LazyServerDialog";
import { ImportReviewDialog } from "@/components/ImportReviewDialog";

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
const SkillsView = lazy(() =>
  import("@/components/SkillsView").then((m) => ({ default: m.SkillsView })),
);
const ActivityView = lazy(() =>
  import("@/components/ActivityView").then((m) => ({ default: m.ActivityView })),
);
const CatalogView = lazy(() =>
  import("@/components/CatalogView").then((m) => ({ default: m.CatalogView })),
);
const ExecutionReview = lazy(() => import("@/components/ExecutionReview"));
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
import type { ConnectAllCard, T3CodeSeen } from "@/components/ClientsView";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import { SkeletonRows } from "@/components/ui/skeleton";
import { EmptyState as SharedEmptyState } from "@/components/ui/empty-state";
import { ErrorCallout, StatusCard } from "@/components/ui/status-card";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Toaster } from "@/components/ui/sonner";
import { useTheme } from "@/lib/theme";
import { fmtTs } from "@/lib/utils";
import { createSingleFlight } from "@/lib/singleFlight";
import {
  modKey,
  resolveShortcut,
  shortcutHelp,
  SKILLS_REFRESH_EVENT,
  SKILLS_SEARCH_EVENT,
} from "@/lib/shortcuts";
import { subscribeToTrayApprovals } from "@/lib/trayApprovals";

/** Above this many servers, "Disable all" asks for confirmation first. */
const BULK_DISABLE_CONFIRM_MIN = 3;

function App() {
  const { t } = useI18n();
  const { resolved: resolvedTheme } = useTheme();
  const [registry, setRegistry] = useState<Registry | null>(null);
  const registryRef = useRef<Registry | null>(null);
  const [clients, setClients] = useState<DetectedClient[]>([]);
  // The T3 Code settings revision these clients match; Clients unmounts behind details.
  const t3Seen = useRef<T3CodeSeen>(null);
  // Connect all's summary stays until dismissed, also while a client's details are open.
  const [connectAllCard, setConnectAllCard] = useState<ConnectAllCard | null>(null);
  const [importPreview, setImportPreview] = useState<ImportItem[] | null>(null);
  const [importing, setImporting] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [togglingAll, setTogglingAll] = useState(false);
  // Gates the "Disable all" bulk action behind a confirm when it turns off more
  // than a couple of servers, so one menu click can't silently kill a big set.
  const [confirmDisableAll, setConfirmDisableAll] = useState(false);
  const [confirmEnableTeam, setConfirmEnableTeam] = useState<ServerEntry | null>(null);
  const [enableReviewError, setEnableReviewError] = useState<string | null>(null);
  const [readyEnableReview, setReadyEnableReview] = useState<ServerEntry | null>(null);
  const [selectedClientId, setSelectedClientId] = useState<string | null>(null);
  const [selectedServerId, setSelectedServerId] = useState<string | null>(null);
  const detailReturnFocus = useRef<HTMLElement | null>(null);
  const [toolsServerId, setToolsServerId] = useState<string | null>(null);
  const [settingsPage, setSettingsPage] = useState<SettingsSubpage>("general");
  const [pendingCount, setPendingCount] = useState(0);
  const [view, setView] = useState<View>("servers");
  const viewRef = useRef(view);
  useEffect(() => {
    viewRef.current = view;
  }, [view]);
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
  const [headerActionsSlot, setHeaderActionsSlot] = useState<HTMLElement | null>(null);
  const [addingLocation, setAddingLocation] = useState(false);
  const [checkingClientRecoveries, setCheckingClientRecoveries] = useState(false);
  const clientScanPending = useRef(false);
  // Keyboard shortcuts (SBS-143). The app had none, which for a developer tool means
  // every interaction is mouse-only.
  const searchRef = useRef<HTMLInputElement>(null);
  const serverSwitches = useRef(new Map<string, HTMLButtonElement>());
  const movedSwitchFocus = useRef<string | null>(null);
  const enableReviewTrigger = useRef<{ id: string; node: HTMLButtonElement } | null>(
    null,
  );
  const registerServerSwitch = useCallback(
    (id: string, node: HTMLButtonElement | null) => {
      const previous = serverSwitches.current.get(id);
      if (node) {
        serverSwitches.current.set(id, node);
      } else {
        // Ref cleanup runs before removal. Restore only the control that still
        // owned focus, never one the user left while its request was pending.
        if (previous && document.activeElement === previous)
          movedSwitchFocus.current = id;
        serverSwitches.current.delete(id);
      }
    },
    [],
  );
  useLayoutEffect(() => {
    const id = movedSwitchFocus.current;
    movedSwitchFocus.current = null;
    if (id && document.activeElement === document.body)
      serverSwitches.current.get(id)?.focus();
  });
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
  // Set by a Refresh or Retry the user chose: the next probe flight to start may
  // ask to unlock the keyring. The window's own probes never do. It stays set
  // until a flight starts, so a request queued behind an automatic probe, or
  // coalesced with others, keeps the user's choice.
  const interactiveProbeRef = useRef(false);
  const takeInteractive = () => {
    const interactive = interactiveProbeRef.current;
    interactiveProbeRef.current = false;
    return interactive;
  };
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
        const results = await probeServers(takeInteractive());
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
    (interactive = false): Promise<ProbeResult[]> => {
      if (interactive) interactiveProbeRef.current = true;
      return probeFlightRef.current.runAfterCurrent(async () => {
        lastProbeRef.current = Date.now();
        setProbing(true);
        try {
          const results = await probeServers(takeInteractive());
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
    },
    [],
  );

  // A row's Retry: the user asked, so it may ask to unlock the keyring.
  const retryProbe = useCallback(
    (): Promise<ProbeResult[]> => reprobeAfterMutation(true),
    [reprobeAfterMutation],
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
  // fires the probe without blocking or toasting. `interactive`: the user asked
  // (Refresh, Ctrl+R), so its probe may ask to unlock the keyring.
  const load = useCallback(
    async (announce = false, interactive = announce) => {
      setLoading(true);
      setError(null);
      setDetailError(null);
      try {
        const [dc, recovery] = await Promise.all([
          detectClients(viewRef.current === "clients"),
          takeRegistryRecoveryNotice(),
        ]);
        // Inspecting locations can adopt an existing gateway and its access.
        const reg = await getRegistry();
        registryRef.current = reg;
        setRegistry(reg);
        setClients(dc);
        if (recovery) {
          const when = fmtTs(recovery.recoveredAtMs);
          const detail =
            recovery.reason === "corrupt"
              ? translate(
                  "The registry file was damaged. Restored from backup ({when}).",
                  { when },
                )
              : recovery.reason === "missing"
                ? translate(
                    "The registry file was missing. Restored from backup ({when}).",
                    { when },
                  )
                : translate("{reason}. Loaded from backup ({when}).", {
                    reason: recovery.reason,
                    when,
                  });
          toast.warning(translate("Registry recovered from backup"), {
            description: recovery.quarantinePath
              ? translate("{value1} A copy of the bad file was saved for inspection.", {
                  value1: detail,
                })
              : detail,
            duration: 12_000,
          });
        }
        loadedOnce.current = true;
        setActivityKey((k) => k + 1);
        if (announce) {
          try {
            const results = await reprobeAfterMutation(interactive);
            if (results.length > 0) {
              const up = results.filter((r) => r.ok).length;
              toast.success(
                translate("{value1} of {value2} servers healthy", {
                  value1: up,
                  value2: results.length,
                }),
              );
            } else {
              toast.success(translate("Refreshed"));
            }
          } catch {
            toast.warning(translate("Refreshed, but couldn't check server health"));
          }
        } else if (interactive) {
          void reprobeAfterMutation(true).catch(() => {});
        } else {
          void reprobe().catch(() => {});
        }
      } catch (e) {
        // After the first successful load, a refresh failure shouldn't blow away a
        // working list. Surface it as a toast and keep what's on screen.
        if (loadedOnce.current) {
          setDetailError(String(e));
          toastError(translate("Couldn't refresh: {value1}", { value1: String(e) }));
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Start the backend load on mount without deferring its loading state.
    load();
  }, [load]);

  const manualRefresh = useCallback(async () => {
    if (viewRef.current !== "clients") return load(true);
    if (clientScanPending.current) return;
    clientScanPending.current = true;
    setCheckingClientRecoveries(true);
    const focused = document.activeElement as HTMLElement | null;
    try {
      await Promise.all([requestClientRecoveryScan(), load(false, true)]);
    } catch (cause: unknown) {
      toastError(
        typeof cause === "string"
          ? cause
          : translate(
              "Recovery did not finish reporting its result. Choose Retry to check the current status.",
            ),
      );
    } finally {
      clientScanPending.current = false;
      setCheckingClientRecoveries(false);
      // Preserve a keyboard refresh trigger; never steal focus the user moved.
      requestAnimationFrame(() => {
        if (document.activeElement !== document.body) return;
        if (focused?.isConnected) focused.focus();
        else document.querySelector<HTMLButtonElement>("[data-client-refresh]")?.focus();
      });
    }
  }, [load]);

  useEffect(() => {
    if (view !== "clients") return;
    let active = true;
    void detectClients(true)
      .then(async (detected) => {
        const registry = await getRegistry();
        if (!active) return;
        registryRef.current = registry;
        setRegistry(registry);
        setClients(detected);
      })
      .catch((error) => {
        if (active) {
          setDetailError(String(error));
          toastError(
            translate("Couldn't refresh clients: {value1}", { value1: String(error) }),
          );
        }
      });
    return () => {
      active = false;
    };
  }, [view]);

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
        t(
          "You were removed from the team. Its shared servers have been removed from your setup.",
        ),
      );
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, [t]);

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
          ? t("{value1} is still starting an old gateway", { value1: names })
          : t("{value1} are still starting an old gateway", { value1: names }),
        {
          description: t(
            "They cached the old path at startup, so restarting them is the only way to pick up the current gateway. Settings keeps the list.",
          ),
          duration: 10000,
        },
      );
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, [t]);

  // Required Teams work lives in Rust for the application's lifetime. The webview
  // observes results; hiding/minimizing it cannot stop config delivery or reporting.
  useEffect(() => {
    const unlisten = listen<Registry>("team-sync-registry", (event) => {
      applyRegistryChange(event.payload);
      if (!event.payload.team?.personalSyncState?.error)
        toast.dismiss("personal-sync-background");
    });
    const unlistenErrors = listen<string>("team-sync-error", (event) => {
      toastError(translate("Sync needs attention"), {
        id: "personal-sync-background",
        description: event.payload.replace(/team server/g, "sync service"),
      });
    });
    return () => {
      void unlisten.then((stop) => stop());
      void unlistenErrors.then((stop) => stop());
    };
  }, [applyRegistryChange]);

  function selectServer(id: string, tab: "overview" | "tools" = "overview") {
    detailReturnFocus.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailError(null);
    setSelectedServerId(id);
    setToolsServerId(tab === "tools" ? id : null);
    setView("servers");
  }

  function backFromDetail(v: View) {
    const previous = detailReturnFocus.current;
    selectView(v);
    requestAnimationFrame(() => {
      if (previous?.isConnected) previous.focus();
      else {
        const id = v === "servers" ? selectedServerId : selectedClientId;
        const attribute = v === "servers" ? "data-detail-open" : "data-client-id";
        const row = [...document.querySelectorAll<HTMLElement>(`[${attribute}]`)].find(
          (node) => node.getAttribute(attribute) === id,
        );
        (row ?? document.querySelector<HTMLElement>("main button"))?.focus();
      }
    });
  }

  function selectClient(id: string) {
    detailReturnFocus.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailError(null);
    setSelectedClientId(id);
    setView("clients");
  }

  // Top-level destinations leave any selected client detail behind.
  function selectView(v: View) {
    setSelectedServerId(null);
    setToolsServerId(null);
    setDetailError(null);
    if (v === "settings") setSettingsPage("general");
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
          // Skills has its own search. Elsewhere, search lives on the servers list; go
          // there first so the shortcut is not a silent no-op from another view.
          if (viewRef.current === "skills") {
            window.dispatchEvent(new Event(SKILLS_SEARCH_EVENT));
            break;
          }
          selectView("servers");
          focusSearchWhenMounted();
          break;
        case "addServer":
          e.preventDefault();
          setAddServerOpen(true);
          break;
        case "refresh":
          e.preventDefault();
          if (viewRef.current === "skills")
            window.dispatchEvent(new Event(SKILLS_REFRESH_EVENT));
          else void manualRefresh();
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
  }, [load, manualRefresh]);

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
  // Not failing: not checked, because the keyring is locked.
  const lockedAttention = attentionServers.filter(
    (s) => !health[s.id]?.authRequired && health[s.id]?.keyringLocked,
  );
  const errorAttention = attentionServers.filter(
    (s) => !health[s.id]?.authRequired && !health[s.id]?.keyringLocked,
  );

  // Count what would actually be imported: the backend's candidates, deduped
  // across clients by connection as the import does. Using raw server counts here
  // made the banner promise imports that the importer then correctly skipped.
  const importable = new Set(
    clients.flatMap(
      (c) =>
        c.importableKeys ??
        importableServers(c, registry).map((s) => s.name.toLowerCase()),
    ),
  ).size;
  const selectedServer = selectedServerId
    ? servers.find((s) => s.id === selectedServerId)
    : undefined;
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Latch the first-run wizard after the backend determines this is a fresh setup.
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
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Resume the wizard on navigation with the existing handoff timing.
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
      if (reviewed) setEnableReviewError(null);
    } catch (e) {
      if (reviewed) {
        setEnableReviewError(e instanceof Error ? e.message : String(e));
        throw e;
      }
      toastError(translate("Couldn't toggle: {value1}", { value1: String(e) }));
    } finally {
      setBusyId(null);
    }
  }

  async function handleToggle(serverId: string, enabled: boolean) {
    if (enabled) {
      const server = servers.find((s) => s.id === serverId);
      if (server && needsTeamEnableReview(server)) {
        const node = serverSwitches.current.get(serverId);
        enableReviewTrigger.current =
          node && document.activeElement === node ? { id: serverId, node } : null;
        setEnableReviewError(null);
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
      toast.success(translate('Removed "{value1}"', { value1: name }));
      return true;
    } catch (e) {
      toastError(translate("Couldn't remove: {value1}", { value1: String(e) }));
      return false;
    } finally {
      setBusyId(null);
    }
  }

  async function handleToggleAll(enable: boolean) {
    if (!profileId || togglingAll) return;
    const pendingReview =
      enable && registry
        ? servers.filter((s) => needsTeamEnableReview(s) && !isEnabled(registry, s.id))
            .length
        : 0;
    if (enable && pendingReview > 0 && pendingReview === servers.length - enabledCount) {
      toast.message(
        pendingReview === 1
          ? translate("That team server still needs review in Teams.")
          : translate("{value1} team servers still need review in Teams.", {
              value1: pendingReview,
            }),
      );
      return;
    }
    setTogglingAll(true);
    try {
      applyRegistryChange(await setAllEnabled(profileId, enable));
      let message = enable
        ? translate("Enabled all servers")
        : translate("Disabled all servers");
      if (enable && pendingReview > 0) {
        message =
          pendingReview === 1
            ? translate("Enabled servers. 1 team server still needs review.")
            : translate("Enabled servers. {value1} team servers still need review.", {
                value1: pendingReview,
              });
      }
      toast.success(message);
    } catch (e) {
      toastError(translate("Couldn't update servers: {value1}", { value1: String(e) }));
    } finally {
      setTogglingAll(false);
    }
  }

  async function handleImport() {
    setImporting(true);
    try {
      const preview = await previewImportServers();
      if (preview.length === 0) {
        toast.success(translate("Nothing new to import"));
        return;
      }
      setImportPreview(preview);
    } catch (e) {
      toastError(translate("Couldn't prepare import: {value1}", { value1: String(e) }));
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
      // Detection decides what each client still has to import; scan again.
      void detectClients(false)
        .then(setClients)
        .catch(() => {});
      const added = next.servers.length - before;
      toast.success(
        added > 0
          ? added === 1
            ? translate("Imported {value1} server", { value1: added })
            : translate("Imported {value1} servers", { value1: added })
          : translate("Nothing new to import"),
      );
      setImportPreview(null);
    } catch (e) {
      toastError(translate("Import failed: {value1}", { value1: String(e) }));
    } finally {
      setImporting(false);
    }
  }

  const syncTitle =
    !registry?.team || isPersonalSync(registry) ? translate("Sync") : translate("Team");
  const pageTitle =
    view === "activity"
      ? t("Activity")
      : view === "skills"
        ? t("Skills")
        : view === "catalog"
          ? translate("Catalog")
          : view === "teams"
            ? syncTitle
            : view === "settings"
              ? translate("Settings")
              : view === "clients"
                ? (selectedClient?.name ?? translate("Clients"))
                : (selectedServer?.name ?? translate("Servers"));
  const pageSubtitle =
    view === "activity"
      ? t("Tool calls routed through Toolport")
      : view === "skills"
        ? t("Skills and rules each agent loads, and where they live")
        : view === "catalog"
          ? translate("Add MCP servers from the registry")
          : view === "teams"
            ? !registry?.team || isPersonalSync(registry)
              ? translate("Your setup, on every machine")
              : translate("Share one MCP server set across your team")
            : view === "settings"
              ? translate("Global discovery and security policy")
              : view === "clients"
                ? selectedClient
                  ? translate("MCP client")
                  : translate("Manage Toolport in your installed AI tools")
                : selectedServer
                  ? translate("MCP server")
                  : error
                    ? translate("Servers unavailable")
                    : loading || !registry
                      ? translate("Loading…")
                      : translate("One gateway in front of every MCP server you run");

  const serverRow = (server: ServerEntry) => (
    <RegistryServerRow
      key={server.id}
      server={server}
      onOpen={(tab) => selectServer(server.id, tab)}
      registry={registry}
      enabled={registry ? isEnabled(registry, server.id) : false}
      busy={busyId === server.id}
      health={health[server.id]}
      onToggle={(en) => handleToggle(server.id, en)}
      onSwitchRef={registerServerSwitch}
      onRemove={() => handleRemove(server.id, server.name)}
      onRegistryChange={applyRegistryChange}
      onReprobe={() => void retryProbe().catch(() => {})}
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
          settingsPage={settingsPage}
          onSettingsPageChange={setSettingsPage}
          pendingCount={pendingCount}
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
          <PageHeader
            title={pageTitle}
            subtitle={pageSubtitle}
            back={
              view === "catalog"
                ? {
                    label: translate("Servers"),
                    ariaLabel: translate("Back to servers"),
                    onClick: () => selectView("servers"),
                  }
                : view === "clients" && selectedClientId
                  ? {
                      label: t("Clients"),
                      ariaLabel: t("Back to clients"),
                      onClick: () => backFromDetail("clients"),
                    }
                  : view === "servers" && selectedServerId
                    ? {
                        label: t("Servers"),
                        ariaLabel: t("Back to servers"),
                        onClick: () => backFromDetail("servers"),
                      }
                    : undefined
            }
            leading={
              view === "clients" && selectedClient ? (
                <ClientLogo id={selectedClient.id} name={selectedClient.name} size={32} />
              ) : view === "servers" && selectedServer ? (
                <ServerLogo
                  name={selectedServer.name}
                  transport={selectedServer.transport}
                  size={32}
                />
              ) : undefined
            }
            actionsSlotRef={setHeaderActionsSlot}
            actions={
              <>
                {view === "servers" && !selectedServerId && (
                  <>
                    <div className="relative">
                      <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
                      <Input
                        ref={searchRef}
                        value={query}
                        onChange={(e) => setQuery(e.target.value)}
                        placeholder={translate("Search servers")}
                        title={translate("Search servers (/ or {value1}F)", {
                          value1: modKey(isMac),
                        })}
                        className="h-8 w-32 pl-8 sm:w-44"
                      />
                    </div>
                    <ServerDialog
                      onSaved={setRegistry}
                      existingNames={servers.map((s) => s.name)}
                      trigger={
                        <Button
                          variant="outline"

                          title={isMac ? t("Add server (⌘N)") : t("Add server (Ctrl+N)")}
                        >
                          <Plus className="size-4" /> {t("Add server")}{" "}
                        </Button>
                      }
                    />
                    <Button
                      variant="outline"

                      title={translate("Browse Toolport's curated server catalog")}
                      onClick={() => selectView("catalog")}
                    >
                      <Store className="size-4" />
                      {translate("Browse catalog")}
                    </Button>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={translate("Import and server actions")}
                          title={translate("Import and server actions")}
                        >
                          <MoreHorizontal className="size-4" />
                        </Button>
                      </DropdownMenuTrigger>

                      <DropdownMenuContent align="end" className="w-38">
                        <DropdownMenuItem onClick={handleImport}>
                          <Download className="mr-2 size-4" />
                          <span>{t("Import")}</span>
                        </DropdownMenuItem>

                        {/* Both bulk actions, named as in the GTK menu. They act on
                            every server, so they stay off while a search hides some,
                            and while a bulk change is running. */}
                        {servers.length > 0 && (
                          <>
                            <DropdownMenuItem
                              onClick={() => void handleToggleAll(true)}
                              disabled={
                                togglingAll ||
                                busyId !== null ||
                                query.trim() !== "" ||
                                enabledCount >= servers.length
                              }
                              title={
                                query.trim() !== ""
                                  ? t("Clear the search to enable or disable all servers")
                                  : undefined
                              }
                            >
                              <Server className="mr-2 size-4" />
                              <span>{t("Enable all reviewed servers")}</span>
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              onClick={() => {
                                // Turning off more than a couple asks first.
                                if (enabledCount > BULK_DISABLE_CONFIRM_MIN)
                                  setConfirmDisableAll(true);
                                else void handleToggleAll(false);
                              }}
                              disabled={
                                togglingAll ||
                                busyId !== null ||
                                query.trim() !== "" ||
                                enabledCount === 0
                              }
                              title={
                                query.trim() !== ""
                                  ? t("Clear the search to enable or disable all servers")
                                  : undefined
                              }
                            >
                              <ServerOff className="mr-2 size-4" />
                              <span>{t("Disable all servers")}</span>
                            </DropdownMenuItem>
                          </>
                        )}
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </>
                )}
                {view === "clients" && !selectedClient && (
                  <>
                    <Button
                      variant="outline"

                      title={translate(
                        "Add a config folder for a client Toolport did not find on its own",
                      )}
                      onClick={() => setAddingLocation(true)}
                    >
                      <FolderPlus className="size-4" />
                      {translate("Add client location")}
                    </Button>
                    <Button
                      variant="outline"

                      title={translate("Review servers found in client configurations")}
                      onClick={handleImport}
                    >
                      <Download className="size-4" />
                      {t("Import")}
                    </Button>
                  </>
                )}
                {/* Skills has its own Scan again; this one reloads servers and clients. */}
                {view !== "skills" && (
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={
                      view === "servers"
                        ? t("Check servers again")
                        : view === "activity"
                          ? t("Refresh activity")
                          : t("Refresh")
                    }
                    data-client-refresh={view === "clients" ? "" : undefined}
                    title={
                      isMac
                        ? t("Reload servers, clients, and health (⌘R)")
                        : t("Reload servers, clients, and health (Ctrl+R)")
                    }
                    onClick={() => void manualRefresh()}
                    disabled={loading || checkingClientRecoveries}
                  >
                    <RefreshCw
                      className={`size-4 ${String(loading || probing || checkingClientRecoveries ? "animate-spin" : "")}`}
                    />
                  </Button>
                )}
              </>
            }
          />

          {!backendReachable && (
            <Callout
              variant="warning"
              role="status"
              className="mx-6 mt-3 flex items-center gap-3"
            >
              <WifiOff className="size-4 shrink-0" aria-hidden="true" />
              <span className="min-w-0 flex-1">
                {" "}
                {t(
                  "Toolport's backend didn't respond to the last health check. Some features may be unavailable, and server status may be stale.",
                )}{" "}
              </span>
              <Button
                variant="outline"
                size="sm"
                className="shrink-0"
                onClick={() => {
                  void reprobeAfterMutation(true).catch(() => {});
                }}
                disabled={probing}
              >
                {" "}
                {t("Retry")}{" "}
              </Button>
            </Callout>
          )}

          <PageHeaderActionsSlot value={headerActionsSlot}>
            <ScrollArea
              key={`${view}:${selectedServerId ?? selectedClientId ?? ""}`}
              className="min-h-0 flex-1"
            >
              <div className="p-3 sm:p-6">
                <ErrorBoundary
                  resetKey={`${view}:${selectedClient?.id ?? ""}`}
                  fallback={(err, retry) => <ViewCrash error={err} onRetry={retry} />}
                >
                  <Suspense fallback={<SkeletonRows label={t("Loading…")} />}>
                    {view === "clients" ? (
                      selectedClientId && (error || detailError) ? (
                        <ErrorState
                          message={(error || detailError)!}
                          onRetry={() => load(true)}
                        />
                      ) : selectedClientId && !selectedClient ? (
                        <SharedEmptyState
                          icon={<ServerOff />}
                          title={t("Client is no longer available")}
                          action={
                            <Button
                              variant="outline"
                              onClick={() => backFromDetail("clients")}
                            >
                              {t("Back to clients")}
                            </Button>
                          }
                        />
                      ) : selectedClient ? (
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
                          error={error}
                          onSelectClient={selectClient}
                          onChanged={load}
                          t3Seen={t3Seen}
                          connectAllCard={connectAllCard}
                          onConnectAllCard={setConnectAllCard}
                        />
                      )
                    ) : view === "skills" ? (
                      <SkillsView />
                    ) : view === "activity" ? (
                      <ActivityView
                        refreshKey={activityKey}
                        registry={registry}
                        onReviewQuarantine={() => {
                          setSettingsPage("safety");
                          setView("settings");
                        }}
                      />
                    ) : view === "catalog" ? (
                      <CatalogView registry={registry} onAdded={applyRegistryChange} />
                    ) : view === "teams" ? (
                      <TeamsView
                        registry={registry}
                        onRegistryChange={applyRegistryChange}
                        health={health}
                        onReprobe={() => void retryProbe().catch(() => {})}
                        onOpenServers={() => selectView("servers")}
                      />
                    ) : view === "settings" ? (
                      <SettingsView
                        page={settingsPage}
                        pendingCount={pendingCount}
                        registry={registry}
                        onRegistryChange={applyRegistryChange}
                      />
                    ) : selectedServerId ? (
                      error || detailError ? (
                        <ErrorState
                          message={(error || detailError)!}
                          onRetry={() => load(true)}
                        />
                      ) : selectedServer ? (
                        <RegistryServerRow
                          key={selectedServer.id}
                          detail
                          openTools={toolsServerId === selectedServer.id}
                          server={selectedServer}
                          registry={registry}
                          enabled={!!registry && isEnabled(registry, selectedServer.id)}
                          busy={busyId === selectedServer.id}
                          health={health[selectedServer.id]}
                          onToggle={(en) => handleToggle(selectedServer.id, en)}
                          onSwitchRef={registerServerSwitch}
                          onRemove={async () => {
                            if (
                              await handleRemove(selectedServer.id, selectedServer.name)
                            )
                              backFromDetail("servers");
                          }}
                          onRegistryChange={applyRegistryChange}
                          onReprobe={() => void retryProbe().catch(() => {})}
                        />
                      ) : (
                        <SharedEmptyState
                          icon={<ServerOff />}
                          title={t("Server is no longer available")}
                          action={
                            <Button
                              variant="outline"
                              onClick={() => backFromDetail("servers")}
                            >
                              {t("Back to servers")}
                            </Button>
                          }
                        />
                      )
                    ) : loading && registry === null ? (
                      <SkeletonRows label={t("Loading servers… ")} />
                    ) : error ? (
                      <ErrorState message={error} onRetry={() => load(true)} />
                    ) : servers.length === 0 ? (
                      <ServersEmptyState
                        importable={importable}
                        onImport={handleImport}
                        onAddServer={() => setAddServerOpen(true)}
                        onBrowseCatalog={() => selectView("catalog")}
                      />
                    ) : visible.length === 0 ? (
                      <SharedEmptyState
                        icon={<Search />}
                        title={t('No servers match "{query}".', { query })}
                        description={t("Try a different server name or transport.")}
                        action={
                          <Button variant="outline" onClick={() => setQuery("")}>
                            <X /> {t("Clear search")}
                          </Button>
                        }
                      />
                    ) : (
                      <div className="flex flex-col gap-6">
                        {backendReachable &&
                          (attentionCount > 0 ? (
                            <ServerNextAction
                              authServers={authAttention}
                              errorServers={errorAttention}
                              lockedServers={lockedAttention}
                            />
                          ) : (
                            <ServerPosture
                              probing={probing}
                              enabled={enabledCount}
                              checked={checkedCount}
                              connected={connectedCount}
                              disabled={servers.length - enabledCount}
                            />
                          ))}
                        {/* Turned-off servers get their own group, as in the GTK shell. */}
                        {[
                          {
                            title: t("Servers"),
                            rows: visible.filter(
                              (server) => registry && isEnabled(registry, server.id),
                            ),
                          },
                          {
                            title: t("Turned off"),
                            rows: visible.filter(
                              (server) => !registry || !isEnabled(registry, server.id),
                            ),
                          },
                        ]
                          .filter((group) => group.rows.length > 0)
                          .map((group) => (
                            <section key={group.title}>
                              <SectionHeader count={group.rows.length}>
                                {group.title}
                              </SectionHeader>
                              <div className="toolport-list">
                                {group.rows.map(serverRow)}
                              </div>
                            </section>
                          ))}
                      </div>
                    )}
                  </Suspense>
                </ErrorBoundary>
              </div>
            </ScrollArea>
          </PageHeaderActionsSlot>
        </main>
        <AddClientLocationDialog
          open={addingLocation}
          onOpenChange={setAddingLocation}
          onAdded={async (id) => {
            await load();
            selectClient(id);
          }}
        />
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
              selectView("servers");
              const id =
                servers.find((server) => !!registry && isEnabled(registry, server.id))
                  ?.id ??
                servers[0]?.id ??
                null;
              if (id) selectServer(id, "tools");
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
      <PendingApprovals onCountChange={setPendingCount} />
      <TeamPairingDialog onConnected={openTeams} />
      {/* Quarantine has no global signal otherwise: the first sign used to be an agent
          call failing, with the only fix buried in Settings (SOU-293). */}
      <QuarantineAlert
        onReview={() => {
          selectView("settings");
          setSettingsPage("safety");
        }}
      />
      <ConfirmDialog
        open={confirmDisableAll}
        onOpenChange={setConfirmDisableAll}
        title={translate("Disable all servers?")}
        description={translate(
          "This turns off all {value1} servers for every client. Clients will lose their tools until you re-enable them.",
          { value1: String(enabledCount) },
        )}
        confirmLabel={translate("Disable all")}
        destructive
        onConfirm={() => handleToggleAll(false)}
      />
      <ConfirmDialog
        open={confirmEnableTeam !== null}
        onCloseAutoFocus={(event) => {
          const trigger = enableReviewTrigger.current;
          enableReviewTrigger.current = null;
          // This controlled dialog has no Radix trigger. Return to the current
          // switch on cancel or success, provided focus has not moved elsewhere.
          if (trigger && document.activeElement === document.body) {
            const current = serverSwitches.current.get(trigger.id);
            if (current) {
              event.preventDefault();
              current.focus();
            }
          }
        }}
        onOpenChange={(open) => {
          if (!open) {
            setEnableReviewError(null);
            setConfirmEnableTeam(null);
          }
        }}
        title={
          confirmEnableTeam
            ? translate('Enable "{value1}"?', {
                value1: String(visibleExecutionText(confirmEnableTeam.name)),
              })
            : translate("Enable server?")
        }
        description={
          confirmEnableTeam ? (
            <>
              <Suspense fallback={<p>{t("Loading definition...")}</p>}>
                <ExecutionReview
                  server={confirmEnableTeam}
                  onReady={setReadyEnableReview}
                />
              </Suspense>
              {enableReviewError && (
                <p role="alert" className="mt-3 text-destructive">
                  {enableReviewError}
                </p>
              )}
            </>
          ) : undefined
        }
        contentClassName="sm:max-w-2xl"
        confirmLabel={translate("Enable")}
        confirmDisabled={readyEnableReview !== confirmEnableTeam}
        onConfirm={() => {
          if (!confirmEnableTeam) return;
          if (readyEnableReview !== confirmEnableTeam)
            throw new Error(
              translate("Wait for the definition to load before enabling."),
            );
          // Re-check the definition against the one that was reviewed. Team sync runs
          // on a timer, so a push landing while this dialog is open would otherwise
          // enable a command or URL the member never saw - the confirmation carried
          // only the id. If it changed under them, re-open review on the new one
          // instead of enabling it.
          const live = registry?.servers.find((s) => s.id === confirmEnableTeam.id);
          if (!live) {
            setEnableReviewError(null);
            setConfirmEnableTeam(null);
            toastError(translate("That server is no longer in your registry."));
            return;
          }
          if (!sameReviewedDefinition(confirmEnableTeam, live)) {
            setEnableReviewError(null);
            setConfirmEnableTeam(live);
            toastError(
              translate(
                "This server changed while you were reviewing it. Check it again.",
              ),
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
            <DialogTitle>{t("Keyboard shortcuts")}</DialogTitle>
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
        position="bottom-center"
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

function ServerPosture({
  probing,
  enabled,
  checked,
  connected,
  disabled,
}: {
  probing: boolean;
  enabled: number;
  checked: number;
  connected: number;
  disabled: number;
}) {
  useI18n();
  // Shown only when the backend answers and nothing needs attention: the backend banner and
  // ServerNextAction already own those cases, and two banners must not say the same thing.
  const { healthy, title, detail } = serverPostureCopy({
    backendReachable: true,
    probing,
    enabled,
    checked,
    connected,
    attention: 0,
    disabled,
  });

  return (
    <StatusCard
      role="status"
      variant={healthy ? "success" : "info"}
      icon={healthy ? <CircleCheck /> : <RefreshCw />}
      title={title}
      detail={detail}
    />
  );
}

/** One page-level owner for the next useful action. The rows below retain the
 * controls and evidence, but no longer compete with multiple warning summaries. */
function ServerNextAction({
  authServers,
  errorServers,
  lockedServers = [],
}: {
  authServers: ServerEntry[];
  errorServers: ServerEntry[];
  lockedServers?: ServerEntry[];
}) {
  const { t } = useI18n();
  const authCount = authServers.length;
  // Servers the window did not check because the keyring is locked: on their
  // own they only need an unlock; alongside other problems they count with
  // those that could not start.
  const lockedOnly =
    lockedServers.length > 0 && authCount === 0 && errorServers.length === 0;
  if (lockedOnly) {
    return (
      <StatusCard
        variant="warning"
        icon={<KeyRound />}
        title={t("Keyring locked")}
        detail={t("Unlock your keyring, then choose Retry.")}
      />
    );
  }
  // Servers waiting for the keyring are named for that, as GTK's summary does,
  // not counted with servers that couldn't start.
  if (lockedServers.length > 0) {
    const parts = [
      authCount === 1
        ? t("{value0} needs sign-in", { value0: authCount })
        : authCount > 1
          ? t("{value0} need sign-in", { value0: authCount })
          : null,
      errorServers.length > 0
        ? t("{value0} failing", { value0: errorServers.length })
        : null,
      t("{value0} waiting for the keyring", { value0: lockedServers.length }),
    ].filter((part): part is string => part !== null);
    return (
      <StatusCard
        variant={errorServers.length > 0 ? "danger" : "warning"}
        icon={<TriangleAlert />}
        title={t("{value1} servers need a quick check", {
          value1: authCount + errorServers.length + lockedServers.length,
        })}
        detail={parts.join(" · ")}
      />
    );
  }
  const errorCount = errorServers.length;
  const title =
    authCount === 1 && errorCount === 0
      ? t("Sign in to {value1}", { value1: authServers[0].name })
      : authCount > 0 && errorCount === 0
        ? t("Sign in to {value1} servers", { value1: authCount })
        : errorCount === 1 && authCount === 0
          ? t("{value1} couldn't start", { value1: errorServers[0].name })
          : errorCount > 0 && authCount === 0
            ? t("{value1} servers couldn't start", { value1: errorCount })
            : t("{value1} servers need a quick check", {
                value1: authCount + errorCount,
              });
  const detail =
    authCount > 0 && errorCount === 0
      ? t("Use the actions below to finish setup. Everything else stays available.")
      : errorCount > 0 && authCount === 0
        ? t("Open the affected rows below for the error and recovery details.")
        : t(
            "{value1} need sign-in; {value2} couldn't start. The other servers stay available.",
            { value1: authCount, value2: errorCount },
          );

  return (
    <StatusCard
      variant={authCount === 0 ? "danger" : "warning"}
      icon={authCount > 0 && errorCount === 0 ? <KeyRound /> : <TriangleAlert />}
      title={title}
      detail={detail}
    />
  );
}

function ServersEmptyState({
  importable,
  onImport,
  onAddServer,
  onBrowseCatalog,
}: {
  importable: number;
  onImport: () => void;
  onAddServer: () => void;
  onBrowseCatalog: () => void;
}) {
  const { t } = useI18n();
  return (
    <SharedEmptyState
      icon={<ServerOff />}
      title={t("No servers yet")}
      description={
        importable > 0
          ? importable === 1
            ? t(
                "Found {value1} server in your installed clients. Import them to get started.",
                { value1: importable },
              )
            : t(
                "Found {value1} servers in your installed clients. Import them to get started.",
                { value1: importable },
              )
          : t("Connect a local command or a remote endpoint once; every client gets it.")
      }
      action={
        <>
          <Button variant="outline" onClick={onAddServer}>
            <Plus /> {t("Add server")}
          </Button>
          {importable > 0 ? (
            <Button variant="outline" onClick={onImport}>
              <Download /> {t("Import")} {importable} {t("from clients")}
            </Button>
          ) : (
            <Button variant="outline" onClick={onBrowseCatalog}>
              <Store /> {t("Browse catalog")}
            </Button>
          )}
        </>
      }
    />
  );
}

function viewLog() {
  void openGatewayLog().catch((e: unknown) => toastError(String(e)));
}

function ViewCrash({ error, onRetry }: { error: Error; onRetry: () => void }) {
  const { t } = useI18n();
  return (
    <ErrorCallout
      title={t("Something went wrong in this view")}
      cause={
        <>
          {t(
            "The rest of Toolport is still running. Try again, or reload the window if it keeps happening.",
          )}
          <span className="mt-1 block font-mono">{error.message}</span>
        </>
      }
      onRetry={onRetry}
      retryLabel={t("Try again")}
      onViewLog={viewLog}
      actions={
        <Button variant="outline" size="sm" onClick={() => window.location.reload()}>
          {t("Reload")}
        </Button>
      }
    />
  );
}

function ErrorState({ message, onRetry }: { message: string; onRetry: () => void }) {
  const { t } = useI18n();
  return (
    <ErrorCallout
      title={t("Couldn't reach the backend")}
      cause={
        <>
          {import.meta.env.DEV ? (
            <>
              {t("Make sure you're running the desktop app with")}{" "}
              <code className="font-mono">{technicalText("npm run tauri dev")}</code>
              {t(", not the browser-only dev server.")}
            </>
          ) : (
            t("Toolport's backend didn't start. Try quitting and reopening the app.")
          )}
          <span className="mt-1 block font-mono">{message}</span>
        </>
      }
      onRetry={onRetry}
      onViewLog={viewLog}
    />
  );
}

export default App;
