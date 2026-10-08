/**
 * One-time "star us on GitHub" ask.
 *
 * New installs get a deferred chip, never a card after onboarding. Existing
 * installs keep their one-off delayed card. Both wait for three enabled servers
 * and successful gateway tool calls on two distinct UTC dates in retained local
 * Activity. No new usage tracking is written.
 *
 * An ask is spent when it is shown, not when it is clicked. Ignoring a prompt
 * and quitting therefore does not bring it back on the next launch, which is
 * the difference between asking and nagging.
 *
 * Deliberately in-app only: no OS notification, no toast that steals focus.
 */

export const STAR_PROMPT_KEY = "toolport.starPrompt";
export const STAR_REPO_URL = "https://github.com/btsouth/toolport";

/** Written and deleted at read time to find out whether a spent ask could be
 *  recorded at all. Never read back, so a leftover from a crash is harmless. */
const STORAGE_PROBE_KEY = "toolport.starPrompt.probe";

export const STAR_MIN_ENABLED_SERVERS = 3;

/** Audit includes governance rows as well as calls. Match audit::tool_call_ok. */
export function toolCallDays(
  entries: readonly { ts: number; ok: boolean; held?: boolean; kind?: string }[],
): number {
  return new Set(
    entries
      .filter(
        (entry) =>
          entry.ok === true &&
          !entry.held &&
          !["approval", "routine", "advisor", "suggestion", "candidate"].includes(
            entry.kind ?? "",
          ) &&
          Number.isFinite(entry.ts) &&
          entry.ts > 0,
      )
      .map((entry) => Math.floor(entry.ts / 86_400_000)),
  ).size;
}

/**
 * What this install is owed next.
 *
 * `card` and `returning` are derived at read time and never stored: an install
 * is one or the other by whether it had onboarded before the prompt existed.
 * Only `later` and `done` are written, since those are the states that have to
 * survive a restart.
 */
export type StarStage = "card" | "returning" | "later" | "done";

/**
 * Set the first time localStorage refuses a read or a write.
 *
 * Storage is the only thing that remembers a spent ask, so once it is gone the
 * honest answer is to stop asking rather than to ask again on every mount. This
 * is module state on purpose: it has to outlive the component that discovered
 * the failure.
 */
let storageBroken = false;

function readRaw(key: string): string | null {
  if (storageBroken) return null;
  try {
    return localStorage.getItem(key);
  } catch {
    storageBroken = true;
    return null;
  }
}

/**
 * Whether a spent ask could actually be recorded.
 *
 * The invariant this prompt promises is "an ask that cannot be recorded is
 * treated as already spent". A latch in module state cannot keep that promise,
 * because a quota-exceeded or blocked store fails writes while still serving
 * reads: the stage would be derived from scratch on every relaunch and the card
 * would come back forever. So the write path is probed, not remembered, and the
 * probe re-runs on each launch. Storage that recovers gets its ask back, which
 * is the honest reading of a store that works again.
 */
function canRecord(): boolean {
  try {
    localStorage.setItem(STORAGE_PROBE_KEY, "1");
    localStorage.removeItem(STORAGE_PROBE_KEY);
    return true;
  } catch {
    storageBroken = true;
    return false;
  }
}

function onboardedAlready(): boolean {
  return (
    readRaw("toolport.onboarded") === "1" ||
    // Pre-rename key, still present on installs that onboarded as Conduit.
    readRaw("conduit.onboarded") === "1"
  );
}

/** Resolve the stage to start this session at. */
export function readStarStage(): StarStage {
  const raw = readRaw(STAR_PROMPT_KEY);
  if (storageBroken || raw === "done") return "done";
  // Every remaining stage can still put something on screen, and showing it is
  // what spends it. Refuse to show what cannot be written down afterwards.
  if (!canRecord()) return "done";
  if (raw === "later") return "later";
  // No record at all. An install that has already onboarded predates this
  // prompt, so it gets the single existing-user card rather than nothing.
  return onboardedAlready() ? "returning" : "card";
}

export function writeStarStage(stage: "later" | "done"): void {
  if (storageBroken) return;
  // "done" is terminal. The effect that spends an ask when it is shown can flush
  // after a click on Star has already finished the ask, and without this guard
  // that late write would reopen it and bring the chip back next launch.
  if (stage === "later" && readRaw(STAR_PROMPT_KEY) === "done") return;
  try {
    localStorage.setItem(STAR_PROMPT_KEY, stage);
  } catch {
    storageBroken = true;
  }
}
