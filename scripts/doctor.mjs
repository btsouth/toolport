#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);
let failures = 0;
function check(label, ok, remedy) {
  console.log(`${ok ? "OK" : "MISSING"} ${label}${ok ? "" : `: ${remedy}`}`);
  if (!ok) failures++;
}
function command(bin, args) {
  return spawnSync(bin, args, { encoding: "utf8", cwd: root }).status === 0;
}
const [nodeMajor, nodeMinor] = process.versions.node.split(".").map(Number);
check(
  "Node compatible with Vite 7",
  (nodeMajor === 20 && nodeMinor >= 19) ||
    (nodeMajor === 22 && nodeMinor >= 12) ||
    nodeMajor > 22,
  "install Node 20.19+ or 22.12+ (Node 21 is unsupported)",
);
for (const dependency of ["vite", "vitest", "@playwright/test"]) {
  let installed = false;
  try {
    require.resolve(dependency);
    installed = true;
  } catch {
    /* missing */
  }
  check(dependency, installed, "run npm ci");
}
check(
  "Rust compiler",
  command("rustc", ["--version"]),
  "install stable Rust with rustup",
);
check("Cargo", command("cargo", ["--version"]), "install stable Rust with rustup");
if (process.platform === "linux") {
  check(
    "Headless native dependencies",
    command("pkg-config", ["--exists", "dbus-1", "openssl"]),
    "install pkg-config, OpenSSL and D-Bus development packages",
  );
  console.log(
    `INFO Desktop headers: ${command("pkg-config", ["--exists", "webkit2gtk-4.1", "gtk+-3.0"]) ? "available" : "missing (optional for headless verification)"}`,
  );
}
let browser =
  process.env.TOOLPORT_BROWSER_BIN ||
  (existsSync("/usr/bin/chromium") ? "/usr/bin/chromium" : "");
if (!browser) {
  try {
    browser = require("@playwright/test").chromium.executablePath();
  } catch {
    /* missing */
  }
}
check(
  "Headless Chromium",
  Boolean(browser && existsSync(browser)),
  "run npx playwright install chromium, or set TOOLPORT_BROWSER_BIN",
);
console.log(`INFO Checkout: ${root}`);
console.log(
  `INFO Cargo artifacts: ${process.env.CARGO_TARGET_DIR || path.join(root, "src-tauri", "target")}`,
);
console.log(
  "INFO Browser fixtures need no API keys, account, database, or running desktop.",
);
process.exitCode = failures ? 1 : 0;
