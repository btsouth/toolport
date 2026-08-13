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
import { readFileSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, posix, win32 } from "node:path";
import process from "node:process";
import { setTimeout } from "node:timers";
import { fileURLToPath, URL } from "node:url";

const GATEWAY = "toolport-gateway";
const LEGACY_GATEWAY = "conduit-gateway";
const defaultFs = { readFileSync, readdirSync, statSync };

function pluginVersion(fsOps) {
  try {
    return JSON.parse(
      fsOps.readFileSync(new URL("../plugin.json", import.meta.url), "utf8"),
    ).version;
  } catch {
    return null;
  }
}

/** Newest gateway binary in a published bin dir, by modification time.
 *  Matches both plain versioned (`toolport-gateway-1.12.0.exe`) and
 *  content-addressed (`...-1.12.0-<digest>.exe`) names. */
function newestPublished(binDir, fsOps, pathImpl, exe) {
  let best = null;
  let bestMtime = 0;
  let entries;
  try {
    entries = fsOps.readdirSync(binDir);
  } catch {
    return null;
  }
  for (const name of entries) {
    if (
      ![GATEWAY, LEGACY_GATEWAY].some((prefix) => name.startsWith(`${prefix}-`)) ||
      !name.endsWith(exe || ".exe")
    )
      continue;
    const full = pathImpl.join(binDir, name);
    try {
      const mtime = fsOps.statSync(full).mtimeMs;
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
function manifestPath(binDir, fsOps, pathImpl) {
  try {
    const manifest = JSON.parse(
      fsOps.readFileSync(pathImpl.join(binDir, "gateway-manifest.json"), "utf8"),
    );
    // Do not preflight with existsSync: MSIX filesystem virtualization can hide a
    // host path from stat/read APIs even though CreateProcess can launch it.
    if (typeof manifest.path === "string") return manifest.path;
  } catch {
    /* no manifest, or unreadable: fall through to scanning */
  }
  return null;
}

export function gatewayCandidates({
  platform = process.platform,
  env = process.env,
  home = homedir(),
  fsOps = defaultFs,
  version,
} = {}) {
  const pathImpl = platform === "win32" ? win32 : posix;
  const exe = platform === "win32" ? ".exe" : "";
  const found = [];
  if (platform === "win32") {
    const roaming = env.APPDATA || pathImpl.join(home, "AppData", "Roaming");
    const local = env.LOCALAPPDATA || pathImpl.join(home, "AppData", "Local");
    for (const leaf of ["Toolport", "Conduit"]) {
      const binDir = pathImpl.join(roaming, leaf, "bin");
      const fromManifest = manifestPath(binDir, fsOps, pathImpl);
      if (fromManifest) found.push(fromManifest);
      // The normal published filename can be constructed from this plugin's
      // lockstep version even when MSIX hides the directory and manifest.
      const candidateVersion = version ?? pluginVersion(fsOps);
      if (candidateVersion) {
        found.push(
          pathImpl.join(binDir, `${GATEWAY}-${candidateVersion}${exe}`),
          pathImpl.join(binDir, `${LEGACY_GATEWAY}-${candidateVersion}${exe}`),
        );
      }
      const published = newestPublished(binDir, fsOps, pathImpl, exe);
      if (published) found.push(published);
    }
    for (const leaf of ["Toolport", "Conduit"]) {
      for (const name of [GATEWAY, LEGACY_GATEWAY]) {
        found.push(pathImpl.join(local, leaf, `${name}${exe}`));
      }
    }
  } else if (platform === "darwin") {
    for (const appsDir of ["/Applications", pathImpl.join(home, "Applications")]) {
      for (const leaf of ["Toolport", "Conduit"]) {
        const contents = pathImpl.join(appsDir, `${leaf}.app`, "Contents");
        found.push(
          pathImpl.join(
            contents,
            "Helpers",
            "ToolportGateway.app",
            "Contents",
            "MacOS",
            GATEWAY,
          ),
          pathImpl.join(
            contents,
            "Helpers",
            "ConduitGateway.app",
            "Contents",
            "MacOS",
            LEGACY_GATEWAY,
          ),
          pathImpl.join(contents, "MacOS", GATEWAY),
          pathImpl.join(contents, "MacOS", LEGACY_GATEWAY),
        );
      }
    }
  } else {
    for (const dir of ["/usr/bin", "/usr/local/bin"]) {
      found.push(pathImpl.join(dir, GATEWAY), pathImpl.join(dir, LEGACY_GATEWAY));
    }
    const configBase = env.XDG_CONFIG_HOME || pathImpl.join(home, ".config");
    for (const leaf of ["Toolport", "Conduit"]) {
      found.push(
        pathImpl.join(configBase, leaf, "bin", GATEWAY),
        pathImpl.join(configBase, leaf, "bin", LEGACY_GATEWAY),
      );
    }
  }
  // Let the OS search PATH only after every absolute candidate has failed.
  found.push(GATEWAY, LEGACY_GATEWAY);
  return [...new Set(found)];
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

export function spawnFirst(
  binaries,
  {
    args = process.argv.slice(2),
    spawnImpl = spawn,
    stdio = "inherit",
    windowsHide = true,
  } = {},
) {
  return new Promise((resolve, reject) => {
    let lastError = null;
    const attempt = (index) => {
      if (index >= binaries.length) {
        reject(lastError ?? new Error("no gateway candidates"));
        return;
      }
      const child = spawnImpl(binaries[index], args, { stdio, windowsHide });
      let started = false;
      child.once("spawn", () => {
        started = true;
        for (const signal of ["SIGINT", "SIGTERM"]) {
          process.once(signal, () => child.kill(signal));
        }
      });
      child.once("error", (error) => {
        if (started) reject(error);
        else {
          lastError = error;
          attempt(index + 1);
        }
      });
      child.once("exit", (code, signal) => resolve(signal ? 1 : (code ?? 1)));
    };
    attempt(0);
  });
}

export function validateGatewayOverride(override) {
  if (!override) return null;
  if (!isAbsolute(override)) {
    throw new Error("TOOLPORT_GATEWAY must be an absolute path");
  }
  return override;
}

async function main() {
  let override;
  try {
    override = validateGatewayOverride(process.env.TOOLPORT_GATEWAY);
  } catch (error) {
    failNotInstalled(error.message);
    return;
  }
  const binaries = override ? [override] : gatewayCandidates();
  try {
    process.exit(await spawnFirst(binaries));
  } catch (error) {
    failNotInstalled(error?.code === "ENOENT" ? "not on PATH either" : error?.message);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) void main();
