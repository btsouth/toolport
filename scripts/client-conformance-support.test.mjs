import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { Fixture, repo, waitExit, waitPidExit, stop, stopTree, wireMetadata } from "./client-conformance-support.mjs";

async function sleeper(cwd) {
  const child = spawn(process.execPath, ["-e", "console.log('ready'); setInterval(() => {}, 1000)"], {
    cwd,
    stdio: ["ignore", "pipe", "ignore"],
  });
  await new Promise((resolve, reject) => {
    child.stdout.once("data", resolve);
    child.once("error", reject);
  });
  return child;
}

test("cleanup waits for daemon and mock exit before removing their working directory", async () => {
  const fixture = await Fixture.create("cursor");
  const children = [];
  try {
    const daemon = await sleeper(fixture.home);
    const mock = await sleeper(fixture.data);
    children.push(daemon, mock);
    await writeFile(path.join(fixture.data, "daemon-test.json"), JSON.stringify({ pid: daemon.pid }));
    await writeFile(path.join(fixture.data, "mock.pid"), `${mock.pid}\n`);
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
    await writeFile(path.join(fixture.data, "daemon-test.json"), JSON.stringify({ pid: child.pid }));
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
  assert.deepEqual(wireMetadata('{"jsonrpc":"2.0","id":7,"result":{"tools":[{},{}],"secret":"private"}}'), {
    jsonrpc: "2.0", id: 7, method: undefined, toolCount: 2,
  });
});

test("live cleanup kills the CLI process group including its grandchild", {
  skip: process.platform === "win32" ? "Windows uses taskkill and published wiretap PIDs" : false,
}, async () => {
  const child = spawn(process.execPath, ["-e", `
    const { spawn } = require('node:child_process');
    const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);
    grandchild.once('spawn', () => console.log(grandchild.pid));
    grandchild.once('exit', () => process.exit(0));
    process.on('SIGTERM', () => {});
  `], { detached: true, stdio: ["ignore", "pipe", "ignore"] });
  const pid = await new Promise((resolve, reject) => {
    child.stdout.once("data", (data) => resolve(Number(data.toString().trim())));
    child.once("error", reject);
  });
  const kill = process.kill;
  const signals = [];
  process.kill = (target, signal) => {
    signals.push([target, signal]);
    return kill(target, signal);
  };
  try {
    await stopTree(child);
    assert(signals.some(([target, signal]) => target === -child.pid && signal === "SIGKILL"));
    // No second signal can target a later process group reusing the leader PID.
    await stopTree(child);
    assert.equal(signals.length, 1);
    await waitPidExit(pid);
  } finally {
    process.kill = kill;
    await stopTree(child);
  }
});
