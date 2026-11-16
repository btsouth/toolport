import { msg, translate } from "./i18n";
export const UNRECORDED_CLIENT_TOOLTIP = msg(
  "Older Toolport versions did not record callers for these rows.",
);

/** The client ID of a Run from a server's Tools tab in Toolport itself. */
const TOOLS_TAB_CLIENT = "adapter:toolport-tools-tab";

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
  // Toolport's own Tools tab Runs: a name Toolport gives, so it is translated.
  if (client.client === TOOLS_TAB_CLIENT) return translate("Toolport Tools tab");
  if (hasTrustedName(client.clientName) && client.clientName) return client.clientName;
  if (client.clientLabel)
    return translate("{name} (reported)", { name: client.clientLabel });
  return client.client || client.clientName === "Unknown client"
    ? translate("Unknown client")
    : translate("Unrecorded client");
}

export function clientIdentityTooltip(
  client: Parameters<typeof trustedClientName>[0],
): string | undefined {
  return !hasTrustedName(client.clientName) &&
    !client.clientLabel &&
    !client.client &&
    client.clientName !== "Unknown client"
    ? translate(UNRECORDED_CLIENT_TOOLTIP)
    : undefined;
}

export function shortenClientLabel(label: string, limit: number): string {
  const chars = Array.from(label);
  return chars.length <= limit ? label : `${chars.slice(0, limit - 1).join("")}…`;
}

/** The name a client reported, when it adds something to the trusted name. */
export function reportedClientLabel(
  client: Parameters<typeof trustedClientName>[0],
): string | null {
  const name = trustedClientName(client);
  const label = client.clientLabel;
  if (!hasTrustedName(client.clientName) || !label || reportsOnlyVersion(name, label)) {
    return null;
  }
  return label;
}

/** One compact identity, with reports kept explicit. */
export function activityClientName(
  client: Parameters<typeof trustedClientName>[0],
): string {
  const name = trustedClientName(client);
  const label = reportedClientLabel(client);
  return label ? translate('{name} (reports "{label}")', { name, label }) : name;
}

/**
 * The leading segments of an Activity meta line: the client (with its host, such as
 * "via T3 Code"), then what it reported, as its own segment so the host stays beside
 * the client it describes.
 */
export function activityClientSegments(
  client: Parameters<typeof trustedClientName>[0],
  hosted: (name: string) => string,
): string[] {
  const label = reportedClientLabel(client);
  return [
    hosted(trustedClientName(client)),
    ...(label ? [translate('reports "{label}"', { label })] : []),
  ];
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
