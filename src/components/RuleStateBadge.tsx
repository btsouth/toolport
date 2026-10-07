import { AlertTriangle, Ban, Check, Clock, Minus } from "lucide-react";
import type { InstructionsApplyState } from "@/lib/types";

/**
 * How each per-client instructions state renders in the Teams tab (org instructions, spec W4).
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
};

/** The state badge for one client, icon + label. */
export function RuleStateBadge({ state }: { state: InstructionsApplyState }) {
  const meta = RULE_STATE_META[state];
  const Icon = meta.Icon;
  return (
    <span
      title={EXPLANATION[state]}
      className={`flex shrink-0 items-center gap-1 text-xs ${meta.className}`}
    >
      <Icon className="size-3.5" />
      {meta.label}
    </span>
  );
}
