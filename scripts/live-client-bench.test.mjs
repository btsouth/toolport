import assert from "node:assert/strict";
import test from "node:test";
import { abstained, finalText, includes, score } from "./live-client-bench.mjs";
import { specifications, tasksFrom } from "./live-client-bench-tasks.mjs";

const catalog = [
  {
    name: "demo__read",
    inputSchema: {
      type: "object",
      properties: { id: { type: "integer" } },
      required: ["id"],
      additionalProperties: false,
    },
  },
  {
    name: "demo__post",
    inputSchema: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
  },
];
const call = (name, args) => ({ namespace: "demo", params: { name, arguments: args } });
const task = { expected: [{ name: "demo__read", args: { id: 42 } }] };
test("requires both valid schema and requested values", () => {
  assert.equal(score(task, [call("read", { id: 42 })], catalog, true).success, true);
  assert.equal(score(task, [call("read", { id: "42" })], catalog, true).success, false);
  assert.equal(score(task, [call("read", { id: 41 })], catalog, true).success, false);
  assert.equal(
    score(task, [call("read", { id: 42, text: "wrong" })], catalog, true).success,
    false,
  );
});
test("wrong downstream calls fail even when the right call follows", () => {
  const result = score(
    task,
    [call("post", { text: "hello" }), call("read", { id: 42 })],
    catalog,
    true,
  );
  assert.equal(result.success, false);
  assert.equal(result.wrongCalls, 1);
});
test("multi-step tasks require ordered completed actions", () => {
  const multi = {
    expected: [...task.expected, { name: "demo__post", args: { text: "link" } }],
  };
  assert.equal(
    score(
      multi,
      [call("read", { id: 42 }), call("post", { text: "link" })],
      catalog,
      true,
    ).success,
    true,
  );
  assert.equal(
    score(
      multi,
      [call("post", { text: "link" }), call("read", { id: 42 })],
      catalog,
      true,
    ).success,
    false,
  );
});
test("an infrastructure failure cannot count as successful abstention", () => {
  assert.equal(score({ expected: [] }, [], catalog, false).success, false);
  assert.equal(
    score({ expected: [] }, [call("read", { id: 42 })], catalog, true).success,
    false,
  );
});
test("development alternatives preserve their own argument shapes", () => {
  const alternative = {
    expected: [
      {
        ...task.expected[0],
        alternatives: [{ name: "demo__post", args: { text: "link" } }],
      },
    ],
  };
  assert.equal(
    score(alternative, [call("post", { text: "link" })], catalog, true).success,
    true,
  );
});
test("invalid public schemas fail closed without being rewritten", () => {
  const invalid = [
    {
      name: "demo__read",
      inputSchema: {
        type: "object",
        properties: { id: { type: "integer", maximum: "bad" } },
      },
    },
  ];
  // Use a unique name so the valid-schema cache cannot hide the broken schema.
  invalid[0].name = "demo__invalid";
  const result = score(
    { expected: [{ name: "demo__invalid", args: { id: 42 } }] },
    [call("invalid", { id: 42 })],
    invalid,
    true,
  );
  assert.equal(result.success, false);
  assert.match(result.calls[0].schemaErrors, /schema is invalid/);
});
test("partial matching accepts optional fields but catches missing values", () => {
  assert(includes({ a: { b: 1, c: 2 } }, { a: { b: 1 } }));
  assert(!includes({ a: {} }, { a: { b: 1 } }));
});

test("no-match acceptance needs an explicit model abstention", () => {
  assert.equal(abstained("Done"), false);
  assert.equal(abstained("Unavailable: no supported capability."), true);
  assert.equal(
    finalText("codex", '{"item":{"type":"agent_message","text":"Unavailable"}}'),
    "Unavailable",
  );
});

test("sending a returned link permits surrounding message text", () => {
  const link = {
    expected: [{ name: "demo__post", args: {}, textIncludes: "https://example.test/42" }],
  };
  assert.equal(
    score(link, [call("post", { text: "Issue: https://example.test/42" })], catalog, true)
      .success,
    true,
  );
  assert.equal(
    score(link, [call("post", { text: "https://example.test/41" })], catalog, true)
      .success,
    false,
  );
});

test("public development file-read alternatives retain requested arguments", () => {
  const dev = specifications.map(([id]) => ({
    id,
    split: "dev",
    primary: "demo__read",
    query: "Read",
    acceptable_alternatives: [],
  }));
  const intent = dev.find((x) => x.id === "r4-304");
  intent.primary = "filesystem__read_text_file";
  intent.acceptable_alternatives = ["filesystem__read_file"];
  const fileTask = tasksFrom(dev).find((x) => x.id === "r4-304");
  assert.deepEqual(fileTask.expected[0].alternatives, [
    {
      name: "filesystem__read_file",
      args: { path: "/fixture/config.txt" },
    },
  ]);
});
