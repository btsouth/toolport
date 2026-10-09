#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import {
  Fixture,
  profiles,
  repo,
  failed,
  textOf,
  deadlineMs,
  stop,
  waitExit,
} from "./client-conformance-support.mjs";

const source = await readFile(path.join(repo, "src-tauri/src/clients.rs"), "utf8");
const inventory = [...source.matchAll(/\bid: "([a-z0-9-]+)"/g)].map((m) => m[1]);
assert.deepEqual(
  [...profiles.map((p) => p.id)].sort(),
  [...inventory].sort(),
  "client inventory changed; research and add its profile",
);
let active;
for (const signal of ["SIGINT", "SIGTERM"])
  process.once(signal, () => {
    (active?.close() || Promise.resolve()).finally(() => process.exit(1));
  });
let checks = 0;
const pass = (message) => {
  checks++;
  console.log(`[PASS] ${message}`);
};
const success = (reply) => assert(!failed(reply), JSON.stringify(reply).slice(0, 2_000));

async function withFixture(profile, full, run) {
  active = await Fixture.create(profile.id, { full });
  try {
    await run(active, active.client(profile));
  } catch (error) {
    const dir = path.join(repo, ".verify/client-conformance-failures");
    await mkdir(dir, { recursive: true });
    for (const name of ["gateway.log", "downstream.jsonl"]) {
      try {
        const raw = await readFile(path.join(active.data, name), "utf8");
        await writeFile(path.join(dir, `${profile.id}-${name}`), raw.slice(-65_536));
      } catch (e) {
        if (e.code !== "ENOENT") throw e;
      }
    }
    throw error;
  } finally {
    await active.close();
    active = undefined;
  }
}

async function catalog(client, expected) {
  const deadline = performance.now() + deadlineMs;
  const tools = await client.list();
  if (tools.some((t) => t.name === expected)) return tools;
  return changedCatalog(client, expected, deadline);
}

async function changedCatalog(
  client,
  expected,
  deadline = performance.now() + deadlineMs,
) {
  // Startup and grow can each emit a valid event. Re-list for every event
  // under one deadline until the changed entry arrives.
  while (performance.now() < deadline) {
    await client.notification(
      "notifications/tools/list_changed",
      deadline - performance.now(),
    );
    const refreshed = await client.list(Math.max(1, deadline - performance.now()));
    if (refreshed.some((t) => t.name === expected)) return refreshed;
  }
  assert.fail(`${client.profile.id}: missing ${expected} after notifications`);
}

async function baseline(profile) {
  await withFixture(profile, false, async (_fixture, client) => {
    await client.initialize();
    const native = ["codex", "cursor"].includes(profile.id);
    const tools = await catalog(client, native ? "mock__echo" : "toolport_search_tools");
    assert.equal(
      tools.some((t) => t.name === "toolport_call_tool"),
      true,
      "Auto clients retain scoped call helpers, including non-refreshing Full clients",
    );
    if (!native) {
      const search = await client.call("toolport_search_tools", {
        query: "mock__echo",
        limit: 1,
      });
      success(search);
      assert(textOf(search).includes("mock__echo"));
    }
    const reply = await client.call(
      "mock__echo",
      { text: "fixture café 東京" },
      !native,
      "echo-string-id",
    );
    success(reply);
    assert(textOf(reply).includes("fixture café 東京"));
    pass(
      `${profile.id}: Auto discovery and call (${profile.initialize ? "captured health initialize" : "synthetic baseline, client behavior unknown"})`,
    );
  });
}

async function replay(profile) {
  await withFixture(profile, true, async (fixture, client) => {
    await client.initialize();
    // Replay actual follow-up methods; health probes need not list tools.
    for (const method of profile.initialMethods.filter(
      (m) => !["initialize", "notifications/initialized", "tools/list"].includes(m),
    )) {
      const reply = await client.request(method);
      success(reply);
    }
    const initial = await catalog(client, "mock__echo");
    const typed = initial.find((t) => t.name === "mock__structured");
    assert.equal(typed?.outputSchema?.properties?.answer?.type, "integer");
    const structured = await client.call("mock__structured");
    success(structured);
    assert.equal(structured.result.structuredContent.answer, 42);
    pass(`${profile.id}: captured startup, outputSchema and structuredContent`);

    // All profiles get the same event. Re-list only for verified refresh behavior;
    // an unknown client is not credited with a handler it has not demonstrated.
    client.notifications = [];
    success(await client.call("mock__grow"));
    if (profile.listChanged.supported === true) {
      await changedCatalog(client, "mock__greet");
      success(await client.call("mock__greet", { name: "fixture" }));
      pass(`${profile.id}: notification then source/documented re-list profile`);
    } else {
      await client.notification("notifications/tools/list_changed");
      if (["codex", "cursor"].includes(profile.id)) {
        // Startup and grow can both notify. Search after each event under the
        // same deadline, without asking this non-refreshing client to re-list.
        const deadline = performance.now() + deadlineMs;
        while (true) {
          const search = await client.call("toolport_search_tools", {
            query: "mock__greet",
          });
          success(search);
          if (textOf(search).includes("mock__greet")) break;
          await client.notification(
            "notifications/tools/list_changed",
            Math.max(1, deadline - performance.now()),
          );
          assert(performance.now() < deadline, "changed tool missing from Full search");
        }
        success(await client.call("mock__greet", { name: "fixture" }, true));
        pass(`${profile.id}: Full helpers recover a changed catalog without re-listing`);
      }
      pass(
        `${profile.id}: notification delivered; client re-list ${profile.listChanged.supported === false ? "unsupported" : "unknown"}`,
      );
    }

    const resources = await client.request("resources/list");
    success(resources);
    assert(resources.result.resources.length >= 1);
    const read = await client.request("resources/read", {
      uri: resources.result.resources[0].uri,
    });
    success(read);
    assert(read.result.contents.some((c) => c.text?.includes("fixture resource")));
    const prompts = await client.request("prompts/list");
    success(prompts);
    assert(prompts.result.prompts.length >= 1);
    const prompt = await client.request("prompts/get", {
      name: prompts.result.prompts[0].name,
    });
    success(prompt);
    assert(prompt.result.messages.some((m) => m.content.text.includes("fixture prompt")));
    pass(`${profile.id}: resource and prompt contracts (synthetic requests)`);

    const slowId = "slow-cancel";
    const slow = client.call("mock__sleep", { ms: 5_000 }, false, slowId);
    let slowPending = true;
    void slow.then(
      () => {
        slowPending = false;
      },
      () => {
        slowPending = false;
      },
    );
    // Assert fast work finishes while the slow call is known to be dispatched.
    await fixture.dispatched(
      (r) => r.method === "tools/call" && r.params.name === "sleep",
    );
    const fast = await client.call("mock__echo", { text: "concurrent fast" });
    success(fast);
    assert(textOf(fast).includes("concurrent fast"));
    assert(slowPending, "slow call completed before concurrent fast call");
    client.send({
      jsonrpc: "2.0",
      method: "notifications/cancelled",
      params: { requestId: slowId, reason: "fixture done" },
    });
    assert(failed(await slow), "cancelled call succeeded");
    await fixture.dispatched((r) => r.method === "notifications/cancelled");
    pass(
      `${profile.id}: long call, concurrent fast call, downstream cancellation (synthetic requests)`,
    );

    const large = await client.call("mock__large", { bytes: 256 * 1024 });
    success(large);
    assert(JSON.stringify(large).length < 256 * 1024, "large output was not budgeted");
    success(await client.call("mock__echo", { text: "after large" }));
    let starts = (await fixture.records()).filter(
      (r) => r.method === "initialize",
    ).length;
    const oversized = await client.call("mock__large", { bytes: 17 * 1024 * 1024 });
    assert(failed(oversized), "oversized downstream frame accepted");
    await catalog(client, "mock__echo");
    await fixture.untilRecords(
      (records) => records.filter((r) => r.method === "initialize").length > starts,
    );
    success(await client.call("mock__echo", { text: "after oversized" }));
    pass(`${profile.id}: bounded large output and oversized-frame recovery`);

    starts = (await fixture.records()).filter((r) => r.method === "initialize").length;
    const crashed = await client.call("mock__die");
    assert(failed(crashed));
    await catalog(client, "mock__echo");
    await fixture.untilRecords(
      (records) => records.filter((r) => r.method === "initialize").length > starts,
    );
    success(await client.call("mock__echo", { text: "after crash" }));
    assert.equal(
      (await fixture.records()).filter(
        (r) => r.method === "tools/call" && r.params.name === "die",
      ).length,
      1,
      "crashing call replayed",
    );
    pass(`${profile.id}: downstream crash and reconnect without replay`);

    // Snapshot before dispatch, so a previous sleep cannot satisfy the barrier.
    const previous = (await fixture.records()).filter(
      (r) => r.method === "tools/call" && r.params.name === "sleep",
    ).length;
    const inflight = client.call("mock__sleep", { ms: 5_000 }, false, "restart-inflight");
    await fixture.untilRecords(
      (records) =>
        records.filter((r) => r.method === "tools/call" && r.params.name === "sleep")
          .length > previous,
    );
    await fixture.killDaemon();
    assert(failed(await inflight));
    await catalog(client, "mock__echo");
    success(await client.call("mock__echo", { text: "after daemon restart" }));
    const sleeps = (await fixture.records()).filter(
      (r) => r.method === "tools/call" && r.params.name === "sleep",
    );
    assert.equal(sleeps.length, 2, "inflight call was replayed or never dispatched");
    pass(`${profile.id}: daemon restart mid-session without replay`);
  });
}

try {
  for (const profile of profiles) await baseline(profile);
  const captures = profiles
    .filter((p) => p.initialize)
    .flatMap((profile) => [
      profile,
      ...(profile.variants || []).map((variant) => ({ ...profile, ...variant })),
    ]);
  for (const profile of captures) await replay(profile);
  const codex = profiles.find((p) => p.id === "codex");
  const claude = profiles.find((p) => p.id === "claude-code");
  await withFixture(codex, true, async (fixture, first) => {
    const second = fixture.client(claude);
    await Promise.all([first.initialize(), second.initialize()]);
    await Promise.all([catalog(first, "mock__echo"), catalog(second, "mock__echo")]);
    const replies = await Promise.all([
      first.call("mock__echo", { text: "codex owned" }, false, "same-id"),
      second.call("mock__echo", { text: "claude owned" }, false, "same-id"),
    ]);
    replies.forEach(success);
    assert(textOf(replies[0]).includes("codex owned"));
    assert(textOf(replies[1]).includes("claude owned"));
    assert.equal((await fixture.descriptors()).length, 1);
    assert.equal(
      (await fixture.records()).filter((r) => r.method === "initialize").length,
      1,
    );
  });
  pass(
    "two real handshake profiles: same request ID, distinct responses, one daemon and downstream",
  );
  for (const revision of ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"]) {
    const profile = {
      id: "protocol-variant",
      initialize: {
        id: "init-version",
        params: {
          protocolVersion: revision,
          capabilities: {},
          clientInfo: { name: "synthetic-version", version: "1" },
        },
      },
    };
    await withFixture(profile, true, async (_fixture, client) => {
      await client.initialize();
      success(await client.request("ping"));
    });
    pass(`synthetic legacy version ${revision}`);
  }
  const modern = { id: "modern-synthetic" };
  await withFixture(modern, true, async (_fixture, client) => {
    const reply = await client.request("server/discover", {
      _meta: { "io.modelcontextprotocol/protocolVersion": "2026-07-28" },
    });
    success(reply);
    assert(reply.result.capabilities.tools);
    assert.equal(reply.result.cacheScope, "private");
  });
  pass("modern sessionless discovery (synthetic)");
  const missingFixture = await Fixture.create("missing-executable");
  try {
    const missing = spawn(path.join(missingFixture.home, "unavailable-client"), [], {
      stdio: "ignore",
    });
    await assert.rejects(waitExit(missing), { code: "ENOENT" });
    await stop(missing);
  } finally {
    await missingFixture.close();
  }
  pass("failed executable spawn cleans up without waiting for an impossible exit");
  console.log(
    `Client conformance: ${checks} scenario groups passed; ${profiles.length} adapter baselines, ${captures.length} captured startup/health variants across ${profiles.filter((p) => p.initialize).length} clients. Authenticated model/GUI acceptance not implied.`,
  );
} catch (error) {
  console.error(`[FAIL] ${error.stack}`);
  process.exitCode = 1;
}
