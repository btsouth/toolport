/**
 * Most MCP clients only read their config at startup (Claude Desktop especially).
 * After Toolport writes a gateway entry, a success toast that only says "Connected"
 * leaves the user thinking the product is broken until they happen to restart
 * (SOU-317). Universal wording: a restart never hurts clients that hot-reload.
 *
 * Toolport Studio injects the gateway into each provider session at start, so a
 * full app restart is unnecessary - a new conversation is enough.
 */
export function clientRestartHint(clientName: string, clientId?: string): string {
  if (clientId === "toolport-studio") {
    return "Start a new conversation in Toolport Studio so it picks up this scope.";
  }
  return `Restart ${clientName} so it loads Toolport.`;
}

/** Short product note for the Studio Clients row (zero-config tools + Connect for scope). */
export function toolportStudioClientBlurb(): string {
  return "Studio discovers Toolport automatically for every conversation. Connect here to pin a profile and show as connected in Activity.";
}

/** Connect/rescope toast body: restart first, then optional scope/backup notes. */
export function connectSuccessDescription(
  clientName: string,
  extras: Array<string | undefined | null | false> = [],
  clientId?: string,
): string {
  return [clientRestartHint(clientName, clientId), ...extras.filter(Boolean)].join(" ");
}
