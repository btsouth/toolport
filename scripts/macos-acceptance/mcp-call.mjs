#!/usr/bin/env node
// Make one real MCP tool call over stdio through a client config entry, the way
// the client would spawn it.
//
// usage: mcp-call.mjs <entry.json> <tool-suffix> [arguments-json]
//   entry.json: {"command": "...", "args": [...], "env": {...}}
// Finds a listed tool whose name is <tool-suffix> or ends with "__<tool-suffix>".
// When only Toolport's search tools are listed (lazy discovery), it searches
// and dispatches through toolport_call_tool instead. Prints a JSON report and
// exits 0 only when the call returned a non-error result and every stdout line
// was JSON-RPC (the MCP stdio transport allows nothing else on stdout).

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import process from "node:process";

const [entryPath, suffix, argsJson = "{}"] = process.argv.slice(2);
if (!entryPath || !suffix) {
  process.stderr.write(
    "usage: mcp-call.mjs <entry.json> <tool-suffix> [arguments-json]\n",
  );
  process.exit(2);
}
const entry = JSON.parse(readFileSync(entryPath, "utf8"));
const toolArgs = JSON.parse(argsJson);
const deadlineMs = Number(process.env.MCP_CALL_TIMEOUT_MS || 60_000);

const child = spawn(entry.command, entry.args || [], {
  env: { ...process.env, ...(entry.env || {}) },
  stdio: ["pipe", "pipe", "pipe"],
});
let stderr = "";
child.stderr.on("data", (d) => {
  stderr = `${stderr}${d}`.slice(-8192);
});

const pending = new Map();
let nextId = 1;
let buffer = "";
const invalidStdout = [];
child.stdout.on("data", (chunk) => {
  buffer += chunk;
  let nl;
  while ((nl = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      msg = undefined;
    }
    if (!msg || typeof msg !== "object" || msg.jsonrpc !== "2.0") {
      invalidStdout.push(line.slice(0, 200));
      continue;
    }
    if (msg.id !== undefined && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  }
});

function request(method, params) {
  const id = nextId++;
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
  return new Promise((resolve, reject) => {
    pending.set(id, (msg) =>
      msg.error
        ? reject(new Error(`${method}: ${msg.error.message}`))
        : resolve(msg.result),
    );
  });
}

const report = { command: entry.command, args: entry.args || [], steps: [] };
const timer = setTimeout(
  () => finish(false, `no answer within ${deadlineMs} ms`),
  deadlineMs,
);
child.on("exit", (code, signal) =>
  finish(false, `gateway exited (${code ?? signal}) before the call finished`),
);
child.on("error", (e) => finish(false, `spawn failed: ${e.message}`));

let done = false;
function finish(ok, detail) {
  if (done) return;
  done = true;
  clearTimeout(timer);
  if (invalidStdout.length) {
    report.invalidStdout = invalidStdout.slice(0, 10);
    if (ok)
      detail = `${detail}, but stdout carried ${invalidStdout.length} non JSON-RPC line(s)`;
    ok = false;
  }
  report.ok = ok;
  report.detail = detail;
  if (!ok) report.stderr = stderr.trim().split("\n").slice(-20);
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  child.kill();
  process.exit(ok ? 0 : 1);
}

function resultText(result) {
  return (result?.content || []).map((c) => c.text || "").join("\n");
}

try {
  const init = await request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "macos-acceptance", version: "1.0.0" },
  });
  report.steps.push({ initialize: init?.serverInfo });
  child.stdin.write(
    `${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`,
  );
  const list = await request("tools/list", {});
  const names = (list?.tools || []).map((t) => t.name);
  report.steps.push({ tools: names.slice(0, 40), toolCount: names.length });
  const direct = names.find((n) => n === suffix || n.endsWith(`__${suffix}`));
  let result;
  if (direct) {
    report.calledVia = direct;
    result = await request("tools/call", { name: direct, arguments: toolArgs });
  } else if (names.includes("toolport_call_tool")) {
    const search = await request("tools/call", {
      name: "toolport_search_tools",
      arguments: { query: suffix.replaceAll("_", " ") },
    });
    const text = resultText(search);
    // Plain token matching, no pattern built from the argument.
    const match = text
      .split(/[^A-Za-z0-9_.-]+/)
      .find((word) => word.endsWith(`__${suffix}`));
    report.steps.push({ search: text.slice(0, 600) });
    const name = match || suffix;
    report.calledVia = `toolport_call_tool(${name})`;
    result = await request("tools/call", {
      name: "toolport_call_tool",
      arguments: { name, arguments: toolArgs },
    });
  } else {
    finish(
      false,
      `no tool named ${suffix} and no toolport_call_tool in ${names.join(", ")}`,
    );
  }
  report.result = resultText(result);
  report.isError = Boolean(result?.isError);
  finish(
    !result?.isError,
    result?.isError ? "tool returned isError" : "tool call returned a result",
  );
} catch (e) {
  finish(false, e.message);
}
