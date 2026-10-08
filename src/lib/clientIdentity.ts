/** Names arrive from the shared backend resolver, never initialize clientInfo. */
export function trustedClientName(client: { clientName?: string | null }): string {
  return client.clientName || "An AI client";
}

export function shortenClientLabel(label: string, limit: number): string {
  const chars = Array.from(label);
  return chars.length <= limit ? label : `${chars.slice(0, limit - 1).join("")}…`;
}
