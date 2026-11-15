#!/usr/bin/env node
// Time from a client launching the gateway to its first tool call result, with
// the shared host daemon not running (cold: the adapter starts it, and it starts
// the server) and already running (warm). Clients launch the gateway the way
// their configs do: no arguments, stdio pipes, a fresh process per session.
//
//   node benchmark/cold-start.mjs [samples] [--json]     (default 20 samples)
//
// Uses TOOLPORT_GATEWAY_BIN and TOOLPORT_MOCK_BIN, or the builds under
// CARGO_TARGET_DIR (default src-tauri/target) for TOOLPORT_BENCH_PROFILE
// (default release). Data lives in a temporary directory that is removed after
// (set TOOLPORT_BENCH_KEEP=1 to keep it and its gateway.log).
// The per-stage medians come from the adapter's own startup trace in gateway.log.

import { spawn } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profile = process.env.TOOLPORT_BENCH_PROFILE || "release";
const build = join(
  resolve(root, process.env.CARGO_TARGET_DIR || "src-tauri/target"),
  profile,
);
const exe = (name) => join(build, process.platform === "win32" ? `${name}.exe` : name);
const GATEWAY = process.env.TOOLPORT_GATEWAY_BIN || exe("toolport-gateway");
const MOCK = process.env.TOOLPORT_MOCK_BIN || exe("mock-mcp-server");
const args = process.argv.slice(2);
const SAMPLES = Math.max(1, Number(args.find((arg) => /^\d+$/.test(arg)) || 20));
const JSON_OUTPUT = args.includes("--json");
const DEADLINE_MS = 30_000;

for (const path of [GATEWAY, MOCK]) {
  if (!existsSync(path)) {
    console.error(`Missing ${path}. Build the gateway and mock-mcp-server first.`);
    process.exit(1);
  }
}

const now = () => Number(process.hrtime.bigint()) / 1e6;
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const stats = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q) => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * q) - 1)];
  return {
    n: sorted.length,
    median: at(0.5),
    p90: at(0.9),
    min: sorted[0],
    max: sorted.at(-1),
  };
};

const children = new Set();
function start(binary, env) {
  const child = spawn(binary, [], { env, stdio: ["pipe", "pipe", "ignore"] });
  children.add(child);
  child.once("close", () => children.delete(child));
  const pending = new Map();
  let buffer = "";
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, end).trim();
      buffer = buffer.slice(end + 1);
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      const request = pending.get(message.id);
      if (!request) continue;
      pending.delete(message.id);
      if (message.error || message.result?.isError)
        request.reject(new Error(JSON.stringify(message.error || message.result)));
      else request.resolve(message.result);
    }
  });
  child.once("close", (code) => {
    for (const request of pending.values()) request.reject(new Error(`exited (${code})`));
  });
  let id = 0;
  const call = (method, params) =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`${method} timed out`)),
        DEADLINE_MS,
      );
      pending.set(++id, {
        resolve: (value) => (clearTimeout(timer), resolve(value)),
        reject: (error) => (clearTimeout(timer), reject(error)),
      });
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  const notify = (method) =>
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method })}\n`);
  const close = () =>
    new Promise((done) => {
      if (child.exitCode !== null) return done();
      const timer = setTimeout(() => child.kill("SIGKILL"), 10_000);
      child.once("close", () => (clearTimeout(timer), done()));
      child.stdin.end();
    });
  return { child, call, notify, close };
}

const INIT = {
  protocolVersion: "2025-06-18",
  capabilities: {},
  clientInfo: { name: "cold-start-bench", version: "0" },
};

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function daemonPids(dir) {
  return readdirSync(dir)
    .filter((name) => /^daemon-.*\.json$/.test(name))
    .map((name) => {
      try {
        return JSON.parse(readFileSync(join(dir, name), "utf8")).pid;
      } catch {
        return undefined;
      }
    })
    .filter((pid) => Number.isInteger(pid) && alive(pid));
}

async function waitForNoDaemon(dir, known) {
  const started = now();
  while (now() - started < DEADLINE_MS) {
    if (daemonPids(dir).length === 0 && known.every((pid) => !alive(pid))) return;
    await sleep(20);
  }
  throw new Error("the daemon did not exit after its idle grace");
}

// One client session: launch, initialize, first tool call. Returns milliseconds
// to the initialize answer and to the first tool result.
async function session(env, tool) {
  const started = now();
  const client = start(GATEWAY, env);
  await client.call("initialize", INIT);
  const initialized = now() - started;
  client.notify("notifications/initialized");
  await client.call("tools/call", {
    name: "toolport_call_tool",
    arguments: { name: tool, arguments: {} },
  });
  const firstResult = now() - started;
  return { initialized, firstResult, pid: client.child.pid, close: client.close };
}

function traceStages(dir, pids) {
  const log = existsSync(join(dir, "gateway.log"))
    ? readFileSync(join(dir, "gateway.log"), "utf8")
    : "";
  const byStage = {};
  for (const match of log.matchAll(
    /stage=(\w+) event=end result=ok elapsed_ms=(\d+) pid=(\d+)/g,
  )) {
    if (!pids.has(Number(match[3]))) continue;
    (byStage[match[1]] ??= []).push(Number(match[2]));
  }
  return Object.fromEntries(
    Object.entries(byStage).map(([stage, values]) => [stage, stats(values)]),
  );
}

async function main() {
  const dir = mkdtempSync(join(tmpdir(), "toolport-cold-start-"));
  const registry = join(dir, "registry.json");
  const base = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !/^(TOOLPORT|CONDUIT)_/.test(key)),
  );
  Object.assign(base, {
    TOOLPORT_DATA_DIR: dir,
    TOOLPORT_REGISTRY: registry,
    TOOLPORT_PROFILE: "bench",
    TOOLPORT_DISCOVERY: "lazy",
  });
  writeFileSync(
    registry,
    JSON.stringify({
      version: 1,
      servers: [
        {
          id: "mock",
          name: "Mock",
          transport: "stdio",
          command: MOCK,
          args: [],
          env: [],
        },
      ],
      profiles: [{ id: "bench", name: "Bench", enabledServerIds: ["mock"] }],
      activeProfileId: "bench",
      lazyDiscovery: true,
    }),
  );
  try {
    const mock = start(MOCK, base);
    await mock.call("initialize", INIT);
    const tool = `mock__${(await mock.call("tools/list", {})).tools[0].name}`;
    await mock.close();

    // Cold: the daemon leaves 300 ms after each client, so every sample starts it.
    const coldEnv = { ...base, TOOLPORT_DAEMON_IDLE_GRACE_MS: "300" };
    const cold = [];
    const coldPids = new Set();
    // One unmeasured run first, so the catalog cache exists as on a real install.
    for (let k = 0; k <= SAMPLES; k++) {
      const known = daemonPids(dir);
      await waitForNoDaemon(dir, known);
      const result = await session(coldEnv, tool);
      const daemons = daemonPids(dir);
      await result.close();
      await waitForNoDaemon(dir, daemons);
      if (k === 0) continue;
      cold.push(result);
      coldPids.add(result.pid);
    }

    // Warm: a long grace keeps the daemon (and its server) up between clients.
    const warmEnv = { ...base, TOOLPORT_DAEMON_IDLE_GRACE_MS: "600000" };
    const primer = await session(warmEnv, tool);
    const warm = [];
    const warmPids = new Set();
    for (let k = 0; k < SAMPLES; k++) {
      const result = await session(warmEnv, tool);
      await result.close();
      warm.push(result);
      warmPids.add(result.pid);
    }
    const daemons = daemonPids(dir);
    await primer.close();
    for (const pid of daemons) {
      try {
        process.kill(pid);
      } catch {
        // already gone
      }
    }
    await waitForNoDaemon(dir, daemons).catch(() => {});

    const summarize = (rows, pids) => ({
      firstResult: stats(rows.map((row) => row.firstResult)),
      initialized: stats(rows.map((row) => row.initialized)),
      stages: traceStages(dir, pids),
    });
    const result = {
      schemaVersion: 1,
      runtime: {
        platform: process.platform,
        arch: process.arch,
        node: process.version,
        profile: process.env.TOOLPORT_GATEWAY_BIN ? "TOOLPORT_GATEWAY_BIN" : profile,
        samples: SAMPLES,
      },
      milliseconds: { cold: summarize(cold, coldPids), warm: summarize(warm, warmPids) },
    };
    if (JSON_OUTPUT) {
      console.log(JSON.stringify(result, null, 2));
      return;
    }
    const row = (label, value) =>
      `  ${label.padEnd(34)}median ${value.median.toFixed(1).padStart(7)} ms   p90 ${value.p90
        .toFixed(1)
        .padStart(7)} ms   (min ${value.min.toFixed(1)}, max ${value.max.toFixed(1)})`;
    console.log(
      `\nFirst tool call through the gateway (${SAMPLES} samples each, ${result.runtime.profile} build, ${process.platform}/${process.arch})\n`,
    );
    for (const mode of ["cold", "warm"]) {
      const value = result.milliseconds[mode];
      console.log(row(`${mode}: launch to first tool result`, value.firstResult));
      console.log(row(`${mode}: launch to initialize answer`, value.initialized));
      for (const [stage, stage_stats] of Object.entries(value.stages))
        console.log(row(`${mode}:   adapter ${stage}`, stage_stats));
    }
    const extra =
      result.milliseconds.cold.firstResult.median -
      result.milliseconds.warm.firstResult.median;
    console.log(
      `\n  Cold start adds ${extra.toFixed(1)} ms (median) to the first tool call.\n`,
    );
  } finally {
    for (const child of children) child.kill("SIGKILL");
    if (process.env.TOOLPORT_BENCH_KEEP) console.error(`Kept ${dir}`);
    else rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}

main().catch((error) => {
  console.error(`Cold start benchmark failed: ${error.message}`);
  process.exitCode = 1;
});
