/** Names arrive from the shared backend resolver, never initialize clientInfo. */
export function trustedClientName(client: { clientName?: string | null }): string {
  return client.clientName || "An AI client";
}

export function shortenClientLabel(label: string, limit: number): string {
  const chars = Array.from(label);
  return chars.length <= limit ? label : `${chars.slice(0, limit - 1).join("")}…`;
}

/** One compact identity for an Activity meta line, with reports kept explicit. */
export function activityClientName(client: { clientName?: string | null; clientLabel?: string | null }): string {
  const name = trustedClientName(client);
  const label = client.clientLabel;
  if (!label || label === name) return name;
  if (label.startsWith(name)) return `${name} ${label.slice(name.length).trim()}`;
  return `${name} (reports "${label}")`;
}
