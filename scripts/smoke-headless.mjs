#!/usr/bin/env node

// Authoritative cross-platform headless gateway security smoke suite. CI runs this
// file on Linux/macOS and through scripts/smoke-headless.ps1 on Windows.

import { access, mkdtemp, rm, writeFile } from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createHmac } from "node:crypto";
import { isDeepStrictEqual } from "node:util";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const defaultBinary = path.join(
  repoRoot,
  "src-tauri",
  "target",
  "debug",
  process.platform === "win32" ? "toolport-gateway.exe" : "toolport-gateway",
);
const gatewayBinary = process.env.TOOLPORT_GATEWAY_BIN || defaultBinary;
const mockBinary = path.join(
  repoRoot,
  "src-tauri",
  "target",
  "debug",
  process.platform === "win32" ? "mock-mcp-server.exe" : "mock-mcp-server",
);
const token = "smoke-test-token-32chars-minimum!!";
const children = new Set();
let smokeDir;
let failures = 0;
let cleanupPromise;
let shuttingDown = false;

function pass(message) {
  console.log(`[PASS] ${message}`);
}

function fail(message) {
  console.error(`[FAIL] ${message}`);
  failures += 1;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function boundedStderr(child) {
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-16_384);
  });
  return () => stderr.trim();
}

function smokeEnvironment({ host, authToken }) {
  const env = { ...process.env };
  for (const name of [
    "CONDUIT_DATA_DIR",
    "CONDUIT_HTTP_HOST",
    "CONDUIT_HTTP_TOKEN",
    "CONDUIT_REGISTRY",
    "TOOLPORT_HTTP_HOST",
    "TOOLPORT_HTTP_TOKEN",
  ]) {
    delete env[name];
  }
  env.TOOLPORT_DATA_DIR = smokeDir;
  env.TOOLPORT_REGISTRY = path.join(smokeDir, "registry.json");
  env.TOOLPORT_HTTP_HOST = host;
  if (authToken) env.TOOLPORT_HTTP_TOKEN = authToken;
  return env;
}

function startGateway({ port, host, authToken, insecureLoopback = false }) {
  if (shuttingDown) throw new Error("cannot start gateway during shutdown");
  const args = ["--http", String(port)];
  if (insecureLoopback) args.push("--insecure-loopback");
  const child = spawn(gatewayBinary, args, {
    cwd: repoRoot,
    env: smokeEnvironment({ host, authToken }),
    stdio: ["ignore", "ignore", "pipe"],
  });
  children.add(child);
  child.once("exit", () => children.delete(child));
  return { child, stderr: boundedStderr(child) };
}

/**
 * Wait, bounded, for a child's stderr to close.
 *
 * `waitForExit` resolves on `exit`, which Node fires as soon as the process ends - while the
 * piped stderr can still hold bytes that have not been delivered. Anything reading the captured
 * stderr at that moment can therefore see a truncated message.
 *
 * That is not theoretical. The Windows runner once reported `refusing to bind 0.0.0.0` without
 * the `without HTTP authentication` tail that `expectBindRefusal` matches on, failing a run in
 * which the gateway had done exactly the right thing. The same assertion passed on the next
 * host, which is the signature of a race rather than a defect.
 *
 * Resolving on `end`/`close` means the capture is complete before it is read. The timeout keeps
 * a stream that never closes from wedging a security smoke test.
 */
function drainStderr(child, timeoutMs = 2_000) {
  const stream = child.stderr;
  if (!stream || stream.readableEnded || stream.destroyed) return Promise.resolve();
  return new Promise((resolve) => {
    function finish() {
      clearTimeout(timer);
      stream.off("end", finish);
      stream.off("close", finish);
      resolve();
    }
    const timer = setTimeout(finish, timeoutMs);
    stream.once("end", finish);
    stream.once("close", finish);
  });
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolve(null);
    }, timeoutMs);
    const onExit = (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    };
    child.once("exit", onExit);
  });
}

async function stopGateway(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  if (await waitForExit(child, 2_000)) return;
  child.kill("SIGKILL");
  await waitForExit(child, 2_000);
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  shuttingDown = true;
  cleanupPromise = (async () => {
    await Promise.all([...children].map(stopGateway));
    if (smokeDir) await rm(smokeDir, { recursive: true, force: true });
  })();
  return cleanupPromise;
}

function installSignalHandler(signal, exitCode) {
  process.once(signal, () => {
    cleanup()
      .catch((error) => console.error(`[FAIL] cleanup after ${signal}: ${error.message}`))
      .finally(() => process.exit(exitCode));
  });
}

installSignalHandler("SIGINT", 130);
installSignalHandler("SIGTERM", 143);

async function availablePort(host) {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, host, resolve);
  });
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : null;
  await new Promise((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
  if (!port) throw new Error(`could not allocate a smoke-test port on ${host}`);
  return port;
}

function request({ port, method = "GET", route = "/", headers = {}, body }) {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? null : Buffer.from(body);
    const requestHeaders = { Connection: "close", ...headers };
    if (payload && requestHeaders["Content-Length"] === undefined) {
      requestHeaders["Content-Length"] = String(payload.length);
    }
    const req = http.request(
      {
        host: "127.0.0.1",
        port,
        method,
        path: route,
        headers: requestHeaders,
        agent: false,
        timeout: 2_000,
      },
      (response) => {
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () =>
          resolve({
            status: response.statusCode,
            headers: response.headers,
            body: Buffer.concat(chunks).toString("utf8"),
          }),
        );
      },
    );
    req.once("timeout", () => req.destroy(new Error("request timed out")));
    req.once("error", reject);
    if (payload) req.write(payload);
    req.end();
  });
}

async function mcpRequest({ port, id, method, params, sessionId }) {
  const headers = {
    Authorization: `Bearer ${token}`,
    "Content-Type": "application/json",
    Accept: "application/json",
  };
  if (sessionId) headers["Mcp-Session-Id"] = sessionId;
  const response = await request({
    port,
    method: "POST",
    route: "/mcp",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id, method, ...(params ? { params } : {}) }),
  });
  if (response.status !== 200) {
    throw new Error(`${method} returned HTTP ${response.status}: ${response.body}`);
  }
  let json;
  try {
    json = JSON.parse(response.body);
  } catch (error) {
    throw new Error(`${method} returned invalid JSON: ${error.message}`, {
      cause: error,
    });
  }
  return { response, json };
}

async function initializeMcp(port) {
  const { response, json } = await mcpRequest({
    port,
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "smoke", version: "0" },
    },
  });
  const sessionId = response.headers["mcp-session-id"];
  if (typeof sessionId !== "string" || !sessionId) {
    throw new Error(`initialize did not return Mcp-Session-Id: ${JSON.stringify(json)}`);
  }
  return sessionId;
}

async function startApprovalBroker() {
  const approvalToken = "routine-smoke-approval-token";
  let resolveRequest;
  let rejectRequest;
  // Bounded: if the gateway never dials the broker (an approval-wiring
  // regression), fail the assertion instead of hanging until the CI job timeout.
  const requestReceived = new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("approval broker received no request within 15s")),
      15_000,
    );
    timer.unref?.();
    resolveRequest = (value) => {
      clearTimeout(timer);
      resolve(value);
    };
    rejectRequest = (error) => {
      clearTimeout(timer);
      reject(error);
    };
  });
  const server = net.createServer((socket) => {
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      buffer += chunk;
      // Line-oriented: the gateway opens with a challenge it expects a proof for
      // before it will send the request (SBS-867), then sends the request itself.
      for (;;) {
        const newline = buffer.indexOf("\n");
        if (newline < 0) return;
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        try {
          const message = JSON.parse(line);
          const nonce = message?.toolportApprovalChallenge;
          if (typeof nonce === "string") {
            const proof = createHmac("sha256", approvalToken).update(nonce).digest("hex");
            socket.write(`${JSON.stringify({ toolportApprovalProof: proof })}\n`);
            continue;
          }
          resolveRequest(message);
          socket.end(`${JSON.stringify("approved")}\n`);
          return;
        } catch (error) {
          rejectRequest(error);
          socket.destroy();
          return;
        }
      }
    });
    socket.once("error", rejectRequest);
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address !== "object") {
    server.close();
    throw new Error("approval broker did not expose a TCP address");
  }
  return {
    server,
    endpoint: `127.0.0.1:${address.port}`,
    token: approvalToken,
    requestReceived,
  };
}

async function waitForStatus(port, expected, child, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let last = "no response";
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      return {
        response: null,
        last: `gateway exited (${child.exitCode ?? child.signalCode})`,
      };
    }
    try {
      const response = await request({ port });
      last = `HTTP ${response.status}`;
      if (response.status === expected) return { response, last };
    } catch (error) {
      last = error.message;
    }
    await delay(100);
  }
  return { response: null, last };
}

async function expectBindRefusal({ name, host }) {
  const port = await availablePort(host);
  const { child, stderr } = startGateway({ port, host });
  try {
    const exited = await waitForExit(child, 10_000);
    // Read the capture only once stderr has closed: `exit` alone does not guarantee the whole
    // refusal message has arrived, and a half-delivered one fails an assertion the gateway passed.
    await drainStderr(child);
    const output = stderr();
    if (
      exited &&
      exited.code !== 0 &&
      /refusing to bind.*without HTTP authentication/s.test(output)
    ) {
      pass(`${name} refuses start (exit ${exited.code})`);
    } else if (!exited) {
      fail(`${name} should refuse start but kept running`);
    } else {
      fail(
        `${name} exited ${exited.code ?? exited.signal}; unexpected stderr: ${output}`,
      );
    }
  } finally {
    await stopGateway(child);
  }
}

async function testInsecureLoopback() {
  const port = await availablePort("127.0.0.1");
  const { child, stderr } = startGateway({
    port,
    host: "127.0.0.1",
    insecureLoopback: true,
  });
  try {
    const { response, last } = await waitForStatus(port, 200, child);
    if (response) pass("explicit insecure loopback starts locally");
    else fail(`explicit insecure loopback did not return 200: ${last}; ${stderr()}`);
  } finally {
    await stopGateway(child);
  }
}

async function testAuthenticatedGateway() {
  const port = await availablePort("127.0.0.1");
  const { child, stderr } = startGateway({
    port,
    host: "127.0.0.1",
    authToken: token,
  });
  try {
    const unauthenticated = await waitForStatus(port, 401, child);
    if (unauthenticated.response) pass("GET / without auth returns 401");
    else
      fail(`GET / without auth did not return 401: ${unauthenticated.last}; ${stderr()}`);

    let incorrectToken;
    try {
      incorrectToken = await request({
        port,
        headers: { Authorization: "Bearer incorrect-smoke-token" },
      });
    } catch (error) {
      fail(`GET / with incorrect bearer failed: ${error.message}`);
    }
    if (incorrectToken?.status === 401) {
      pass("GET / with incorrect bearer returns 401");
    } else if (incorrectToken) {
      fail(`GET / with incorrect bearer returned ${incorrectToken.status}`);
    }

    let authenticated;
    try {
      authenticated = await request({
        port,
        headers: { Authorization: `Bearer ${token}` },
      });
    } catch (error) {
      fail(`GET / with bearer failed: ${error.message}`);
    }
    if (authenticated?.status === 200) pass("GET / with bearer returns 200");
    else if (authenticated) fail(`GET / with bearer returned ${authenticated.status}`);

    await testMcpHandshake(port);
  } finally {
    await stopGateway(child);
  }
}

async function testMcpHandshake(port) {
  let sessionId;
  try {
    sessionId = await initializeMcp(port);
  } catch (error) {
    fail(`MCP initialize request failed: ${error.message}`);
    return;
  }
  pass("MCP initialize returns 200 + Mcp-Session-Id");

  let listed;
  try {
    listed = await mcpRequest({
      port,
      id: 2,
      method: "tools/list",
      sessionId,
    });
  } catch (error) {
    fail(`MCP tools/list request failed: ${error.message}`);
    return;
  }

  const tools = listed.json?.result?.tools;
  if (Array.isArray(tools) && tools.length >= 4) {
    pass(`MCP tools/list returns ${tools.length} tools`);
  } else {
    fail(`MCP tools/list returned ${tools?.length ?? 0} tools`);
  }
}

async function testRoutinePersistsAcrossGatewayRestart() {
  const routineSource =
    "const echoed = toolport.call('mock__echo', { text: input.value }); return { value: echoed.content[0].text, frozen: Object.isFrozen(input) };";
  const routineInputSchema = {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    type: "object",
    properties: { value: { type: "string" } },
    required: ["value"],
    additionalProperties: false,
  };
  await writeFile(
    path.join(smokeDir, "registry.json"),
    `${JSON.stringify(
      {
        version: 1,
        codeMode: true,
        allowRoutineWrites: true,
        humanApproval: false,
        servers: [
          {
            id: "mock",
            name: "Mock",
            transport: "stdio",
            command: mockBinary,
            args: [],
            env: [],
            source: "manual",
          },
        ],
        profiles: [{ id: "default", name: "Default", enabledServerIds: ["mock"] }],
        activeProfileId: "default",
      },
      null,
      2,
    )}\n`,
  );

  const broker = await startApprovalBroker();
  await writeFile(
    path.join(smokeDir, "approval-endpoint.json"),
    `${JSON.stringify({ endpoint: broker.endpoint, token: broker.token })}\n`,
  );

  let routineId;
  let routineToolName;
  const firstPort = await availablePort("127.0.0.1");
  const first = startGateway({ port: firstPort, host: "127.0.0.1", authToken: token });
  try {
    const ready = await waitForStatus(firstPort, 401, first.child);
    if (!ready.response)
      throw new Error(`first gateway did not start: ${ready.last}; ${first.stderr()}`);
    const sessionId = await initializeMcp(firstPort);
    const listed = await mcpRequest({
      port: firstPort,
      id: 2,
      method: "tools/list",
      sessionId,
    });
    const names = listed.json.result.tools.map((tool) => tool.name);
    for (const name of [
      "toolport_save_routine",
      "toolport_list_routines",
      "toolport_run_routine",
    ]) {
      if (!names.includes(name)) throw new Error(`${name} was not advertised`);
    }

    const codeRun = await mcpRequest({
      port: firstPort,
      id: 3,
      method: "tools/call",
      sessionId,
      params: {
        name: "toolport_run_script",
        arguments: {
          script: routineSource,
          input: { value: "before-restart" },
          inputSchema: routineInputSchema,
        },
      },
    });
    const codeRunMeta = codeRun.json?.result?.structuredContent?.toolportScript;
    const runId = codeRunMeta?.runId;
    if (
      codeRun.json?.result?.isError ||
      !/^run_[0-9a-f]{32}$/.test(runId ?? "") ||
      codeRunMeta?.inputMode !== "immutable" ||
      codeRunMeta?.routineCandidate?.eligible !== true ||
      codeRunMeta?.routineCandidate?.promotionAvailable !== true
    ) {
      throw new Error(
        `immutable Code Run did not yield a Candidate: ${JSON.stringify(codeRun.json)}`,
      );
    }

    const saved = await mcpRequest({
      port: firstPort,
      id: 4,
      method: "tools/call",
      sessionId,
      params: {
        name: "toolport_save_routine",
        arguments: {
          runId,
          name: "restart-smoke",
          description: "real-process restart fixture",
        },
      },
    });
    const approval = await broker.requestReceived;
    routineId = saved.json?.result?.structuredContent?.routine?.id;
    routineToolName = saved.json?.result?.structuredContent?.advertisedAs;
    if (
      !routineId ||
      !/^toolport_routine_[A-Za-z0-9_]+$/.test(routineToolName ?? "") ||
      routineToolName.length > 64 ||
      routineToolName.includes("__") ||
      saved.json?.result?.isError
    ) {
      throw new Error(`save_routine failed: ${JSON.stringify(saved.json)}`);
    }
    const savedHash = saved.json?.result?.structuredContent?.routine?.contentHash;
    const approvalLimits = approval.arguments?.limits;
    const limitsAreBound =
      approvalLimits &&
      [
        "maxCalls",
        "wallClockMs",
        "maxParallel",
        "maxPromiseJobs",
        "loopIterationLimit",
        "recursionLimit",
      ].every(
        (field) => Number.isInteger(approvalLimits[field]) && approvalLimits[field] > 0,
      );
    const approvalIsContentBound =
      approval.token === broker.token &&
      approval.reason === "persistent_code_write" &&
      approval.server === "toolport" &&
      approval.tool === "save_routine" &&
      approval.arguments?.name === "restart-smoke" &&
      approval.arguments?.description === "real-process restart fixture" &&
      approval.arguments?.runId === runId &&
      approval.arguments?.source === routineSource &&
      isDeepStrictEqual(approval.arguments?.inputSchema, routineInputSchema) &&
      approval.arguments?.contentHash === savedHash &&
      /^sha256:[0-9a-f]{64}$/.test(savedHash ?? "") &&
      /^sha256:[0-9a-f]{64}$/.test(approval.arguments?.definitionFingerprint ?? "") &&
      approval.arguments?.evidence?.sourceRunId === runId &&
      approval.arguments?.evidence?.calls === 1 &&
      approval.arguments?.evidence?.observedDependencies?.[0]?.name === "mock__echo" &&
      limitsAreBound &&
      !("validationArguments" in approval.arguments) &&
      !("toolFingerprint" in approval);
    if (!approvalIsContentBound) {
      throw new Error(
        `approval was not bound to the exact definition: ${JSON.stringify(approval)}`,
      );
    }
    pass("real immutable Code Run promotes by runId with exact-definition approval");
  } catch (error) {
    fail(`Routine save before restart failed: ${error.message}`);
  } finally {
    await stopGateway(first.child);
    await new Promise((resolve) => broker.server.close(resolve));
  }

  if (!routineId || !routineToolName) return;
  const secondPort = await availablePort("127.0.0.1");
  const second = startGateway({ port: secondPort, host: "127.0.0.1", authToken: token });
  try {
    const ready = await waitForStatus(secondPort, 401, second.child);
    if (!ready.response)
      throw new Error(
        `restarted gateway did not start: ${ready.last}; ${second.stderr()}`,
      );
    const sessionId = await initializeMcp(secondPort);
    const toolList = await mcpRequest({
      port: secondPort,
      id: 2,
      method: "tools/list",
      sessionId,
    });
    const virtualTool = toolList.json?.result?.tools?.find(
      (tool) => tool.name === routineToolName,
    );
    if (
      !virtualTool ||
      !isDeepStrictEqual(virtualTool.inputSchema, routineInputSchema) ||
      !virtualTool.description?.includes("real-process restart fixture")
    ) {
      throw new Error(
        `saved routine was not advertised as a first-class tool: ${JSON.stringify(toolList.json)}`,
      );
    }

    const listed = await mcpRequest({
      port: secondPort,
      id: 3,
      method: "tools/call",
      sessionId,
      params: {
        name: "toolport_list_routines",
        arguments: { query: "restart echo", limit: 8 },
      },
    });
    const routines = listed.json?.result?.structuredContent?.routines;
    if (
      !Array.isArray(routines) ||
      !routines.some((routine) => routine.id === routineId)
    ) {
      throw new Error(
        `saved routine missing after restart: ${JSON.stringify(listed.json)}`,
      );
    }
    const run = await mcpRequest({
      port: secondPort,
      id: 4,
      method: "tools/call",
      sessionId,
      params: {
        name: routineToolName,
        arguments: { value: "after-restart" },
      },
    });
    const result = run.json?.result?.structuredContent?.result;
    if (
      run.json?.result?.isError ||
      result?.value !== "after-restart" ||
      result?.frozen !== true
    ) {
      throw new Error(
        `virtual Routine tool failed after restart: ${JSON.stringify(run.json)}`,
      );
    }
    pass(
      "Routine is advertised and runs as a first-class MCP tool after Gateway restart",
    );
  } catch (error) {
    fail(`Routine restart verification failed: ${error.message}`);
  } finally {
    await stopGateway(second.child);
  }
}

async function main() {
  try {
    await access(gatewayBinary);
  } catch {
    throw new Error(
      `gateway binary not found at ${gatewayBinary}; run npm run build:gateway first`,
    );
  }
  try {
    await access(mockBinary);
  } catch {
    throw new Error(
      `mock MCP binary not found at ${mockBinary}; run npm run build:gateway first`,
    );
  }

  smokeDir = await mkdtemp(path.join(os.tmpdir(), "toolport-headless-smoke-"));
  await writeFile(
    path.join(smokeDir, "registry.json"),
    `${JSON.stringify(
      {
        version: 1,
        humanApproval: false,
        servers: [],
        profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
        activeProfileId: "default",
      },
      null,
      2,
    )}\n`,
  );

  console.log(`Using gateway: ${gatewayBinary}`);
  await expectBindRefusal({
    name: "non-loopback without token",
    host: "0.0.0.0",
  });
  await expectBindRefusal({
    name: "loopback without auth",
    host: "127.0.0.1",
  });
  await testInsecureLoopback();
  await testAuthenticatedGateway();
  await testRoutinePersistsAcrossGatewayRestart();

  console.log("");
  if (failures > 0) throw new Error(`${failures} smoke assertion(s) failed`);
  console.log("All headless smoke tests passed.");
}

main()
  .catch((error) => {
    console.error(`[FAIL] ${error.message}`);
    process.exitCode = 1;
  })
  .finally(async () => {
    try {
      await cleanup();
    } catch (error) {
      console.error(`[FAIL] cleanup failed: ${error.message}`);
      process.exitCode = 1;
    }
  });
