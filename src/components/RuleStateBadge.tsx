import { useI18n } from "@/lib/i18n";
import { AlertTriangle, Ban, Check, Clock, Minus, Pencil } from "lucide-react";
import type { InstructionsApplyState } from "@/lib/types";

/**
 * How each per-client rules state renders. Shared by the Teams tab (org instructions, spec W4)
 * and the Rules tab (the user's own sets): both run through the same writer and report the same
 * `ApplyState`, so they must read identically. A user seeing "Not applied yet" in one place and
 * different wording for the same on-disk situation in the other would reasonably think they were
 * different problems.
 */
const RULE_STATE_META: Record<
  InstructionsApplyState,
  { label: string; className: string; Icon: typeof Check }
> = {
  applied: { label: "Applied", className: "text-success", Icon: Check },
  stale: { label: "Not applied yet", className: "text-warning", Icon: Clock },
  blocked_override: {
    label: "Blocked by a local override",
    className: "text-warning",
    Icon: Ban,
  },
  too_long: {
    label: "Too long for this client",
    className: "text-warning",
    Icon: AlertTriangle,
  },
  unsupported: {
    label: "Copy manually",
    className: "text-muted-foreground",
    Icon: Minus,
  },
  error: { label: "Write error", className: "text-destructive", Icon: AlertTriangle },
  drifted: { label: "Edited on disk", className: "text-warning", Icon: Pencil },
};

/** Why a client is in this state, in one sentence. Surfaced as the badge's tooltip. */
const EXPLANATION: Record<InstructionsApplyState, string> = {
  applied: "This client's rules file is up to date.",
  stale: "The current rules are not on disk for this client yet.",
  blocked_override:
    "This client has a local override file that makes it ignore the file Toolport writes.",
  too_long: "This client caps its global rules file, and these rules would exceed it.",
  unsupported:
    "This client has no global rules file Toolport can write. Paste the rules in by hand.",
  error: "The rules file could not be read or written. It was left untouched.",
  drifted:
    "Toolport wrote this block and it has been changed in the file since. Toolport leaves it alone until you pull the change into the set or overwrite it.",
};

/** The state badge for one client, icon + label. */
export function RuleStateBadge({ state }: { state: InstructionsApplyState }) {
  const { t: tr } = useI18n();
  const meta = RULE_STATE_META[state];
  const Icon = meta.Icon;
  return (
    <span
      title={tr(EXPLANATION[state])}
      className={`flex shrink-0 items-center gap-1 text-xs ${meta.className}`}
    >
      <Icon className="size-3.5" />
      {tr(meta.label)}
    </span>
  );
}
