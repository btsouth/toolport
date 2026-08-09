/** The hosted Toolport Teams **app**, prefilled as the default server URL. Self-hosters
 * replace it with their own server. */
export const HOSTED_TEAMS_URL = "https://teams.toolport.app";

/** The public **explainer** page — deliberately not [`HOSTED_TEAMS_URL`].
 *
 * Onboarding's "What is Toolport for Teams?" link targets this: someone reading it has
 * no team and no invite code yet, so sending them to the app would land them on a sign-in
 * for something they have not been told about. The two lived as three separate string
 * literals across two components, which is how they drifted (SBS-461). */
export const TEAMS_MARKETING_URL = "https://toolport.app/teams";

export function teamUrlError(raw: string): string | null {
  const value = raw.trim();
  if (!value) return "Server URL is required.";

  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return "Team server URL must start with https://.";
  }

  if (url.protocol === "https:") return null;
  if (url.protocol !== "http:") return "Team server URL must start with https://.";

  const host = url.hostname.toLowerCase();
  const loopback =
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "127.0.0.1" ||
    host === "::1" ||
    host === "[::1]";

  return loopback
    ? null
    : "Team server URL must use https:// unless it is loopback HTTP for local development.";
}
