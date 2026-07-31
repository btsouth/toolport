#!/usr/bin/env node
// Vendor-neutral local MCP gateway benchmark.
//
// Both products ingest the same generated MCP catalog from fixture-mcp.mjs and
// are driven through their public stdio MCP surfaces. No model, network service,
// account, or API key participates in the measured workload.
//
//   npm run bench:compare -- --install-ratel
//   node benchmark/compare-local.mjs --products=toolport --sizes=25,100
//   node benchmark/compare-local.mjs --json --out=benchmark/local-compare.json

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "..");
const CONFIG = JSON.parse(readFileSync(join(HERE, "compare-local.config.json"), "utf8"));
const FIXTURE = join(HERE, "fixture-mcp.mjs");
const TARGET = process.env.CARGO_TARGET_DIR
  ? resolve(process.env.CARGO_TARGET_DIR)
  : join(ROOT, "src-tauri", "target");
const DEBUG = join(TARGET, "debug");
const RELEASE = join(TARGET, "release");
const RELEASE_GATEWAY = executable(RELEASE, "toolport-gateway");
const DEBUG_GATEWAY = executable(DEBUG, "toolport-gateway");
const TOOLPORT_GATEWAY =
  process.env.TOOLPORT_GATEWAY ||
  (existsSync(RELEASE_GATEWAY) ? RELEASE_GATEWAY : DEBUG_GATEWAY);
const argv = process.argv.slice(2);

const options = {
  products: valueArg("products", "toolport,ratel")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean),
  sizes: valueArg("sizes", CONFIG.defaults.catalogSizes.join(","))
    .split(",")
    .map(Number)
    .filter((value) => Number.isInteger(value) && value > 0),
  iterations: Math.max(20, Number(valueArg("iterations", CONFIG.defaults.iterations))),
  settleMilliseconds: Math.max(
    0,
    Number(valueArg("settle-ms", CONFIG.defaults.settleMilliseconds)),
  ),
  topK: Math.max(1, Number(valueArg("top-k", CONFIG.defaults.topK))),
  json: argv.includes("--json"),
  check: argv.includes("--check"),
  profileCalls: argv.includes("--profile-calls"),
  installRatel: argv.includes("--install-ratel"),
  out: optionalValueArg("out"),
};

for (const product of options.products) {
  if (!["toolport", "ratel"].includes(product)) {
    fail(`unknown product "${product}" (expected toolport and/or ratel)`);
  }
}
if (options.sizes.length === 0) fail("at least one positive catalog size is required");

const SIGNAL_TOOLS = [
  {
    name: "github_create_pull_request",
    description: "Open a new GitHub pull request from a branch with a title and body.",
    query: "open a pull request for my branch",
  },
  {
    name: "postgres_run_query",
    description: "Execute a SQL query against a PostgreSQL database and return rows.",
    query: "run a SQL query against my database",
  },
  {
    name: "stripe_refund_payment",
    description: "Refund a Stripe payment or charge to the customer.",
    query: "refund a customer payment",
  },
  {
    name: "resend_send_email",
    description: "Send a transactional email message to a recipient.",
    query: "send a welcome email to a new signup",
  },
  {
    name: "filesystem_read_file",
    description: "Read text content from a file on the local filesystem.",
    query: "read a local file from disk",
  },
  {
    name: "vercel_list_projects",
    description: "List the projects deployed in a Vercel account or team.",
    query: "show my deployed Vercel projects",
  },
  {
    name: "sentry_list_issues",
    description: "List recent application errors and unresolved issues from Sentry.",
    query: "find recent production application errors",
  },
  {
    name: "cloudflare_purge_cache",
    description: "Purge cached assets for a Cloudflare zone.",
    query: "clear the CDN cache for my website",
  },
  {
    name: "calendar_create_event",
    description: "Create a calendar event with attendees, start time, and end time.",
    query: "schedule a meeting with the team",
  },
  {
    name: "slack_post_message",
    description: "Post a message to a Slack channel.",
    query: "send an update to our Slack channel",
  },
];

// Plausible near-matches make the retrieval fixture test intent, not merely
// keyword presence. They are deliberately useful tools rather than nonsense
// distractors, so ranking the requested action first requires the verb and
// surrounding context to matter.
const DECOY_TOOLS = [
  {
    name: "github_list_pull_requests",
    description: "List existing open GitHub pull requests and their branches.",
  },
  {
    name: "postgres_explain_query",
    description: "Explain the execution plan for a PostgreSQL query without running it.",
  },
  {
    name: "stripe_list_refunds",
    description: "List previous Stripe payment refunds for a customer.",
  },
  {
    name: "resend_list_emails",
    description: "List transactional email messages previously sent through Resend.",
  },
  {
    name: "filesystem_search_files",
    description: "Search local file names and paths without reading file content.",
  },
  {
    name: "vercel_get_project",
    description: "Get configuration for one known Vercel project.",
  },
  {
    name: "sentry_get_issue",
    description: "Get details for one known Sentry issue identifier.",
  },
  {
    name: "cloudflare_get_cache_rules",
    description: "Read the configured Cloudflare CDN cache rules for a zone.",
  },
  {
    name: "calendar_list_events",
    description: "List existing calendar events in a time range.",
  },
  {
    name: "slack_list_messages",
    description: "List recent messages from a Slack channel.",
  },
];

const INIT = {
  protocolVersion: "2025-06-18",
  capabilities: {},
  clientInfo: { name: "toolport-local-comparison", version: "1" },
};

function valueArg(name, fallback) {
  return optionalValueArg(name) ?? fallback;
}

function optionalValueArg(name) {
  const prefix = `--${name}=`;
  const inline = argv.find((arg) => arg.startsWith(prefix));
  if (inline) return inline.slice(prefix.length);
  const index = argv.indexOf(`--${name}`);
  if (index >= 0) return argv[index + 1];
  return undefined;
}

function executable(dir, name) {
  return join(dir, process.platform === "win32" ? `${name}.exe` : name);
}

function fail(message) {
  console.error(message);
  process.exit(2);
}

function now() {
  return Number(process.hrtime.bigint()) / 1e6;
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

function stats(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const quantile = (q) =>
    sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * q) - 1)];
  return {
    median: quantile(0.5),
    p95: quantile(0.95),
    min: sorted[0],
    max: sorted.at(-1),
  };
}

async function measure(fn, iterations) {
  const values = [];
  for (let i = 0; i < iterations; i++) {
    const started = now();
    await fn(i);
    values.push(now() - started);
  }
  return stats(values);
}

function makeCatalog(size) {
  const selectedSignals = SIGNAL_TOOLS.slice(0, Math.min(size, SIGNAL_TOOLS.length));
  const tools = selectedSignals.map(({ name, description }) =>
    toolDefinition(name, description),
  );
  for (const { name, description } of DECOY_TOOLS) {
    if (tools.length >= size) break;
    tools.push(toolDefinition(name, description));
  }
  const domains = [
    "inventory",
    "analytics",
    "billing",
    "deployment",
    "monitoring",
    "documents",
    "support",
    "identity",
  ];
  for (let i = tools.length; i < size; i++) {
    const domain = domains[i % domains.length];
    tools.push(
      toolDefinition(
        `service_${String(i).padStart(4, "0")}_${domain}_operation`,
        `Perform operation ${i} for the ${domain} service using its resource identifier.`,
      ),
    );
  }
  return { tools };
}

function toolDefinition(name, description) {
  return {
    name,
    description,
    inputSchema: {
      type: "object",
      properties: {
        resourceId: { type: "string", description: "Target resource identifier." },
        options: {
          type: "object",
          description: "Optional operation settings.",
          additionalProperties: true,
        },
      },
      required: ["resourceId"],
    },
  };
}

class McpProcess {
  constructor(command, args, spawnOptions = {}) {
    this.stderr = "";
    this.callProfiles = [];
    this.pending = new Map();
    this.nextId = 0;
    this.proc = spawn(command, args, {
      ...spawnOptions,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    const stderrLines = createInterface({ input: this.proc.stderr, crlfDelay: Infinity });
    stderrLines.on("line", (line) => {
      this.stderr = `${this.stderr}${line}\n`.slice(-12_000);
      const prefix = "toolport-call-profile ";
      if (!line.startsWith(prefix)) return;
      try {
        const profile = JSON.parse(line.slice(prefix.length));
        const timingFields = [
          "preflightUs",
          "downstreamUs",
          "postprocessUs",
          "auditUs",
          "totalUs",
        ];
        if (timingFields.every((field) => Number.isFinite(profile[field]))) {
          this.callProfiles.push(profile);
        }
      } catch {
        // A diagnostic line must never affect the benchmark transport.
      }
    });
    const lines = createInterface({ input: this.proc.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => {
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        return;
      }
      const pending = this.pending.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.pending.delete(message.id);
      pending.resolve(message);
    });
    this.proc.on("error", (error) => this.rejectAll(error));
    this.proc.on("exit", (code) => {
      if (this.pending.size > 0) {
        this.rejectAll(
          new Error(`process exited with code ${code}\n${this.stderr.trim()}`),
        );
      }
    });
  }

  call(method, params = {}, timeoutMs = 20_000) {
    return new Promise((resolvePromise, reject) => {
      const id = ++this.nextId;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(
          new Error(`${method} timed out after ${timeoutMs} ms\n${this.stderr.trim()}`),
        );
      }, timeoutMs);
      this.pending.set(id, { resolve: resolvePromise, reject, timer });
      this.proc.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`,
      );
    });
  }

  notify(method, params = {}) {
    this.proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  rejectAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  stop() {
    try {
      this.proc.stdin.end();
      this.proc.kill();
    } catch {
      // Best-effort cleanup of an already-exited benchmark child.
    }
  }
}

function fixtureArgs(catalogPath) {
  return [FIXTURE, catalogPath];
}

function toolportAdapter(registryPath) {
  if (!existsSync(TOOLPORT_GATEWAY)) {
    fail(
      `missing Toolport gateway at ${TOOLPORT_GATEWAY}\n` +
        "Build it with: cargo build --manifest-path src-tauri/Cargo.toml --bins",
    );
  }
  return {
    name: "toolport",
    command: TOOLPORT_GATEWAY,
    args: [],
    env: {
      ...process.env,
      TOOLPORT_REGISTRY: registryPath,
      // Keep audit/search traces, result cursors, and cache files inside this
      // disposable run. Otherwise prior local Toolport activity can change I/O
      // cost and make repeated benchmark runs drift.
      TOOLPORT_DATA_DIR: dirname(registryPath),
      TOOLPORT_PROFILE: "benchmark",
      TOOLPORT_DISCOVERY: "lazy",
      ...(options.profileCalls ? { TOOLPORT_PROFILE_CALLS: "1" } : {}),
    },
    searchTool: "toolport_search_tools",
    invokeTool: "toolport_call_tool",
    searchArgs: (query, topK) => ({ query, limit: topK }),
    invokeArgs: (toolId, args) => ({ name: toolId, arguments: args }),
    parseSearch: parseToolportSearch,
    parseHits: parseToolportHits,
  };
}

function ratelAdapter(configPath, ratelBin, benchmarkHome) {
  return {
    name: "ratel",
    command: process.execPath,
    args: [ratelBin, "serve", configPath],
    env: {
      ...process.env,
      HOME: benchmarkHome,
      USERPROFILE: benchmarkHome,
      RATEL_TELEMETRY: "off",
    },
    searchTool: "search_capabilities",
    invokeTool: "invoke_tool",
    searchArgs: (query, topK) => ({ query, topKTools: topK, topKSkills: 1 }),
    invokeArgs: (toolId, args) => ({ toolId, args }),
    parseSearch: parseRatelSearch,
    parseHits: parseRatelHits,
  };
}

function parseToolportSearch(response) {
  return parseToolportHits(response).map((hit) => hit.toolId);
}

function parseRatelSearch(response) {
  return parseRatelHits(response).map((hit) => hit.toolId);
}

function parseToolportHits(response) {
  const text = response.result?.content?.map((item) => item.text ?? "").join("\n") ?? "";
  const marker = text.lastIndexOf("\n\n[");
  if (marker < 0) return [];
  try {
    const tools = JSON.parse(text.slice(marker + 2));
    if (!Array.isArray(tools)) return [];
    return tools
      .filter((tool) => tool && typeof tool.name === "string")
      .map((tool) => ({ toolId: tool.name, inputSchema: tool.inputSchema }));
  } catch {
    return [];
  }
}

function parseRatelHits(response) {
  let payload = response.result?.structuredContent;
  if (!payload) {
    const text = response.result?.content?.[0]?.text;
    try {
      payload = JSON.parse(text);
    } catch {
      payload = {};
    }
  }
  return (payload.tools?.groups ?? []).flatMap((group) =>
    (group.hits ?? [])
      .filter((hit) => typeof hit?.toolId === "string")
      .map((hit) => ({ toolId: hit.toolId, inputSchema: hit.inputSchema })),
  );
}

async function waitForCatalog(client, adapter, expectedToolId, readinessQuery) {
  const started = now();
  let lastNames = [];
  while (now() - started < 20_000) {
    const response = await client.call(
      "tools/call",
      {
        name: adapter.searchTool,
        arguments: adapter.searchArgs(readinessQuery, options.topK),
      },
      20_000,
    );
    lastNames = adapter.parseSearch(response);
    if (lastNames.includes(expectedToolId)) return;
    await sleep(25);
  }
  throw new Error(
    `${adapter.name} catalog never surfaced ${expectedToolId}; last results: ${lastNames.join(", ")}`,
  );
}

async function benchmarkDirect(catalogPath, toolName) {
  const client = new McpProcess(process.execPath, fixtureArgs(catalogPath));
  try {
    await client.call("initialize", INIT);
    client.notify("notifications/initialized");
    for (let i = 0; i < 10; i++) {
      await client.call("tools/call", {
        name: toolName,
        arguments: { resourceId: "benchmark" },
      });
    }
    return await measure(
      () =>
        client.call("tools/call", {
          name: toolName,
          arguments: { resourceId: "benchmark" },
        }),
      options.iterations,
    );
  } finally {
    client.stop();
  }
}

async function benchmarkProduct(
  adapter,
  direct,
  expectedToolId,
  retrievalCases,
  catalogTools,
) {
  const processStarted = now();
  const client = new McpProcess(adapter.command, adapter.args, {
    env: adapter.env,
    cwd: ROOT,
  });
  try {
    const handshakeStarted = now();
    const initialized = await client.call("initialize", INIT);
    const handshake = now() - handshakeStarted;
    if (initialized.error) throw new Error(JSON.stringify(initialized.error));
    client.notify("notifications/initialized");
    await waitForCatalog(client, adapter, expectedToolId, retrievalCases[0].query);
    const catalogReady = now() - processStarted;
    // Toolport makes its catalog searchable before all background integrity/cache
    // work has necessarily gone quiet, while Ratel completes ingestion during its
    // initialize handshake. Give both the same unmeasured quiet period so the
    // steady-state operation timings do not accidentally score startup work.
    await sleep(options.settleMilliseconds);

    const listed = await client.call("tools/list", {});
    const exposedTools = listed.result?.tools ?? [];
    const toolDefinitionBytes = Buffer.byteLength(JSON.stringify(exposedTools));
    const instructions = initialized.result?.instructions ?? "";
    const instructionBytes = Buffer.byteLength(instructions);
    const alwaysOnBytes = toolDefinitionBytes + instructionBytes;

    // Measure dispatch before the search workload so index construction, trace
    // writes, and CPU/cache churn from repeated searches cannot contaminate it.
    for (let i = 0; i < 10; i++) {
      await client.call("tools/call", {
        name: adapter.invokeTool,
        arguments: adapter.invokeArgs(expectedToolId, { resourceId: "benchmark" }),
      });
    }
    client.callProfiles.length = 0;
    const routedCall = await measure(
      () =>
        client.call("tools/call", {
          name: adapter.invokeTool,
          arguments: adapter.invokeArgs(expectedToolId, { resourceId: "benchmark" }),
        }),
      options.iterations,
    );
    const callProfiles = client.callProfiles.splice(0);

    for (let i = 0; i < 10; i++) {
      await client.call("tools/call", {
        name: adapter.searchTool,
        arguments: adapter.searchArgs(
          retrievalCases[i % retrievalCases.length].query,
          options.topK,
        ),
      });
    }
    const searchPayloadBytes = [];
    const search = await measure(async (i) => {
      const response = await client.call("tools/call", {
        name: adapter.searchTool,
        arguments: adapter.searchArgs(
          retrievalCases[i % retrievalCases.length].query,
          options.topK,
        ),
      });
      searchPayloadBytes.push(Buffer.byteLength(JSON.stringify(response.result ?? {})));
    }, options.iterations);

    const expectedSchemas = new Map(
      catalogTools.map((tool) => [`fixture__${tool.name}`, tool.inputSchema ?? {}]),
    );
    const cases = [];
    for (const testCase of retrievalCases) {
      const response = await client.call("tools/call", {
        name: adapter.searchTool,
        arguments: adapter.searchArgs(testCase.query, options.topK),
      });
      const hits = adapter.parseHits(response);
      const names = hits.map((hit) => hit.toolId);
      const wanted = `fixture__${testCase.name}`;
      const index = names.indexOf(wanted);
      const firstResponseBytes = Buffer.byteLength(JSON.stringify(response.result ?? {}));
      let readyResponseBytes = firstResponseBytes;
      let readyRoundTrips = 1;
      let schemaMatches =
        index >= 0 &&
        isDeepStrictEqual(hits[index]?.inputSchema ?? null, expectedSchemas.get(wanted));

      // A result without the exact schema is discoverable but not yet actionable.
      // Measure the real recovery cost instead of rewarding a smaller first response
      // that forces the agent to search again before it can invoke the tool.
      if (index >= 0 && !schemaMatches) {
        const schemaResponse = await client.call("tools/call", {
          name: adapter.searchTool,
          arguments: adapter.searchArgs(wanted, options.topK),
        });
        readyRoundTrips += 1;
        readyResponseBytes += Buffer.byteLength(
          JSON.stringify(schemaResponse.result ?? {}),
        );
        const schemaHit = adapter
          .parseHits(schemaResponse)
          .find((hit) => hit.toolId === wanted);
        schemaMatches = isDeepStrictEqual(
          schemaHit?.inputSchema ?? null,
          expectedSchemas.get(wanted),
        );
      }
      cases.push({
        query: testCase.query,
        expected: wanted,
        rank: index >= 0 ? index + 1 : null,
        topK: names,
        schemaMatches,
        readyToInvoke: index >= 0 && schemaMatches,
        readyRoundTrips,
        readyResponseBytes,
        readyEstimatedTokens: Math.ceil(readyResponseBytes / 4),
      });
    }

    const hits = cases.filter((item) => item.rank !== null);
    const ready = cases.filter((item) => item.readyToInvoke);
    const reciprocalRank =
      cases.reduce((sum, item) => sum + (item.rank ? 1 / item.rank : 0), 0) /
      cases.length;
    return {
      handshake,
      catalogReady,
      exposedSurface: {
        toolCount: exposedTools.length,
        toolDefinitionBytes,
        instructionBytes,
        alwaysOnBytes,
        estimatedTokens: Math.ceil(alwaysOnBytes / 4),
        names: exposedTools.map((tool) => tool.name).sort(),
      },
      search,
      searchPayload: {
        bytes: stats(searchPayloadBytes),
        estimatedTokens: stats(searchPayloadBytes.map((bytes) => Math.ceil(bytes / 4))),
      },
      retrieval: {
        cases: cases.length,
        hitsAtK: hits.length,
        recallAtK: hits.length / cases.length,
        meanReciprocalRank: reciprocalRank,
        readyAtK: ready.length / cases.length,
        readyResponseTokens: stats(cases.map((item) => item.readyEstimatedTokens)),
        readyRoundTrips: stats(cases.map((item) => item.readyRoundTrips)),
        details: cases,
      },
      routedCall,
      directCall: direct,
      gatewayOverhead: {
        median: routedCall.median - direct.median,
        p95: routedCall.p95 - direct.p95,
      },
      ...(callProfiles.length > 0
        ? {
            callStagesMicroseconds: Object.fromEntries(
              ["preflight", "downstream", "postprocess", "audit", "total"].map(
                (stage) => [
                  stage,
                  stats(callProfiles.map((profile) => profile[`${stage}Us`])),
                ],
              ),
            ),
          }
        : {}),
    };
  } finally {
    client.stop();
  }
}

function writeConfigs(dir, catalogPath) {
  const registryPath = join(dir, "toolport-registry.json");
  writeFileSync(
    registryPath,
    JSON.stringify({
      version: 1,
      servers: [
        {
          id: "fixture",
          name: "Fixture",
          transport: "stdio",
          command: process.execPath,
          args: fixtureArgs(catalogPath),
          env: [],
        },
      ],
      profiles: [
        {
          id: "benchmark",
          name: "Benchmark",
          enabledServerIds: ["fixture"],
        },
      ],
      activeProfileId: "benchmark",
      lazyDiscovery: true,
    }),
  );

  const ratelPath = join(dir, "ratel-config.json");
  writeFileSync(
    ratelPath,
    JSON.stringify({
      mcpServers: {
        fixture: {
          type: "stdio",
          command: process.execPath,
          args: fixtureArgs(catalogPath),
        },
      },
    }),
  );
  return { registryPath, ratelPath };
}

function resolveRatelBinary() {
  if (process.env.RATEL_LOCAL_BIN) return resolve(process.env.RATEL_LOCAL_BIN);
  const vendorRoot = join(
    tmpdir(),
    "toolport-competitive-bench",
    `ratel-local-${CONFIG.ratel.version}`,
  );
  const packageRoot = join(
    vendorRoot,
    "node_modules",
    ...CONFIG.ratel.package.split("/"),
  );
  const binary = join(packageRoot, "dist", "bin.js");
  if (!existsSync(binary) && options.installRatel) {
    mkdirSync(vendorRoot, { recursive: true });
    const bundledNpm = join(
      dirname(process.execPath),
      "node_modules",
      "npm",
      "bin",
      "npm-cli.js",
    );
    const npmCommand = existsSync(bundledNpm) ? process.execPath : "npm";
    const npmArgs = existsSync(bundledNpm) ? [bundledNpm] : [];
    const installed = spawnSync(
      npmCommand,
      [
        ...npmArgs,
        "install",
        "--prefix",
        vendorRoot,
        "--no-audit",
        "--no-fund",
        `${CONFIG.ratel.package}@${CONFIG.ratel.version}`,
      ],
      { encoding: "utf8", windowsHide: true },
    );
    if (installed.status !== 0) {
      fail(
        `could not install Ratel Local:\n` +
          `${installed.error?.message ?? ""}\n${installed.stdout ?? ""}\n${installed.stderr ?? ""}`,
      );
    }
  }
  if (!existsSync(binary)) {
    fail(
      `Ratel Local ${CONFIG.ratel.version} is not prepared.\n` +
        "Run once with --install-ratel, or set RATEL_LOCAL_BIN to dist/bin.js.",
    );
  }
  return binary;
}

function gitRevision() {
  const result = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: ROOT,
    encoding: "utf8",
    windowsHide: true,
  });
  return result.status === 0 ? result.stdout.trim() : null;
}

function gitDirty() {
  const result = spawnSync("git", ["status", "--porcelain", "--untracked-files=no"], {
    cwd: ROOT,
    encoding: "utf8",
    windowsHide: true,
  });
  return result.status === 0 ? result.stdout.trim().length > 0 : null;
}

function fileSha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function toolportBudgetViolations(result) {
  const budget = CONFIG.toolportBudgets;
  if (!budget) return [];
  const violations = [];
  const maximum = (label, actual, limit, catalogSize) => {
    if (typeof actual !== "number" || typeof limit !== "number") {
      violations.push(`${catalogSize} tools: ${label} has no numeric metric or budget`);
      return;
    }
    if (actual > limit) {
      violations.push(
        `${catalogSize} tools: ${label} ${actual.toFixed(2)} exceeded ${limit}`,
      );
    }
  };
  const minimum = (label, actual, limit, catalogSize) => {
    if (typeof actual !== "number" || typeof limit !== "number") {
      violations.push(`${catalogSize} tools: ${label} has no numeric metric or budget`);
      return;
    }
    if (actual < limit) {
      violations.push(
        `${catalogSize} tools: ${label} ${actual.toFixed(3)} fell below ${limit}`,
      );
    }
  };

  for (const run of result.runs) {
    const metrics = run.products.toolport;
    if (!metrics) continue;
    maximum(
      "catalog-ready ms",
      metrics.catalogReady,
      budget.maxCatalogReadyMilliseconds,
      run.catalogSize,
    );
    maximum(
      "always-on estimated tokens",
      metrics.exposedSurface.estimatedTokens,
      budget.maxAlwaysOnEstimatedTokens,
      run.catalogSize,
    );
    maximum(
      "search median ms",
      metrics.search.median,
      budget.maxSearchMedianMilliseconds,
      run.catalogSize,
    );
    maximum(
      "search p95 ms",
      metrics.search.p95,
      budget.maxSearchP95Milliseconds,
      run.catalogSize,
    );
    maximum(
      "search median estimated tokens",
      metrics.searchPayload.estimatedTokens.median,
      budget.maxSearchMedianEstimatedTokens,
      run.catalogSize,
    );
    maximum(
      "search p95 estimated tokens",
      metrics.searchPayload.estimatedTokens.p95,
      budget.maxSearchP95EstimatedTokens,
      run.catalogSize,
    );
    minimum(
      `recall@${result.runtime.topK}`,
      metrics.retrieval.recallAtK,
      budget.minRecallAtK,
      run.catalogSize,
    );
    minimum(
      `schema-ready@${result.runtime.topK}`,
      metrics.retrieval.readyAtK,
      budget.minSchemaReadyAtK,
      run.catalogSize,
    );
    maximum(
      "call overhead p95 ms",
      metrics.gatewayOverhead.p95,
      budget.maxCallOverheadP95Milliseconds,
      run.catalogSize,
    );
  }
  return violations;
}

function markdown(result) {
  const lines = [
    "# Local MCP gateway comparison",
    "",
    `Same generated MCP server, ${result.runtime.iterations} measured iterations per operation.`,
    "Token counts below estimate the always-exposed MCP tool-definition payload as JSON bytes / 4.",
    "",
    `| Catalog | Product | Ready ms | Exposed tools | Always-on est. tokens | Search tokens p50 / p95 | Search p50 / p95 ms | Recall@${result.runtime.topK} | Schema-ready@${result.runtime.topK} | Ready tokens / trips p50 | MRR | Call overhead p50 / p95 ms |`,
    "| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
  ];
  for (const run of result.runs) {
    for (const [product, metrics] of Object.entries(run.products)) {
      lines.push(
        `| ${run.catalogSize} | ${product} | ${metrics.catalogReady.toFixed(2)} | ` +
          `${metrics.exposedSurface.toolCount} | ${metrics.exposedSurface.estimatedTokens} | ` +
          `${metrics.searchPayload.estimatedTokens.median.toFixed(0)} / ${metrics.searchPayload.estimatedTokens.p95.toFixed(0)} | ` +
          `${metrics.search.median.toFixed(2)} / ${metrics.search.p95.toFixed(2)} | ` +
          `${(metrics.retrieval.recallAtK * 100).toFixed(0)}% | ` +
          `${(metrics.retrieval.readyAtK * 100).toFixed(0)}% | ` +
          `${metrics.retrieval.readyResponseTokens.median.toFixed(0)} / ${metrics.retrieval.readyRoundTrips.median.toFixed(0)} | ` +
          `${metrics.retrieval.meanReciprocalRank.toFixed(3)} | ` +
          `${metrics.gatewayOverhead.median.toFixed(2)} / ${metrics.gatewayOverhead.p95.toFixed(2)} |`,
      );
    }
  }
  lines.push(
    "",
    "Interpretation: this measures local gateway mechanics and lexical retrieval over a shared fixture.",
    "It does not claim end-to-end agent accuracy; model-graded task runs remain a separate benchmark.",
  );
  const profiled = result.runs.filter(
    (run) => run.products.toolport?.callStagesMicroseconds,
  );
  if (profiled.length > 0) {
    lines.push(
      "",
      "## Toolport routed-call stages",
      "",
      "Opt-in gateway stage timings in microseconds. Profiling output is emitted after the measured total.",
      "",
      "| Catalog | Stage | p50 / p95 µs |",
      "| ---: | --- | ---: |",
    );
    for (const run of profiled) {
      for (const [stage, values] of Object.entries(
        run.products.toolport.callStagesMicroseconds,
      )) {
        lines.push(
          `| ${run.catalogSize} | ${stage} | ${values.median.toFixed(0)} / ${values.p95.toFixed(0)} |`,
        );
      }
    }
  }
  return `${lines.join("\n")}\n`;
}

async function main() {
  const ratelBin = options.products.includes("ratel") ? resolveRatelBinary() : null;
  const result = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    fairness: {
      sameUpstreamProcess: true,
      sameCatalog: true,
      sameQueries: true,
      sameIterations: true,
      networkRequiredDuringMeasurement: false,
      tokenEstimate: "serialized MCP tools/list JSON bytes divided by four",
    },
    versions: {
      toolportRevision: gitRevision(),
      toolportDirty: gitDirty(),
      toolportBinary: TOOLPORT_GATEWAY,
      toolportBinarySha256: fileSha256(TOOLPORT_GATEWAY),
      toolportBuildProfile: TOOLPORT_GATEWAY === RELEASE_GATEWAY ? "release" : "debug",
      ratelPackage: ratelBin ? `${CONFIG.ratel.package}@${CONFIG.ratel.version}` : null,
    },
    runtime: {
      platform: process.platform,
      arch: process.arch,
      node: process.version,
      iterations: options.iterations,
      settleMilliseconds: options.settleMilliseconds,
      topK: options.topK,
      profileCalls: options.profileCalls,
    },
    runs: [],
  };

  for (const catalogSize of options.sizes) {
    const dir = mkdtempSync(join(tmpdir(), `toolport-local-compare-${catalogSize}-`));
    try {
      const catalogPath = join(dir, "catalog.json");
      const catalog = makeCatalog(catalogSize);
      writeFileSync(catalogPath, JSON.stringify(catalog));
      const { registryPath, ratelPath } = writeConfigs(dir, catalogPath);
      const retrievalCases = SIGNAL_TOOLS.slice(
        0,
        Math.min(catalogSize, SIGNAL_TOOLS.length),
      );
      const expectedToolId = `fixture__${retrievalCases[0].name}`;
      const direct = await benchmarkDirect(catalogPath, retrievalCases[0].name);
      const products = {};
      for (const product of options.products) {
        const adapter =
          product === "toolport"
            ? toolportAdapter(registryPath)
            : ratelAdapter(ratelPath, ratelBin, dir);
        products[product] = await benchmarkProduct(
          adapter,
          direct,
          expectedToolId,
          retrievalCases,
          catalog.tools,
        );
      }
      result.runs.push({ catalogSize, products });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }

  const budgetViolations = options.check ? toolportBudgetViolations(result) : [];
  if (options.check) {
    result.toolportBudgetCheck = {
      passed: budgetViolations.length === 0,
      budgets: CONFIG.toolportBudgets,
      violations: budgetViolations,
    };
  }
  const rendered = options.json
    ? `${JSON.stringify(result, null, 2)}\n`
    : markdown(result);
  if (options.out) {
    const outputPath = resolve(options.out);
    writeFileSync(outputPath, rendered);
    if (!options.json) console.log(`Wrote ${outputPath}`);
  }
  process.stdout.write(rendered);
  if (budgetViolations.length > 0) {
    fail(`Toolport regression budget failed:\n- ${budgetViolations.join("\n- ")}`);
  }
}

main().catch((error) => {
  console.error(error.stack || error.message || error);
  process.exit(1);
});
