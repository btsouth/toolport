import type { SecurityEvent } from "./api";
import type { Registry } from "./types";

const levels = ["off", "ask", "strict"] as const;

/** Whether Activity asks the user to review or accept tool changes. Only when
 * nothing can be blocked (effective level Off, no kept or team-forced quarantine
 * or injection block) are changes shown as plain history. Same rule as
 * `Registry::tool_change_reviews_enabled` in src-tauri/src/registry.rs. */
export function toolChangeReviewsEnabled(registry: Registry | null): boolean {
  if (!registry) return true;
  const legacy = registry.version < 2 && registry.safetyLevel == null;
  const member =
    registry.safetyLevel ??
    (!legacy || registry.humanApproval || registry.confirmDestructive ? "ask" : "off");
  const effective = Math.max(
    levels.indexOf(member),
    levels.indexOf(registry.teamMinSafetyLevel ?? "off"),
    registry.teamForcedHumanApproval ? 1 : 0,
  );
  if (effective > 0) return true;
  const kept = legacy
    ? {
        quarantineOnDrift: registry.quarantineOnDrift,
        blockOnInjection: registry.blockOnInjection,
      }
    : (registry.keptV1Safety ?? {});
  return Boolean(
    registry.teamForcedQuarantineOnDrift ||
    registry.teamForcedBlockOnInjection ||
    kept.quarantineOnDrift ||
    kept.blockOnInjection,
  );
}

/** A tool change that was actually blocked asks for a review whatever the level. */
export function toolChangeAsksReview(event: SecurityEvent, reviewsEnabled: boolean) {
  return reviewsEnabled || event.blocked === true;
}
