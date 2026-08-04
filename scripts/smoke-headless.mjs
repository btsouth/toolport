#!/usr/bin/env node

// Authoritative headless gateway security smoke suite. CI runs this file on Linux;
// scripts/smoke-headless.ps1 is the Windows mirror and must cover the same
// real-process HTTP scenarios.

import { access, mkdtemp, rm, writeFile } from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const defaultBinary = path.join(
  repoRoot,
  "src-tauri",
  "target",
  "debug",
  process.platform === "win32" ? "toolport-gateway.exe" : "toolport-gateway",
);
const gatewayBinary = process.env.TOOLPORT_GATEWAY_BIN || defaultBinary;
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
    const exited = await waitForExit(child, 5_000);
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
  const initBody = JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "smoke", version: "0" },
    },
  });
  let initialized;
  try {
    initialized = await request({
      port,
      method: "POST",
      route: "/mcp",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: initBody,
    });
  } catch (error) {
    fail(`MCP initialize request failed: ${error.message}`);
    return;
  }

  const sessionId = initialized.headers["mcp-session-id"];
  if (initialized.status !== 200) {
    fail(`MCP initialize returned ${initialized.status} (expected 200)`);
    return;
  }
  if (typeof sessionId !== "string" || !sessionId) {
    fail("MCP initialize did not return Mcp-Session-Id");
    return;
  }
  pass("MCP initialize returns 200 + Mcp-Session-Id");

  let listed;
  try {
    listed = await request({
      port,
      method: "POST",
      route: "/mcp",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        "Mcp-Session-Id": sessionId,
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list" }),
    });
  } catch (error) {
    fail(`MCP tools/list request failed: ${error.message}`);
    return;
  }

  try {
    const tools = JSON.parse(listed.body)?.result?.tools;
    if (listed.status === 200 && Array.isArray(tools) && tools.length >= 4) {
      pass(`MCP tools/list returns ${tools.length} tools`);
    } else {
      fail(`MCP tools/list returned ${listed.status} with ${tools?.length ?? 0} tools`);
    }
  } catch (error) {
    fail(`MCP tools/list returned invalid JSON: ${error.message}`);
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
