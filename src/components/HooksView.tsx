import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Check, Clock, Eye, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Callout } from "@/components/Callout";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { hooksPreview, hooksRecent, hooksSetEnabled, hooksView } from "@/lib/api";
import type {
  HookEvent,
  HookProfileStatus,
  HooksPreview,
  HooksView as HooksViewData,
} from "@/lib/types";

/** How many recent rows to pull just to show that the sensor is producing any. */
const RECENT_SAMPLE = 200;

/** How many of those rows to actually list. The card is a signal, not the activity log. */
const RECENT_ROWS = 12;

/** How often the tab re-reads the log while it is open, in milliseconds. */
const LIVE_TICK_MS = 3000;

/**
 * Agent activity: record what your AI agents do OUTSIDE Toolport.
 *
 * Toolport sees every MCP call because it routes them. It sees none of what Claude Code does
 * natively - `Bash`, `Edit`, `Read`, `WebFetch` - because none of that is MCP. This screen turns
 * on a small recorder that Claude Code runs at three points in its own lifecycle.
 *
 * Two claims this screen makes, both of which the backend holds structurally rather than by
 * promise, and both of which are stated here because the user is being asked to let software
 * watch their agent:
 *
 *   * **It cannot stop anything.** The recorder is not registered on the event that can refuse a
 *     tool call, so no bug in it can block your agent.
 *   * **It stores no content.** A row is a tool name, a session, a folder and a fingerprint.
 *     Never the command, never the file, never the output.
 *
 * Needs no MCP server and no gateway, like the Rules tab.
 *
 * `refreshKey` is the same counter the header Refresh bumps for Activity. Without it this tab
 * would keep rendering whatever it read on mount, so a recorder turned off in another window -
 * or a profile edited by hand - would still be reported here as "Recording".
 */
export function HooksView({ refreshKey = 0 }: { refreshKey?: number }) {
  const [data, setData] = useState<HooksViewData | null>(null);
  const [recent, setRecent] = useState<HookEvent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<HooksPreview[] | null>(null);

  /**
   * Re-read the view and the recent rows. Deliberately does NOT touch `error`: this also runs
   * after a failed action, to reseat the toggle on what the backend actually did, and clearing
   * the error there would wipe the only explanation the user gets.
   */
  const refresh = useCallback(async () => {
    setData(await hooksView());
    // Best-effort and separate: a log that cannot be read must not blank the whole tab, but it
    // also must not be reported as "no activity". `null` means unknown.
    try {
      setRecent(await hooksRecent(RECENT_SAMPLE));
    } catch {
      setRecent(null);
    }
  }, []);

  /**
   * The first-load path, and the only one allowed to blank the tab. Used by the retry button on
   * the failed-load screen, where there is nothing on screen to preserve anyway.
   */
  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [refresh]);

  /**
   * The re-read path once the tab is up. Uses `busy` rather than `loading` so the Refresh button
   * spins in place: dropping the whole tab back to "Loading…" for a re-read throws away the
   * profile list the user is looking at, for no gain.
   */
  const reload = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  // Mount load, plus a re-read whenever the parent's refresh counter changes. Written as a
  // cancellable promise chain rather than an awaited call, to match `RulesView` and to avoid
  // setting state synchronously inside the effect body. `cancelled` stops a slow read from
  // writing into a tab the user has already left. It never raises `loading`, so a refresh does
  // not blank a tab that already has data.
  useEffect(() => {
    let cancelled = false;
    hooksView()
      .then(async (v) => {
        if (cancelled) return;
        setData(v);
        setError(null);
        try {
          const rows = await hooksRecent(RECENT_SAMPLE);
          if (!cancelled) setRecent(rows);
        } catch {
          if (!cancelled) setRecent(null);
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [refreshKey]);

  // The tab tells the user "start a Claude Code session and events will appear here", which is
  // only true if it looks again. Nothing in the registry changes when an agent runs a tool, so
  // the parent's refreshKey never bumps for it. Tick while the tab is open, the same way
  // Activity does. Read through a ref so an in-flight action does not become an effect
  // dependency and fire an extra read every time `busy` falls.
  const busyRef = useRef(busy);
  useEffect(() => {
    busyRef.current = busy;
  }, [busy]);

  const [liveTick, setLiveTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => {
      // Skip while an action is in flight: `act` reseats both pieces of state itself, and a tick
      // landing mid-write would show the state the backend is still leaving.
      if (document.visibilityState === "visible" && !busyRef.current) {
        setLiveTick((t) => t + 1);
      }
    }, LIVE_TICK_MS);
    return () => clearInterval(id);
  }, []);

  // The tick's read is silent on purpose: it touches neither `loading`, `busy` nor `error`. A
  // background poll that fails is not something the user did, and surfacing it would make a
  // danger callout blink on and off every few seconds while they read the page.
  useEffect(() => {
    if (liveTick === 0) return;
    let alive = true;
    hooksRecent(RECENT_SAMPLE)
      .then((rows) => {
        if (alive) setRecent(rows);
      })
      .catch(() => {
        /* keep the last known rows; a failed poll is not an empty log */
      });
    hooksView()
      .then((v) => {
        if (alive) setData(v);
      })
      .catch(() => {
        /* keep the last known view */
      });
    return () => {
      alive = false;
    };
  }, [liveTick]);

  /** Run a mutating call, then reseat the view on whatever it returns. */
  async function act(run: () => Promise<HooksViewData>) {
    setBusy(true);
    setError(null);
    try {
      setData(await run());
      try {
        setRecent(await hooksRecent(RECENT_SAMPLE));
      } catch {
        setRecent(null);
      }
    } catch (e) {
      setError(String(e));
      // The call failed, so the view we hold may no longer match disk. Re-read, rather than leave
      // a toggle showing a state the backend refused to enter. `refresh` keeps the error above;
      // a re-read that failed too leaves both the stale view and the original message, which is
      // the more useful of the two.
      try {
        await refresh();
      } catch {
        /* keep the error that actually explains the failure */
      }
    } finally {
      setBusy(false);
    }
  }

  async function openPreview() {
    setBusy(true);
    setError(null);
    try {
      setPreview(await hooksPreview());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (loading) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (!data) {
    // A dead end is worse than the failure: without a way to look again, the only recovery is
    // leaving the tab and coming back. The Refresh button below lives inside the profiles card,
    // which this branch never renders.
    return (
      <div className="flex flex-col gap-3">
        <Callout variant="danger" role="alert">
          <strong className="font-medium">Agent activity could not be read.</strong>{" "}
          {error ?? "Unknown error."}
        </Callout>
        <Button
          size="sm"
          variant="outline"
          className="self-start"
          disabled={busy}
          onClick={() => void load()}
        >
          <RefreshCw className="size-3.5" />
          Try again
        </Button>
      </div>
    );
  }

  const profiles = data.profiles;
  // A profile the backend could not read arrives as `installed: false`, which is the absence of
  // knowledge and not the absence of a recorder. Counting it as "off" would contradict the
  // "Could not read" badge on the very same row, so it leaves the fraction entirely.
  const readable = profiles.filter((p) => !p.error);
  const installed = readable.filter((p) => p.installed).length;
  const broken = profiles.length - readable.length;
  const canInstall = Boolean(data.binary);
  const rows = recent?.slice(0, RECENT_ROWS) ?? [];

  return (
    <div className="grid gap-4">
      {error && (
        <Callout variant="danger" role="alert">
          <strong className="font-medium">That did not work.</strong> {error}
        </Callout>
      )}

      <div className="rounded-xl border bg-card p-5">
        <label className="flex items-start gap-3">
          <input
            type="checkbox"
            className="mt-1"
            checked={data.enabled}
            // Locked while the preview is open: the dialog shows the bytes that would be written
            // NEXT, and letting the toggle write behind it would leave the user reading a
            // prediction of something that already happened.
            disabled={busy || preview !== null || (!data.enabled && !canInstall)}
            aria-label="Record what my agents do"
            onChange={(e) => {
              // Read the new value now, before the await: React re-renders this controlled input
              // back to `data.enabled` while the call is in flight, so a lazy read inside the
              // callback sees the OLD value and the toggle silently does nothing.
              const next = e.target.checked;
              void act(() => hooksSetEnabled(next));
            }}
          />
          <span className="min-w-0">
            <span className="text-sm font-medium">Record what my agents do</span>
            <span className="mt-1 block text-xs text-muted-foreground">
              Claude Code runs a small Toolport recorder when a session starts, after each
              tool it uses, and when the session ends. You get one line per event: which
              tool, in which folder, in which session.
            </span>
          </span>
        </label>

        <ul className="mt-3 grid gap-1 border-t pt-3 text-xs text-muted-foreground">
          <li className="flex items-start gap-2">
            <Check className="mt-0.5 size-3.5 shrink-0 text-success" />
            <span>
              <strong className="font-medium text-foreground">
                It cannot stop your agent.
              </strong>{" "}
              The recorder is not attached to the step that can refuse a tool call, so
              nothing it does can block your work.
            </span>
          </li>
          <li className="flex items-start gap-2">
            <Check className="mt-0.5 size-3.5 shrink-0 text-success" />
            <span>
              <strong className="font-medium text-foreground">
                It does not read your work.
              </strong>{" "}
              Commands, file contents and tool output are dropped. A row keeps the tool
              name, the folder, the session, and a fingerprint that cannot be turned back
              into the input.
            </span>
          </li>
          <li className="flex items-start gap-2">
            <Check className="mt-0.5 size-3.5 shrink-0 text-success" />
            <span>
              <strong className="font-medium text-foreground">
                It stays on this machine.
              </strong>{" "}
              Rows are appended to a local file. Turning this off removes the recorder
              from every file Toolport wrote.
            </span>
          </li>
        </ul>

        {!canInstall && !data.enabled && (
          <p className="mt-3 text-xs text-warning">
            No gateway binary has been published yet, so there is nothing to install.
            Connect a client first, then come back.
          </p>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <div className="mb-1 flex items-center justify-between gap-3">
          <h2 className="text-sm font-medium">Claude Code profiles</h2>
          <span className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              disabled={busy || !canInstall}
              title={
                canInstall
                  ? "See exactly what would be written"
                  : "No gateway binary has been published yet"
              }
              onClick={() => void openPreview()}
              className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
            >
              <Eye className="size-3.5" />
              Preview
            </button>
            <button
              type="button"
              disabled={busy}
              title="Re-read the profiles on disk"
              onClick={() => void reload()}
              className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
            >
              <RefreshCw className={`size-3.5 ${busy ? "animate-spin" : ""}`} />
              Refresh
            </button>
          </span>
        </div>
        <p className="mb-3 text-xs text-muted-foreground">
          {profiles.length === 0
            ? "No Claude Code profile was found on this machine."
            : readable.length === 0
              ? `No profile could be read, so whether ${
                  profiles.length === 1 ? "it carries" : "they carry"
                } the recorder is unknown.`
              : `${installed} of ${readable.length} carry the recorder. A machine can have more than one profile, and each needs it separately.${
                  broken > 0
                    ? ` ${broken} more could not be read and ${
                        broken === 1 ? "is" : "are"
                      } not counted.`
                    : ""
                }`}
        </p>

        {profiles.length > 0 && (
          <ul className="grid gap-1.5">
            {profiles.map((p) => (
              <li
                key={p.path}
                className="flex items-center justify-between gap-3 text-sm"
              >
                <span className="truncate font-mono text-xs" title={p.path}>
                  {p.path}
                </span>
                <ProfileBadge profile={p} enabled={data.enabled} />
              </li>
            ))}
          </ul>
        )}

        {broken > 0 && (
          <p className="mt-3 text-xs text-warning">
            {broken === 1 ? "One profile" : `${broken} profiles`} could not be read or
            written, so
            {broken === 1 ? " it was" : " they were"} left untouched. Nothing was
            overwritten.
          </p>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <h2 className="mb-1 text-sm font-medium">Recorded so far</h2>
        <p className="text-xs text-muted-foreground">
          {recent === null ? (
            // Unreadable is NOT the same as empty. Saying "no activity" for a log we failed to
            // read would be a comfortable lie about whether anything is being recorded.
            <span className="text-warning">
              The activity log could not be read, so this count is unknown.
            </span>
          ) : recent.length === 0 ? (
            data.enabled ? (
              "Nothing yet. Start a Claude Code session and events will appear here."
            ) : (
              "Nothing recorded. Turn the recorder on above to start."
            )
          ) : (
            `${recent.length === RECENT_SAMPLE ? `${RECENT_SAMPLE}+` : recent.length} recent ${
              recent.length === 1 ? "event" : "events"
            }${lastToolLabel(recent)}`
          )}
        </p>

        {rows.length > 0 && (
          // The toggle promises "one line per event: which tool, in which folder, in which
          // session". A count alone does not show that, and it is also the only way a user can
          // check the no-content claim for themselves: what is on screen is all that is stored.
          <ul className="mt-3 grid gap-1 border-t pt-3">
            {rows.map((r, i) => (
              <li
                key={`${r.ts ?? ""}-${r.sessionId ?? ""}-${r.tool ?? r.event ?? ""}-${i}`}
                className="flex items-center gap-3 text-xs"
              >
                <span className="min-w-0 flex-1 truncate font-mono text-foreground">
                  {r.tool ?? r.event ?? "unknown"}
                  {r.event === "guard" && r.decision && (
                    // A guard row (Cursor hook): what the hook answered, and in observe mode
                    // what it would have answered, so the user can read the policy's effect
                    // before letting it act.
                    <span
                      className={`ml-2 rounded px-1 text-[10px] ${
                        // In observe mode the answer is always allow; colour by what the
                        // rules WOULD have done, which is the thing worth seeing.
                        (r.mode === "observe" ? (r.wouldBe ?? "allow") : r.decision) ===
                        "deny"
                          ? "bg-destructive/15 text-destructive"
                          : (r.mode === "observe"
                                ? (r.wouldBe ?? "allow")
                                : r.decision) === "ask"
                            ? "bg-warning/15 text-warning"
                            : "bg-muted text-muted-foreground"
                      }`}
                      title={r.rule ? `rule ${r.rule}` : undefined}
                    >
                      {r.mode === "observe" && r.wouldBe && r.wouldBe !== "allow"
                        ? `would ${r.wouldBe}`
                        : r.decision}
                    </span>
                  )}
                </span>
                <span
                  className="min-w-0 flex-1 truncate text-muted-foreground"
                  title={r.cwd}
                >
                  {folderLabel(r.cwd) ?? "—"}
                </span>
                <span
                  className="shrink-0 font-mono text-muted-foreground/70"
                  title={r.sessionId ? `Session ${r.sessionId}` : "No session recorded"}
                >
                  {sessionLabel(r.sessionId) ?? "—"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>

      <PreviewDialog previews={preview} onClose={() => setPreview(null)} />
    </div>
  );
}

/** The most recent named tool, when there is one, as a "you can see it working" signal. */
function lastToolLabel(recent: HookEvent[]): string {
  const tool = recent.find((r) => typeof r.tool === "string" && r.tool.length > 0)?.tool;
  return tool ? `, most recently ${tool}.` : ".";
}

/** Last path segment, so a row reads "toolport" rather than a full home directory. */
function folderLabel(cwd?: string): string | null {
  if (!cwd) return null;
  const parts = cwd.split(/[/\\]+/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : cwd;
}

/** Enough of a session id to tell two sessions apart without printing a whole UUID. */
function sessionLabel(id?: string): string | null {
  if (!id) return null;
  return id.length > 8 ? id.slice(0, 8) : id;
}

/**
 * One profile's state.
 *
 * An error outranks everything: a profile we could not read is neither "on" nor "off", and
 * rendering it as off would claim we know something we do not.
 */
function ProfileBadge({
  profile,
  enabled,
}: {
  profile: HookProfileStatus;
  enabled: boolean;
}) {
  if (profile.error) {
    return (
      <span
        title={profile.error}
        className="flex shrink-0 items-center gap-1 text-xs text-destructive"
      >
        <AlertTriangle className="size-3.5" />
        Could not read
      </span>
    );
  }
  if (profile.installed) {
    return (
      <span
        title="This profile runs the recorder."
        className="flex shrink-0 items-center gap-1 text-xs text-success"
      >
        <Check className="size-3.5" />
        Recording
      </span>
    );
  }
  return (
    <span
      title={
        enabled
          ? "The recorder is on but has not been written to this profile yet."
          : "Nothing is written to this profile."
      }
      className={`flex shrink-0 items-center gap-1 text-xs ${
        enabled ? "text-warning" : "text-muted-foreground"
      }`}
    >
      <Clock className="size-3.5" />
      {enabled ? "Not written yet" : "Off"}
    </span>
  );
}

/**
 * The exact bytes that would be written, per profile, before anything is.
 *
 * Uses the shared Radix dialog rather than a hand-rolled overlay, so Escape closes it, focus is
 * trapped inside it, and a click outside dismisses it - none of which a bare positioned div
 * gives you, and all of which a user reading a "nothing has been written yet" screen will try.
 */
function PreviewDialog({
  previews,
  onClose,
}: {
  previews: HooksPreview[] | null;
  onClose: () => void;
}) {
  return (
    <Dialog open={previews !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>What would be written</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4 overflow-auto">
          {previews?.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No Claude Code profile was found, so there is nothing to write.
            </p>
          )}
          {previews?.map((p) => {
            const changedLines = changedLineIndexes(p.before, p.after);
            return (
              <div key={p.path} className="grid gap-1">
                <p className="font-mono text-xs text-muted-foreground">{p.path}</p>
                {p.error ? (
                  // A profile the backend could not parse has no dry run, and its empty
                  // `after` would otherwise render as a blank block over the caption
                  // "would be created" — telling the user a file that plainly exists does
                  // not. Say what actually happened, and leave the healthy profiles above
                  // and below it alone.
                  <p className="rounded-md bg-warning/10 px-3 py-2 text-xs text-warning">
                    No preview for this profile: {p.error}
                  </p>
                ) : (
                  <>
                    <pre className="max-h-64 overflow-auto rounded-md border bg-muted/40 p-3 text-xs">
                      {p.after.split("\n").map((line, i) => (
                        // The backend rewrites one top-level key, so the changed region is
                        // contiguous. Mark the whole region rather than only command lines:
                        // braces and array entries are part of what Toolport would add too.
                        <span
                          key={i}
                          className={changedLines.has(i) ? "bg-success/15" : undefined}
                        >
                          {line}
                          {"\n"}
                        </span>
                      ))}
                    </pre>
                    {p.before === "" && (
                      <p className="text-xs text-muted-foreground">
                        This file does not exist yet and would be created.
                      </p>
                    )}
                  </>
                )}
              </div>
            );
          })}
        </div>
        <DialogFooter className="text-xs text-muted-foreground sm:justify-start">
          Nothing has been written. Everything outside the highlighted block, including
          your comments, is left exactly as it is.
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Lines in the one contiguous region changed by the backend's top-level-key rewrite. */
function changedLineIndexes(before: string, after: string): Set<number> {
  const beforeLines = before.split("\n");
  const afterLines = after.split("\n");
  let prefix = 0;
  while (
    prefix < beforeLines.length &&
    prefix < afterLines.length &&
    beforeLines[prefix] === afterLines[prefix]
  ) {
    prefix += 1;
  }

  let suffix = 0;
  while (
    suffix < beforeLines.length - prefix &&
    suffix < afterLines.length - prefix &&
    beforeLines[beforeLines.length - 1 - suffix] ===
      afterLines[afterLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  return new Set(
    Array.from({ length: afterLines.length - prefix - suffix }, (_, i) => prefix + i),
  );
}
