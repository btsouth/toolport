#!/usr/bin/env node
// A minimal stdio MCP server for the macOS acceptance job. It refuses to start
// unless MOCK_API_TOKEN is set, so a successful tool call proves Toolport
// pulled the secret from the keychain and injected it. check_secret compares
// the token in constant time with the run's expected token and returns only
// "token matches" or "token does not match", never the token or a hash of it.
// The expected token is read from the file named in expected-token-path next
// to this script (the job's secret file, outside the evidence folder), or from
// $MOCK_EXPECTED_TOKEN_FILE.
//
// Every request is appended to $MOCK_MCP_LOG when set.

import { appendFileSync, readFileSync } from "node:fs";
import { timingSafeEqual } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import process from "node:process";

const token = process.env.MOCK_API_TOKEN;
const logFile = process.env.MOCK_MCP_LOG;
const note = (entry) => {
  if (logFile)
    appendFileSync(
      logFile,
      `${JSON.stringify({ at: new Date().toISOString(), ...entry })}\n`,
    );
};

if (!token) {
  note({ event: "refused", reason: "MOCK_API_TOKEN is not set" });
  process.stderr.write("mock-mcp: MOCK_API_TOKEN is not set\n");
  process.exit(1);
}

function expectedTokenFile() {
  if (process.env.MOCK_EXPECTED_TOKEN_FILE) return process.env.MOCK_EXPECTED_TOKEN_FILE;
  const pointer = path.join(
    path.dirname(fileURLToPath(import.meta.url)),
    "expected-token-path",
  );
  try {
    return readFileSync(pointer, "utf8").trim();
  } catch {
    return "";
  }
}

// Returns "token matches", "token does not match", or why it could not compare.
export function compareToken(actual, expectedFile) {
  if (!expectedFile) return "no expected token configured";
  let expected;
  try {
    expected = readFileSync(expectedFile);
  } catch (e) {
    return `cannot read the expected token: ${e.code || e.message}`;
  }
  const got = Buffer.from(actual, "utf8");
  if (expected.length === 0) return "expected token is empty";
  // Lengths are compared first because timingSafeEqual needs equal sizes.
  if (got.length !== expected.length) return "token does not match";
  return timingSafeEqual(got, expected) ? "token matches" : "token does not match";
}
const TOOLS = [
  {
    name: "check_secret",
    description:
      "Report whether the API token this server was started with matches the expected one.",
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
  process.stdout.write(
    `${JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } })}\n`,
  );
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
      return {
        content: [{ type: "text", text: compareToken(token, expectedTokenFile()) }],
      };
    }
    if (params?.name === "echo") {
      return {
        content: [{ type: "text", text: String(params?.arguments?.message ?? "") }],
      };
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
