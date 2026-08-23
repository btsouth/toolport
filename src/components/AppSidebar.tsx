import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  ArrowUpCircle,
  ClipboardList,
  Compass,
  FileText,
  FlaskConical,
  FolderOpen,
  Layers,
  Loader2,
  MonitorCog,
  ScrollText,
  Settings,
  Share2,
  Store,
  Users,
  Zap,
  ShieldCheck,
} from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { openExternal } from "@/lib/openUrl";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import type { Update } from "@tauri-apps/plugin-updater";
import { type Registry, type SavingsSummary, type View } from "@/lib/types";
import {
  gatherDiagnostics,
  getSavingsSummary,
  listQuarantined,
  openDataDir,
} from "@/lib/api";
import { fmtTokens } from "@/lib/utils";
import { checkForUpdate, installUpdate, type UpdateProgress } from "@/lib/updater";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ProfileBar } from "@/components/ProfileBar";
import { ShareDialog } from "@/components/ShareDialog";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
const NAV_ITEM = `flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left text-sm transition-colors hover:bg-accent ${FOCUS_RING}`;
const ICON_BTN = `rounded text-muted-foreground transition hover:text-foreground ${FOCUS_RING}`;
const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

function updateProgressLabel(progress: UpdateProgress | null): string {
  if (!progress) return "Preparing update…";
  if (progress.phase === "installing") return "Installing…";
  if (progress.totalBytes && progress.totalBytes > 0) {
    const percent = Math.min(
      100,
      Math.round((progress.downloadedBytes / progress.totalBytes) * 100),
    );
    return `Downloading ${percent}%`;
  }
  const megabytes = progress.downloadedBytes / (1024 * 1024);
  return megabytes > 0
    ? `Downloading ${megabytes.toFixed(megabytes >= 10 ? 0 : 1)} MB…`
    : "Downloading…";
}

/** Footer showing the running version, and an in-app update button when a newer
 * release is published. The check is best-effort: any failure (dev build,
 * offline, no manifest yet) just shows the current version. Clicking downloads,
 * installs, and relaunches into the new version. */
function VersionFooter({
  onImport,
  onReplay,
}: {
  onImport: (r: Registry) => void;
  onReplay: () => void;
}) {
  const [version, setVersion] = useState("");
  const [update, setUpdate] = useState<Update | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installProgress, setInstallProgress] = useState<UpdateProgress | null>(null);
  const [checking, setChecking] = useState(false);
  const [showNotes, setShowNotes] = useState(false);
  const mountedRef = useRef(false);
  const checkingRef = useRef(false);
  const announceCheckRef = useRef(false);
  const installingRef = useRef(false);
  const lastCheckRef = useRef(0);

  useEffect(() => {
    mountedRef.current = true;
    getVersion()
      .then((v) => {
        if (mountedRef.current) setVersion(v);
      })
      .catch(() => {
        // Never let a failed version lookup hide the whole footer toolbar.
        if (mountedRef.current) setVersion("?");
      });
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const runUpdateCheck = useCallback(async (announce: boolean, force = false) => {
    const now = Date.now();
    if (checkingRef.current) {
      // A tray/manual request can arrive while the quiet startup/visibility check
      // is already in flight. Upgrade that request to an announced result instead
      // of dropping the user's explicit action or starting a duplicate request.
      if (announce) announceCheckRef.current = true;
      return;
    }
    if (installingRef.current) {
      if (announce) toast.info("An update is already in progress");
      return;
    }
    if (
      !force &&
      lastCheckRef.current !== 0 &&
      now - lastCheckRef.current < UPDATE_CHECK_INTERVAL_MS
    ) {
      return;
    }
    checkingRef.current = true;
    if (mountedRef.current) setChecking(true);
    try {
      const result = await checkForUpdate();
      if (result.kind !== "error") lastCheckRef.current = Date.now();
      if (!mountedRef.current) return;
      const shouldAnnounce = announce || announceCheckRef.current;
      announceCheckRef.current = false;
      if (result.kind === "update") {
        setUpdate(result.update);
        if (shouldAnnounce) setShowNotes(true);
      } else if (result.kind === "current") {
        setUpdate(null);
        if (shouldAnnounce) toast.success("You're on the latest version");
      } else if (shouldAnnounce) {
        toastError("Couldn't check for updates", {
          description: "You may be offline. Try again in a moment.",
        });
      }
    } finally {
      checkingRef.current = false;
      if (mountedRef.current) setChecking(false);
    }
  }, []);

  useEffect(() => {
    void runUpdateCheck(false);
    const interval = window.setInterval(
      () => void runUpdateCheck(false),
      UPDATE_CHECK_INTERVAL_MS,
    );
    const visibleUnlisten = listen<boolean>("team-window-visible", (event) => {
      if (event.payload) void runUpdateCheck(false);
    });
    const trayUnlisten = listen("tray-check-updates", () => {
      void runUpdateCheck(true, true);
    });
    return () => {
      window.clearInterval(interval);
      void visibleUnlisten.then((unlisten) => unlisten());
      void trayUnlisten.then((unlisten) => unlisten());
    };
  }, [runUpdateCheck]);

  async function manualCheck() {
    await runUpdateCheck(true, true);
  }

  async function applyUpdate() {
    if (!update) return;
    installingRef.current = true;
    setInstalling(true);
    setInstallProgress(null);
    toast.info(`Updating to v${update.version}…`, {
      description:
        "Downloading first; MCP clients stay connected until the installer is ready to replace files.",
    });
    try {
      await installUpdate(update, (progress) => {
        if (mountedRef.current) setInstallProgress(progress);
      });
    } catch (e) {
      installingRef.current = false;
      setInstalling(false);
      setInstallProgress(null);
      const message = e instanceof Error ? e.message : String(e);
      const recoveryAdvice =
        e &&
        typeof e === "object" &&
        "recoveryAdvice" in e &&
        typeof e.recoveryAdvice === "string"
          ? e.recoveryAdvice
          : null;
      toastError(`Update failed: ${message}`, {
        description:
          recoveryAdvice || "You can download it manually from the releases page.",
        action: {
          label: "Open",
          onClick: () =>
            openExternal("https://github.com/tsouth89/toolport/releases/latest"),
        },
      });
    }
  }

  const progressLabel = updateProgressLabel(installProgress);

  if (!version) return null;
  return (
    <div className="mt-auto flex items-center justify-between gap-2 border-t px-4 py-3 text-xs">
      {update ? (
        <button
          onClick={() => setShowNotes(true)}
          disabled={installing}
          className={`flex min-w-0 items-center gap-1.5 rounded text-success transition hover:underline disabled:opacity-70 ${FOCUS_RING}`}
        >
          {installing ? (
            <Loader2 className="size-3.5 shrink-0 animate-spin" />
          ) : (
            <ArrowUpCircle className="size-3.5 shrink-0" />
          )}
          <span className="truncate">
            {installing ? progressLabel : `Update to v${update.version}`}
          </span>
        </button>
      ) : (
        <button
          onClick={manualCheck}
          disabled={checking}
          title="Check for updates"
          className={`rounded text-muted-foreground transition hover:text-foreground disabled:opacity-70 ${FOCUS_RING}`}
        >
          {checking ? "Checking…" : `Toolport v${version}`}
        </button>
      )}

      <UpdateNotes
        open={showNotes}
        onOpenChange={setShowNotes}
        update={update}
        installing={installing}
        progressLabel={progressLabel}
        onInstall={applyUpdate}
      />
      <div className="flex shrink-0 items-center gap-2">
        <ShareDialog
          onImported={onImport}
          trigger={
            <button
              title="Share or import a setup"
              aria-label="Share setup"
              className={ICON_BTN}
            >
              <Share2 className="size-3.5" />
            </button>
          }
        />
        <button
          onClick={onReplay}
          title="Run setup again"
          aria-label="Run setup again"
          className={ICON_BTN}
        >
          <Compass className="size-3.5" />
        </button>
        <button
          onClick={() =>
            openDataDir().catch(() => toastError("Couldn't open data folder"))
          }
          title="Open data folder (config, logs)"
          aria-label="Open data folder"
          className={ICON_BTN}
        >
          <FolderOpen className="size-3.5" />
        </button>
        <button
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(await gatherDiagnostics());
              toast.success("Diagnostics copied, paste them into your bug report");
            } catch {
              toastError("Couldn't copy diagnostics");
            }
          }}
          title="Copy diagnostics for a bug report"
          aria-label="Copy diagnostics"
          className={ICON_BTN}
        >
          <ClipboardList className="size-3.5" />
        </button>
      </div>
    </div>
  );
}

/** Release-notes dialog shown before installing an update, so the user sees
 * what's changing and confirms the restart. */
function UpdateNotes({
  open,
  onOpenChange,
  update,
  installing,
  progressLabel,
  onInstall,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  update: Update | null;
  installing: boolean;
  progressLabel: string;
  onInstall: () => void;
}) {
  if (!update) return null;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Update available: v{update.version}</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          {update.body ? (
            <div className="max-h-60 overflow-y-auto rounded-md border bg-muted/30 p-3 text-sm whitespace-pre-wrap text-muted-foreground">
              {update.body}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              A new version is ready to install.
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              {installing ? "Hide" : "Later"}
            </Button>
            <Button onClick={onInstall} disabled={installing}>
              {installing ? (
                <>
                  <Loader2 className="size-4 animate-spin" /> {progressLabel}
                </>
              ) : (
                <>
                  <ArrowUpCircle className="size-4" /> Install and restart
                </>
              )}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

interface Props {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
  view: View;
  onSelectView: (view: View) => void;
  onReplayOnboarding: () => void;
}

export function AppSidebar({
  registry,
  onRegistryChange,
  view,
  onSelectView,
  onReplayOnboarding,
}: Props) {
  const [savings, setSavings] = useState<SavingsSummary | null>(null);
  // `null` means "no confirmed count": the first poll hasn't answered yet. It
  // must render distinctly from a confirmed zero so a gateway that never
  // answered never reads as "all clear" (#741).
  const [quarantinedCount, setQuarantinedCount] = useState<number | null>(null);
  // A failed poll keeps the last confirmed count and flags it stale instead of
  // dropping back to `null`, so one transient blip never erases a real number.
  // Before the first poll answers (`null`, not stale) no badge renders at all —
  // the "?" glyph and its "Could not reach the gateway" tooltip must only appear
  // once a poll has actually failed, not on every app start (#742).
  const [quarantineStale, setQuarantineStale] = useState(false);
  // Surface the running token savings in the sidebar so the headline number isn't
  // hidden one click away in Activity. Refresh on a light interval as calls flow.
  useEffect(() => {
    let alive = true;
    const load = () =>
      getSavingsSummary()
        .then((s) => alive && setSavings(s))
        .catch(() => {});
    load();
    const id = setInterval(load, 60_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  // Keep a blocked-tool count on the Settings row, so the state stays discoverable after
  // the QuarantineAlert card is dismissed (SOU-293). Slower than the card's own poll:
  // this is a persistent indicator, not the thing that has to catch your eye.
  useEffect(() => {
    let alive = true;
    // Monotonic request id alongside `alive`: that flag only covers unmount, so a slow
    // response could still land after a newer one and pin the badge to a stale count
    // until the next tick. Only the newest request may write.
    let latest = 0;
    const load = () => {
      const id = ++latest;
      return listQuarantined()
        .then((q) => {
          if (alive && id === latest) {
            setQuarantinedCount(q.length);
            setQuarantineStale(false);
          }
        })
        .catch(() => {
          // A failed poll must not read as a confirmed zero, but it must not
          // erase a confirmed count either: keep the last answer and mark it
          // stale so the badge never claims "all clear" and never loses a
          // real number over one blip (#741, #742).
          if (alive && id === latest) setQuarantineStale(true);
        });
    };
    load();
    const id = setInterval(load, 10_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  // One sidebar nav row. The active row gets the accent background, a foreground
  // icon (not muted), and aria-current so screen readers announce the selection.
  const navItem = (
    Icon: typeof Layers,
    label: string,
    active: boolean,
    onClick: () => void,
    badge?: number | null,
    stale?: boolean,
  ) => (
    <button
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      className={`${NAV_ITEM} ${active ? "bg-accent font-medium text-foreground" : "text-muted-foreground"}`}
    >
      <Icon
        className={`size-4 shrink-0 ${active ? "text-primary" : "text-muted-foreground"}`}
      />
      <span>{label}</span>
      {badge !== undefined && badge !== null && badge > 0 && (
        <span
          className="ml-auto inline-flex shrink-0 items-center rounded-full bg-warning/15 px-1.5 text-[10px] font-medium text-warning"
          aria-label={`${badge} tool${badge === 1 ? "" : "s"} blocked`}
          title={
            stale
              ? "Could not reach the gateway — quarantine count may be stale"
              : undefined
          }
        >
          {badge}
        </span>
      )}
      {badge === null && stale && (
        <span
          className="ml-auto inline-flex shrink-0 items-center rounded-full bg-warning/15 px-1.5 text-[10px] font-medium text-warning"
          aria-label="Quarantine status unknown"
          title="Could not reach the gateway — quarantine status unknown"
        >
          ?
        </span>
      )}
    </button>
  );

  return (
    <aside className="flex h-screen w-72 shrink-0 flex-col border-r bg-sidebar">
      <div className="flex items-center gap-2.5 px-4 py-4">
        <svg className="size-8" viewBox="0 0 48 48" aria-hidden="true">
          <rect width="48" height="48" rx="11" fill="#1E3A66" />
          <circle
            cx="24"
            cy="24"
            r="18.28"
            fill="none"
            stroke="#ffffff"
            strokeWidth="8.44"
          />
          <g fill="#1E3A66">
            <polygon points="24.00,3.47 22.05,4.59 22.05,6.84 24.00,7.97 25.95,6.84 25.95,4.59" />
            <polygon points="42.28,21.75 40.33,22.88 40.33,25.12 42.28,26.25 44.23,25.12 44.23,22.88" />
            <polygon points="24.00,40.03 22.05,41.16 22.05,43.41 24.00,44.53 25.95,43.41 25.95,41.16" />
            <polygon points="5.72,21.75 3.77,22.88 3.77,25.12 5.72,26.25 7.67,25.12 7.67,22.88" />
          </g>
          <g fill="#ffffff">
            <polygon points="24.00,4.78 23.19,5.25 23.19,6.19 24.00,6.66 24.81,6.19 24.81,5.25" />
            <polygon points="42.28,23.06 41.47,23.53 41.47,24.47 42.28,24.94 43.09,24.47 43.09,23.53" />
            <polygon points="24.00,41.34 23.19,41.81 23.19,42.75 24.00,43.22 24.81,42.75 24.81,41.81" />
            <polygon points="5.72,23.06 4.91,23.53 4.91,24.47 5.72,24.94 6.53,24.47 6.53,23.53" />
          </g>
          <circle cx="24" cy="24" r="4.88" fill="#F97316" />
        </svg>
        <div className="leading-tight">
          <div className="font-semibold tracking-tight">Toolport</div>
          <div className="text-xs text-muted-foreground">MCP control center</div>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        {registry && (
          <div className="px-3 pb-2">
            <div className="px-2.5 pb-1.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
              Profile
            </div>
            <ProfileBar registry={registry} onChange={onRegistryChange} />
          </div>
        )}

        <nav aria-label="Views" className="flex flex-col gap-0.5 px-3 pt-2">
          {navItem(Layers, "All servers", view === "servers", () =>
            onSelectView("servers"),
          )}
          {navItem(MonitorCog, "Clients", view === "clients", () =>
            onSelectView("clients"),
          )}
          {navItem(Store, "Browse catalog", view === "catalog", () =>
            onSelectView("catalog"),
          )}
          {navItem(FlaskConical, "Playground", view === "playground", () =>
            onSelectView("playground"),
          )}
          {navItem(ScrollText, "Activity", view === "activity", () =>
            onSelectView("activity"),
          )}
          {navItem(FileText, "Agent rules", view === "rules", () =>
            onSelectView("rules"),
          )}
          {navItem(Activity, "Agent activity", view === "hooks", () =>
            onSelectView("hooks"),
          )}
          {navItem(ShieldCheck, "Agent permissions", view === "permissions", () =>
            onSelectView("permissions"),
          )}
          {navItem(Users, "Teams", view === "teams", () => onSelectView("teams"))}
          {navItem(
            Settings,
            "Settings",
            view === "settings",
            () => onSelectView("settings"),
            quarantinedCount,
            quarantineStale,
          )}
        </nav>

        {savings && savings.tokensSaved > 0 && (
          <button
            onClick={() => onSelectView("activity")}
            className="mx-3 mt-2 flex items-center gap-2 rounded-lg border border-success/30 bg-success/5 px-3 py-2 text-left text-xs transition-colors hover:bg-success/10"
            title="Tool-definition tokens lazy discovery has kept out of your agent's context. Click for the breakdown."
          >
            <Zap className="size-3.5 shrink-0 text-success" />
            <span className="text-muted-foreground">
              <span className="font-semibold text-foreground">
                {fmtTokens(savings.tokensSaved)}
              </span>{" "}
              tokens saved
            </span>
          </button>
        )}
      </div>

      <VersionFooter onImport={onRegistryChange} onReplay={onReplayOnboarding} />
    </aside>
  );
}
