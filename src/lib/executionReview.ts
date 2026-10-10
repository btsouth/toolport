import type { ServerEntry } from "./types";
import { secretReferenceReview } from "./secretRefs";

/** Keep controls and invisible Unicode visible in plain React text nodes. */
export function visibleExecutionText(text: string): string {
  return Array.from(text, (c) => {
    const n = c.codePointAt(0)!;
    return /[\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}\u2028\u2029]/u.test(c)
      ? `\\u{${n.toString(16).toUpperCase().padStart(4, "0")}}`
      : c;
  }).join("");
}
function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([k, v]) => [k, canonical(v)]),
    );
  return value ?? null;
}
function show(value: unknown): string {
  return visibleExecutionText(
    typeof value === "string" ? value : JSON.stringify(canonical(value)),
  );
}
export function executionReviewFields(server: ServerEntry): Record<string, string> {
  const fields: Record<string, string> = {
    Command: show(server.command),
    Arguments: show(server.args),
    "Working directory": server.cwd == null ? "Client default" : show(server.cwd),
    Transport: show(server.transport),
    URL: show(server.url),
    inheritEnv: String(server.inheritEnv ?? false),
    "Launch bindings": show(server.launch?.bindings),
  };
  for (const [label, rows] of [
    ["Environment", server.env],
    ["Launch input", server.launch?.inputs ?? []],
  ] as const)
    rows.forEach((row, index) => {
      fields[`${label} [${index}] ${show(row.key)}`] =
        `${row.secret ? "<masked secret>" : show(row.value)}; reference: ${show(row.source?.ref)}`;
    });
  return fields;
}
export function executionReviewLines(server: ServerEntry): string[] {
  const fields = executionReviewFields(server);
  const previous = server.syncExecutionReview;
  const lines = Object.entries(fields)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(
      ([key, value]) => `${previous?.[key] === value ? "" : "CHANGED: "}${key}: ${value}`,
    );
  for (const key of Object.keys(previous ?? {}))
    if (!(key in fields)) lines.push(`CHANGED: ${visibleExecutionText(key)}: removed`);
  return [...lines, ...secretReferenceReview(server).map(visibleExecutionText)];
}
