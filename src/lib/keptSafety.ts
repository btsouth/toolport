import type { KeptV1Safety } from "@/lib/types";

// Same wording as `KeptV1Safety::summary` in src-tauri/src/registry.rs.
export function keptSafetySummary(kept: KeptV1Safety | undefined): string | null {
  if (!kept) return null;
  const parts = [
    kept.holdUntrusted && "asks before calls from shared or registry servers",
    kept.denyDestructive && "hides destructive tools",
    kept.quarantineOnDrift && "pauses tools whose definitions change",
    kept.blockOnInjection && "blocks results that look like prompt injection",
  ].filter((part): part is string => Boolean(part));
  if (parts.length === 0) return null;
  const list =
    parts.length === 1
      ? parts[0]
      : parts.length === 2
        ? `${parts[0]} and ${parts[1]}`
        : `${parts.slice(0, -1).join(", ")}, and ${parts[parts.length - 1]}`;
  return `Kept from 1.x: Toolport also ${list}.`;
}
