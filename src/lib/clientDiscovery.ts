import type { DetectedClient, Registry } from "@/lib/types";

/** The canonical name of a discovery mode someone chose, or "" for Auto (empty,
 * "inherit" or unknown). Mirrors clients::discovery::chosen_mode: "code-mode" also
 * accepts "codemode", "code_mode" and "code". */
export function chosenDiscoveryMode(mode: string | null | undefined): string {
  switch (mode?.trim().toLowerCase()) {
    case "full":
      return "full";
    case "lazy":
      return "lazy";
    case "grouped":
      return "grouped";
    case "code-mode":
    case "codemode":
    case "code_mode":
    case "code":
      return "code-mode";
    default:
      return "";
  }
}

/** How a client discovers tools: its own override, else Auto. Auto is the user's global
 * choice if they made one (mirrors Registry::chosen_global_discovery_mode), else the
 * backend client default, which lists a small catalog in full for clients with a
 * measured crossover. */
export function clientDiscovery(client: DetectedClient, registry: Registry | null) {
  const chosenGlobal =
    chosenDiscoveryMode(registry?.discoveryMode) ||
    (registry?.lazyDiscovery === false ? "full" : "") ||
    (registry?.legacyLazyDiscovery ? "lazy" : "");
  const autoMode = chosenGlobal || client.discovery?.autoMode || "lazy";
  const autoFullMax = chosenGlobal ? null : (client.discovery?.autoFullMaxTools ?? null);
  const clientMode = chosenDiscoveryMode(registry?.clientDiscovery?.[client.id]);
  return { autoMode, autoFullMax, clientMode, effectiveMode: clientMode || autoMode };
}
