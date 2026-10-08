import { useEffect, useState, type ReactNode } from "react";
import { Star, X } from "lucide-react";
import { getAuditLog } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { openExternal } from "@/lib/openUrl";
import { modalLayerOpen, useModalOpen, useWindowVisible } from "@/lib/windowVisible";
import {
  STAR_MIN_ENABLED_SERVERS,
  toolCallDays,
  STAR_REPO_URL,
  readStarStage,
  writeStarStage,
  type StarStage,
} from "@/lib/starPrompt";

/** How long an existing user is left alone before the one-off card appears. The
 *  clock only runs while the window is actually on screen: Toolport lives in the
 *  tray, so a launch-time timer would spend the single ask on nobody. */
const RETURNING_DELAY_MS = 8000;

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
const SHELL =
  "pointer-events-auto animate-in fade-in slide-in-from-bottom-2 border bg-popover/95 text-popover-foreground shadow-2xl backdrop-blur";

/** Which surface is on screen (null when none), for the toast-offset callback. */
export type StarSurface = "returning" | "chip" | null;

interface Props {
  /** True once the wizard has been finished in this session (no ask in that session). */
  justOnboarded: boolean;
  /** Enabled servers in the active profile. */
  enabledCount: number;
  onboardingOpen?: boolean;
  refreshKey?: number;
  /** Told which surface is on screen, so the toast stack can move up by the
   *  right amount instead of landing on top of it. Both live bottom-right. */
  onVisibleChange?: (surface: StarSurface) => void;
}

/**
 * The GitHub star ask. See `@/lib/starPrompt` for the rules this implements.
 */
export function GitHubStarPrompt({
  justOnboarded,
  enabledCount,
  onboardingOpen = false,
  refreshKey = 0,
  onVisibleChange,
}: Props) {
  // Freeze the audience for this launch; onboarding books a future chip.
  const [stage] = useState<StarStage>(readStarStage);
  const [dismissed, setDismissed] = useState(false);
  const [days, setDays] = useState(0);
  const [shown, setShown] = useState(false);
  const [returningReady, setReturningReady] = useState(false);
  // Nothing is shown, and so nothing is spent, unless the corner is genuinely
  // reachable. The app sits in the tray and the gateway can enable servers from
  // there, which would otherwise let the chip appear and burn its one showing
  // with no window on screen; a modal dialog is the same problem one layer up,
  // since it covers the corner, traps focus and aria-hides everything under it.
  const windowVisible = useWindowVisible();
  const modalOpen = useModalOpen();
  const reachable = windowVisible && !modalOpen && !onboardingOpen && !justOnboarded;

  const eligible = shown || (enabledCount >= STAR_MIN_ENABLED_SERVERS && days >= 2);
  const surface: StarSurface =
    dismissed || !reachable || !eligible
      ? null
      : stage === "returning" && returningReady
        ? "returning"
        : stage === "later"
          ? "chip"
          : null;

  useEffect(() => {
    if (
      !reachable ||
      stage === "done" ||
      shown ||
      enabledCount < STAR_MIN_ENABLED_SERVERS
    )
      return;
    let cancelled = false;
    // The retained audit file is already bounded by the gateway. Read all retained
    // rows so a busy day cannot hide an older qualifying day behind a page limit.
    void getAuditLog(2_147_483_647)
      .then((entries) => {
        if (!cancelled) setDays(toolCallDays(entries));
      })
      .catch(() => {
        if (!cancelled) setDays(0);
      });
    return () => {
      cancelled = true;
    };
  }, [reachable, stage, shown, enabledCount, refreshKey]);

  useEffect(() => {
    if (stage !== "returning" || !reachable) return;
    const timer = setTimeout(() => setReturningReady(true), RETURNING_DELAY_MS);
    return () => clearTimeout(timer);
  }, [stage, reachable]);

  // Book the deferred chip before the onboarding flag marks this as returning.
  useEffect(() => {
    if (stage === "card" && justOnboarded) writeStarStage("later");
  }, [stage, justOnboarded]);

  // Spend only after the DOM settles, so a modal mounting in the same commit
  // cannot consume an ask behind its overlay.
  useEffect(() => {
    if (!surface || modalOpen) return;
    const spend = setTimeout(() => {
      if (modalLayerOpen()) return;
      writeStarStage("done");
      setShown(true);
    });
    return () => clearTimeout(spend);
  }, [surface, modalOpen]);

  useEffect(() => {
    onVisibleChange?.(surface);
  }, [surface, onVisibleChange]);

  // Unmounting has to release the offset too, otherwise toasts stay pushed up.
  useEffect(() => () => onVisibleChange?.(null), [onVisibleChange]);

  function star() {
    void openExternal(STAR_REPO_URL);
    writeStarStage("done");
    setDismissed(true);
  }

  if (!surface) return null;

  if (surface === "chip") {
    return (
      <Corner>
        <div
          role="status"
          className={`${SHELL} flex items-center gap-1 rounded-full py-1 pr-1 pl-3`}
        >
          <button
            type="button"
            onClick={star}
            className={`inline-flex items-center gap-1.5 rounded-full text-xs font-medium transition hover:text-primary ${FOCUS_RING}`}
          >
            <Star className="size-3.5" />
            Star Toolport on GitHub
          </button>
          <CloseButton onClick={() => setDismissed(true)} className="rounded-full p-1" />
        </div>
      </Corner>
    );
  }

  return (
    <Corner>
      <div
        role="status"
        aria-label="Star Toolport on GitHub"
        className={`${SHELL} w-[min(20rem,calc(100vw-2rem))] rounded-xl p-4`}
      >
        <div className="flex items-start gap-2">
          <Star className="mt-0.5 size-4 shrink-0 text-warning" />
          <p className="flex-1 text-sm font-medium">Enjoying Toolport?</p>
          <CloseButton
            onClick={() => setDismissed(true)}
            className="-mt-0.5 -mr-0.5 rounded p-0.5"
          />
        </div>
        <p className="mt-1.5 text-sm text-muted-foreground">
          A GitHub star helps other developers find it.
        </p>
        <div className="mt-3 flex items-center gap-2">
          <Button size="sm" onClick={star}>
            <Star className="size-3.5" />
            Star on GitHub
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDismissed(true)}>
            No thanks
          </Button>
        </div>
      </div>
    </Corner>
  );
}

function Corner({ children }: { children: ReactNode }) {
  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-40 flex justify-end">
      {children}
    </div>
  );
}

/** Closing is always a "not now", never a separate refusal: the stage was
 *  already spent when the surface appeared, so this only hides it. */
function CloseButton({ onClick, className }: { onClick: () => void; className: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Dismiss"
      className={`text-muted-foreground transition hover:text-foreground ${FOCUS_RING} ${className}`}
    >
      <X className="size-3.5" />
    </button>
  );
}
