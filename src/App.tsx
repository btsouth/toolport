import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeft,
  ChevronDown,
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
  mainWindowVisible,
  previewImportServers,
  probeServers,
  removeServer,
  setAllEnabled,
  setServerEnabled,
  teamSyncWait,
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
import { AppSidebar } from "@/components/AppSidebar";
import { ClientLogo } from "@/components/ClientLogo";
import { PendingApprovals } from "@/components/PendingApprovals";
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
const PlaygroundView = lazy(() =>
  import("@/components/PlaygroundView").then((m) => ({ default: m.PlaygroundView })),
);
const RulesView = lazy(() =>
  import("@/components/RulesView").then((m) => ({ default: m.RulesView })),
);
const AgentPermissionsView = lazy(() =>
  import("@/components/AgentPermissionsView").then((m) => ({
    default: m.AgentPermissionsView,
  })),
);
const HooksView = lazy(() =>
  import("@/components/HooksView").then((m) => ({ default: m.HooksView })),
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
  // Set when the wizard is completed in this session, which is the only trigger
  // for the GitHub star card (see GitHubStarPrompt).
  const [justOnboarded, setJustOnboarded] = useState(false);
  // The star prompt shares the bottom-right corner with the toast stack, so
  // toasts are offset upward while it is on screen instead of covering it.
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
        value?.activeProfileId ?? value?.profiles[0]?.id;
      const enabledIds = (value: Registry | null) =>
        new Set(
          value?.profiles.find((profile) => profile.id === activeId(value))
            ?.enabledServerIds ?? [],
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
              : `The registry file was missing. Restored from backup (${when}).`;
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

  // Keep a team member's shared server set AND security policy current even if they never
  // open the Teams tab: an admin tightening a force-quarantine / approval policy must reach
  // every member near-instantly, not just those who happen to click "Sync now". This runs a
  // continuous long-poll: each call parks on the server for up to WAIT_SECS and returns the
  // instant the team config view changes (or the wait elapses), so a dashboard edit lands in
  // ~1s while staying cheap when idle. Not tied to the Teams view. Keyed on the team id so it
  // starts on connect and tears down on disconnect/removal.
  //
  // While the app is hidden to the tray the loop PAUSES entirely (zero requests): a
  // connected-but-idle client otherwise re-polls every ~25s forever, and each poll hits the
  // team server's database, which pins a scale-to-zero Postgres (Neon) awake around the clock
  // and burns compute even when nobody is using Toolport (SOU-256). We resume with an immediate
  // catch-up poll the instant the window is shown, so a policy change that landed while hidden
  // is picked up the moment the user comes back.
  //
  // The visibility signal comes from the Rust side: the `team-window-visible` event emitted on
  // show/hide, seeded by an initial `mainWindowVisible()` pull for a straight-to-tray launch.
  // The webview's own Page Visibility API is NOT reliable here - on Windows, hiding a Tauri
  // window to the tray does not flip `document.hidden`, so a purely web-based gate kept polling
  // from the tray. We still fold in `document.hidden` as a secondary signal for the platforms
  // where it does fire (e.g. a real minimize).
  const teamId = registry?.team?.teamId;
  useEffect(() => {
    if (!teamId) return;
    let cancelled = false;
    const WAIT_SECS = 25;
    // Floor between re-parks. A change is applied the instant it arrives (below), so this only
    // paces the NEXT poll, never delays enforcement. It also keeps us gentle against an OLDER
    // server that doesn't support `?wait` and so returns immediately: without a floor that would
    // be a hot loop; with it, an old server degrades to a polite ~3s poll.
    const FLOOR_MS = 3000;
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

    // Window visibility, source of truth = Rust; `hidden()` also folds in the webview signal.
    // A backgrounded loop parks in `waitUntilVisible` (issuing zero requests) and wakes on show
    // or on teardown.
    let windowVisible = true;
    const hidden = () => !windowVisible || document.hidden;
    let wake: (() => void) | null = null;
    const maybeWake = () => {
      if (!hidden() && wake) {
        wake();
        wake = null;
      }
    };
    const setWindowVisible = (v: boolean) => {
      windowVisible = v;
      maybeWake();
    };
    document.addEventListener("visibilitychange", maybeWake);
    // Seed initial state (covers a launch that goes straight to the tray via --hidden), then
    // track live show/hide from the Rust side.
    void mainWindowVisible()
      .then((v) => {
        if (!cancelled) setWindowVisible(v);
      })
      .catch(() => {});
    const unlisten = listen<boolean>("team-window-visible", (e) => {
      if (!cancelled) setWindowVisible(e.payload);
    });
    const waitUntilVisible = () =>
      new Promise<void>((resolve) => {
        if (!hidden() || cancelled) {
          resolve();
          return;
        }
        wake = resolve;
      });

    const loop = async () => {
      while (!cancelled) {
        if (hidden()) {
          await waitUntilVisible();
          if (cancelled) break;
          // Fall straight through to a poll so we resync immediately on show.
        }
        const started = Date.now();
        try {
          const fresh = await teamSyncWait(WAIT_SECS);
          if (cancelled) break;
          applyRegistryChange(fresh);
        } catch {
          // Network blip or server down: back off before re-parking so we don't spin.
          // Removal is a clean 401/403 the backend turns into a cleared team + the
          // team-removed event, not a throw, so it won't land here.
          if (cancelled) break;
          await sleep(15000);
          continue;
        }
        // If the call returned well before the wait window (a real change, already applied
        // above, or an old server ignoring `?wait`), pace the next park.
        const elapsed = Date.now() - started;
        if (!cancelled && elapsed < FLOOR_MS) await sleep(FLOOR_MS - elapsed);
      }
    };
    void loop();
    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", maybeWake);
      void unlisten.then((f) => f());
      // Unblock a loop parked in waitUntilVisible so its cancelled check runs and it exits.
      if (wake) {
        wake();
        wake = null;
      }
    };
  }, [applyRegistryChange, teamId]);

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
    ? (registry.activeProfileId ?? registry.profiles[0]?.id)
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
  const byName = (a: ServerEntry, b: ServerEntry) =>
    a.name.toLowerCase().localeCompare(b.name.toLowerCase());

  const visible = servers.filter(matches);
  const grouped: Record<Group, ServerEntry[]> = {
    attention: visible.filter((s) => groupOf(s) === "attention").sort(byName),
    checking: visible.filter((s) => groupOf(s) === "checking").sort(byName),
    active: visible.filter((s) => groupOf(s) === "active").sort(byName),
    disabled: visible.filter((s) => groupOf(s) === "disabled").sort(byName),
  };
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

  async function applyToggle(serverId: string, enabled: boolean, reviewed = false) {
    if (!profileId) return;
    setBusyId(serverId);
    try {
      const next = await setServerEnabled(profileId, serverId, enabled, reviewed);
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
          onReplayOnboarding={() => {
            setOnboardingStep(0);
            setShowOnboarding(true);
          }}
        />

        <main className="flex min-w-0 flex-1 flex-col">
          <header className="flex items-center justify-between gap-4 border-b px-6 py-4">
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
                      : view === "playground"
                        ? "Playground"
                        : view === "rules"
                          ? "Agent rules"
                          : view === "hooks"
                            ? "Agent activity"
                            : view === "permissions"
                              ? "Agent permissions"
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
                      : view === "playground"
                        ? "Invoke a server's tools and see the raw result"
                        : view === "rules"
                          ? "Write your rules once, apply them to every AI client"
                          : view === "hooks"
                            ? "See what your agents do outside Toolport"
                            : view === "permissions"
                              ? "Rules Claude Code enforces on its own native tool calls"
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
            <div className="flex shrink-0 items-center gap-2">
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
                      className="h-9 w-44 pl-8"
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
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button variant="ghost" size="icon" aria-label="More actions">
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
            <div className="p-6">
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
                  ) : view === "playground" ? (
                    <PlaygroundView
                      registry={registry}
                      onRegistryChange={applyRegistryChange}
                    />
                  ) : view === "rules" ? (
                    <RulesView />
                  ) : view === "hooks" ? (
                    <HooksView refreshKey={activityKey} />
                  ) : view === "permissions" ? (
                    <AgentPermissionsView />
                  ) : view === "teams" ? (
                    <TeamsView
                      registry={registry}
                      onRegistryChange={applyRegistryChange}
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
                      <ServerGroup title="To finish" count={grouped.attention.length}>
                        {grouped.attention.map(serverRow)}
                      </ServerGroup>
                      <ServerGroup title="Checking" count={grouped.checking.length}>
                        {grouped.checking.map(serverRow)}
                      </ServerGroup>
                      <ServerGroup title="Ready" count={grouped.active.length}>
                        {grouped.active.map(serverRow)}
                      </ServerGroup>
                      <ServerGroup
                        title="Disabled"
                        count={grouped.disabled.length}
                        defaultCollapsed
                      >
                        {grouped.disabled.map(serverRow)}
                      </ServerGroup>
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
            onOpenPlayground={() => {
              setShowOnboarding(false);
              selectView("playground");
            }}
            onOpenRules={() => {
              // Mark onboarding done, not merely hidden: someone who finished the rules
              // path IS set up, and re-showing the wizard on next launch because they
              // never added a server would be the same MCP assumption again (SBS-826).
              finishOnboarding();
              selectView("rules");
            }}
            onFinish={finishOnboarding}
          />
        </Suspense>
      )}
      <GitHubStarPrompt
        justOnboarded={justOnboarded}
        enabledCount={enabledCount}
        onVisibleChange={setStarSurface}
      />
      <PendingApprovals />
      {/* Quarantine has no global signal otherwise: the first sign used to be an agent
          call failing, with the only fix buried in Settings (SOU-293). */}
      <QuarantineAlert onReview={() => selectView("settings")} />
      <ConfirmDialog
        open={confirmDisableAll}
        onOpenChange={setConfirmDisableAll}
        title="Disable all servers?"
        description={`This turns off all ${servers.length} servers for this profile. Clients will lose their tools until you re-enable them.`}
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
            ? confirmEnableTeam.transport === "stdio" || confirmEnableTeam.command
              ? `This runs a local command on your machine: ${[confirmEnableTeam.command, ...(confirmEnableTeam.args ?? [])].join(" ")}. Only enable it if you trust your team and recognize this command.`
              : `This connects Toolport to ${confirmEnableTeam.url ?? ""}, a private/LAN address. Only enable it if you trust your team.`
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
          return applyToggle(confirmEnableTeam.id, true, true);
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
              : undefined
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
      ? `${disabled} server${disabled === 1 ? "" : "s"} disabled in this profile.`
      : probing
        ? `${checked} of ${enabled} checked so far.`
        : checked < enabled
          ? `${checked} of ${enabled} checked. Refresh to try again.`
          : attention > 0
            ? `${attention} need${attention === 1 ? "s" : ""} a quick check.`
            : disabled > 0
              ? `${disabled} disabled in this profile.`
              : "Everything enabled in this profile is ready.";
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
      ? "Use Authenticate below to finish setup. Everything else stays available."
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

/** A titled, collapsible section of server rows. Renders nothing when empty, so
 * the page only shows the buckets that have servers. Collapse state persists per
 * group; the Disabled bucket starts collapsed. */
function ServerGroup({
  title,
  count,
  defaultCollapsed = false,
  children,
}: {
  title: string;
  count: number;
  defaultCollapsed?: boolean;
  children: ReactNode;
}) {
  const slug = title.toLowerCase().replace(/\s+/g, "-");
  const storageKey = `toolport.group.${slug}`;
  const legacyStorageKey = `conduit.group.${slug}`;
  const [collapsed, setCollapsed] = useState(() => {
    const v = localStorage.getItem(storageKey) ?? localStorage.getItem(legacyStorageKey);
    return v === null ? defaultCollapsed : v === "1";
  });
  if (count === 0) return null;
  function toggle() {
    setCollapsed((c) => {
      const next = !c;
      localStorage.setItem(storageKey, next ? "1" : "0");
      localStorage.removeItem(legacyStorageKey);
      return next;
    });
  }
  return (
    <section>
      <button
        onClick={toggle}
        aria-expanded={!collapsed}
        className="mb-2 flex w-full items-center gap-2 rounded text-left focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      >
        <ChevronDown
          className={`size-3.5 text-muted-foreground/60 transition-transform ${
            collapsed ? "-rotate-90" : ""
          }`}
          aria-hidden="true"
        />
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {title}
        </h2>
        <span className="text-xs text-muted-foreground/70">{count}</span>
      </button>
      {!collapsed && (
        <div className="overflow-hidden rounded-xl border border-border/60 bg-card/40 shadow-[0_1px_0_rgba(255,255,255,.02)_inset,0_10px_28px_-24px_rgba(0,0,0,.9)]">
          {children}
        </div>
      )}
    </section>
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
