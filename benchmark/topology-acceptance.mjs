#!/usr/bin/env node
// Linux process, memory, and latency evidence for one-gateway-per-host rollout.
// Runs real gateway and mock-downstream processes in isolated data directories.
// Build first with npm run build:gateway. Output is a JSON artifact under .verify/.

import { spawn } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
  mkdirSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "linux") {
  throw new Error("topology acceptance currently reads Linux /proc metrics");
}

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const target = resolve(repo, process.env.CARGO_TARGET_DIR || "src-tauri/target");
const gateway =
  process.env.TOOLPORT_GATEWAY_BIN || join(target, "debug/toolport-gateway");
const mock = process.env.TOOLPORT_MOCK_BIN || join(target, "debug/mock-mcp-server");
const clientCount = Number(process.argv[2] || 3);
if (!Number.isInteger(clientCount) || clientCount < 2 || clientCount > 20) {
  throw new Error("client count must be an integer from 2 to 20");
}
for (const path of [gateway, mock]) {
  if (!existsSync(path)) throw new Error("missing binary: " + path);
}

const now = () => Number(process.hrtime.bigint()) / 1e6;
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const scratch = mkdtempSync(join(tmpdir(), "toolport-topology-acceptance-"));
const children = new Set();
const daemons = new Set();

function rpc(proc) {
  let nextId = 0;
  let buffer = "";
  const pending = new Map();
  let stopped = null;
  const fail = (error) => {
    if (stopped) return;
    stopped = error;
    for (const request of pending.values()) {
      clearTimeout(request.timer);
      request.reject(error);
    }
    pending.clear();
  };
  proc.once("error", fail);
  proc.once("close", (code, signal) =>
    fail(new Error("gateway closed: " + (code ?? signal))),
  );
  proc.stdin.on("error", fail);
  proc.stdout.on("data", (chunk) => {
    buffer += chunk.toString();
    let newline;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      const request = pending.get(message.id);
      if (!request) continue;
      pending.delete(message.id);
      clearTimeout(request.timer);
      if (message.error || message.result?.isError) {
        request.reject(
          new Error("RPC error: " + JSON.stringify(message.error || message.result)),
        );
      } else {
        request.resolve(message.result);
      }
    }
  });
  return {
    call(method, params, timeoutMs = 30000) {
      return new Promise((resolveCall, reject) => {
        if (stopped) return reject(stopped);
        const id = ++nextId;
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(method + " timed out"));
        }, timeoutMs);
        pending.set(id, { resolve: resolveCall, reject, timer });
        proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
      });
    },
    notify(method) {
      proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method }) + "\n");
    },
  };
}

function procTable() {
  const table = new Map();
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    const pid = Number(name);
    try {
      const stat = readFileSync("/proc/" + name + "/stat", "utf8");
      const after = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      table.set(pid, { pid, ppid: Number(after[1]) });
    } catch {
      // A short-lived process may disappear during this read.
    }
  }
  return table;
}

function memoryKiB(pid) {
  let privateKiB = 0;
  let residentKiB = 0;
  let sampled = false;
  try {
    const rollup = readFileSync("/proc/" + pid + "/smaps_rollup", "utf8");
    sampled = true;
    for (const line of rollup.split("\n")) {
      const match =
        /^(Private_Clean|Private_Dirty|Private_Hugetlb|Rss):\s+(\d+)\s+kB/.exec(line);
      if (!match) continue;
      if (match[1] === "Rss") residentKiB = Number(match[2]);
      else privateKiB += Number(match[2]);
    }
  } catch {
    // A process may exit between the topology and memory snapshots.
  }
  return { privateKiB, residentKiB, sampled };
}

function processMetrics(rootPids, heavyPids) {
  const table = procTable();
  const tree = new Set(rootPids);
  let changed;
  do {
    changed = false;
    for (const row of table.values()) {
      if (tree.has(row.ppid) && !tree.has(row.pid)) {
        tree.add(row.pid);
        changed = true;
      }
    }
  } while (changed);
  let privateKiB = 0;
  let residentKiB = 0;
  let memorySampledProcesses = 0;
  for (const pid of tree) {
    const memory = memoryKiB(pid);
    privateKiB += memory.privateKiB;
    residentKiB += memory.residentKiB;
    if (memory.sampled) memorySampledProcesses++;
  }
  const daemonPids = [...tree].filter((pid) => {
    try {
      const command = readFileSync("/proc/" + pid + "/cmdline", "utf8");
      return command.includes(gateway) && command.includes("--daemon");
    } catch {
      return false;
    }
  });
  return {
    processCount: tree.size,
    memorySampledProcesses,
    daemonProcesses: daemonPids.length,
    daemonPids,
    directDownstreamChildren: [...table.values()].filter(
      (row) => heavyPids.includes(row.ppid) && !rootPids.includes(row.pid),
    ).length,
    privateMiB: Math.round((privateKiB / 1024) * 10) / 10,
    residentMiB: Math.round((residentKiB / 1024) * 10) / 10,
  };
}

function descriptor(dir) {
  for (const name of readdirSync(dir)) {
    if (!/^daemon-.*\.json$/.test(name)) continue;
    try {
      return JSON.parse(readFileSync(join(dir, name), "utf8"));
    } catch {
      // Publication is atomic, but a daemon may exit during this scan.
    }
  }
  return null;
}

async function waitForEcho(client) {
  const deadline = now() + 30000;
  while (now() < deadline) {
    const result = await client.call("tools/list", {});
    const tool = result.tools?.find((entry) => entry.name.endsWith("__echo"));
    if (tool) return tool.name;
    await sleep(50);
  }
  throw new Error("mock echo tool did not appear within 30 seconds");
}

async function runArm(topology) {
  const dir = join(scratch, topology);
  mkdirSync(dir);
  const transcript = join(dir, "downstream.jsonl");
  const registry = {
    version: 1,
    servers: [
      {
        id: "mock",
        name: "Mock",
        transport: "stdio",
        command: mock,
        args: [],
        env: [{ key: "MOCK_MCP_TRANSCRIPT", value: transcript, secret: false }],
        source: "manual",
      },
    ],
    profiles: [{ id: "acceptance", name: "Acceptance", enabledServerIds: ["mock"] }],
    activeProfileId: "acceptance",
    lazyDiscovery: false,
    ...(topology === "daemon" ? { gatewayTopology: "daemon" } : {}),
  };
  const registryPath = join(dir, "registry.json");
  writeFileSync(registryPath, JSON.stringify(registry));
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !/^(TOOLPORT|CONDUIT)_/.test(key)),
  );
  Object.assign(env, {
    TOOLPORT_DATA_DIR: dir,
    TOOLPORT_REGISTRY: registryPath,
  });
  const clients = Array.from({ length: clientCount }, (_, index) => {
    const spawned = now();
    const proc = spawn(gateway, [], {
      env: { ...env, TOOLPORT_CLIENT_ID: "acceptance-" + index },
      stdio: ["pipe", "pipe", "pipe"],
    });
    children.add(proc);
    proc.once("close", () => children.delete(proc));
    let stderr = "";
    proc.stderr.on("data", (chunk) => {
      stderr = (stderr + chunk.toString()).slice(-4000);
    });
    return { proc, client: rpc(proc), spawned, stderr: () => stderr };
  });
  try {
    const latency = await Promise.all(
      clients.map(async ({ client, spawned, stderr }, index) => {
        try {
          await client.call("initialize", {
            protocolVersion: "2025-06-18",
            capabilities: {},
            clientInfo: { name: "topology-acceptance-" + index, version: "1" },
          });
          const coldStartMs = now() - spawned;
          client.notify("notifications/initialized");
          const tool = await waitForEcho(client);
          const catalogReadyMs = now() - spawned;
          const callStarted = now();
          await client.call("tools/call", {
            name: tool,
            arguments: { text: "acceptance" },
          });
          return { coldStartMs, catalogReadyMs, firstCallMs: now() - callStarted };
        } catch (error) {
          throw new Error(error.message + "\n" + stderr(), { cause: error });
        }
      }),
    );
    await sleep(500);
    const elected = descriptor(dir);
    if (topology === "daemon" && !elected) throw new Error("no daemon descriptor");
    if (elected) daemons.add(elected.pid);
    const gatewayPids = clients.map(({ proc }) => proc.pid);
    const heavyPids = topology === "daemon" ? [elected.pid] : gatewayPids;
    const roots = [...new Set([...gatewayPids, ...heavyPids])];
    let daemonTopology = null;
    if (elected) {
      const response = await fetch("http://" + elected.endpoint + "/host/topology", {
        headers: { Authorization: "Bearer " + elected.token },
        signal: AbortSignal.timeout(5000),
      });
      if (!response.ok)
        throw new Error("private topology probe returned " + response.status);
      daemonTopology = await response.json();
      if (daemonTopology.sessions !== clientCount) {
        throw new Error("daemon session count was " + daemonTopology.sessions);
      }
    }
    const transcriptLines = existsSync(transcript)
      ? readFileSync(transcript, "utf8").split("\n").filter(Boolean)
      : [];
    const downstreamMethods = transcriptLines.map((line) => {
      try {
        return JSON.parse(line).method;
      } catch {
        return "<invalid>";
      }
    });
    const downstreamInitializes = downstreamMethods.filter(
      (method) => method === "initialize",
    ).length;
    const expectedLaunches = topology === "daemon" ? 1 : clientCount;
    if (downstreamInitializes !== expectedLaunches) {
      throw new Error(
        topology +
          " made " +
          downstreamInitializes +
          " downstream initializes; expected " +
          expectedLaunches +
          " (observed methods: " +
          downstreamMethods.slice(0, 12).join(",") +
          ")",
      );
    }
    const processCounts = processMetrics(roots, heavyPids);
    for (const pid of processCounts.daemonPids) daemons.add(pid);
    const expectedDaemons = topology === "daemon" ? 1 : 0;
    if (processCounts.daemonProcesses !== expectedDaemons) {
      throw new Error(
        topology +
          " ran " +
          processCounts.daemonProcesses +
          " daemons; expected " +
          expectedDaemons,
      );
    }
    if (processCounts.directDownstreamChildren !== expectedLaunches) {
      throw new Error(
        topology +
          " had " +
          processCounts.directDownstreamChildren +
          " live downstream children; expected " +
          expectedLaunches,
      );
    }
    if (processCounts.memorySampledProcesses !== processCounts.processCount) {
      throw new Error("memory snapshot missed a process in the gateway tree");
    }
    return {
      topology,
      clients: clientCount,
      heavyGateways: heavyPids.length,
      adapters: topology === "daemon" ? clientCount : 0,
      gatewayPids,
      downstreamInitializes,
      daemonTopology,
      ...processCounts,
      latencyMs: latency,
    };
  } finally {
    for (const { proc } of clients) {
      proc.stdin.end();
    }
    await sleep(500);
    for (const { proc } of clients) {
      if (proc.exitCode === null && proc.signalCode === null) proc.kill("SIGTERM");
    }
  }
}

try {
  const legacy = await runArm("legacy");
  const daemon = await runArm("daemon");
  const artifact = {
    schemaVersion: 1,
    kind: "isolated-process-fixture",
    measuredAt: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    gatewayBuildProfile: gateway.includes("/release/") ? "release" : "debug",
    fixture: { downstreamServers: 1, clientSessions: clientCount },
    gateway,
    mock,
    legacy,
    daemon,
  };
  const outDir = join(repo, ".verify");
  mkdirSync(outDir, { recursive: true });
  const outPath = join(outDir, "topology-acceptance-" + Date.now() + ".json");
  writeFileSync(outPath, JSON.stringify(artifact, null, 2) + "\n");
  process.stdout.write(
    JSON.stringify({ artifact: outPath, legacy, daemon }, null, 2) + "\n",
  );
} finally {
  for (const child of children) child.kill("SIGKILL");
  for (const name of ["legacy", "daemon"]) {
    const dir = join(scratch, name);
    if (existsSync(dir)) {
      const found = descriptor(dir);
      if (found) daemons.add(found.pid);
    }
  }
  for (const pid of daemons) {
    try {
      const cmdline = readFileSync("/proc/" + pid + "/cmdline", "utf8");
      if (cmdline.includes(gateway) && cmdline.includes("--daemon"))
        process.kill(pid, "SIGTERM");
    } catch {
      // The daemon may already have exited.
    }
  }
  rmSync(scratch, { recursive: true, force: true });
}
