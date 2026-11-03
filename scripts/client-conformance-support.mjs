// Shared by the offline replay and opt-in real CLI probes. No ambient auth/config.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { watch } from "node:fs";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { pathToFileURL } from "node:url";

export const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const target = path.resolve(repo, process.env.CARGO_TARGET_DIR || "src-tauri/target");
const binary = (name) =>
  path.join(target, "debug", name + (process.platform === "win32" ? ".exe" : ""));
export const gateway = process.env.TOOLPORT_GATEWAY_BIN || binary("toolport-gateway");
export const mock = process.env.TOOLPORT_MOCK_BIN || binary("mock-mcp-server");
export const profiles = JSON.parse(
  await readFile(path.join(repo, "test/client-conformance/profiles.json"), "utf8"),
).profiles;
export const deadlineMs = 20_000;
// One intentional restart: probe rechecks 8s + election 20s + descriptor lock 5s
// + ready 10s + lifetime headers 6s + handshake 10s, plus normal RPC 20s.
// Round 79s to 80s. Ordinary RPCs retain 20s; this is only a fixture budget.
export const coldRecoveryDeadlineMs = 80_000;

export async function catalog(client, expected, timeoutMs = deadlineMs) {
  const deadline = performance.now() + timeoutMs;
  const tools = await client.list(Math.max(1, deadline - performance.now()));
  if (tools.some((tool) => tool.name === client.spell(expected))) return tools;
  return changedCatalog(client, expected, deadline);
}

export async function changedCatalog(
  client,
  expected,
  deadline = performance.now() + deadlineMs,
) {
  while (performance.now() < deadline) {
    await client.notification(
      "notifications/tools/list_changed",
      deadline - performance.now(),
    );
    const tools = await client.list(Math.max(1, deadline - performance.now()));
    if (tools.some((tool) => tool.name === client.spell(expected))) return tools;
  }
  assert.fail(`${client.profile.id}: missing ${expected} after notifications`);
}

export async function coldRecoveryCatalog(
  client,
  expected,
  onObservation = async () => {},
) {
  const started = performance.now();
  // Dispatch immediately. Observation never resends or waits for idle recovery.
  const pending = catalog(client, expected, coldRecoveryDeadlineMs);
  let observation;
  let observationError;
  let pendingElapsedMs;
  const observe = (state) =>
    Promise.resolve()
      .then(() => onObservation(state))
      .catch((error) => {
        observationError ??= error;
      });
  const timer = setTimeout(() => {
    pendingElapsedMs = Math.round(performance.now() - started);
    observation = observe({ phase: "pending", elapsedMs: pendingElapsedMs });
  }, deadlineMs);
  let phase = "failed";
  let result;
  let requestError;
  try {
    const tools = await pending;
    phase = "completed";
    result = { tools, elapsedMs: Math.round(performance.now() - started) };
  } catch (error) {
    requestError = error;
  } finally {
    clearTimeout(timer);
    const elapsedMs = Math.round(performance.now() - started);
    if (observation) {
      await observation;
      await observe({ phase, elapsedMs, pendingElapsedMs });
    }
  }
  if (phase === "failed") {
    if (observationError)
      console.error("cold recovery diagnostics failed:", observationError);
    throw requestError;
  }
  if (observationError) throw observationError;
  return result;
}

export async function writeFixtureDiagnostics(fixture, observation) {
  const dir = path.join(repo, ".verify/client-conformance-failures");
  await mkdir(dir, { recursive: true });
  for (const name of ["gateway.log", "downstream.jsonl"]) {
    try {
      const raw = await readFile(path.join(fixture.data, name), "utf8");
      await writeFile(path.join(dir, `${fixture.id}-${name}`), raw.slice(-65_536));
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  if (observation) {
    // Exclude descriptors, PIDs, endpoints, tokens, and reply bodies.
    await writeFile(
      path.join(dir, `${fixture.id}-cold-recovery.json`),
      JSON.stringify(
        {
          profile: fixture.id,
          phase: observation.phase,
          elapsedMs: observation.elapsedMs,
          pendingElapsedMs: observation.pendingElapsedMs,
        },
        null,
        2,
      ),
    );
  }
}

export function clientInventoryFromSource(source) {
  const definitions = source.match(/^fn defs\(\) -> Vec<ClientDef> \{([\s\S]*?)^\}/m);
  assert(definitions, "client definition inventory could not be read");
  return [...definitions[1].matchAll(/\bid:\s*"([a-z0-9-]+)"/g)].map((match) => match[1]);
}

export function cleanEnvironment(home) {
  const env = {};
  // Executable/OS locators only. Never inherit model keys, Toolport overrides,
  // XDG session bus/runtime, CLI config overrides, or workspace settings.
  for (const key of [
    "PATH",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "LANG",
    "LC_ALL",
  ])
    if (process.env[key]) env[key] = process.env[key];
  return Object.assign(env, {
    HOME: home,
    USERPROFILE: home,
    APPDATA: path.join(home, "AppData/Roaming"),
    LOCALAPPDATA: path.join(home, "AppData/Local"),
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
    CODEX_HOME: path.join(home, ".codex"),
    CLAUDE_CONFIG_DIR: path.join(home, ".claude"),
    GEMINI_CLI_HOME: home,
    QWEN_HOME: path.join(home, ".qwen"),
    GOOSE_PATH_ROOT: path.join(home, ".goose"),
    HERMES_HOME: path.join(home, ".hermes"),
    TMPDIR: home,
    TEMP: home,
    TMP: home,
    TERM: "dumb",
    DBUS_SESSION_BUS_ADDRESS: `unix:path=${path.join(home, "unavailable-session-bus")}`,
    NO_COLOR: "1",
    DO_NOT_TRACK: "1",
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  });
}

export async function waitExit(child, ms = 5_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve, reject) => {
    const done = () => {
      clearTimeout(timer);
      child.off("error", error);
      resolve();
    };
    const error = (e) => {
      clearTimeout(timer);
      child.off("exit", done);
      reject(e);
    };
    const timer = setTimeout(() => {
      child.off("exit", done);
      child.off("error", error);
      reject(new Error("child exit deadline"));
    }, ms);
    child.once("exit", done);
    child.once("error", error);
  });
}
export async function stop(child) {
  // A failed spawn has no PID and will never emit a later exit event.
  if (!child.pid) return;
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill();
  try {
    await waitExit(child);
  } catch {
    child.kill("SIGKILL");
    await waitExit(child);
  }
}
export async function waitPidExit(pid, ms = 5_000) {
  assert(Number.isSafeInteger(pid) && pid > 0, "invalid fixture PID");
  const deadline = performance.now() + ms;
  for (;;) {
    try {
      process.kill(pid, 0);
    } catch (error) {
      if (error.code === "ESRCH") return;
      throw error;
    }
    if (performance.now() >= deadline) throw new Error(`PID ${pid} exit deadline`);
    await delay(25);
  }
}
export async function killPid(pid) {
  assert(Number.isSafeInteger(pid) && pid > 0, "invalid fixture PID");
  try {
    process.kill(pid, "SIGKILL");
  } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
  await waitPidExit(pid);
}
const stoppedTrees = new WeakSet();
export async function stopTree(child) {
  if (!child.pid || stoppedTrees.has(child)) return;
  if (process.platform === "win32") {
    if (child.exitCode === null && child.signalCode === null) {
      const killer = spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
        stdio: "ignore",
      });
      await waitExit(killer);
    }
  } else {
    // The live CLI has its own process group. Its wiretap may outlive the CLI.
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  }
  await stop(child);
  stoppedTrees.add(child);
}
export function wireMetadata(line) {
  let message;
  try {
    message = JSON.parse(line);
    if (!message || typeof message !== "object") return { nonJson: true };
  } catch {
    return { nonJson: true };
  }
  const safe = { jsonrpc: message.jsonrpc, id: message.id, method: message.method };
  if (message.method === "initialize") safe.params = message.params;
  // Sessionless (2026-07-28) clients declare themselves per request in _meta.
  if (["server/discover", "subscriptions/listen"].includes(message.method))
    safe.params = protocolMeta(message.params);
  if (message.result?.protocolVersion)
    safe.result = {
      protocolVersion: message.result.protocolVersion,
      capabilities: message.result.capabilities,
    };
  else if (message.result?.supportedVersions)
    safe.result = {
      supportedVersions: message.result.supportedVersions,
      capabilities: message.result.capabilities,
    };
  if (message.result?.tools) safe.toolCount = message.result.tools.length;
  // Error codes only; messages can echo client or tool text.
  if (Number.isInteger(message.error?.code)) safe.errorCode = message.error.code;
  return safe;
}

// Protocol fields only: standard `_meta` keys and non-meta subscription filters.
function protocolMeta(params) {
  if (!params || typeof params !== "object") return undefined;
  const meta = Object.fromEntries(
    Object.entries(params._meta || {}).filter(([key]) =>
      key.startsWith("io.modelcontextprotocol/"),
    ),
  );
  return { ...params, _meta: meta };
}

export function failed(reply) {
  return Boolean(reply.error || reply.result?.isError);
}
export function textOf(reply) {
  return (reply.result?.content || [])
    .filter((b) => b.type === "text")
    .map((b) => b.text)
    .join("\n");
}

export class Fixture {
  static async create(id, { full = false, concurrent = true } = {}) {
    const home = await mkdtemp(path.join(os.tmpdir(), "toolport-client-conformance-"));
    const fixture = new Fixture(home, id);
    await mkdir(fixture.data);
    const env = [
      ["MOCK_MCP_CONFORMANCE", "1"],
      ["MOCK_MCP_TRANSCRIPT", fixture.transcript],
      ["MOCK_MCP_PID_FILE", path.join(fixture.data, "mock.pid")],
      ...(concurrent
        ? [
            ["MOCK_MCP_CONCURRENT", "1"],
            ["MOCK_MCP_SLEEP_BARRIER", "2"],
          ]
        : []),
    ].map(([key, value]) => ({ key, value, secret: false }));
    fixture.registry = {
      version: 3,
      safetyLevel: "off",
      servers: [
        {
          id: "mock",
          name: "Mock",
          enabled: true,
          transport: "stdio",
          command: mock,
          args: [],
          env,
          inheritEnv: false,
        },
      ],
      profiles: [],
      clientDiscovery: full ? { [id]: "full" } : {},
    };
    await fixture.save();
    return fixture;
  }
  constructor(home, id) {
    this.home = home;
    this.id = id;
    this.data = path.join(home, "data");
    this.transcript = path.join(this.data, "downstream.jsonl");
    this.clients = [];
    this.killedPids = new Set();
  }
  async save() {
    await writeFile(path.join(this.data, "registry.json"), JSON.stringify(this.registry));
  }
  env(id = this.id) {
    return {
      ...cleanEnvironment(this.home),
      TOOLPORT_DATA_DIR: this.data,
      TOOLPORT_REGISTRY: path.join(this.data, "registry.json"),
      TOOLPORT_CLIENT_ID: `client:${id}`,
      TOOLPORT_SECRET_KEY: "disposable-client-conformance-key",
      TOOLPORT_CODE_MODE: "0",
    };
  }
  client(profile, overrides = {}) {
    const client = new RpcClient(this, profile, overrides);
    this.clients.push(client);
    return client;
  }
  async records() {
    try {
      return (await readFile(this.transcript, "utf8"))
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(JSON.parse);
    } catch (e) {
      if (e.code === "ENOENT") return [];
      throw e;
    }
  }
  async dispatched(predicate) {
    return this.untilRecords((records) => records.some(predicate));
  }
  async untilRecords(predicate) {
    // Subscribe first, then read: no fixed sleeps or gaps between readiness checks.
    await new Promise((resolve, reject) => {
      let checking = false,
        dirty = false,
        finished = false;
      const end = (error) => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        watcher.close();
        if (error) reject(error);
        else resolve();
      };
      const check = async () => {
        if (checking) {
          dirty = true;
          return;
        }
        checking = true;
        try {
          do {
            dirty = false;
            if (predicate(await this.records())) {
              end();
              break;
            }
          } while (dirty && !finished);
        } catch (error) {
          end(error);
        }
        checking = false;
      };
      const watcher = watch(this.data, () => {
        void check();
      });
      watcher.on("error", end);
      const timer = setTimeout(
        () => end(new Error("downstream dispatch deadline")),
        deadlineMs,
      );
      void check();
    });
  }
  async descriptors() {
    let names;
    try {
      names = (await readdir(this.data)).filter((n) => /^daemon-.*\.json$/.test(n));
    } catch (error) {
      if (error.code === "ENOENT") return [];
      throw error;
    }
    const result = [];
    for (const name of names) {
      try {
        result.push(JSON.parse(await readFile(path.join(this.data, name), "utf8")));
      } catch (e) {
        if (e.code !== "ENOENT") throw e;
      }
    }
    return result;
  }
  async killOwnedPid(pid) {
    // A descriptor can survive SIGKILL. Never signal its PID again after exit.
    if (this.killedPids.has(pid)) return;
    await killPid(pid);
    this.killedPids.add(pid);
  }
  async killDaemon() {
    const descriptors = await this.descriptors();
    assert.equal(descriptors.length, 1);
    const pid = descriptors[0].pid;
    await this.killOwnedPid(pid);
    return pid;
  }
  async close() {
    await Promise.all(this.clients.map((c) => stop(c.child)));
    // Stop the producers first, then consume every published mock/wiretap PID.
    for (const descriptor of await this.descriptors())
      await this.killOwnedPid(descriptor.pid);
    for (const name of ["wiretap.pid", "mock.pid"]) {
      let pids;
      try {
        pids = await readFile(path.join(this.data, name), "utf8");
      } catch (error) {
        if (error.code === "ENOENT") continue;
        throw error;
      }
      for (const pid of new Set(pids.trim().split(/\s+/).filter(Boolean).map(Number)))
        await this.killOwnedPid(pid);
    }
    await rm(this.home, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 100,
    });
  }
}

export class RpcClient {
  constructor(fixture, profile, overrides) {
    this.fixture = fixture;
    this.profile = profile;
    this.id = 100;
    this.pending = new Map();
    this.notifications = [];
    this.waiters = [];
    this.stderr = "";
    this.closed = false;
    this.child = spawn(gateway, ["--stdio-adapter"], {
      cwd: repo,
      env: { ...fixture.env(profile.id), ...overrides },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stdin.on("error", (e) => this.abort(e));
    this.child.on("error", (e) => this.abort(e));
    this.child.stderr.on("data", (data) => {
      this.stderr = (this.stderr + data).slice(-4_096);
    });
    const lines = createInterface({ input: this.child.stdout });
    lines.on("line", (line) => {
      try {
        this.receive(JSON.parse(line));
      } catch (e) {
        this.abort(e);
      }
    });
    lines.on("close", () => this.abort(new Error(`adapter closed: ${this.stderr}`)));
  }
  abort(error) {
    this.closed = true;
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
    for (const entry of this.waiters) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.waiters = [];
  }
  send(message) {
    assert(!this.closed, `adapter closed: ${this.stderr}`);
    this.child.stdin.write(JSON.stringify(message) + "\n");
  }
  receive(message) {
    assert.equal(message.jsonrpc, "2.0");
    if (message.method && Object.hasOwn(message, "id")) {
      // Server request IDs have their own namespace, even if a client ID matches.
      const reply =
        message.method === "roots/list"
          ? { result: { roots: [{ uri: pathToFileURL(this.fixture.home).href }] } }
          : message.method === "elicitation/create"
            ? { result: { action: "decline" } }
            : { error: { code: -32601, message: "Unsupported fixture client request" } };
      this.send({ jsonrpc: "2.0", id: message.id, ...reply });
      return;
    }
    if (Object.hasOwn(message, "id")) {
      const entry = this.pending.get(message.id);
      assert(entry, `unexpected response ID ${message.id}`);
      this.pending.delete(message.id);
      clearTimeout(entry.timer);
      entry.resolve(message);
      return;
    }
    const index = this.waiters.findIndex((entry) => entry.method === message.method);
    if (index >= 0) {
      const [entry] = this.waiters.splice(index, 1);
      clearTimeout(entry.timer);
      entry.resolve(message);
    } else {
      this.notifications.push(message);
      assert(this.notifications.length <= 128, "notification queue exceeded");
    }
  }
  request(method, params = {}, id = this.id++, timeoutMs = deadlineMs) {
    assert(!this.pending.has(id), `duplicate request ID ${id}`);
    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${this.profile.id} ${method} deadline: ${this.stderr}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try {
        this.send({ jsonrpc: "2.0", id, method, params });
      } catch (e) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(e);
      }
    });
    // Pipelined requests can fail while their caller awaits a dispatch barrier.
    // Keep cleanup reachable, while preserving the rejected promise for await.
    void promise.catch(() => {});
    return promise;
  }
  async initialize() {
    const captured = this.profile.initialize;
    const params = captured?.params || {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "toolport-synthetic-client", version: "1" },
    };
    const reply = await this.request(
      "initialize",
      params,
      captured?.id ?? "synthetic-init",
    );
    assert.equal(reply.result?.protocolVersion, params.protocolVersion);
    assert.equal(reply.result.capabilities.tools.listChanged, true);
    this.send({ jsonrpc: "2.0", method: "notifications/initialized" });
    return reply;
  }
  notification(method, timeoutMs = deadlineMs) {
    const index = this.notifications.findIndex((n) => n.method === method);
    if (index >= 0) return Promise.resolve(this.notifications.splice(index, 1)[0]);
    return new Promise((resolve, reject) => {
      const entry = { method, resolve, reject };
      entry.timer = setTimeout(() => {
        this.waiters = this.waiters.filter((w) => w !== entry);
        reject(new Error(`${method} notification deadline`));
      }, timeoutMs);
      this.waiters.push(entry);
    });
  }
  async list(timeoutMs = deadlineMs) {
    const reply = await this.request("tools/list", {}, undefined, timeoutMs);
    assert(!failed(reply), JSON.stringify(reply));
    assert(Array.isArray(reply.result.tools));
    return reply.result.tools;
  }
  // Grok drops tool names containing `__`, so its view lists `server-tool`.
  spell(name) {
    return this.profile.id === "grok" ? name.replaceAll("__", "-") : name;
  }

  call(name, args = {}, lazy = false, id) {
    return this.request(
      "tools/call",
      lazy
        ? { name: "toolport_call_tool", arguments: { name, arguments: args } }
        : { name: this.spell(name), arguments: args },
      id,
    );
  }
}
