import type { ProbeResult } from "./types";
import { errorHeadline } from "./errors";

export function serverFailureLabel(health: ProbeResult): string {
  if (health.authRequired)
    return health.authTarget === "service_credential"
      ? "Service key required"
      : "Needs sign-in";
  if (health.error?.includes("downstream server exited")) return "Server stopped";
  if (health.error?.includes("Broken pipe")) return "Connection closed";
  switch (health.failure?.kind) {
    case "auth":
      return health.failure.target === "scope" ? "Permission required" : "Needs sign-in";
    case "timeout":
      return "Timed out";
    case "unavailable":
      return "Unreachable";
    case "server_error":
      return "Server failed";
    case "quota":
      return "Rate limited";
    case "cancelled":
      return "Check cancelled";
    default:
      return health.error ? errorHeadline(health.error) : "Connection failed";
  }
}
