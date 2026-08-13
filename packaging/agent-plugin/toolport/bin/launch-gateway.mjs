#!/usr/bin/env node
// Locates the installed toolport-gateway binary and runs it over stdio.
//
// Agent-plugin clients spawn `node <this file>` from mcp.json. The gateway
// itself is installed by the Toolport desktop app, whose location differs per
// OS and install method, so this launcher mirrors the same search order the
// app uses when it writes client configs (clients.rs::resolve_gateway_path /
// gateway_publish.rs), newest-first:
//
//   any OS   $TOOLPORT_GATEWAY (explicit override, absolute path)
//   Windows  %APPDATA%\Toolport\bin\gateway-manifest.json -> recorded path,
//            else the newest toolport-gateway-*.exe in that bin dir,
//            else the NSIS install dir %LOCALAPPDATA%\Toolport
//   macOS    Toolport.app helper bundle (Contents/Helpers/ToolportGateway.app),
//            else the Contents/MacOS symlink, in /Applications then ~/Applications
//   Linux    /usr/bin, /usr/local/bin (deb), else <config>/Toolport/bin (the
//            AppImage stable copy)
//   any OS   bare `toolport-gateway` on PATH
//
// The legacy `Conduit` data-dir leaf and `conduit-gateway` names are checked as
// fallbacks so an install updated in place keeps working. If nothing is found,
// the launcher answers the client's first JSON-RPC request with a clear error
// instead of dying silently.

import { spawn } from "node:child_process";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import process from "node:process";

const GATEWAY = "toolport-gateway";
const LEGACY_GATEWAY = "conduit-gateway";
const EXE = process.platform === "win32" ? ".exe" : "";

/** Newest gateway binary in a published bin dir, by modification time.
 *  Matches both plain versioned (`toolport-gateway-1.12.0.exe`) and
 *  content-addressed (`...-1.12.0-<digest>.exe`) names. */
function newestPublished(binDir) {
  let best = null;
  let bestMtime = 0;
  let entries;
  try {
    entries = readdirSync(binDir);
  } catch {
    return null;
  }
  for (const name of entries) {
    if (!name.startsWith(`${GATEWAY}-`) || !name.endsWith(EXE || ".exe")) continue;
    const full = join(binDir, name);
    try {
      const mtime = statSync(full).mtimeMs;
      if (mtime > bestMtime) {
        best = full;
        bestMtime = mtime;
      }
    } catch {
      /* unreadable entry: skip */
    }
  }
  return best;
}

/** The path recorded by the app in gateway-manifest.json, when still present. */
function manifestPath(binDir) {
  try {
    const manifest = JSON.parse(
      readFileSync(join(binDir, "gateway-manifest.json"), "utf8"),
    );
    if (typeof manifest.path === "string" && existsSync(manifest.path))
      return manifest.path;
  } catch {
    /* no manifest, or unreadable: fall through to scanning */
  }
  return null;
}

function candidates() {
  const home = homedir();
  const found = [];
  if (process.platform === "win32") {
    const roaming = process.env.APPDATA || join(home, "AppData", "Roaming");
    const local = process.env.LOCALAPPDATA || join(home, "AppData", "Local");
    for (const leaf of ["Toolport", "Conduit"]) {
      const binDir = join(roaming, leaf, "bin");
      const fromManifest = manifestPath(binDir);
      if (fromManifest) found.push(fromManifest);
      const published = newestPublished(binDir);
      if (published) found.push(published);
    }
    for (const leaf of ["Toolport", "Conduit"]) {
      for (const name of [GATEWAY, LEGACY_GATEWAY]) {
        found.push(join(local, leaf, `${name}${EXE}`));
      }
    }
  } else if (process.platform === "darwin") {
    for (const appsDir of ["/Applications", join(home, "Applications")]) {
      const contents = join(appsDir, "Toolport.app", "Contents");
      found.push(
        join(contents, "Helpers", "ToolportGateway.app", "Contents", "MacOS", GATEWAY),
        join(
          contents,
          "Helpers",
          "ConduitGateway.app",
          "Contents",
          "MacOS",
          LEGACY_GATEWAY,
        ),
        join(contents, "MacOS", GATEWAY),
      );
    }
  } else {
    found.push(join("/usr/bin", GATEWAY), join("/usr/local/bin", GATEWAY));
    const configBase = process.env.XDG_CONFIG_HOME || join(home, ".config");
    for (const leaf of ["Toolport", "Conduit"]) {
      found.push(join(configBase, leaf, "bin", GATEWAY));
    }
  }
  return found;
}

/** Speak just enough JSON-RPC to surface a useful error in the client's UI,
 *  then exit. Clients treat a silent instant exit as a broken server. */
function failNotInstalled(detail) {
  const message =
    `Toolport is not installed on this machine (the ${GATEWAY} binary was not found` +
    (detail ? `: ${detail}` : "") +
    "). Install Toolport from https://toolport.app and open it once, then restart this MCP server.";
  process.stderr.write(`${message}\n`);
  let buffered = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => {
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) >= 0) {
      const line = buffered.slice(0, newline).trim();
      buffered = buffered.slice(newline + 1);
      if (!line) continue;
      try {
        const request = JSON.parse(line);
        if (request.id !== undefined && request.id !== null) {
          process.stdout.write(
            `${JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code: -32603, message } })}\n`,
          );
          process.exit(1);
        }
      } catch {
        /* not a full JSON line yet */
      }
    }
  });
  process.stdin.on("end", () => process.exit(1));
  // A client that never sends a request would otherwise keep us alive forever.
  setTimeout(() => process.exit(1), 30_000);
}

function run(binary, isLastResort) {
  const child = spawn(binary, process.argv.slice(2), {
    stdio: "inherit",
    windowsHide: true,
  });
  child.on("error", (err) => {
    if (isLastResort)
      failNotInstalled(err.code === "ENOENT" ? "not on PATH either" : err.message);
    else failNotInstalled(err.message);
  });
  child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 1)));
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => child.kill(signal));
  }
}

const override = process.env.TOOLPORT_GATEWAY;
if (override) {
  // An explicit override is used as-is, never silently substituted: if it is
  // wrong, the spawn error surfaces through failNotInstalled.
  run(override, false);
} else {
  const resolved = candidates().find((p) => existsSync(p));
  if (resolved) run(resolved, false);
  // Last resort: let the OS search PATH (covers a from-source `cargo install`
  // or a user who added the binary to PATH themselves).
  else run(GATEWAY, true);
}
