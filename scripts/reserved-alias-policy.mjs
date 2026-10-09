import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { Fixture, profiles, failed, textOf } from "./client-conformance-support.mjs";

const codex = profiles.find((profile) => profile.id === "codex");
for (const alias of [
  "custom_echo",
  "toolport_call_tool",
  "toolport_search_tools",
  "toolport_custom_echo",
]) {
  const target = alias.startsWith("toolport_") ? "mock__echo" : alias;
  const fixture = await Fixture.create("codex", { full: true });
  try {
    fixture.registry.toolOverrides = { mock: { echo: { name: alias } } };
    fixture.registry.teamForcedQuarantineOnDrift = true;
    await fixture.save();
    const quarantine = {
      [alias]: {
        tool: alias,
        change: "changed",
        reason: "previous schema drift awaiting reapproval",
        server: "mock",
        severity: "high",
      },
    };
    await writeFile(`${fixture.data}/quarantine.json`, JSON.stringify(quarantine));
    const client = fixture.client(codex);
    await client.initialize();
    assert(!(await client.list()).some((tool) => tool.name === target), alias);
    for (const helper of [false, true]) {
      const reply = await client.call(target, { text: "blocked legacy alias" }, helper);
      assert(failed(reply), `${alias}: ${textOf(reply)}`);
    }
    assert(
      !(await fixture.records()).some(
        (record) => record.method === "tools/call" && record.params?.name === "echo",
      ),
      `${alias}: quarantined tool dispatched`,
    );
    const persisted = JSON.parse(
      await readFile(`${fixture.data}/quarantine.json`, "utf8"),
    );
    assert(persisted[alias], `${alias}: quarantine silently released`);
    console.log(
      `[PASS] ${alias}: hidden, direct/helper calls blocked, quarantine retained`,
    );
  } finally {
    await fixture.close();
  }

  const capped = await Fixture.create("codex", { full: true });
  try {
    capped.registry.toolOverrides = { mock: { echo: { name: alias } } };
    capped.registry.team = {
      serverUrl: "https://fixture.invalid",
      teamId: "fixture",
      role: "member",
      rateLimits: [{ id: "echo", window: "day", maxCalls: 1, tool: "mock/echo" }],
    };
    await capped.save();
    const day = new Date().toISOString().slice(0, 10);
    await writeFile(
      `${capped.data}/rate_limit_counters.json`,
      JSON.stringify({ counts: { [`day:${day}:mock/echo`]: 1 } }),
    );
    const client = capped.client(codex);
    await client.initialize();
    assert((await client.list()).some((tool) => tool.name === target));
    const reply = await client.call(target, { text: "over existing cap" }, true);
    assert(failed(reply), `${alias}: persisted cap bypassed`);
    assert.match(textOf(reply), /limit|cap/i);
    assert(
      !(await capped.records()).some(
        (record) => record.method === "tools/call" && record.params?.name === "echo",
      ),
      `${alias}: capped tool dispatched`,
    );
    console.log(`[PASS] ${alias}: original-identity rate cap retained`);
  } finally {
    await capped.close();
  }
}
console.log("Reserved alias policy replay: 8 scenarios passed");
