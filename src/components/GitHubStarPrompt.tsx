import { useI18n } from "@/lib/i18n";
import { useEffect, useState, type ReactNode } from "react";
import { CircleCheck, Star, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { openExternal } from "@/lib/openUrl";
import { modalLayerOpen, useModalOpen, useWindowVisible } from "@/lib/windowVisible";
import {
  CHIP_MIN_ENABLED_SERVERS,
  RETURNING_MIN_ENABLED_SERVERS,
  STAR_REPO_URL,
  readStarStage,
  writeStarStage,
  type StarStage,
} from "@/lib/starPrompt";

/** Lets the onboarding dialog finish closing before the card slides in, so the
 *  two do not animate over each other. */
const CARD_DELAY_MS = 700;

/** How long an existing user is left alone before the one-off card appears. The
 *  clock only runs while the window is actually on screen: Toolport lives in the
 *  tray, so a launch-time timer would spend the single ask on nobody. */
const RETURNING_DELAY_MS = 8000;

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
const SHELL =
  "pointer-events-auto animate-in fade-in slide-in-from-bottom-2 border bg-popover/95 text-popover-foreground shadow-2xl backdrop-blur";

/** Which surface is on screen (null when none), for the toast-offset callback. */
export type StarSurface = "card" | "returning" | "chip" | null;

interface Props {
  /** True once the wizard has been finished in this session (card trigger). */
  justOnboarded: boolean;
  /** Enabled servers in the active profile. */
  enabledCount: number;
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
  onVisibleChange,
}: Props) {
  const { t } = useI18n();
  // Frozen at mount, so a stage change made during this session cannot promote
  // one surface into another. A "Later" answered by a chip minutes later would
  // be nagging; the chip belongs to the next launch.
  const [stage] = useState<StarStage>(readStarStage);
  const [dismissed, setDismissed] = useState(false);
  const [cardReady, setCardReady] = useState(false);
  const [returningReady, setReturningReady] = useState(false);
  // The most servers that have been enabled at once this session. The count is
  // a gate for reaching the ask, not a condition to keep meeting: without the
  // high-water mark, toggling a server off and back on retracts a prompt and
  // then shows it again, which is flicker for an ask that is already spent.
  const [peakEnabled, setPeakEnabled] = useState(enabledCount);
  if (enabledCount > peakEnabled) setPeakEnabled(enabledCount);
  // Nothing is shown, and so nothing is spent, unless the corner is genuinely
  // reachable. The app sits in the tray and the gateway can enable servers from
  // there, which would otherwise let the chip appear and burn its one showing
  // with no window on screen; a modal dialog is the same problem one layer up,
  // since it covers the corner, traps focus and aria-hides everything under it.
  const windowVisible = useWindowVisible();
  const modalOpen = useModalOpen();
  const reachable = windowVisible && !modalOpen;

  const surface: StarSurface =
    dismissed || !reachable
      ? null
      : stage === "card" && justOnboarded && cardReady
        ? "card"
        : stage === "returning" &&
            returningReady &&
            peakEnabled >= RETURNING_MIN_ENABLED_SERVERS
          ? "returning"
          : stage === "later" && peakEnabled >= CHIP_MIN_ENABLED_SERVERS
            ? "chip"
            : null;

  useEffect(() => {
    if (stage !== "card" || !justOnboarded) return;
    const timer = setTimeout(() => setCardReady(true), CARD_DELAY_MS);
    return () => clearTimeout(timer);
  }, [stage, justOnboarded]);

  useEffect(() => {
    if (stage !== "returning" || !reachable) return;
    const timer = setTimeout(() => setReturningReady(true), RETURNING_DELAY_MS);
    return () => clearTimeout(timer);
  }, [stage, reachable]);

  // Finishing the wizard is what makes this install definitely a new one, and
  // that fact is only in storage while the session lasts. The card itself is
  // delayed and gated on the window, so a launch that hides to the tray in
  // between would record nothing at all, and the next launch would read an
  // onboarding flag with no star record and mistake a day-old install for a
  // months-old one. Booking the chip fallback here costs a new user nothing:
  // it is the same value the card writes when it appears.
  useEffect(() => {
    if (stage === "card" && justOnboarded) writeStarStage("later");
  }, [stage, justOnboarded]);

  // Showing is what spends the ask. Quitting without answering must not earn a
  // second showing of the same surface. The new-user card falls back to the
  // chip; everything else is the last ask this install gets.
  //
  // Recorded a beat after the render rather than during it, because "shown" is
  // a claim about the screen and the screen is not settled yet. A dialog going
  // up in the same beat raises its overlay through a portal, which React mounts
  // on a later commit, so a surface can render into a corner that is about to
  // be covered. Letting the DOM settle and re-reading it is what stops the one
  // ask being spent behind a blurred overlay nobody can click through.
  useEffect(() => {
    if (!surface || modalOpen) return;
    const spend = setTimeout(() => {
      if (modalLayerOpen()) return;
      writeStarStage(surface === "card" ? "later" : "done");
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
            <Star className="size-3.5" /> {t("Star Toolport on GitHub")}{" "}
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
        aria-label={t("Star Toolport on GitHub")}
        className={`${SHELL} w-[min(20rem,calc(100vw-2rem))] rounded-xl p-4`}
      >
        <div className="flex items-start gap-2">
          {surface === "card" ? (
            <CircleCheck className="mt-0.5 size-4 shrink-0 text-success" />
          ) : (
            <Star className="mt-0.5 size-4 shrink-0 text-warning" />
          )}
          <p className="flex-1 text-sm font-medium">
            {surface === "card" ? t("You're all set") : t("Enjoying Toolport?")}
          </p>
          <CloseButton
            onClick={() => setDismissed(true)}
            className="-mt-0.5 -mr-0.5 rounded p-0.5"
          />
        </div>
        <p className="mt-1.5 text-sm text-muted-foreground">
          {surface === "card"
            ? t("If Toolport is useful, a GitHub star helps other developers find it.")
            : t("A GitHub star helps other developers find it.")}
        </p>
        <div className="mt-3 flex items-center gap-2">
          <Button size="sm" onClick={star}>
            <Star className="size-3.5" /> {t("Star on GitHub")}{" "}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDismissed(true)}>
            {/* "Later" is only honest on the new-user card, which really does
                get a second (and final) chance as the chip. */}
            {surface === "card" ? t("Later") : t("No thanks")}
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
  const { t } = useI18n();
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={t("Dismiss")}
      className={`text-muted-foreground transition hover:text-foreground ${FOCUS_RING} ${className}`}
    >
      <X className="size-3.5" />
    </button>
  );
}
