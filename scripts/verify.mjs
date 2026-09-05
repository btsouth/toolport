#!/usr/bin/env node
// One bounded, logged verification flow. Fails at the first unsuccessful stage.
import { spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { createWriteStream } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (!["--frontend", "--headless"].includes(arg))
    throw new Error(`Unknown option: ${arg}`);
}
if (args.size > 1)
  throw new Error("Choose --frontend or --headless, or omit both for all checks");
const output = path.join(root, ".verify", `run-${Date.now()}-${process.pid}`);
await mkdir(output, { recursive: true });
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const steps = [];
const runNpm = (name) => steps.push([name, npm, ["run", name]]);
if (!args.has("--headless")) {
  for (const name of [
    "format:check",
    "lint",
    "build",
    "test",
    "bench:bundle",
    "smoke:browser",
  ])
    runNpm(name);
}
if (!args.has("--frontend")) {
  steps.push([
    "rust-tests",
    "cargo",
    [
      "test",
      "--locked",
      "--manifest-path",
      "src-tauri/Cargo.toml",
      "--no-default-features",
      "--lib",
      "--bins",
      "--tests",
    ],
  ]);
  runNpm("build:gateway");
  runNpm("smoke:headless");
}
const results = [];
console.log(`Verification logs: ${output}`);
for (const [name, bin, argv] of steps) {
  const start = Date.now();
  const logPath = path.join(output, `${name.replaceAll(":", "-")}.log`);
  const log = createWriteStream(logPath);
  console.log(`Running ${name}...`);
  const status = await new Promise((resolve) => {
    const child = spawn(bin, argv, {
      cwd: root,
      env: {
        ...process.env,
        CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS || "2",
        NO_COLOR: "1",
      },
      shell: process.platform === "win32" && bin === "npm.cmd",
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });
    child.stdout.pipe(log, { end: false });
    child.stderr.pipe(log, { end: false });
    let requestedExit;
    let forceTimer;
    const killTree = (signal) => {
      if (!child.pid) return;
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], {
          stdio: "ignore",
          timeout: 10_000,
        });
      } else {
        try {
          process.kill(-child.pid, signal);
        } catch (error) {
          if (error.code !== "ESRCH") log.write(`Cleanup failed: ${error.message}\n`);
        }
      }
    };
    const stop = (code) => {
      if (requestedExit !== undefined) return;
      requestedExit = code;
      killTree("SIGTERM");
      forceTimer = setTimeout(() => killTree("SIGKILL"), 2000);
    };
    const onInterrupt = () => stop(130);
    const onTerminate = () => stop(143);
    process.once("SIGINT", onInterrupt);
    process.once("SIGTERM", onTerminate);
    const timer = setTimeout(() => stop(124), 30 * 60 * 1000);
    child.once("error", (error) => {
      log.write(`${error.message}\n`);
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      clearTimeout(forceTimer);
      process.off("SIGINT", onInterrupt);
      process.off("SIGTERM", onTerminate);
      // Descendants can outlive npm even after its own process has closed.
      if (requestedExit !== undefined) killTree("SIGKILL");
      log.end(() => resolve(requestedExit ?? code ?? 1));
    });
  });
  results.push({ name, status, seconds: (Date.now() - start) / 1000, log: logPath });
  await writeFile(
    path.join(output, "summary.json"),
    JSON.stringify(results, null, 2) + "\n",
  );
  console.log(
    `${status === 0 ? "PASS" : "FAIL"} ${name} (${results.at(-1).seconds.toFixed(1)}s)`,
  );
  if (status !== 0) {
    console.error(`Inspect ${logPath}`);
    process.exitCode = status;
    break;
  }
}
