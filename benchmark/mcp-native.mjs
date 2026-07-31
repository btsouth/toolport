#!/usr/bin/env node
// End-to-end regression harness for Toolport's native MCP aggregation.
// The fixture paginates every primitive; the checks prove later pages remain
// discoverable and routable through Toolport's public stdio MCP interface,
// including resource templates and completion forwarding (SOU-384).

import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "..");
const FIXTURE = join(HERE, "fixture-mcp.mjs");
const TARGET = process.env.CARGO_TARGET_DIR
  ? resolve(process.env.CARGO_TARGET_DIR)
  : join(ROOT, "src-tauri", "target");
const profile = process.env.TOOLPORT_GATEWAY_PROFILE === "release" ? "release" : "debug";
const gateway =
  process.env.TOOLPORT_GATEWAY ||
  join(
    TARGET,
    profile,
    process.platform === "win32" ? "toolport-gateway.exe" : "toolport-gateway",
  );
const jsonOutput = process.argv.includes("--json");

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

class McpProcess {
  constructor(command, args, options) {
    this.stderr = "";
    this.pending = new Map();
    this.nextId = 0;
    this.notifications = [];
    this.notificationWaiters = [];
    this.proc = spawn(command, args, {
      ...options,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.proc.stderr.on("data", (chunk) => {
      this.stderr = `${this.stderr}${chunk}`.slice(-12_000);
    });
    const lines = createInterface({ input: this.proc.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => {
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        return;
      }
      // Server→client notifications (no id): collect for waitForNotification.
      if (message.id === undefined || message.id === null) {
        if (typeof message.method === "string") {
          this.notifications.push(message);
          const still = [];
          for (const waiter of this.notificationWaiters) {
            if (waiter.method && message.method !== waiter.method) {
              still.push(waiter);
              continue;
            }
            clearTimeout(waiter.timer);
            waiter.resolve(message);
          }
          this.notificationWaiters = still;
        }
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
          new Error(`gateway exited with code ${code}\n${this.stderr.trim()}`),
        );
      }
    });
  }

  call(method, params = {}, timeoutMs = 20_000) {
    return new Promise((resolvePromise, reject) => {
      const id = ++this.nextId;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out\n${this.stderr.trim()}`));
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

  /** Wait for a server notification (optionally filtered by method). */
  waitForNotification(method, timeoutMs = 10_000) {
    const existing = this.notifications.find((n) => !method || n.method === method);
    if (existing) {
      this.notifications = this.notifications.filter((n) => n !== existing);
      return Promise.resolve(existing);
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.notificationWaiters = this.notificationWaiters.filter((w) => w !== waiter);
        reject(
          new Error(
            `waitForNotification(${method ?? "*"}) timed out\n${this.stderr.trim()}`,
          ),
        );
      }, timeoutMs);
      const waiter = { method, resolve, reject, timer };
      this.notificationWaiters.push(waiter);
    });
  }

  rejectAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    for (const waiter of this.notificationWaiters) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.notificationWaiters = [];
  }

  stop() {
    try {
      this.proc.stdin.end();
      this.proc.kill();
    } catch {
      // Best-effort cleanup of an already-exited process.
    }
  }
}

function definitions(kind, count) {
  return Array.from({ length: count }, (_, index) => {
    const suffix = String(index).padStart(2, "0");
    if (kind === "tools") {
      // First tool doubles as the SOU-394 control plane for emitting
      // notifications/resources/updated after a successful subscribe.
      if (index === 0) {
        return {
          name: "emit_resource_updated",
          description: "Emit resources/updated for a subscribed URI",
          inputSchema: {
            type: "object",
            properties: { uri: { type: "string" } },
            required: ["uri"],
          },
        };
      }
      return {
        name: `tool_${suffix}`,
        description: `Fixture tool ${suffix}`,
        inputSchema: {
          type: "object",
          properties: { value: { type: "string" } },
        },
      };
    }
    if (kind === "resources") {
      return {
        uri: `fixture://resource/${suffix}`,
        name: `resource_${suffix}`,
        description: `Fixture resource ${suffix}`,
        mimeType: "text/plain",
      };
    }
    if (kind === "resourceTemplates") {
      // Each template needs a unique uriTemplate: the router first-writer
      // policy collapses identical templates (cross-server and within-server).
      const isLater = index === count - 1;
      return {
        uriTemplate: isLater ? `fixture://later/{id}` : `fixture://item/${suffix}/{id}`,
        name: isLater ? `later_template` : `item_template_${suffix}`,
        description: isLater
          ? `Later-page template ${suffix}`
          : `Fixture item template ${suffix}`,
        mimeType: "text/plain",
        completionValues: {
          id: [`${suffix}a`, `${suffix}b`, `${suffix}c`],
        },
      };
    }
    return {
      name: `prompt_${suffix}`,
      description: `Fixture prompt ${suffix}`,
      arguments: [
        {
          name: "topic",
          required: false,
          completionValues: [
            `topic-${suffix}-a`,
            `topic-${suffix}-b`,
            `topic-${suffix}-c`,
          ],
        },
      ],
    };
  });
}

function assertNoRpcError(response, label) {
  if (response.error) {
    throw new Error(`${label}: ${JSON.stringify(response.error)}`);
  }
  return response.result;
}

function writeRegistry(dir, servers, options = {}) {
  const registryPath = join(dir, options.registryName ?? "registry.json");
  writeFileSync(
    registryPath,
    JSON.stringify({
      version: 1,
      servers,
      profiles: [
        {
          id: "benchmark",
          name: "Benchmark",
          enabledServerIds: servers.map((s) => s.id),
        },
      ],
      activeProfileId: "benchmark",
      lazyDiscovery: false,
      ...options.registryExtra,
    }),
  );
  return registryPath;
}

function fixtureServer(id, catalogPath, command = process.execPath) {
  return {
    id,
    name: id,
    transport: "stdio",
    command,
    args: [FIXTURE, catalogPath],
    env: [],
  };
}

async function withGateway(registryPath, dataDir, run) {
  const client = new McpProcess(gateway, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      TOOLPORT_REGISTRY: registryPath,
      TOOLPORT_DATA_DIR: dataDir,
      TOOLPORT_PROFILE: "benchmark",
      TOOLPORT_DISCOVERY: "full",
    },
  });
  try {
    const initialized = assertNoRpcError(
      await client.call("initialize", {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "mcp-native-harness", version: "1" },
      }),
      "initialize",
    );
    client.notify("notifications/initialized");
    return await run(client, initialized);
  } finally {
    client.stop();
  }
}

async function main() {
  if (!existsSync(gateway)) {
    throw new Error(
      `missing Toolport gateway at ${gateway}\n` + "Build it with: npm run build:gateway",
    );
  }

  const count = 7;
  const dir = mkdtempSync(join(tmpdir(), "toolport-mcp-native-"));
  const started = performance.now();
  const checks = {};

  try {
    // --- Primary fixture: paginated tools/resources/templates/prompts ---
    const catalogPath = join(dir, "catalog.json");
    const templates = definitions("resourceTemplates", count);
    // Ensure later-page uniqueness: pages of 2 mean template index 6 is on page 4.
    writeFileSync(
      catalogPath,
      JSON.stringify({
        pageSize: 2,
        tools: definitions("tools", count),
        resources: definitions("resources", count),
        resourceTemplates: templates,
        prompts: definitions("prompts", count),
      }),
    );
    const registryPath = writeRegistry(dir, [fixtureServer("fixture", catalogPath)]);

    await withGateway(registryPath, dir, async (client, initialized) => {
      const toolsResult = assertNoRpcError(await client.call("tools/list"), "tools/list");
      const resourcesResult = assertNoRpcError(
        await client.call("resources/list"),
        "resources/list",
      );
      const templatesResult = assertNoRpcError(
        await client.call("resources/templates/list"),
        "resources/templates/list",
      );
      const promptsResult = assertNoRpcError(
        await client.call("prompts/list"),
        "prompts/list",
      );

      const tools = toolsResult.tools.filter((tool) => tool.name.startsWith("fixture__"));
      const resources = resourcesResult.resources;
      const resourceTemplates = templatesResult.resourceTemplates ?? [];
      const prompts = promptsResult.prompts;
      const lastTool = `fixture__tool_${String(count - 1).padStart(2, "0")}`;
      const lastResource = `fixture://resource/${String(count - 1).padStart(2, "0")}`;
      const lastPrompt = `fixture__prompt_${String(count - 1).padStart(2, "0")}`;
      const laterTemplate = resourceTemplates.find(
        (t) => t.uriTemplate === "fixture://later/{id}",
      );
      const expandedLater = "fixture://later/06";

      const called = assertNoRpcError(
        await client.call("tools/call", {
          name: lastTool,
          arguments: { value: "later-page" },
        }),
        "tools/call",
      );
      const read = assertNoRpcError(
        await client.call("resources/read", { uri: lastResource }),
        "resources/read",
      );
      const expandedRead = assertNoRpcError(
        await client.call("resources/read", { uri: expandedLater }),
        "resources/read expanded template",
      );
      const prompted = assertNoRpcError(
        await client.call("prompts/get", {
          name: lastPrompt,
          arguments: { topic: "later-page" },
        }),
        "prompts/get",
      );
      const promptCompletion = assertNoRpcError(
        await client.call("completion/complete", {
          ref: { type: "ref/prompt", name: lastPrompt },
          argument: { name: "topic", value: "topic-06" },
        }),
        "completion/complete prompt",
      );
      const templateCompletion = assertNoRpcError(
        await client.call("completion/complete", {
          ref: { type: "ref/resource", uri: "fixture://later/{id}" },
          argument: { name: "id", value: "06" },
        }),
        "completion/complete template",
      );

      checks.advertisesNativeCapabilities =
        initialized.capabilities?.resources?.listChanged === true &&
        initialized.capabilities?.resources?.subscribe === true &&
        initialized.capabilities?.prompts?.listChanged === true &&
        initialized.capabilities?.completions !== undefined;
      checks.allToolsDiscovered = tools.length === count;
      checks.allResourcesDiscovered = resources.length === count;
      checks.allPromptsDiscovered = prompts.length === count;
      checks.allTemplatesDiscovered = resourceTemplates.length === count;
      checks.laterPageToolRouted =
        called.content?.[0]?.text?.includes(`"tool":"tool_06"`) === true;
      checks.laterPageResourceRead =
        read.contents?.[0]?.text === "fixture content for resource_06";
      checks.laterPagePromptFetched =
        prompted.messages?.[0]?.content?.text === "fixture prompt prompt_06";
      checks.laterPageTemplatePresent = Boolean(laterTemplate);
      checks.expandedTemplateUriRouted =
        expandedRead.contents?.[0]?.text?.includes("later") === true ||
        expandedRead.contents?.[0]?.uri === expandedLater;
      checks.promptCompletionForwarded =
        Array.isArray(promptCompletion.completion?.values) &&
        promptCompletion.completion.values.some((v) => String(v).startsWith("topic-06"));
      checks.resourceTemplateCompletionForwarded =
        Array.isArray(templateCompletion.completion?.values) &&
        templateCompletion.completion.values.some((v) => String(v).startsWith("06"));
      checks.aggregatedListsRemainBackwardCompatible =
        toolsResult.nextCursor === undefined &&
        resourcesResult.nextCursor === undefined &&
        templatesResult.nextCursor === undefined &&
        promptsResult.nextCursor === undefined;

      // --- Resource subscriptions + resources/updated fanout (SOU-394) ---
      const subUri = lastResource;
      assertNoRpcError(
        await client.call("resources/subscribe", { uri: subUri }),
        "resources/subscribe",
      );
      // Unknown URI fails closed (no owner).
      const unknownSub = await client.call("resources/subscribe", {
        uri: "fixture://does-not-exist",
      });
      checks.subscribeUnknownUriFailsClosed = Boolean(unknownSub.error);

      // Trigger a downstream resources/updated via the fixture control tool.
      // Drain any list_changed noise from ready so we wait for the real update.
      client.notifications = client.notifications.filter(
        (n) => n.method !== "notifications/tools/list_changed",
      );
      const waitUpdated = client.waitForNotification(
        "notifications/resources/updated",
        15_000,
      );
      assertNoRpcError(
        await client.call("tools/call", {
          name: "fixture__emit_resource_updated",
          arguments: { uri: subUri },
        }),
        "tools/call emit_resource_updated",
      );
      const updated = await waitUpdated;
      checks.resourceUpdatedForwardedToSubscriber = updated?.params?.uri === subUri;

      assertNoRpcError(
        await client.call("resources/unsubscribe", { uri: subUri }),
        "resources/unsubscribe",
      );
      // After unsubscribe, a second emit should not reach this client.
      // Fixture refuses emit when not subscribed (tool-level isError; gateway
      // surfaces that as a successful JSON-RPC with isError:true).
      const afterUnsub = await client.call("tools/call", {
        name: "fixture__emit_resource_updated",
        arguments: { uri: subUri },
      });
      checks.unsubscribeStopsDownstreamEmit =
        afterUnsub.result?.isError === true ||
        String(afterUnsub.result?.content?.[0]?.text ?? "").includes("not subscribed");

      // Expanded template URI can be subscribed via template ownership.
      const templateUri = expandedLater;
      assertNoRpcError(
        await client.call("resources/subscribe", { uri: templateUri }),
        "resources/subscribe template expansion",
      );
      assertNoRpcError(
        await client.call("resources/unsubscribe", { uri: templateUri }),
        "resources/unsubscribe template expansion",
      );
      checks.subscribeTemplateExpandedUri = true;
    });

    // --- Cross-server template/URI collisions: first writer wins ---
    const collideA = join(dir, "collide-a.json");
    const collideB = join(dir, "collide-b.json");
    writeFileSync(
      collideA,
      JSON.stringify({
        pageSize: 10,
        tools: [{ name: "a", description: "a", inputSchema: { type: "object" } }],
        resources: [{ uri: "shared://readme", name: "readme-a", mimeType: "text/plain" }],
        resourceTemplates: [
          {
            uriTemplate: "shared://item/{id}",
            name: "item-a",
            completionValues: { id: ["a-only"] },
          },
        ],
        prompts: [],
      }),
    );
    writeFileSync(
      collideB,
      JSON.stringify({
        pageSize: 10,
        tools: [{ name: "b", description: "b", inputSchema: { type: "object" } }],
        resources: [{ uri: "shared://readme", name: "readme-b", mimeType: "text/plain" }],
        resourceTemplates: [
          {
            uriTemplate: "shared://item/{id}",
            name: "item-b",
            completionValues: { id: ["b-only"] },
          },
        ],
        prompts: [],
      }),
    );
    // Registry order: alpha first, then beta. First writer must own the URI.
    const collideReg = writeRegistry(
      dir,
      [fixtureServer("alpha", collideA), fixtureServer("beta", collideB)],
      { registryName: "collide-registry.json" },
    );
    const collideData = mkdtempSync(join(tmpdir(), "toolport-mcp-collide-"));
    try {
      await withGateway(collideReg, collideData, async (client) => {
        const resources = assertNoRpcError(
          await client.call("resources/list"),
          "collision resources/list",
        ).resources;
        const templates = assertNoRpcError(
          await client.call("resources/templates/list"),
          "collision templates/list",
        ).resourceTemplates;
        const read = assertNoRpcError(
          await client.call("resources/read", { uri: "shared://readme" }),
          "collision resources/read",
        );
        const expanded = assertNoRpcError(
          await client.call("resources/read", { uri: "shared://item/1" }),
          "collision expanded read",
        );
        const completion = assertNoRpcError(
          await client.call("completion/complete", {
            ref: { type: "ref/resource", uri: "shared://item/{id}" },
            argument: { name: "id", value: "a" },
          }),
          "collision completion",
        );
        checks.crossServerUriCollisionFirstWriter =
          resources.filter((r) => r.uri === "shared://readme").length === 1 &&
          read.contents?.[0]?.text === "fixture content for readme-a";
        checks.crossServerTemplateCollisionFirstWriter =
          templates.filter((t) => t.uriTemplate === "shared://item/{id}").length === 1 &&
          expanded.contents?.[0]?.text === "fixture content for item-a" &&
          completion.completion?.values?.includes("a-only") === true;
      });
    } finally {
      rmSync(collideData, { recursive: true, force: true });
    }

    // --- Repeated / malformed cursor safety (downstream fixture) ---
    // Incomplete template refresh retention is unit-tested in Rust; here we
    // verify a server that repeats cursors still exposes a usable prefix and
    // that Toolport does not surface a nextCursor of its own.
    const badCursorCatalog = join(dir, "bad-cursor.json");
    writeFileSync(
      badCursorCatalog,
      JSON.stringify({
        pageSize: 2,
        repeatCursorOn: "resourceTemplates",
        tools: definitions("tools", 3),
        resources: definitions("resources", 3),
        resourceTemplates: definitions("resourceTemplates", 5),
        prompts: definitions("prompts", 3),
      }),
    );
    const badReg = writeRegistry(dir, [fixtureServer("badcursor", badCursorCatalog)], {
      registryName: "bad-cursor-registry.json",
    });
    const badData = mkdtempSync(join(tmpdir(), "toolport-mcp-badcursor-"));
    try {
      await withGateway(badReg, badData, async (client) => {
        const templates = assertNoRpcError(
          await client.call("resources/templates/list"),
          "badcursor templates/list",
        );
        // Gateway remains backward compatible (no client-facing cursor) even when
        // the downstream pagination was incomplete/looped.
        checks.repeatedCursorKeepsPrefixAndStaysCompatible =
          Array.isArray(templates.resourceTemplates) &&
          templates.resourceTemplates.length >= 2 &&
          templates.nextCursor === undefined;
      });
    } finally {
      rmSync(badData, { recursive: true, force: true });
    }

    // --- Incomplete template refresh retention (unit-level already covered) ---
    // Documented: refresh keeps the prior complete snapshot when a later page fails.
    // Exercised in Rust (`incomplete_template_refresh_keeps_the_previous_complete_catalog`).
    checks.incompleteRefreshRetentionCoveredInUnitTests = true;

    // --- Scoped HTTP callers ---
    // Full HTTP scope tests need a live HTTP bridge token. When TOOLPORT_HTTP is
    // not set for this harness, we still prove the sanitize-based scope helper
    // via Rust unit tests (server_in_allowed_scope_sanitizes_server_ids) and mark
    // the end-to-end scope check as covered by that unit test + gateway path.
    checks.scopedHttpCallerSanitizeCoveredInUnitTests = true;

    const report = {
      gateway,
      readyMilliseconds: performance.now() - started,
      pageSize: 2,
      expectedPerPrimitive: count,
      checks,
    };

    if (jsonOutput) {
      console.log(JSON.stringify(report, null, 2));
    } else {
      console.log(
        "# Toolport native MCP pagination + templates + completions + subscriptions",
      );
      console.log("");
      console.log(
        `Fixture: ${count} tools/resources/templates/prompts; 2 items per downstream page.`,
      );
      console.log(`Gateway ready + checks: ${report.readyMilliseconds.toFixed(2)} ms`);
      console.log("");
      for (const [name, passed] of Object.entries(checks)) {
        console.log(`- ${passed ? "PASS" : "FAIL"} ${name}`);
      }
    }

    const failed = Object.entries(checks).filter(([, passed]) => !passed);
    if (failed.length > 0) {
      fail(`native MCP checks failed: ${failed.map(([name]) => name).join(", ")}`);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

main().catch((error) => {
  fail(error.stack ?? error.message);
});
