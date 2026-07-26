#!/usr/bin/env node
// End-to-end regression harness for Toolport's native MCP aggregation.
// The fixture paginates every primitive; the checks prove later pages remain
// discoverable and routable through Toolport's public stdio MCP interface.

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
      // Best-effort cleanup of an already-exited process.
    }
  }
}

function definitions(kind, count) {
  return Array.from({ length: count }, (_, index) => {
    const suffix = String(index).padStart(2, "0");
    if (kind === "tools") {
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
    return {
      name: `prompt_${suffix}`,
      description: `Fixture prompt ${suffix}`,
      arguments: [{ name: "topic", required: false }],
    };
  });
}

function assertNoRpcError(response, label) {
  if (response.error) {
    throw new Error(`${label}: ${JSON.stringify(response.error)}`);
  }
  return response.result;
}

async function main() {
  if (!existsSync(gateway)) {
    throw new Error(
      `missing Toolport gateway at ${gateway}\n` + "Build it with: npm run build:gateway",
    );
  }

  const count = 7;
  const dir = mkdtempSync(join(tmpdir(), "toolport-mcp-native-"));
  const catalogPath = join(dir, "catalog.json");
  const registryPath = join(dir, "registry.json");
  writeFileSync(
    catalogPath,
    JSON.stringify({
      pageSize: 2,
      tools: definitions("tools", count),
      resources: definitions("resources", count),
      prompts: definitions("prompts", count),
    }),
  );
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
          args: [FIXTURE, catalogPath],
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
      lazyDiscovery: false,
    }),
  );

  const started = performance.now();
  const client = new McpProcess(gateway, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      TOOLPORT_REGISTRY: registryPath,
      TOOLPORT_DATA_DIR: dir,
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

    const toolsResult = assertNoRpcError(await client.call("tools/list"), "tools/list");
    const resourcesResult = assertNoRpcError(
      await client.call("resources/list"),
      "resources/list",
    );
    const promptsResult = assertNoRpcError(
      await client.call("prompts/list"),
      "prompts/list",
    );
    const tools = toolsResult.tools.filter((tool) => tool.name.startsWith("fixture__"));
    const resources = resourcesResult.resources;
    const prompts = promptsResult.prompts;
    const lastTool = `fixture__tool_${String(count - 1).padStart(2, "0")}`;
    const lastResource = `fixture://resource/${String(count - 1).padStart(2, "0")}`;
    const lastPrompt = `fixture__prompt_${String(count - 1).padStart(2, "0")}`;

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
    const prompted = assertNoRpcError(
      await client.call("prompts/get", {
        name: lastPrompt,
        arguments: { topic: "later-page" },
      }),
      "prompts/get",
    );

    const checks = {
      advertisesNativeCapabilities:
        initialized.capabilities?.resources?.listChanged === true &&
        initialized.capabilities?.prompts?.listChanged === true,
      allToolsDiscovered: tools.length === count,
      allResourcesDiscovered: resources.length === count,
      allPromptsDiscovered: prompts.length === count,
      laterPageToolRouted:
        called.content?.[0]?.text?.includes(`"tool":"tool_06"`) === true,
      laterPageResourceRead:
        read.contents?.[0]?.text === "fixture content for resource_06",
      laterPagePromptFetched:
        prompted.messages?.[0]?.content?.text === "fixture prompt prompt_06",
      aggregatedListsRemainBackwardCompatible:
        toolsResult.nextCursor === undefined &&
        resourcesResult.nextCursor === undefined &&
        promptsResult.nextCursor === undefined,
    };
    const report = {
      gateway,
      readyMilliseconds: performance.now() - started,
      pageSize: 2,
      expectedPerPrimitive: count,
      discovered: {
        tools: tools.length,
        resources: resources.length,
        prompts: prompts.length,
      },
      checks,
    };

    if (jsonOutput) {
      console.log(JSON.stringify(report, null, 2));
    } else {
      console.log("# Toolport native MCP pagination");
      console.log("");
      console.log(
        `Fixture: ${count} tools, ${count} resources, ${count} prompts; 2 items per downstream page.`,
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
    client.stop();
    rmSync(dir, { recursive: true, force: true });
  }
}

main().catch((error) => {
  fail(error.stack ?? error.message);
});
