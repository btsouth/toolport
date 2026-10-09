// Shared by the offline replay and opt-in real CLI probes. No ambient auth/config.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { watch } from "node:fs";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { pathToFileURL } from "node:url";

export const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const target = path.resolve(repo, process.env.CARGO_TARGET_DIR || "src-tauri/target");
const binary = (name) => path.join(target, "debug", name + (process.platform === "win32" ? ".exe" : ""));
export const gateway = process.env.TOOLPORT_GATEWAY_BIN || binary("toolport-gateway");
export const mock = process.env.TOOLPORT_MOCK_BIN || binary("mock-mcp-server");
export const profiles = JSON.parse(await readFile(path.join(repo, "test/client-conformance/profiles.json"), "utf8")).profiles;
export const deadlineMs = 20_000;

export function cleanEnvironment(home) {
  const env = {};
  // Executable/OS locators only. Never inherit model keys, Toolport overrides,
  // XDG session bus/runtime, CLI config overrides, or workspace settings.
  for (const key of ["PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT", "LANG", "LC_ALL"])
    if (process.env[key]) env[key] = process.env[key];
  return Object.assign(env, {
    HOME: home, USERPROFILE: home, APPDATA: path.join(home, "AppData/Roaming"),
    LOCALAPPDATA: path.join(home, "AppData/Local"), XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"), XDG_CACHE_HOME: path.join(home, ".cache"),
    CODEX_HOME: path.join(home, ".codex"), CLAUDE_CONFIG_DIR: path.join(home, ".claude"),
    GEMINI_CLI_HOME: home, QWEN_HOME: path.join(home, ".qwen"),
    GOOSE_PATH_ROOT: path.join(home, ".goose"), HERMES_HOME: path.join(home, ".hermes"),
    TMPDIR: home, TEMP: home, TMP: home, TERM: "dumb", NO_COLOR: "1",
    DO_NOT_TRACK: "1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  });
}

export async function waitExit(child, ms = 5_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve, reject) => {
    const done = () => { clearTimeout(timer); resolve(); };
    const timer = setTimeout(() => { child.off("exit", done); reject(new Error("child exit deadline")); }, ms);
    child.once("exit", done);
  });
}
export async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill();
  try { await waitExit(child); } catch { child.kill("SIGKILL"); await waitExit(child); }
}
export function failed(reply) {
  return Boolean(reply.error || reply.result?.isError);
}
export function textOf(reply) {
  return (reply.result?.content || []).filter((b) => b.type === "text").map((b) => b.text).join("\n");
}

export class Fixture {
  static async create(id, { full = false, concurrent = true } = {}) {
    const home = await mkdtemp(path.join(os.tmpdir(), "toolport-client-conformance-"));
    const fixture = new Fixture(home, id);
    await mkdir(fixture.data);
    const env = [
      ["MOCK_MCP_CONFORMANCE", "1"], ["MOCK_MCP_TRANSCRIPT", fixture.transcript],
      ["MOCK_MCP_PID_FILE", path.join(fixture.data, "mock.pid")],
      ...(concurrent ? [["MOCK_MCP_CONCURRENT", "1"]] : []),
    ].map(([key, value]) => ({ key, value, secret: false }));
    fixture.registry = {
      version: 3, safetyLevel: "off", servers: [{ id: "mock", name: "Mock", enabled: true,
        transport: "stdio", command: mock, args: [], env, inheritEnv: false }], profiles: [],
      clientDiscovery: full ? { [id]: "full" } : {},
    };
    await fixture.save();
    return fixture;
  }
  constructor(home, id) {
    this.home = home; this.id = id; this.data = path.join(home, "data");
    this.transcript = path.join(this.data, "downstream.jsonl"); this.clients = [];
  }
  async save() { await writeFile(path.join(this.data, "registry.json"), JSON.stringify(this.registry)); }
  env(id = this.id) {
    return { ...cleanEnvironment(this.home), TOOLPORT_DATA_DIR: this.data,
      TOOLPORT_REGISTRY: path.join(this.data, "registry.json"), TOOLPORT_CLIENT_ID: `client:${id}`,
      TOOLPORT_SECRET_KEY: "disposable-client-conformance-key", TOOLPORT_CODE_MODE: "0" };
  }
  client(profile, overrides = {}) {
    const client = new RpcClient(this, profile, overrides); this.clients.push(client); return client;
  }
  async records() {
    try { return (await readFile(this.transcript, "utf8")).trim().split("\n").filter(Boolean).map(JSON.parse); }
    catch (e) { if (e.code === "ENOENT") return []; throw e; }
  }
  async dispatched(predicate) {
    return this.untilRecords((records) => records.some(predicate));
  }
  async untilRecords(predicate) {
    // Subscribe first, then read: no fixed sleeps or gaps between readiness checks.
    await new Promise((resolve, reject) => {
      let checking = false, dirty = false, finished = false;
      const end = (error) => { if (finished) return; finished = true; clearTimeout(timer); watcher.close(); error ? reject(error) : resolve(); };
      const check = async () => {
        if (checking) { dirty = true; return; }
        checking = true;
        try {
          do { dirty = false; if (predicate(await this.records())) { end(); break; } } while (dirty && !finished);
        } catch (error) { end(error); }
        checking = false;
      };
      const watcher = watch(this.data, () => { void check(); });
      watcher.on("error", end);
      const timer = setTimeout(() => end(new Error("downstream dispatch deadline")), deadlineMs);
      void check();
    });
  }
  async descriptors() {
    const names = (await readdir(this.data)).filter((n) => /^daemon-.*\.json$/.test(n));
    const result = [];
    for (const name of names) {
      try { result.push(JSON.parse(await readFile(path.join(this.data, name), "utf8"))); }
      catch (e) { if (e.code !== "ENOENT") throw e; }
    }
    return result;
  }
  async killDaemon() {
    const descriptors = await this.descriptors(); assert.equal(descriptors.length, 1);
    const pid = descriptors[0].pid;
    assert(Number.isSafeInteger(pid) && pid > 0);
    try { process.kill(pid, "SIGKILL"); } catch (e) { if (e.code !== "ESRCH") throw e; }
    return pid;
  }
  async close() {
    await Promise.all(this.clients.map((c) => stop(c.child)));
    // Only PIDs published in this fresh, private fixture directory.
    for (const descriptor of await this.descriptors()) {
      try { process.kill(descriptor.pid, "SIGKILL"); } catch (e) { if (e.code !== "ESRCH") throw e; }
    }
    await rm(this.home, { recursive: true, force: true });
  }
}

export class RpcClient {
  constructor(fixture, profile, overrides) {
    this.fixture = fixture; this.profile = profile; this.id = 100; this.pending = new Map();
    this.notifications = []; this.waiters = []; this.stderr = ""; this.closed = false;
    this.child = spawn(gateway, ["--stdio-adapter"], { cwd: fixture.home,
      env: { ...fixture.env(profile.id), ...overrides }, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stdin.on("error", (e) => this.abort(e));
    this.child.on("error", (e) => this.abort(e));
    this.child.stderr.on("data", (data) => { this.stderr = (this.stderr + data).slice(-4_096); });
    const lines = createInterface({ input: this.child.stdout });
    lines.on("line", (line) => {
      try { this.receive(JSON.parse(line)); } catch (e) { this.abort(e); }
    });
    lines.on("close", () => this.abort(new Error(`adapter closed: ${this.stderr}`)));
  }
  abort(error) {
    this.closed = true;
    for (const entry of this.pending.values()) { clearTimeout(entry.timer); entry.reject(error); }
    this.pending.clear();
    for (const entry of this.waiters) { clearTimeout(entry.timer); entry.reject(error); }
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
      const reply = message.method === "roots/list" ? { result: { roots: [{ uri: pathToFileURL(this.fixture.home).href }] } }
        : message.method === "elicitation/create" ? { result: { action: "decline" } }
          : { error: { code: -32601, message: "Unsupported fixture client request" } };
      this.send({ jsonrpc: "2.0", id: message.id, ...reply }); return;
    }
    if (Object.hasOwn(message, "id")) {
      const entry = this.pending.get(message.id); assert(entry, `unexpected response ID ${message.id}`);
      this.pending.delete(message.id); clearTimeout(entry.timer); entry.resolve(message); return;
    }
    const index = this.waiters.findIndex((entry) => entry.method === message.method);
    if (index >= 0) { const [entry] = this.waiters.splice(index, 1); clearTimeout(entry.timer); entry.resolve(message); }
    else { this.notifications.push(message); assert(this.notifications.length <= 128, "notification queue exceeded"); }
  }
  request(method, params = {}, id = this.id++) {
    assert(!this.pending.has(id), `duplicate request ID ${id}`);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${this.profile.id} ${method} deadline: ${this.stderr}`)); }, deadlineMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.send({ jsonrpc: "2.0", id, method, params }); }
      catch (e) { this.pending.delete(id); clearTimeout(timer); reject(e); }
    });
  }
  async initialize() {
    const captured = this.profile.initialize;
    const params = captured?.params || { protocolVersion: "2025-06-18", capabilities: {},
      clientInfo: { name: "toolport-synthetic-client", version: "1" } };
    const reply = await this.request("initialize", params, captured?.id ?? "synthetic-init");
    assert.equal(reply.result?.protocolVersion, params.protocolVersion);
    assert.equal(reply.result.capabilities.tools.listChanged, true);
    this.send({ jsonrpc: "2.0", method: "notifications/initialized" });
    return reply;
  }
  notification(method) {
    const index = this.notifications.findIndex((n) => n.method === method);
    if (index >= 0) return Promise.resolve(this.notifications.splice(index, 1)[0]);
    return new Promise((resolve, reject) => {
      const entry = { method, resolve, reject };
      entry.timer = setTimeout(() => { this.waiters = this.waiters.filter((w) => w !== entry); reject(new Error(`${method} notification deadline`)); }, deadlineMs);
      this.waiters.push(entry);
    });
  }
  async list() {
    const reply = await this.request("tools/list"); assert(!failed(reply), JSON.stringify(reply));
    assert(Array.isArray(reply.result.tools)); return reply.result.tools;
  }
  call(name, args = {}, lazy = false, id) {
    return this.request("tools/call", lazy ? { name: "toolport_call_tool", arguments: { name, arguments: args } }
      : { name, arguments: args }, id);
  }
}
