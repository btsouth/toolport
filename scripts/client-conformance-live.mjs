#!/usr/bin/env node
// Explicitly opt in. Configs, working directory and gateway data are all disposable.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import {
  Fixture,
  profiles,
  gateway,
  repo,
  stop,
  stopTree,
  wireMetadata,
  waitExit,
} from "./client-conformance-support.mjs";

const self = fileURLToPath(import.meta.url);
if (process.argv[2] === "--wiretap") {
  const [trace, data, id, home] = process.argv.slice(3);
  const fixture = new Fixture(home, id);
  assert.equal(data, fixture.data, "wiretap fixture data mismatch");
  await appendFile(path.join(fixture.data, "wiretap.pid"), `${process.pid}\n`);
  const child = spawn(gateway, ["--stdio-adapter"], {
    cwd: repo,
    env: fixture.env(),
    stdio: ["pipe", "pipe", "pipe"],
  });
  let writes = Promise.resolve();
  function record(direction, line) {
    // Retain metadata only; non-JSON chatter must not crash tracing or leak text.
    const safe = wireMetadata(line);
    writes = writes.then(() =>
      appendFile(
        trace,
        JSON.stringify({ direction, elapsedMs: performance.now(), message: safe }) + "\n",
      ),
    );
  }
  const input = createInterface({ input: process.stdin });
  input.on("line", (line) => {
    record("client", line);
    child.stdin.write(line + "\n");
  });
  input.on("close", () => child.stdin.end());
  child.stdin.on("error", () => input.close());
  child.stderr.pipe(process.stderr);
  const output = createInterface({ input: child.stdout });
  output.on("line", (line) => {
    record("server", line);
    process.stdout.write(line + "\n");
  });
  child.on("error", (e) => {
    console.error(e.message);
    process.exitCode = 1;
    input.close();
  });
  child.on("exit", (code) => {
    process.exitCode = code ?? 1;
    input.close();
  });
  await waitExit(child, 60_000).catch(async () => {
    await stop(child);
    process.exitCode = 1;
  });
  await writes;
} else {
  const id = process.argv[2];
  const executable = process.argv[3];
  const output = process.argv[4];
  const commands = {
    "claude-code": ["mcp", "list"],
    cursor: ["mcp", "list-tools", "toolport"],
    opencode: ["mcp", "list"],
    "kilo-code": ["mcp", "list"],
    "gemini-cli": ["mcp", "list"],
    "qwen-code": ["mcp", "list"],
    "github-copilot-cli": ["mcp", "list"],
    codex: ["app-server"],
    hermes: ["mcp", "test", "toolport"],
  };
  assert(
    commands[id] && executable && output,
    "Usage: node scripts/client-conformance-live.mjs <client-id> <CLI executable> <new evidence directory>",
  );
  assert(path.isAbsolute(executable), "CLI executable must be an absolute path");
  const profile = profiles.find((p) => p.id === id);
  assert(profile);
  await mkdir(output, { recursive: false });
  const fixture = await Fixture.create(id, { full: true });
  let child;
  try {
    const trace = path.resolve(output, "traffic.jsonl");
    const shim = [self, "--wiretap", trace, fixture.data, id, fixture.home];
    const command = process.execPath;
    const mcp = { command, args: shim };
    const config = async (name, value) => {
      const filename = path.join(fixture.home, name);
      await mkdir(path.dirname(filename), { recursive: true });
      await writeFile(
        filename,
        typeof value === "string" ? value : JSON.stringify(value),
      );
    };
    if (id === "hermes")
      await config(".hermes/config.yaml", { mcp_servers: { toolport: mcp } });
    else if (id === "codex")
      await config(
        ".codex/config.toml",
        `[mcp_servers.toolport]\ncommand = ${JSON.stringify(command)}\nargs = ${JSON.stringify(shim)}\n`,
      );
    else if (["opencode", "kilo-code"].includes(id))
      await config(
        id === "opencode" ? ".config/opencode/opencode.json" : ".config/kilo/kilo.jsonc",
        {
          mcp: {
            toolport: { type: "local", command: [command, ...shim], enabled: true },
          },
        },
      );
    else {
      const names = {
        "claude-code": ".claude/.claude.json",
        cursor: ".cursor/mcp.json",
        "gemini-cli": ".gemini/settings.json",
        "qwen-code": ".qwen/settings.json",
        "github-copilot-cli": ".copilot/mcp-config.json",
      };
      const root = { mcpServers: { toolport: mcp } };
      if (id === "gemini-cli") root.security = { folderTrust: { enabled: false } };
      if (id === "github-copilot-cli") mcp.tools = ["*"];
      await config(names[id], root);
    }
    child = spawn(executable, commands[id], {
      cwd: fixture.home,
      detached: process.platform !== "win32",
      env: fixture.env(),
      stdio: ["pipe", "pipe", "pipe"],
    });
    child.stdin.on("error", () => {});
    let status = "",
      stderr = "";
    child.stdout.on("data", (d) => {
      status = (status + d).slice(-16_384);
    });
    child.stderr.on("data", (d) => {
      stderr = (stderr + d).slice(-16_384);
    });
    if (id === "codex") {
      // Ask the app server for MCP status without starting a model turn.
      const responses = createInterface({ input: child.stdout });
      let requested = false;
      responses.on("line", (line) => {
        try {
          const reply = JSON.parse(line);
          if (reply.id === 1 && !requested) {
            requested = true;
            child.stdin.write(
              JSON.stringify({ method: "initialized", params: {} }) + "\n",
            );
            child.stdin.write(
              JSON.stringify({ id: 2, method: "mcpServerStatus/list", params: {} }) +
                "\n",
            );
          }
          if (reply.id === 2) child.stdin.end();
        } catch {
          /* Human CLI output is retained in status, not wire evidence. */
        }
      });
      child.stdin.write(
        JSON.stringify({
          id: 1,
          method: "initialize",
          params: {
            clientInfo: { name: "toolport-conformance", version: "1" },
            capabilities: { experimentalApi: true },
          },
        }) + "\n",
      );
    } else child.stdin.end();
    let timedOut = false,
      cliError;
    try {
      await waitExit(child, 30_000);
    } catch (error) {
      timedOut = error.message === "child exit deadline";
      cliError = error.message;
      await stopTree(child);
    }
    // Close surviving wiretaps before reading their final trace.
    await stopTree(child);
    let records = [];
    try {
      records = (await readFile(trace, "utf8"))
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(JSON.parse);
    } catch (e) {
      if (e.code !== "ENOENT") throw e;
    }
    const initialize = records.find(
      (r) => r.direction === "client" && r.message.method === "initialize",
    );
    const summary = {
      client: id,
      exitCode: child.exitCode,
      timedOut,
      cliError,
      handshakeCaptured: Boolean(initialize),
      initialize: initialize?.message,
      methods: records
        .filter((r) => r.direction === "client")
        .map((r) => r.message.method)
        .filter(Boolean),
      scope:
        "isolated health command only; no authenticated model call or GUI acceptance",
    };
    // No auth was supplied and only fixture status is retained.
    await writeFile(path.join(output, "status.txt"), status + stderr);
    await writeFile(
      path.join(output, "summary.json"),
      JSON.stringify(summary, null, 2) + "\n",
    );
    console.log(JSON.stringify(summary));
    if (!initialize || timedOut || child.exitCode !== 0) process.exitCode = 1;
  } finally {
    if (child) await stopTree(child);
    await fixture.close();
  }
}
