export const UNRECORDED_CLIENT_TOOLTIP =
  "Older Toolport versions did not record callers for these rows.";

function hasTrustedName(name?: string | null): boolean {
  return (
    !!name && !["An AI client", "Unknown client", "Unrecorded client"].includes(name)
  );
}

/** Display identity only. Reported labels never select access scope. */
export function trustedClientName(client: {
  clientName?: string | null;
  clientLabel?: string | null;
  client?: string | null;
}): string {
  if (hasTrustedName(client.clientName) && client.clientName) return client.clientName;
  if (client.clientLabel) return `${client.clientLabel} (reported)`;
  return client.client || client.clientName === "Unknown client"
    ? "Unknown client"
    : "Unrecorded client";
}

export function clientIdentityTooltip(
  client: Parameters<typeof trustedClientName>[0],
): string | undefined {
  return trustedClientName(client) === "Unrecorded client"
    ? UNRECORDED_CLIENT_TOOLTIP
    : undefined;
}

export function shortenClientLabel(label: string, limit: number): string {
  const chars = Array.from(label);
  return chars.length <= limit ? label : `${chars.slice(0, limit - 1).join("")}…`;
}

/** One compact identity for an Activity meta line, with reports kept explicit. */
export function activityClientName(
  client: Parameters<typeof trustedClientName>[0],
): string {
  const name = trustedClientName(client);
  const label = client.clientLabel;
  if (!hasTrustedName(client.clientName) || !label || reportsOnlyVersion(name, label)) {
    return name;
  }
  return `${name} (reports "${label}")`;
}

/** "inbox 1" or "Claude Code 2.1" adds nothing to the name it follows. */
function reportsOnlyVersion(name: string, label: string): boolean {
  const lower = label.toLowerCase();
  const prefix = name.toLowerCase();
  return (
    lower === prefix ||
    (lower.startsWith(`${prefix} `) && !lower.slice(prefix.length + 1).includes(" "))
  );
}
