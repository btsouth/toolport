import type { ServerEntry } from "./types";

import { visibleExecutionText } from "./visibleExecutionText";
export { visibleExecutionText } from "./visibleExecutionText";

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
  const fields: Record<string, string> = {};
  if (server.transport === "stdio") {
    if (server.command) fields.Command = show(server.command);
    if (server.args.length) fields.Arguments = show(server.args);
    if (server.cwd) fields["Working folder"] = show(server.cwd);
    fields["Uses this machine's environment"] = server.inheritEnv ? "yes" : "no";
    if (server.launch?.bindings.length)
      fields["Argument values"] = server.launch.bindings
        .map(
          (binding) =>
            `Argument ${binding.index + 1} = ${binding.parts
              .map((part) =>
                part.kind === "literal" ? show(part.value) : `{${show(part.key)}}`,
              )
              .join("")}`,
        )
        .join("\n");
  } else if (server.url) fields.URL = show(server.url);
  for (const [label, rows] of [
    ["Environment", server.env],
    ["Input", server.launch?.inputs ?? []],
    ["Header", server.headerKeys ?? []],
  ] as const)
    rows.forEach((row) => {
      fields[`${label}: ${show(row.key)}`] =
        "env" in row && row.env
          ? `Uses environment: ${show(row.env)}`
          : row.source?.ref
            ? `Password manager: ${show(row.source.ref)}`
            : "secret" in row && row.secret
              ? "<masked secret>"
              : !("value" in row) || row.value == null
                ? "Set on this machine"
                : show(row.value);
    });
  return fields;
}
export function executionReviewFieldLine(key: string, value: string): string {
  return /^(Environment|Input|Header):/.test(key)
    ? `${key} = ${value}`
    : `${key}: ${value}`;
}
export function executionReviewLines(server: ServerEntry): string[] {
  const fields = executionReviewFields(server);
  const previous = server.syncExecutionReview;
  const lines = Object.entries(fields)
    .sort(([a], [b]) => a.localeCompare(b))
    .filter(([key, value]) => !previous || previous[key] !== value)
    .map(([key, value]) => executionReviewFieldLine(key, value));
  for (const key of Object.keys(previous ?? {}))
    if (!(key in fields))
      lines.push(executionReviewFieldLine(visibleExecutionText(key), "Removed"));
  return previous ? lines : ["New server", ...lines];
}
