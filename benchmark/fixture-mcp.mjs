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
const resourceTemplates = Array.isArray(catalog.resourceTemplates)
  ? catalog.resourceTemplates
  : [];
const prompts = Array.isArray(catalog.prompts) ? catalog.prompts : [];
const pageSize =
  Number.isInteger(catalog.pageSize) && catalog.pageSize > 0
    ? catalog.pageSize
    : Number.POSITIVE_INFINITY;
// Optional failure modes for incomplete-refresh regression coverage.
const failTemplatesAfterPage =
  Number.isInteger(catalog.failTemplatesAfterPage) && catalog.failTemplatesAfterPage >= 0
    ? catalog.failTemplatesAfterPage
    : null;
const repeatCursorOn =
  typeof catalog.repeatCursorOn === "string" ? catalog.repeatCursorOn : null;

const byName = new Map(tools.map((tool) => [tool.name, tool]));
const resourcesByUri = new Map(resources.map((resource) => [resource.uri, resource]));
const promptsByName = new Map(prompts.map((prompt) => [prompt.name, prompt]));
const pageCounters = {
  tools: 0,
  resources: 0,
  resourceTemplates: 0,
  prompts: 0,
};
/** URIs this fixture session has accepted via resources/subscribe (SOU-394). */
const resourceSubscriptions = new Set();
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

function page(items, cursor, counterKey) {
  let offset = 0;
  if (cursor !== undefined) {
    const match = /^fixture:(\d+)$/.exec(cursor);
    if (!match) return null;
    offset = Number(match[1]);
  }
  pageCounters[counterKey] = (pageCounters[counterKey] ?? 0) + 1;
  const pageIndex = pageCounters[counterKey];
  if (
    counterKey === "resourceTemplates" &&
    failTemplatesAfterPage != null &&
    pageIndex > failTemplatesAfterPage
  ) {
    return { fail: "forced incomplete template refresh" };
  }
  const selected = items.slice(offset, offset + pageSize);
  const next = offset + selected.length;
  let nextCursor;
  if (next < items.length) {
    nextCursor = `fixture:${next}`;
    if (repeatCursorOn === counterKey && pageIndex >= 2) {
      // Force a repeated cursor after the second page for regression coverage.
      nextCursor = `fixture:0`;
    }
  }
  return {
    items: selected,
    ...(nextCursor ? { nextCursor } : {}),
  };
}

function expandTemplate(uriTemplate, values) {
  return uriTemplate.replace(/\{([+]?[A-Za-z0-9_]+)\}/g, (_, raw) => {
    const name = raw.startsWith("+") ? raw.slice(1) : raw;
    return values[name] ?? "";
  });
}

function templateMatches(uri, uriTemplate) {
  // Level-1 `{var}` placeholders only (matches Toolport's router matcher).
  let pattern = "^";
  let rest = uriTemplate;
  while (true) {
    const open = rest.indexOf("{");
    if (open < 0) break;
    const literal = rest.slice(0, open);
    pattern += literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const close = rest.indexOf("}", open);
    if (close < 0) return false;
    const expr = rest.slice(open + 1, close);
    pattern += expr.startsWith("+") || expr.startsWith("#") ? ".+" : "[^/]+";
    rest = rest.slice(close + 1);
  }
  pattern += rest.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  pattern += "$";
  return new RegExp(pattern).test(uri);
}

function completeValues(ref, argument, context) {
  const argName = argument?.name ?? "";
  const prefix = String(argument?.value ?? "");
  const ctx = context?.arguments ?? {};
  let pool = [];
  if (ref?.type === "ref/prompt") {
    const prompt = promptsByName.get(ref.name);
    if (!prompt) return null;
    const argDef = (prompt.arguments ?? []).find((a) => a.name === argName);
    pool = Array.isArray(argDef?.completionValues)
      ? argDef.completionValues
      : [
          `${ref.name}-${argName}-alpha`,
          `${ref.name}-${argName}-beta`,
          `${ref.name}-${argName}-gamma`,
        ];
  } else if (ref?.type === "ref/resource") {
    const template = resourceTemplates.find((t) => t.uriTemplate === ref.uri);
    if (!template) return null;
    pool = Array.isArray(template.completionValues?.[argName])
      ? template.completionValues[argName]
      : [`${argName}-01`, `${argName}-02`, `${argName}-03`];
    // Context arguments can refine the pool (multi-arg templates).
    if (ctx.kind && Array.isArray(template.completionValuesByKind?.[ctx.kind])) {
      pool = template.completionValuesByKind[ctx.kind];
    }
  } else {
    return null;
  }
  const values = pool.filter((v) => String(v).startsWith(prefix)).slice(0, 100);
  return {
    completion: {
      values,
      total: values.length,
      hasMore: false,
    },
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
          ...(resources.length > 0 || resourceTemplates.length > 0
            ? { resources: { subscribe: true } }
            : {}),
          ...(prompts.length > 0 ? { prompts: {} } : {}),
          ...(prompts.length > 0 || resourceTemplates.length > 0
            ? { completions: {} }
            : {}),
        },
        serverInfo: { name: "toolport-benchmark-fixture", version: "1" },
      });
      break;
    case "ping":
      result(request.id, {});
      break;
    case "tools/list": {
      const listed = page(tools, request.params?.cursor, "tools");
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      if (listed.fail) {
        error(request.id, -32603, listed.fail);
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
      // Control plane for SOU-394: emit resources/updated for a subscribed URI
      // so the gateway fanout path can be exercised end-to-end.
      if (name === "emit_resource_updated") {
        const uri = request.params?.arguments?.uri;
        if (typeof uri !== "string" || !uri) {
          error(request.id, -32602, "emit_resource_updated requires arguments.uri");
          break;
        }
        if (!resourceSubscriptions.has(uri)) {
          error(request.id, -32602, `emit_resource_updated: uri not subscribed: ${uri}`);
          break;
        }
        write({
          jsonrpc: "2.0",
          method: "notifications/resources/updated",
          params: { uri },
        });
        result(request.id, {
          content: [{ type: "text", text: JSON.stringify({ ok: true, uri }) }],
          isError: false,
        });
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
      const listed = page(resources, request.params?.cursor, "resources");
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      if (listed.fail) {
        error(request.id, -32603, listed.fail);
        break;
      }
      result(request.id, {
        resources: listed.items,
        ...(listed.nextCursor ? { nextCursor: listed.nextCursor } : {}),
      });
      break;
    }
    case "resources/templates/list": {
      const listed = page(resourceTemplates, request.params?.cursor, "resourceTemplates");
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      if (listed.fail) {
        error(request.id, -32603, listed.fail);
        break;
      }
      // Strip fixture-only completion metadata from the wire response.
      const items = listed.items.map((item) => {
        const copy = { ...item };
        delete copy.completionValues;
        delete copy.completionValuesByKind;
        return copy;
      });
      result(request.id, {
        resourceTemplates: items,
        ...(listed.nextCursor ? { nextCursor: listed.nextCursor } : {}),
      });
      break;
    }
    case "resources/read": {
      const uri = request.params?.uri;
      let resource = resourcesByUri.get(uri);
      if (!resource) {
        // Expanded template URI: synthesize content when a template matches.
        const template = resourceTemplates.find((t) =>
          templateMatches(uri, t.uriTemplate),
        );
        if (template) {
          resource = {
            uri,
            name: template.name,
            mimeType: template.mimeType ?? "text/plain",
          };
        }
      }
      if (!resource) {
        error(request.id, -32602, `unknown fixture resource: ${uri}`);
        break;
      }
      result(request.id, {
        contents: [
          {
            uri: resource.uri,
            mimeType: resource.mimeType ?? "text/plain",
            text: `fixture content for ${resource.name ?? uri}`,
          },
        ],
      });
      break;
    }
    case "resources/subscribe": {
      const uri = request.params?.uri;
      if (!uri || typeof uri !== "string") {
        error(request.id, -32602, "resources/subscribe requires params.uri");
        break;
      }
      let known = resourcesByUri.has(uri);
      if (!known) {
        known = resourceTemplates.some((t) => templateMatches(uri, t.uriTemplate));
      }
      if (!known) {
        error(request.id, -32602, `unknown fixture resource: ${uri}`);
        break;
      }
      resourceSubscriptions.add(uri);
      result(request.id, {});
      break;
    }
    case "resources/unsubscribe": {
      const uri = request.params?.uri;
      if (!uri || typeof uri !== "string") {
        error(request.id, -32602, "resources/unsubscribe requires params.uri");
        break;
      }
      resourceSubscriptions.delete(uri);
      // Idempotent success even when the URI was not subscribed.
      result(request.id, {});
      break;
    }
    case "prompts/list": {
      const listed = page(prompts, request.params?.cursor, "prompts");
      if (!listed) {
        error(request.id, -32602, "invalid fixture cursor");
        break;
      }
      if (listed.fail) {
        error(request.id, -32603, listed.fail);
        break;
      }
      // completionValues lives on arguments; strip those extras for list.
      const items = listed.items.map((prompt) => {
        const copy = { ...prompt };
        if (!Array.isArray(copy.arguments)) return copy;
        copy.arguments = copy.arguments.map((arg) => {
          const argCopy = { ...arg };
          delete argCopy.completionValues;
          return argCopy;
        });
        return copy;
      });
      result(request.id, {
        prompts: items,
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
    case "completion/complete": {
      const completed = completeValues(
        request.params?.ref,
        request.params?.argument,
        request.params?.context,
      );
      if (!completed) {
        error(request.id, -32602, "unknown completion reference");
        break;
      }
      result(request.id, completed);
      break;
    }
    default:
      error(request.id, -32601, `method not found: ${request.method}`);
  }
});

// Silence unused helper warning for expandTemplate (kept for fixture clarity).
void expandTemplate;
