#!/usr/bin/env node
// Opt-in authenticated benchmark. Evidence and credentials never enter the repo.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtemp, readdir, readFile, realpath, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import { connect, createServer } from "node:net";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import Ajv from "ajv";
import addFormats from "ajv-formats";
import { tasksFrom } from "./live-client-bench-tasks.mjs";

const self = fileURLToPath(import.meta.url);
const mock = path.join(path.dirname(self), "live-client-bench-mock.mjs");
const record = (file, value) => appendFileSync(file, JSON.stringify(value) + "\n");
const readJson = (file) => JSON.parse(readFileSync(file, "utf8"));
const records = (file) => {
  try {
    return readFileSync(file, "utf8").split("\n").filter(Boolean).map(JSON.parse);
  } catch (e) {
    if (e.code === "ENOENT") return [];
    throw e;
  }
};
const ajv = new Ajv({ strict: false, allErrors: true, validateFormats: false });
addFormats(ajv);
const validators = new Map();
const schemaErrors = new Map();
function validator(tool) {
  if (!validators.has(tool.name)) {
    try {
      validators.set(tool.name, ajv.compile(tool.inputSchema));
    } catch (error) {
      schemaErrors.set(tool.name, error.message);
      validators.set(tool.name, null);
    }
  }
  return validators.get(tool.name);
}
export function includes(actual, expected) {
  if (expected && typeof expected === "object")
    return Object.entries(expected).every(([k, v]) => includes(actual?.[k], v));
  return actual === expected;
}

export function score(task, calls, catalog, finished) {
  const choices = task.expected.map((x) => [x, ...(x.alternatives || [])]);
  const wanted = choices.flat().map((x) => x.name);
  const normalized = calls.map((r) => ({
    name: (r.namespace === "catalog"
      ? r.params.name
      : `${r.namespace}__${r.params.name}`
    ).replaceAll("-", "_"),
    args: r.params.arguments || {},
  }));
  const details = normalized.map((call) => {
    const tool = catalog.find((t) => t.name.replaceAll("-", "_") === call.name);
    const validate = tool && validator(tool);
    const validSchema = Boolean(validate?.(call.args));
    const expected = choices.flat().find((x) => x.name === call.name);
    const validValues = expected
      ? includes(call.args, expected.args) &&
        (!expected.textIncludes ||
          (typeof call.args.text === "string" &&
            call.args.text.includes(expected.textIncludes)))
      : false;
    return {
      ...call,
      validSchema,
      validValues,
      schemaErrors: schemaErrors.get(tool?.name) || validate?.errors,
    };
  });
  const wrongCalls = details.filter((x) => !wanted.includes(x.name)).length;
  const successful = details.filter((x) => x.validSchema && x.validValues);
  const matched = choices.every((expected, index) =>
    successful.some(
      (x, callIndex) =>
        expected.some((choice) => x.name === choice.name) &&
        (index === 0 ||
          successful
            .slice(0, callIndex)
            .some((prior) =>
              choices[index - 1].some((choice) => prior.name === choice.name),
            )),
    ),
  );
  // Abstention needs a completed model turn, not merely zero dispatched calls.
  return {
    success: Boolean(finished && matched && wrongCalls === 0),
    wrongCalls,
    validCalls: successful.length,
    calls: details,
  };
}

async function configMetadata() {
  const files = [
    ".codex/config.toml",
    ".claude.json",
    ".claude/settings.json",
    ".cursor/mcp.json",
    ".cursor/cli-config.json",
    ".config/opencode/opencode.json",
    ".config/opencode/opencode.jsonc",
  ];
  const result = {};
  for (const file of files) {
    const metadata = await stat(path.join(os.homedir(), file)).catch(() => null);
    result[file] = metadata ? { mtimeMs: metadata.mtimeMs, size: metadata.size } : null;
  }
  return result;
}

async function runProcess(executable, args, options, timeoutMs) {
  const start = performance.now();
  const child = spawn(executable, args, {
    ...options,
    detached: true,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stdout = "",
    stderr = "",
    timedOut = false;
  child.stdout.on("data", (d) => {
    stdout += d;
  });
  child.stderr.on("data", (d) => {
    stderr += d;
  });
  child.stdin.on("error", () => {});
  child.stdin.end();
  const kill = (signal) => {
    try {
      process.kill(-child.pid, signal);
    } catch (e) {
      if (e.code !== "ESRCH") throw e;
    }
  };
  let forceTimer;
  const timer = setTimeout(() => {
    timedOut = true;
    kill("SIGTERM");
    forceTimer = setTimeout(() => kill("SIGKILL"), 2000);
  }, timeoutMs);
  let exitCode, error;
  try {
    exitCode = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", resolve);
    });
  } catch (e) {
    error = e.message;
  } finally {
    clearTimeout(timer);
    clearTimeout(forceTimer);
  }
  return { exitCode, error, timedOut, wallMs: performance.now() - start, stdout, stderr };
}

// The wiretap inherits only an explicitly written disposable gateway environment.
async function wiretap(configPath) {
  const cfg = readJson(configPath);
  const child = cfg.bridgePort
    ? null
    : spawn(cfg.gateway, ["--stdio-adapter"], {
        cwd: cfg.home,
        env: cfg.env,
        stdio: ["pipe", "pipe", "pipe"],
      });
  const connection = cfg.bridgePort ? connect(cfg.bridgePort, "127.0.0.1") : null;
  const sink = connection || child.stdin;
  const source = connection || child.stdout;
  const input = createInterface({ input: process.stdin });
  const heldIds = new Set();
  let heldReply,
    holdTimer,
    notificationDelivered = false;
  const release = () => {
    clearTimeout(holdTimer);
    if (heldReply) {
      process.stdout.write(heldReply + "\n");
      heldReply = undefined;
    }
  };
  const log = (direction, line) => {
    const m = JSON.parse(line);
    const safe = { at: Date.now(), direction, id: m.id, method: m.method };
    if (m.method === "initialize") safe.params = m.params;
    if (m.method === "tools/call") safe.params = m.params;
    if (m.result?.tools) {
      safe.toolCount = m.result.tools.length;
      safe.names = m.result.tools.map((t) => t.name);
      safe.catalogBytes = Buffer.byteLength(JSON.stringify(m.result));
    }
    safe.message = m;
    if (m.error) safe.error = m.error;
    record(cfg.wire, safe);
  };
  input.on("line", (line) => {
    log("client", line);
    const message = JSON.parse(line);
    if (
      cfg.notificationHold &&
      message.method === "tools/call" &&
      (message.params.name === "slack__slack_list_channels" ||
        message.params.arguments?.name === "slack__slack_list_channels")
    )
      heldIds.add(message.id);
    sink.write(line + "\n");
  });
  input.on("close", () => sink.end());
  sink.on("error", () => input.close());
  createInterface({ input: source }).on("line", (line) => {
    log("server", line);
    const message = JSON.parse(line);
    if (message.method === "notifications/tools/list_changed") {
      notificationDelivered = true;
      process.stdout.write(line + "\n");
      release();
      return;
    }
    if (heldIds.has(message.id) && message.result && !notificationDelivered) {
      heldReply = line;
      holdTimer = setTimeout(() => {
        record(cfg.wire, {
          at: Date.now(),
          method: "benchmark/notification-deadline",
          direction: "harness",
        });
        release();
      }, 10000);
      return;
    }
    process.stdout.write(line + "\n");
  });
  child?.stderr.pipe(process.stderr);
  (connection || child).on("error", (error) => {
    console.error(error.message);
    input.close();
    process.exitCode = 1;
  });
  child?.on("exit", (code) => {
    input.close();
    process.exitCode = code ?? 1;
  });
  connection?.on("end", () => input.close());
  process.on("SIGTERM", () => {
    release();
    child?.kill();
    connection?.destroy();
    input.close();
  });
}

async function coldBridge(cfg, output) {
  const sockets = new Set(),
    children = new Set();
  const server = createServer((socket) => {
    sockets.add(socket);
    const child = spawn(cfg.gateway, ["--stdio-adapter"], {
      cwd: cfg.home,
      env: cfg.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    children.add(child);
    socket.pipe(child.stdin);
    child.stdout.pipe(socket);
    child.stdin.on("error", () => socket.destroy());
    child.stderr.on("data", (data) =>
      appendFileSync(path.join(output, "gateway-stderr.txt"), data),
    );
    child.on("error", (error) => {
      record(path.join(output, "bridge-errors.jsonl"), { error: error.message });
      socket.destroy();
    });
    child.on("exit", () => {
      children.delete(child);
      socket.end();
    });
    socket.on("close", () => {
      sockets.delete(socket);
      child.kill();
    });
    socket.on("error", () => child.kill());
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  cfg.bridgePort = server.address().port;
  return async () => {
    for (const socket of sockets) socket.destroy();
    for (const child of children) child.kill();
    await new Promise((resolve) => server.close(resolve));
  };
}

async function prewarm(cfg) {
  const child = spawn(cfg.gateway, ["--stdio-adapter"], {
    env: {
      ...cfg.env,
      TOOLPORT_CLIENT_ID: `client:${cfg.warmClient}`,
      TOOLPORT_DISCOVERY: "full",
    },
    cwd: cfg.home,
    stdio: ["pipe", "pipe", "pipe"],
  });
  const lines = createInterface({ input: child.stdout });
  let requestId = 2,
    pending = false,
    dirty = false;
  const list = () => {
    if (pending) {
      dirty = true;
      return;
    }
    pending = true;
    child.stdin.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: requestId++,
        method: "tools/list",
        params: {},
      }) + "\n",
    );
  };
  const done = new Promise((resolve, reject) => {
    child.once("error", reject);
    lines.on("line", (line) => {
      const message = JSON.parse(line);
      if (message.id === 1) {
        child.stdin.write(
          JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n",
        );
        list();
      }
      if (message.method === "notifications/tools/list_changed") list();
      if (message.id >= 2) {
        pending = false;
        if (message.result?.tools?.length === 1709) resolve(1709);
        else if (dirty) {
          dirty = false;
          list();
        }
      }
    });
  });
  child.stdin.write(
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "codex", version: "bench" },
      },
    }) + "\n",
  );
  let timer;
  try {
    const count = await Promise.race([
      done,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("Warm catalog deadline")), 25000);
      }),
    ]);
    assert.equal(
      count,
      1709,
      "Warm fixture must expose all 1707 public tools and two gateway tools",
    );
  } finally {
    clearTimeout(timer);
    child.kill();
    lines.close();
  }
}

async function cleanup(data) {
  for (const filename of await readdir(data)) {
    if (!/^daemon-.*\.json$/.test(filename)) continue;
    const descriptor = JSON.parse(await readFile(path.join(data, filename), "utf8"));
    if (Number.isSafeInteger(descriptor.pid) && descriptor.pid > 0) {
      try {
        process.kill(descriptor.pid, "SIGTERM");
      } catch (e) {
        if (e.code !== "ESRCH") throw e;
      }
    }
  }
}

export function finalText(client, output) {
  const events = output.split("\n").flatMap((line) => {
    try {
      return [JSON.parse(line)];
    } catch {
      return [];
    }
  });
  if (client === "claude-code" || client === "cursor")
    return events.findLast((e) => typeof e.result === "string")?.result || "";
  if (client === "codex")
    return events.findLast((e) => e.item?.type === "agent_message")?.item.text || "";
  return events
    .filter((e) => e.type === "text")
    .map((e) => e.part?.text || "")
    .join("\n");
}

export function abstained(text) {
  return /unavailable|not available|no (?:matching |suitable |supported )?(?:tool|capability)|cannot|can't|not supported|does not (?:have|support)/i.test(
    text,
  );
}

function usage(client, output) {
  const events = output.split("\n").flatMap((line) => {
    try {
      return [JSON.parse(line)];
    } catch {
      return [];
    }
  });
  if (client === "claude-code") {
    const u = events.findLast((e) => e.usage)?.usage;
    if (!u) return null;
    return {
      input: u.input_tokens,
      cachedInput: u.cache_read_input_tokens,
      cacheWrite: u.cache_creation_input_tokens,
      output: u.output_tokens,
      reported: u,
    };
  }
  if (client === "cursor") {
    const u = events.findLast((e) => e.type === "result")?.usage;
    return u
      ? {
          input: u.inputTokens,
          cachedInput: u.cacheReadTokens || 0,
          cacheWrite: u.cacheWriteTokens || 0,
          output: u.outputTokens,
          reported: u,
        }
      : null;
  }
  if (client === "codex") {
    const u = events.findLast((e) => e.type === "turn.completed")?.usage;
    return u
      ? {
          input: u.input_tokens,
          cachedInput: u.cached_input_tokens,
          output: u.output_tokens,
          reported: u,
        }
      : null;
  }
  const parts = events
    .filter((e) => e.type === "step_finish")
    .map((e) => e.part?.tokens)
    .filter(Boolean);
  if (!parts.length) return null;
  return {
    input: parts.reduce((n, p) => n + p.input, 0),
    cachedInput: parts.reduce((n, p) => n + (p.cache?.read || 0), 0),
    cacheWrite: parts.reduce((n, p) => n + (p.cache?.write || 0), 0),
    output: parts.reduce((n, p) => n + p.output, 0),
    reasoning: parts.reduce((n, p) => n + (p.reasoning || 0), 0),
    reported: parts,
  };
}

async function benchmark(options) {
  const {
    catalog: catalogPath,
    dev: devPath,
    gateway,
    executable,
    client,
    mode,
    out,
  } = options;
  assert(
    ["claude-code", "codex", "opencode", "cursor"].includes(client),
    "Supported benchmark client required",
  );
  assert(["full", "lazy", "grouped"].includes(mode), "Explicit discovery mode required");
  assert(path.isAbsolute(gateway) && path.isAbsolute(executable));
  mkdirSync(out, { recursive: true, mode: 0o700 });
  const catalog = readJson(catalogPath),
    dev = readJson(devPath);
  assert.equal(catalog.length, 1707);
  let tasks = tasksFrom(dev);
  // Validate the frozen expected arguments before paying for any model turns.
  for (const task of tasks)
    for (const expected of task.expected) {
      const tool = catalog.find((t) => t.name.replaceAll("-", "_") === expected.name);
      assert(tool, `${task.id}: missing public tool`);
      const validate = validator(tool);
      if (validate)
        assert(
          validate(
            expected.textIncludes
              ? { ...expected.args, text: expected.textIncludes }
              : expected.args,
          ),
          `${task.id}: invalid reference args ${JSON.stringify(validate.errors)}`,
        );
      else
        console.log(
          JSON.stringify({
            task: task.id,
            invalidPublicSchema: schemaErrors.get(tool.name),
            scoring: "fails strict schema validity; catalog unchanged",
          }),
        );
    }
  if (options.tasks) tasks = tasks.filter((t) => options.tasks.split(",").includes(t.id));
  if (options.start)
    tasks = [
      {
        id: "session-start",
        prompt: "Reply with just ready. Do not call any tools.",
        expected: [],
      },
    ];
  if (options.notification)
    tasks = [
      {
        id: "list-changed",
        prompt:
          "List 5 Slack channels using MCP. After that call, find and call the new Slack capability bench_added_after_notification. Recover using discovery if needed. Do not call any other downstream capabilities.",
        expected: [
          { name: "slack__slack_list_channels", args: { limit: 5 } },
          { name: "slack__bench_added_after_notification", args: {} },
        ],
      },
    ];
  tasks = tasks.slice(0, Number(options.limit || tasks.length));
  let infrastructureFailures = 0;
  for (const task of tasks) {
    const label = `${client}-${mode}-${task.id}${options.delay ? `-delay${options.delay}` : ""}`;
    const output = path.join(out, label);
    if (
      options.resume &&
      (await stat(path.join(output, "summary.json")).catch(() => null))
    )
      continue;
    if (options.resume && (await stat(output).catch(() => null))) {
      // Preserve a cancelled worker's incomplete artifacts alongside the rerun.
      const { rename } = await import("node:fs/promises");
      await rename(output, `${output}-cancelled-${Date.now()}`);
    }
    mkdirSync(output, { recursive: false, mode: 0o700 });
    const home = await mkdtemp(path.join(os.tmpdir(), "toolport-live-bench-"));
    const data = path.join(home, "gateway-data");
    mkdirSync(data, { mode: 0o700 });
    const wire = path.join(output, "wire.jsonl"),
      downstream = path.join(output, "downstream.jsonl");
    const namespaces = options["single-server"]
      ? ["catalog"]
      : [...new Set(catalog.map((t) => t.name.split("__")[0]))];
    const warmClient = client === "codex" ? "claude-code" : "codex";
    const registry = {
      version: 3,
      safetyLevel: "off",
      servers: namespaces.map((id) => ({
        id,
        name: id,
        enabled: true,
        transport: "stdio",
        command: process.execPath,
        args: [
          mock,
          path.resolve(catalogPath),
          id,
          downstream,
          String(options.delay || 0),
          options.notification ? "1" : "0",
        ],
        env: [],
        inheritEnv: false,
      })),
      profiles: [],
      clientDiscovery: { [client]: mode, [warmClient]: "full" },
    };
    writeFileSync(path.join(data, "registry.json"), JSON.stringify(registry));
    const cfg = {
      home,
      gateway,
      wire,
      warmClient,
      notificationHold: Boolean(options.notification),
      env: {
        PATH: process.env.PATH,
        HOME: home,
        TMPDIR: home,
        XDG_CONFIG_HOME: path.join(home, ".config"),
        XDG_DATA_HOME: path.join(home, ".local/share"),
        XDG_CACHE_HOME: path.join(home, ".cache"),
        DBUS_SESSION_BUS_ADDRESS: `unix:path=${path.join(home, "no-session-bus")}`,
        TOOLPORT_DATA_DIR: data,
        TOOLPORT_REGISTRY: path.join(data, "registry.json"),
        TOOLPORT_CLIENT_ID: options["adapter-id"] || `client:${client}`,
        TOOLPORT_DISCOVERY: mode,
        TOOLPORT_CODE_MODE: "0",
        TOOLPORT_SECRET_KEY: "disposable-public-fixture-key",
      },
    };
    const shimFile = path.join(home, "gateway.json");
    writeFileSync(shimFile, JSON.stringify(cfg));
    const mcp = { command: process.execPath, args: [self, "--wiretap", shimFile] };
    const recovery = options.recovery
      ? " First call toolport_status and use the exact server prefixes it returns; never assume a service name is the server prefix. If advertised, explicitly use toolport_search_tools to discover the requested capability with that server filter. If the catalog is loading, retry discovery at most twice before saying unavailable."
      : "";
    const prompt = `${options.bootstrap ? "First inspect ALL_TOOLS for toolport discovery helpers by filtering only for toolport. Use toolport_search_tools or a help_ tool to discover the requested capability, even if a capability-specific filter of ALL_TOOLS returns no matches. " : ""}This is an isolated MCP capability test with deterministic mock results. Use only tools from the toolport MCP server. Do not use shell, files, web, other servers, or delegate work. Execute the request, then reply briefly. If no matching capability exists, say unavailable without making an unrelated call.${recovery} Request: ${task.prompt}`;
    const env = { ...process.env, NO_COLOR: "1", TERM: "dumb", TMPDIR: home, PWD: home };
    for (const key of Object.keys(env))
      if (/^(TOOLPORT_|CONDUIT_|T3_|CLAUDECODE)/.test(key)) delete env[key];
    let args;
    let launchExecutable = executable;
    let codexHomeProtected = false;
    const configFile = path.join(home, "mcp.json");
    writeFileSync(configFile, JSON.stringify({ mcpServers: { toolport: mcp } }));
    if (client === "claude-code") {
      args = [
        "-p",
        prompt,
        "--mcp-config",
        configFile,
        "--strict-mcp-config",
        "--output-format",
        "json",
        "--model",
        options.model || "haiku",
        "--setting-sources",
        "",
        "--settings",
        JSON.stringify({
          disableAllHooks: true,
          permissions: { allow: ["mcp__toolport__*"] },
        }),
        "--tools",
        "ToolSearch",
        "--no-session-persistence",
        "--max-budget-usd",
        "0.50",
      ];
      env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1";
    } else if (client === "codex") {
      args = [
        "exec",
        "--json",
        "--ignore-user-config",
        "--ignore-rules",
        "--ephemeral",
        "--skip-git-repo-check",
        "-s",
        "read-only",
        "-C",
        home,
        "-c",
        `mcp_servers.toolport={command=${JSON.stringify(mcp.command)},args=${JSON.stringify(mcp.args)},startup_timeout_sec=45,tool_timeout_sec=45,required=true,default_tools_approval_mode="auto"}`,
        "-c",
        'model_reasoning_effort="low"',
        "-c",
        'history.persistence="none"',
        "-c",
        `log_dir=${JSON.stringify(path.join(home, "logs"))}`,
        "-c",
        `sqlite_home=${JSON.stringify(path.join(home, "state"))}`,
        "-c",
        "feedback.enabled=false",
        "-c",
        "features.plugins=false",
        "-c",
        "features.apps=false",
        "-c",
        "features.remote_plugin=false",
        "-c",
        'developer_instructions="Use only the supplied MCP server for the test."',
        prompt,
      ];
      if (options.model) args.splice(1, 0, "-m", options.model);
      if (options.debug) {
        env.RUST_LOG = "codex_core=debug";
        args.splice(args.indexOf("--ephemeral"), 1);
      }
      if (options["fixture-approvals"]) {
        const sandbox = args.indexOf("-s");
        args.splice(sandbox, 2, "--dangerously-bypass-approvals-and-sandbox");
      }
      // Real home is used only by the CLI's existing login. No auth file is read/copied.
      env.CODEX_HOME = path.join(os.homedir(), ".codex");
      assert.equal(
        process.platform,
        "linux",
        "Codex login preservation requires the Linux read-only mount",
      );
      const original = path.join(home, "codex-original");
      const upper = path.join(home, "codex-writes");
      const work = path.join(home, "codex-overlay-work");
      for (const dir of [original, upper, work]) mkdirSync(dir, { mode: 0o700 });
      const readonlyArgs = [
        "--unshare-pid",
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--bind",
        home,
        home,
        "--bind",
        output,
        output,
        "--ro-bind",
        env.CODEX_HOME,
        original,
        "--overlay-src",
        env.CODEX_HOME,
        "--overlay",
        upper,
        work,
        env.CODEX_HOME,
      ];
      // The CLI reads its normal login directly. These files cannot be copied
      // up, refreshed or changed by the writable runtime overlay.
      for (const file of ["auth.json", "config.toml"]) {
        if (await stat(path.join(env.CODEX_HOME, file)).catch(() => null))
          readonlyArgs.push(
            "--ro-bind",
            path.join(env.CODEX_HOME, file),
            path.join(env.CODEX_HOME, file),
          );
      }
      const empty = path.join(home, "empty-skills");
      mkdirSync(empty);
      for (const dir of [
        path.join(env.CODEX_HOME, "skills"),
        path.join(os.homedir(), ".agents/skills"),
      ]) {
        if (await stat(dir).catch(() => null))
          readonlyArgs.push("--bind", empty, await realpath(dir));
      }
      const instructions = path.join(home, "empty-instructions.md");
      writeFileSync(instructions, "");
      for (const file of [
        path.join(env.CODEX_HOME, "AGENTS.md"),
        path.join(os.homedir(), ".agents/AGENTS.md"),
      ]) {
        if (await stat(file).catch(() => null))
          readonlyArgs.push("--ro-bind", instructions, await realpath(file));
      }
      args = [...readonlyArgs, "--", executable, ...args];
      launchExecutable = "/usr/bin/bwrap";
      codexHomeProtected = true;
    } else if (client === "cursor") {
      const projectConfig = path.join(home, ".cursor");
      mkdirSync(projectConfig);
      writeFileSync(
        path.join(projectConfig, "mcp.json"),
        JSON.stringify({ mcpServers: { toolport: mcp } }),
      );
      env.CURSOR_CONFIG_DIR = path.join(home, "cursor-config");
      env.CURSOR_DATA_DIR = path.join(home, "cursor-data");
      args = [
        "--print",
        "--output-format",
        "stream-json",
        "--approve-mcps",
        "--trust",
        "--force",
        "--workspace",
        home,
        prompt,
      ];
      if (options.model) args.unshift("--model", options.model);
      // Keep the existing login readable, but hide the real global MCP config.
      const emptyMcp = path.join(home, "empty-mcp.json");
      writeFileSync(emptyMcp, '{"mcpServers":{}}');
      env.AGENT_CLI_CREDENTIAL_STORE = "file";
      const cursorHome = path.join(os.homedir(), ".cursor");
      const original = path.join(home, "cursor-original"),
        upper = path.join(home, "cursor-writes"),
        work = path.join(home, "cursor-overlay-work");
      for (const dir of [original, upper, work]) mkdirSync(dir, { mode: 0o700 });
      const readonly = [
        "--unshare-pid",
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--bind",
        home,
        home,
        "--bind",
        output,
        output,
        "--ro-bind",
        cursorHome,
        original,
        "--overlay-src",
        cursorHome,
        "--overlay",
        upper,
        work,
        cursorHome,
      ];
      const globalMcp = path.join(os.homedir(), ".cursor/mcp.json");
      if (await stat(globalMcp).catch(() => null))
        readonly.push("--ro-bind", emptyMcp, globalMcp);
      args = [...readonly, "--", executable, ...args];
      launchExecutable = "/usr/bin/bwrap";
    } else {
      const opencodeFile = path.join(home, "opencode.json");
      writeFileSync(
        opencodeFile,
        JSON.stringify({
          mcp: {
            toolport: {
              type: "local",
              command: [mcp.command, ...mcp.args],
              enabled: true,
              timeout: 45000,
            },
          },
          permission: { "*": "deny", "toolport_*": "allow" },
        }),
      );
      env.OPENCODE_CONFIG = opencodeFile;
      env.OPENCODE_CONFIG_DIR = path.join(home, "opencode-config");
      env.XDG_CONFIG_HOME = path.join(home, ".config");
      env.OPENCODE_DISABLE_PROJECT_CONFIG = "true";
      args = ["run", "--pure", "--format", "json", prompt];
      if (options.model) args.splice(1, 0, "-m", options.model);
      if (options.debug) args.unshift("--print-logs", "--log-level", "DEBUG");
    }
    let before;
    let result;
    let closeBridge;
    try {
      if (client === "codex" && options.delay) {
        closeBridge = await coldBridge(cfg, output);
        writeFileSync(shimFile, JSON.stringify(cfg));
      }
      if (!options.delay) await prewarm(cfg);
      before = await configMetadata();
      result = await runProcess(
        launchExecutable,
        args,
        { cwd: home, env },
        Number(options.timeout || 90000),
      );
      writeFileSync(path.join(output, "stdout.jsonl"), result.stdout, { mode: 0o600 });
      writeFileSync(path.join(output, "stderr.txt"), result.stderr, { mode: 0o600 });
      const after = await configMetadata();
      const changes = Object.keys({ ...before, ...after }).filter(
        (name) => !includes(before[name], after[name]),
      );
      writeFileSync(
        path.join(output, "real-config-metadata.json"),
        JSON.stringify({ before, after, changes }, null, 2),
      );
      const events = records(wire),
        calls = records(downstream).filter((e) => e.method === "tools/call");
      const finished =
        result.exitCode === 0 &&
        !result.timedOut &&
        (client !== "codex" || result.stdout.includes('"type":"turn.completed"')) &&
        (client !== "claude-code" || result.stdout.includes('"is_error":false')) &&
        (client !== "opencode" || result.stdout.includes('"type":"step_finish"')) &&
        (client !== "cursor" ||
          result.stdout.includes('"type":"result","subtype":"success"'));
      const scored = options.start
        ? { success: finished, wrongCalls: calls.length, calls: [] }
        : options.notification
          ? {
              success:
                finished &&
                calls.some(
                  (r) =>
                    r.namespace === "slack" &&
                    r.params.name === "bench_added_after_notification",
                ),
              calls,
            }
          : score(task, calls, catalog, finished);
      if (!task.expected.length && !options.start)
        scored.success &&= abstained(finalText(client, result.stdout));
      const summary = {
        label,
        client,
        mode,
        task: task.id,
        delayMs: Number(options.delay || 0),
        warm: !options.delay,
        fixtureServerCount: namespaces.length,
        ...scored,
        finished,
        exitCode: result.exitCode,
        error: result.error,
        timedOut: result.timedOut,
        wallMs: result.wallMs,
        usage: usage(client, result.stdout),
        requestedModel:
          options.model || (client === "claude-code" ? "haiku" : "CLI default"),
        modelUsage:
          client === "claude-code"
            ? records(path.join(output, "stdout.jsonl")).findLast((e) => e.modelUsage)
                ?.modelUsage
            : undefined,
        codexHomeProtected,
        fixtureApprovalBypass: Boolean(options["fixture-approvals"]),
        explicitRecovery: Boolean(options.recovery),
        nativeBootstrap: Boolean(options.bootstrap),
        runtimeHome: home,
        realCodexHomeChanges: changes,
        searchRounds: events.filter(
          (e) =>
            e.direction === "client" &&
            e.method === "tools/call" &&
            (e.params.name === "toolport_search_tools" ||
              e.params.name.startsWith("help_")),
        ).length,
        describeRounds: events.filter(
          (e) =>
            e.direction === "client" &&
            e.method === "tools/call" &&
            e.params.name === "toolport_search_tools" &&
            e.params.arguments?.name,
        ).length,
        toolLists: events.filter(
          (e) => e.direction === "client" && e.method === "tools/list",
        ).length,
        listedCounts: events
          .filter((e) => e.toolCount !== undefined)
          .map((e) => e.toolCount),
        notifications: events.filter(
          (e) => e.method === "notifications/tools/list_changed",
        ).length,
        initialize: events.find((e) => e.method === "initialize")?.params,
        catalogSha256: createHash("sha256")
          .update(readFileSync(catalogPath))
          .digest("hex"),
      };
      await writeFile(
        path.join(output, "summary.json"),
        JSON.stringify(summary, null, 2),
      );
      record(path.join(out, "results.jsonl"), summary);
      console.log(
        JSON.stringify({
          label,
          success: summary.success,
          finished,
          calls: calls.length,
          usage: summary.usage,
          wallMs: summary.wallMs,
          homeChanges: changes.length,
        }),
      );
      if (
        !finished &&
        !(summary.usage?.input || summary.usage?.cachedInput || summary.usage?.output)
      )
        infrastructureFailures++;
      else infrastructureFailures = 0;
      if (
        changes.some((name) =>
          ({
            codex: [".codex/config.toml"],
            cursor: [".cursor/mcp.json", ".cursor/cli-config.json"],
            "claude-code": [".claude.json", ".claude/settings.json"],
            opencode: [
              ".config/opencode/opencode.json",
              ".config/opencode/opencode.jsonc",
            ],
          })[client].includes(name),
        ) &&
        !codexHomeProtected &&
        client !== "cursor"
      ) {
        console.log(
          "Stopped: client config metadata changed; inspect the retained summary.",
        );
        break;
      }
      if (infrastructureFailures >= 2) {
        console.log("Stopped after two consecutive infrastructure failures.");
        break;
      }
    } finally {
      if (closeBridge) await closeBridge();
      await cleanup(data);
    }
  }
}

if (process.argv[1] === self) {
  if (process.argv[2] === "--wiretap") await wiretap(process.argv[3]);
  else {
    const options = {};
    for (let i = 2; i < process.argv.length; i++) {
      const key = process.argv[i].replace(/^--/, "");
      options[key] = [
        "start",
        "notification",
        "fixture-approvals",
        "recovery",
        "single-server",
        "resume",
        "debug",
        "bootstrap",
      ].includes(key)
        ? true
        : process.argv[++i];
    }
    await benchmark(options);
  }
}
