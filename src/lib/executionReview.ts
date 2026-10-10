import type { ArgBinding, ServerEntry } from "./types";

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
function argumentValues(bindings: ArgBinding[]): string {
  return bindings
    .map(
      (binding) =>
        `Argument ${binding.index + 1} = ${binding.parts
          .map((part) =>
            part.kind === "literal" ? show(part.value) : `{${show(part.key)}}`,
          )
          .join("")}`,
    )
    .join("\n");
}
function reviewBaseline(previous: Record<string, string>): Record<string, string> {
  const fields = { ...previous };
  if (!("Transport" in fields) && !("inheritEnv" in fields)) return fields;
  const stdio = fields.Transport === "stdio";
  delete fields.Transport;
  for (const key of ["Command", "URL", "Arguments"])
    if (["null", "[]", ""].includes(fields[key])) delete fields[key];
  if (fields.Arguments?.startsWith("[")) {
    try {
      const args: string[] = JSON.parse(fields.Arguments);
      fields.Arguments = args.map((arg, i) => `\n  ${i + 1}. ${show(arg)}`).join("");
    } catch {
      /* Keep an unfamiliar saved value visible as a change. */
    }
  }
  const cwd = fields["Working directory"];
  delete fields["Working directory"];
  if (cwd && cwd !== "Client default") fields["Working folder"] = cwd;
  if (stdio)
    fields["Uses this machine's environment"] =
      fields.inheritEnv === "true" ? "yes" : "no";
  delete fields.inheritEnv;
  const bindings = fields["Launch bindings"];
  delete fields["Launch bindings"];
  if (bindings && bindings !== "null" && bindings !== "[]") {
    try {
      fields["Argument values"] = argumentValues(JSON.parse(bindings));
    } catch {
      fields["Argument values"] = bindings;
    }
  }
  for (const key of Object.keys(fields)) {
    const match = /^(Environment|Launch input) \[\d+\] (.*)$/.exec(key);
    if (!match) continue;
    const value = fields[key];
    const split = value.lastIndexOf("; reference: ");
    const local = split < 0 ? value : value.slice(0, split);
    const ref = split < 0 ? "null" : value.slice(split + 13);
    fields[`${match[1] === "Environment" ? "Environment" : "Input"}: ${match[2]}`] =
      ref !== "null"
        ? `Password manager: ${ref}`
        : local === "null"
          ? "Set on this machine"
          : local;
    delete fields[key];
  }
  return fields;
}
export function executionReviewFields(server: ServerEntry): Record<string, string> {
  const fields: Record<string, string> = {};
  if (server.transport === "stdio") {
    if (server.command) fields.Command = show(server.command);
    if (server.args.length)
      fields.Arguments = server.args
        .map((arg, i) => `\n  ${i + 1}. ${show(arg)}`)
        .join("");
    if (server.cwd) fields["Working folder"] = show(server.cwd);
    fields["Uses this machine's environment"] = server.inheritEnv ? "yes" : "no";
    if (server.launch?.bindings.length)
      fields["Argument values"] = argumentValues(server.launch.bindings);
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
  const previous = server.syncExecutionReview
    ? reviewBaseline(server.syncExecutionReview)
    : undefined;
  const lines = Object.entries(fields)
    .sort(([a], [b]) => a.localeCompare(b))
    .filter(([key, value]) => !previous || previous[key] !== value)
    .map(([key, value]) => executionReviewFieldLine(key, value));
  for (const key of Object.keys(previous ?? {}))
    if (!(key in fields))
      lines.push(executionReviewFieldLine(visibleExecutionText(key), "Removed"));
  return previous ? lines : ["New server", ...lines];
}
