import type { CatalogEntry, Registry } from "./types";

/** Launch/endpoint identity shared with catalog.rs. Names never imply installed. */
export function catalogIdentity(
  entry: Pick<CatalogEntry, "transport" | "command" | "args" | "url">,
): string | null {
  if (entry.transport !== "stdio") {
    if (!entry.url) return null;
    try {
      const url = new URL(entry.url.trim());
      if (!["http:", "https:"].includes(url.protocol)) return null;
      url.username = "";
      url.password = "";
      url.hash = "";
      const params = new URLSearchParams(url.search);
      const secretKeys = new Set([
        "token",
        "access_token",
        "api_key",
        "apikey",
        "key",
        "secret",
        "client_secret",
        "password",
        "auth",
        "authorization",
        "sig",
        "signature",
        "x-api-key",
        "credential",
        "credentials",
        "api-key",
      ]);
      for (const key of [...params.keys()]) {
        if (secretKeys.has(key.toLowerCase())) params.delete(key);
      }
      const sorted = [...params.entries()].sort(([ak, av], [bk, bv]) =>
        ak < bk ? -1 : ak > bk ? 1 : av < bv ? -1 : av > bv ? 1 : 0,
      );
      url.search = new URLSearchParams(sorted).toString();
      url.pathname = url.pathname.replace(/\/+$/, "") || "/";
      return `remote:${url.href}`;
    } catch {
      return null;
    }
  }
  const command = entry.command?.trim();
  if (!command) return null;
  const runner = command
    .split(/[\\/]/)
    .at(-1)!
    .replace(/\.(cmd|exe)$/, "");
  const args = [...entry.args];
  if (runner === "npx" || runner === "uvx") {
    if (runner === "npx" && ["-y", "--yes"].includes(args[0])) args.shift();
    const index = runner === "uvx" && args[0] === "--from" ? 1 : 0;
    if (args[index]) {
      if (runner === "npx") args[index] = args[index].replace(/(.+)@[^@]+$/, "$1");
      else args[index] = args[index].split(/==|@/)[0].replace(/_/g, "-").toLowerCase();
    }
    return JSON.stringify([runner, args]);
  }
  if (runner === "docker" && args[0] === "run") {
    // Skip run flags and their values before the image, retaining all arguments.
    const valueFlags = new Set([
      "-e",
      "--env",
      "--env-file",
      "-v",
      "--volume",
      "-p",
      "--publish",
      "--name",
      "--network",
      "--entrypoint",
      "-w",
      "--workdir",
      "-u",
      "--user",
      "--mount",
    ]);
    let index = 1;
    while (args[index]?.startsWith("-")) {
      index += valueFlags.has(args[index]) ? 2 : 1;
    }
    if (args[index])
      args[index] = args[index].split("@sha256:")[0].replace(/:[^/:]+$/, "");
    return JSON.stringify([runner, args]);
  }
  return JSON.stringify([command, args]);
}

export function catalogInstalledIdentities(
  server: Pick<
    Registry["servers"][number],
    "transport" | "command" | "args" | "url" | "name" | "source"
  >,
): string[] {
  const identity = catalogIdentity(server);
  return [
    ...(identity === null ? [] : [identity]),
    ...(server.source === "catalog:curated" ? [`curated:${server.name}`] : []),
  ];
}

export function installed(have: Set<string>, entry: CatalogEntry): boolean {
  const identity =
    catalogIdentity(entry) ??
    (entry.source === "curated" ? `curated:${entry.name}` : null);
  return identity !== null && have.has(identity);
}
