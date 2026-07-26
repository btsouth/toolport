#!/usr/bin/env node
// Deterministic MCP server shared by the local-gateway comparison harness.
// It intentionally has no package dependencies: both products launch this exact
// process and ingest the exact same generated catalog.

import { readFileSync } from "node:fs";
import { createInterface } from "node:readline";

const catalogPath = process.argv[2];
if (!catalogPath) {
  console.error("usage: node fixture-mcp.mjs <catalog.json>");
  process.exit(2);
}

const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
const tools = catalog.tools;
if (!Array.isArray(tools)) {
  console.error("catalog must contain a tools array");
  process.exit(2);
}
const resources = Array.isArray(catalog.resources) ? catalog.resources : [];
const prompts = Array.isArray(catalog.prompts) ? catalog.prompts : [];
const pageSize =
  Number.isInteger(catalog.pageSize) && catalog.pageSize > 0
    ? catalog.pageSize
    : Number.POSITIVE_INFINITY;

const byName = new Map(tools.map((tool) => [tool.name, tool]));
const resourcesByUri = new Map(resources.map((resource) => [resource.uri, resource]));
const promptsByName = new Map(prompts.map((prompt) => [prompt.name, prompt]));
const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });

function write(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function result(id, value) {
  write({ jsonrpc: "2.0", id, result: value });
}

function error(id, code, message) {
  write({ jsonrpc: "2.0", id, error: { code, message } });
}

function page(items, cursor) {
  let offset = 0;
  if (cursor !== undefined) {
    const match = /^fixture:(\d+)$/.exec(cursor);
    if (!match) return null;
    offset = Number(match[1]);
  }
  const selected = items.slice(offset, offset + pageSize);
  const next = offset + selected.length;
  return {
    items: selected,
    ...(next < items.length ? { nextCursor: `fixture:${next}` } : {}),
  };
}

rl.on("line", (line) => {
  if (!line.trim()) return;
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  if (request.id == null) return;

  switch (request.method) {
    case "initialize":
      result(request.id, {
        protocolVersion: "2025-06-18",
        capabilities: {
          tools: {},
          ...(resources.length > 0 ? { resources: {} } : {}),
          ...(prompts.length > 0 ? { prompts: {} } : {}),
        },
        serverInfo: { name: "toolport-benchmark-fixture", version: "1" },
      });
      break;
    case "ping":
      result(request.id, {});
      break;
    case "tools/list": {
      const listed = page(tools, request.params?.cursor);
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      result(request.id, {
        tools: listed.items,
        ...(listed.nextCursor ? { nextCursor: listed.nextCursor } : {}),
      });
      break;
    }
    case "tools/call": {
      const name = request.params?.name;
      if (!byName.has(name)) {
        error(request.id, -32602, `unknown fixture tool: ${name}`);
        break;
      }
      result(request.id, {
        content: [
          {
            type: "text",
            text: JSON.stringify({
              ok: true,
              tool: name,
              arguments: request.params?.arguments ?? {},
            }),
          },
        ],
        isError: false,
      });
      break;
    }
    case "resources/list": {
      const listed = page(resources, request.params?.cursor);
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      result(request.id, {
        resources: listed.items,
        ...(listed.nextCursor ? { nextCursor: listed.nextCursor } : {}),
      });
      break;
    }
    case "resources/read": {
      const resource = resourcesByUri.get(request.params?.uri);
      if (!resource) {
        error(request.id, -32602, `unknown fixture resource: ${request.params?.uri}`);
        break;
      }
      result(request.id, {
        contents: [
          {
            uri: resource.uri,
            mimeType: resource.mimeType ?? "text/plain",
            text: `fixture content for ${resource.name}`,
          },
        ],
      });
      break;
    }
    case "prompts/list": {
      const listed = page(prompts, request.params?.cursor);
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      result(request.id, {
        prompts: listed.items,
        ...(listed.nextCursor ? { nextCursor: listed.nextCursor } : {}),
      });
      break;
    }
    case "prompts/get": {
      const prompt = promptsByName.get(request.params?.name);
      if (!prompt) {
        error(request.id, -32602, `unknown fixture prompt: ${request.params?.name}`);
        break;
      }
      result(request.id, {
        description: prompt.description,
        messages: [
          {
            role: "user",
            content: {
              type: "text",
              text: `fixture prompt ${prompt.name}`,
            },
          },
        ],
      });
      break;
    }
    default:
      error(request.id, -32601, `method not found: ${request.method}`);
  }
});
