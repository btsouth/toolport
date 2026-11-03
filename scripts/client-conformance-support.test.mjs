import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import {
  Fixture,
  clientInventoryFromSource,
  profiles,
  repo,
  waitExit,
  waitPidExit,
  stop,
  stopTree,
  wireMetadata,
  deadlineMs,
  coldRecoveryDeadlineMs,
  catalog,
  coldRecoveryCatalog,
} from "./client-conformance-support.mjs";

test("ordinary catalog keeps 20 seconds and fast cold recovery creates no observation", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const timeouts = [];
  const client = {
    spell: (name) => name,
    list: async (timeout) => {
      timeouts.push(timeout);
      return [{ name: "echo" }];
    },
  };
  await catalog(client, "echo");
  let observed = false;
  await coldRecoveryCatalog(client, "echo", () => {
    observed = true;
  });
  t.mock.timers.tick(coldRecoveryDeadlineMs);
  assert.equal(deadlineMs, 20_000);
  assert(timeouts[0] > 0 && timeouts[0] <= deadlineMs);
  assert(timeouts[1] > deadlineMs && timeouts[1] <= coldRecoveryDeadlineMs);
  assert.equal(observed, false);
});

test("cold catalog notification and re-list share the original end deadline", async (t) => {
  let now = 0;
  t.mock.method(performance, "now", () => now);
  const timeouts = [];
  const client = {
    spell: (name) => name,
    list: async (timeout) => {
      timeouts.push(timeout);
      if (timeouts.length === 1) {
        now = 50_000;
        return [];
      }
      return [{ name: "echo" }];
    },
    notification: async (_method, timeout) => {
      assert.equal(timeout, 30_000);
      now = 55_000;
    },
  };
  await catalog(client, "echo", coldRecoveryDeadlineMs);
  assert.deepEqual(timeouts, [80_000, 25_000]);
});

test("20 second observation waits for the same request and finishes I/O before cleanup", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let now = 0;
  t.mock.method(performance, "now", () => now);
  let complete;
  const reply = new Promise((resolve) => {
    complete = resolve;
  });
  let releaseObservation;
  const writing = new Promise((resolve) => {
    releaseObservation = resolve;
  });
  let calls = 0;
  const states = [];
  const pending = coldRecoveryCatalog(
    {
      spell: (name) => name,
      list: () => {
        calls++;
        return reply;
      },
    },
    "echo",
    async (state) => {
      states.push(state);
      if (state.phase === "pending") await writing;
    },
  );
  now = 20_000;
  t.mock.timers.tick(deadlineMs);
  await new Promise(setImmediate);
  assert.equal(states[0].phase, "pending");
  now = 21_000;
  complete([{ name: "echo" }]);
  let settled = false;
  void pending.then(() => {
    settled = true;
  });
  await new Promise(setImmediate);
  assert.equal(settled, false);
  releaseObservation();
  const result = await pending;
  assert.equal(calls, 1);
  assert.equal(result.elapsedMs, 21_000);
  assert.deepEqual(states, [
    { phase: "pending", elapsedMs: 20_000 },
    { phase: "completed", elapsedMs: 21_000, pendingElapsedMs: 20_000 },
  ]);
});

test("cold recovery still fails at its own deadline without another request", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let calls = 0;
  const states = [];
  const pending = coldRecoveryCatalog(
    {
      spell: (name) => name,
      list: (timeout) => {
        calls++;
        return new Promise((_resolve, reject) =>
          setTimeout(() => reject(new Error("cold RPC deadline")), timeout),
        );
      },
    },
    "echo",
    (state) => {
      states.push(state.phase);
    },
  );
  const failed = assert.rejects(pending, /cold RPC deadline/);
  t.mock.timers.tick(deadlineMs);
  await new Promise(setImmediate);
  t.mock.timers.tick(coldRecoveryDeadlineMs - deadlineMs);
  await failed;
  assert.equal(calls, 1);
  assert.deepEqual(states, ["pending", "failed"]);
});

test("client inventory includes every definition and excludes unrelated fixture IDs", () => {
  const source = `
const example = HttpClient { id: "example", label: "Example" };
fn defs() -> Vec<ClientDef> {
    vec![
        ClientDef { id: "cursor", name: "Cursor" },
        ClientDef {
            // A definition can document its config before its ID.
            id: "new-client", name: "New client"
        },
    ]
}
#[cfg(test)]
mod tests {
    let fixture = HttpClient { id: "real", label: "My assistant" };
    let other = ClientDef { id: "fixture-only", name: "Fixture" };
}
`;
  assert.deepEqual(clientInventoryFromSource(source), ["cursor", "new-client"]);
});

test("client inventory fails closed when its definition factory is missing", () => {
  assert.throws(
    () => clientInventoryFromSource('HttpClient { id: "real" }'),
    /client definition inventory could not be read/,
  );
});

test("source client inventory matches every offline adapter profile", async () => {
  const source = await readFile(path.join(repo, "src-tauri/src/clients.rs"), "utf8");
  assert.deepEqual(
    clientInventoryFromSource(source).sort(),
    profiles.map((profile) => profile.id).sort(),
  );
});

function firstOutput(child) {
  return new Promise((resolve, reject) => {
    const done = (error, data) => {
      clearTimeout(timer);
      child.stdout.off("data", output);
      child.off("error", fail);
      child.off("exit", exited);
      if (error) reject(error);
      else resolve(data.toString().trim());
    };
    const output = (data) => done(null, data);
    const fail = (error) => done(error);
    const exited = () => done(new Error("fixture exited before readiness"));
    const timer = setTimeout(() => done(new Error("fixture readiness deadline")), 5_000);
    child.stdout.once("data", output);
    child.once("error", fail);
    child.once("exit", exited);
  });
}

async function sleeper(cwd) {
  const child = spawn(
    process.execPath,
    ["-e", "console.log('ready'); setInterval(() => {}, 1000)"],
    {
      cwd,
      stdio: ["ignore", "pipe", "ignore"],
    },
  );
  try {
    await firstOutput(child);
  } catch (error) {
    await stop(child);
    throw error;
  }
  return child;
}

test("cleanup waits for daemon and mock exit before removing their working directory", async () => {
  const fixture = await Fixture.create("cursor");
  const children = [];
  try {
    const daemon = await sleeper(fixture.home);
    children.push(daemon);
    const mock = await sleeper(fixture.data);
    children.push(mock);
    const wiretap = await sleeper(fixture.home);
    children.push(wiretap);
    await writeFile(
      path.join(fixture.data, "daemon-test.json"),
      JSON.stringify({ pid: daemon.pid }),
    );
    await writeFile(path.join(fixture.data, "mock.pid"), `${mock.pid}\n`);
    await writeFile(path.join(fixture.data, "wiretap.pid"), `${wiretap.pid}\n`);
    await fixture.close();
    await Promise.all(children.map((child) => waitExit(child)));
    await assert.rejects(access(fixture.home), { code: "ENOENT" });
  } finally {
    await Promise.all(children.map(stop));
    await fixture.close();
  }
});

test("cleanup never signals a daemon descriptor already killed mid-session", async () => {
  const fixture = await Fixture.create("cursor");
  const child = await sleeper(repo);
  try {
    await writeFile(
      path.join(fixture.data, "daemon-test.json"),
      JSON.stringify({ pid: child.pid }),
    );
    await fixture.killDaemon();
    const kill = process.kill;
    process.kill = (pid, ...args) => {
      assert.notEqual(pid, child.pid, "stale descriptor was signalled again");
      return kill(pid, ...args);
    };
    try {
      await fixture.close();
    } finally {
      process.kill = kill;
    }
  } finally {
    await stop(child);
    await fixture.close();
  }
});

test("wire metadata tolerates non-JSON output without retaining it", () => {
  assert.deepEqual(wireMetadata("private CLI chatter"), { nonJson: true });
  assert.deepEqual(wireMetadata("null"), { nonJson: true });
  assert.deepEqual(
    wireMetadata(
      '{"jsonrpc":"2.0","id":7,"result":{"tools":[{},{}],"secret":"private"}}',
    ),
    {
      jsonrpc: "2.0",
      id: 7,
      method: undefined,
      toolCount: 2,
    },
  );
});

test("wire metadata keeps only protocol _meta for sessionless requests", () => {
  const meta = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientInfo": { name: "c", version: "1" },
    "vendor/session": "private",
  };
  for (const method of ["server/discover", "subscriptions/listen"]) {
    const filters = method === "subscriptions/listen" ? { notifications: ["tools"] } : {};
    assert.deepEqual(
      wireMetadata(
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method,
          params: { ...filters, _meta: meta },
        }),
      ),
      {
        jsonrpc: "2.0",
        id: 1,
        method,
        params: {
          ...filters,
          _meta: {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": { name: "c", version: "1" },
          },
        },
      },
    );
  }
  assert.equal(
    wireMetadata('{"jsonrpc":"2.0","id":2,"method":"server/discover","params":"text"}')
      .params,
    undefined,
  );
});

test("wire metadata keeps discover versions and integer error codes only", () => {
  assert.deepEqual(
    wireMetadata(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 3,
        result: {
          supportedVersions: ["2026-07-28"],
          capabilities: { tools: {} },
          instructions: "private",
        },
      }),
    ),
    {
      jsonrpc: "2.0",
      id: 3,
      method: undefined,
      result: { supportedVersions: ["2026-07-28"], capabilities: { tools: {} } },
    },
  );
  assert.equal(
    wireMetadata('{"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"private"}}')
      .errorCode,
    -32601,
  );
  const fractional = wireMetadata(
    '{"jsonrpc":"2.0","id":5,"error":{"code":1.5,"message":"private"}}',
  );
  assert.equal(fractional.errorCode, undefined);
  assert.ok(!JSON.stringify(fractional).includes("private"));
});

test("live cleanup kills the CLI and its grandchild", async () => {
  const child = spawn(
    process.execPath,
    [
      "-e",
      `
    const { spawn } = require('node:child_process');
    const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);
    grandchild.once('spawn', () => console.log(grandchild.pid));
    grandchild.once('exit', () => process.exit(0));
    process.on('SIGTERM', () => {});
  `,
    ],
    { detached: process.platform !== "win32", stdio: ["ignore", "pipe", "ignore"] },
  );
  let pid;
  try {
    pid = Number(await firstOutput(child));
  } catch (error) {
    await stopTree(child);
    throw error;
  }
  const kill = process.kill;
  const signals = [];
  process.kill = (target, signal) => {
    signals.push([target, signal]);
    return kill(target, signal);
  };
  try {
    await stopTree(child);
    if (process.platform !== "win32")
      assert(
        signals.some(([target, signal]) => target === -child.pid && signal === "SIGKILL"),
      );
    // No second signal can target a later process group reusing the leader PID.
    const count = signals.length;
    await stopTree(child);
    assert.equal(signals.length, count);
    await waitPidExit(pid);
  } finally {
    process.kill = kill;
    await stopTree(child);
  }
});
