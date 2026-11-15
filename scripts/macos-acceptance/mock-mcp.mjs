#!/usr/bin/env node
// A minimal stdio MCP server for the macOS acceptance job. It refuses to start
// unless MOCK_API_TOKEN is set, so a successful tool call proves Toolport
// pulled the secret from the keychain and injected it. The tool returns a
// SHA-256 of the token (never the token) so the job can compare it.
//
// Every request is appended to $MOCK_MCP_LOG when set.

import { appendFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";
import process from "node:process";

const token = process.env.MOCK_API_TOKEN;
const logFile = process.env.MOCK_MCP_LOG;
const note = (entry) => {
  if (logFile) appendFileSync(logFile, `${JSON.stringify({ at: new Date().toISOString(), ...entry })}\n`);
};

if (!token) {
  note({ event: "refused", reason: "MOCK_API_TOKEN is not set" });
  process.stderr.write("mock-mcp: MOCK_API_TOKEN is not set\n");
  process.exit(1);
}

const digest = createHash("sha256").update(token).digest("hex");
const TOOLS = [
  {
    name: "check_secret",
    description: "Report the SHA-256 of the API token this server was started with.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
  {
    name: "echo",
    description: "Echo a message back.",
    inputSchema: {
      type: "object",
      properties: { message: { type: "string" } },
      required: ["message"],
    },
  },
];

function reply(id, result) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
}

function error(id, code, message) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } })}\n`);
}

export function handle(msg) {
  const { id, method, params } = msg;
  note({ event: "request", method, tool: params?.name });
  if (method === "initialize") {
    return {
      protocolVersion: params?.protocolVersion || "2025-06-18",
      capabilities: { tools: {} },
      serverInfo: { name: "mock-acceptance", version: "1.0.0" },
    };
  }
  if (method === "tools/list") return { tools: TOOLS };
  if (method === "tools/call") {
    if (params?.name === "check_secret") {
      return { content: [{ type: "text", text: `sha256:${digest}` }] };
    }
    if (params?.name === "echo") {
      return { content: [{ type: "text", text: String(params?.arguments?.message ?? "") }] };
    }
    throw Object.assign(new Error(`unknown tool ${params?.name}`), { code: -32602 });
  }
  if (method === "ping") return {};
  if (id === undefined) return undefined;
  throw Object.assign(new Error(`method not found: ${method}`), { code: -32601 });
}

note({ event: "started", pid: process.pid });
createInterface({ input: process.stdin }).on("line", (line) => {
  if (!line.trim()) return;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return error(null, -32700, "parse error");
  }
  if (msg.id === undefined) {
    handle(msg);
    return;
  }
  try {
    reply(msg.id, handle(msg));
  } catch (e) {
    error(msg.id, e.code || -32603, e.message);
  }
});
