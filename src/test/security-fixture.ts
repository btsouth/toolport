import type { SecurityEvent } from "@/lib/api";

/** Synthetic package-update findings, never the installed registry. */
export function securityFixture(): SecurityEvent[] {
  const ts = Date.now() - 4 * 60 * 60 * 1000;
  return [
    ...Array.from({ length: 26 }, (_, i) => ({
      ts,
      type: "tool_drift",
      server: "cloudflare_full_api",
      tool: `cloudflare_full_api__${i === 0 ? "update_dns_record" : `update_tool_${i}`}`,
      change: "changed",
      changed_fields: ["input_schema"],
      parameters: { added: ["comment"], removed: ["legacy_id"], changed: ["ttl"] },
      severity: "high" as const,
      blocked: false,
    })),
    ...Array.from({ length: 4 }, (_, i) => ({
      ts,
      type: "tool_drift",
      server: "revenuecat",
      tool: `revenuecat__read_tool_${i}`,
      change: "changed",
      changed_fields: ["description"],
      severity: "warn" as const,
      blocked: false,
    })),
  ];
}
