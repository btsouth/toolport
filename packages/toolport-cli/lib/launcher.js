// Finds the native toolport binary for this machine and runs it.
//
// The binary ships in one optional dependency per platform (the esbuild and Biome
// pattern): npm installs only the package whose `os` and `cpu` match, so there is no
// postinstall script and no download at run time.
import { spawnSync } from "node:child_process";
import {
  existsSync,
  openSync,
  readSync,
  closeSync,
  realpathSync,
  statSync,
} from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const PLATFORMS = {
  "darwin-arm64": "@toolport/cli-darwin-arm64",
  "darwin-x64": "@toolport/cli-darwin-x64",
  "linux-arm64": "@toolport/cli-linux-arm64",
  "linux-x64": "@toolport/cli-linux-x64",
  "win32-x64": "@toolport/cli-win32-x64",
};

// The native binary embeds this string. A `toolport` on PATH without it is some other
// program, such as the Linux desktop app's, which opens its window when run without a
// command, so it is never run.
export const BINARY_MARKER = "toolport-cli-binary-v1";

const LAUNCHER = fileURLToPath(new URL("../bin/toolport.js", import.meta.url));

export class LaunchError extends Error {}

export function binaryName(platform) {
  return platform === "win32" ? "toolport.exe" : "toolport";
}

export function platformKey(platform, arch) {
  return `${platform}-${arch}`;
}

function real(file) {
  try {
    return realpathSync(file);
  } catch {
    return null;
  }
}

/** Whether the first part of a file or the whole of a binary contains `needle`. */
function fileContains(file, needle, limit = 64 * 1024 * 1024) {
  let fd;
  try {
    const size = statSync(file).size;
    if (size > limit) return false;
    fd = openSync(file, "r");
    const chunk = Buffer.alloc(1024 * 1024);
    const target = Buffer.from(needle);
    let carry = Buffer.alloc(0);
    for (;;) {
      const n = readSync(fd, chunk, 0, chunk.length, null);
      if (n <= 0) return false;
      const window = Buffer.concat([carry, chunk.subarray(0, n)]);
      if (window.includes(target)) return true;
      carry = window.subarray(Math.max(0, window.length - target.length));
    }
  } catch {
    return false;
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/** A `toolport` on PATH that is the native CLI, never this launcher or the desktop app. */
export function findOnPath(env, platform) {
  const dirs = (env.PATH || env.Path || "").split(path.delimiter).filter(Boolean);
  const names = platform === "win32" ? ["toolport.exe"] : ["toolport"];
  const self = real(LAUNCHER);
  for (const dir of dirs) {
    for (const name of names) {
      const candidate = path.join(dir, name);
      if (!existsSync(candidate)) continue;
      const resolved = real(candidate);
      if (!resolved || resolved === self) continue;
      if (fileContains(resolved, "@toolport/cli", 4096)) continue;
      if (fileContains(resolved, BINARY_MARKER)) return resolved;
    }
  }
  return null;
}

/**
 * The binary to run. Order: TOOLPORT_CLI_BIN, the platform package, a native `toolport`
 * on PATH. Throws a LaunchError with a next step when none fits.
 */
export function resolveBinary({
  platform = process.platform,
  arch = process.arch,
  env = process.env,
  paths,
} = {}) {
  const override = env.TOOLPORT_CLI_BIN;
  if (override) {
    if (!existsSync(override)) {
      throw new LaunchError(
        `TOOLPORT_CLI_BIN points at ${override}, which doesn't exist.`,
      );
    }
    return override;
  }
  const key = platformKey(platform, arch);
  const pkg = PLATFORMS[key];
  if (pkg) {
    const require = createRequire(import.meta.url);
    try {
      const manifest = require.resolve(
        `${pkg}/package.json`,
        paths ? { paths } : undefined,
      );
      const bin = path.join(path.dirname(manifest), "bin", binaryName(platform));
      if (existsSync(bin)) return bin;
    } catch {}
  }
  const onPath = findOnPath(env, platform);
  if (onPath) return onPath;
  const supported = Object.keys(PLATFORMS).join(", ");
  if (!pkg) {
    throw new LaunchError(
      `toolport has no prebuilt binary for ${key}. Prebuilt: ${supported}.\n` +
        "Build it from source with: cargo install --git https://github.com/btsouth/toolport conduit --bin toolport --no-default-features --features cli\n" +
        "then put it on PATH or set TOOLPORT_CLI_BIN to its path.",
    );
  }
  throw new LaunchError(
    `The ${pkg} package that holds toolport's binary for ${key} isn't installed.\n` +
      "Reinstall without skipping optional dependencies (no --omit=optional or --no-optional),\n" +
      "or set TOOLPORT_CLI_BIN to a toolport binary.",
  );
}

/** Run the binary with the same arguments and stdio, and return its exit code. */
export function run(argv = process.argv.slice(2), options = {}) {
  let bin;
  try {
    bin = resolveBinary(options);
  } catch (error) {
    if (error instanceof LaunchError) {
      process.stderr.write(`error: ${error.message}\n`);
      return 1;
    }
    throw error;
  }
  const result = spawnSync(bin, argv, {
    stdio: "inherit",
    env: options.env || process.env,
  });
  if (result.error) {
    process.stderr.write(`error: couldn't start ${bin}: ${result.error.message}\n`);
    return 1;
  }
  if (result.signal) {
    // Exit the way the child did, so a shell sees the same signal.
    process.kill(process.pid, result.signal);
    return 128;
  }
  return result.status ?? 1;
}
