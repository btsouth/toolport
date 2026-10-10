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
} from "./client-conformance-support.mjs";

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
