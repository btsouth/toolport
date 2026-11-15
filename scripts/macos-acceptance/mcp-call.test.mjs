// Tests for mcp-call.mjs against mock-mcp.mjs. Run: node --test scripts/macos-acceptance/mcp-call.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const callScript = path.join(here, "mcp-call.mjs");
const mock = path.join(here, "mock-mcp.mjs");

function entry(env) {
  const dir = mkdtempSync(path.join(tmpdir(), "mcp-call-"));
  const file = path.join(dir, "entry.json");
  writeFileSync(file, JSON.stringify({ command: process.execPath, args: [mock], env }));
  return file;
}

function expectedFile(text) {
  const file = path.join(mkdtempSync(path.join(tmpdir(), "mcp-exp-")), "secret");
  writeFileSync(file, text);
  return file;
}

test("check_secret reports a matching token without revealing it", () => {
  const out = execFileSync(
    process.execPath,
    [
      callScript,
      entry({
        MOCK_API_TOKEN: "abc-123",
        MOCK_EXPECTED_TOKEN_FILE: expectedFile("abc-123"),
      }),
      "check_secret",
    ],
    { encoding: "utf8" },
  );
  const report = JSON.parse(out);
  assert.equal(report.ok, true);
  assert.equal(report.calledVia, "check_secret");
  assert.equal(report.result, "token matches");
  assert.ok(!out.includes("abc-123"));
});

test("check_secret reports a different or missing expected token", () => {
  for (const [file, want] of [
    [expectedFile("abc-124"), "token does not match"],
    [expectedFile("abc"), "token does not match"],
    [path.join(tmpdir(), "definitely-missing-file"), /cannot read the expected token/],
  ]) {
    const out = execFileSync(
      process.execPath,
      [
        callScript,
        entry({ MOCK_API_TOKEN: "abc-123", MOCK_EXPECTED_TOKEN_FILE: file }),
        "check_secret",
      ],
      { encoding: "utf8" },
    );
    const result = JSON.parse(out).result;
    if (typeof want === "string") assert.equal(result, want);
    else assert.match(result, want);
  }
});

test("fails when the server prints anything but JSON-RPC on stdout", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "mcp-noisy-"));
  const noisy = path.join(dir, "noisy.mjs");
  writeFileSync(
    noisy,
    `process.stdout.write("starting gateway\\n");\nawait import(${JSON.stringify(mock)});\n`,
  );
  const file = path.join(dir, "entry.json");
  writeFileSync(
    file,
    JSON.stringify({
      command: process.execPath,
      args: [noisy],
      env: { MOCK_API_TOKEN: "abc" },
    }),
  );
  const run = spawnSync(
    process.execPath,
    [callScript, file, "echo", JSON.stringify({ message: "hi" })],
    {
      encoding: "utf8",
    },
  );
  assert.equal(run.status, 1);
  const report = JSON.parse(run.stdout);
  assert.deepEqual(report.invalidStdout, ["starting gateway"]);
});

test("finds a namespaced tool through toolport search without a regex", () => {
  // The suffix contains regex metacharacters; token matching must not choke on it.
  const run = spawnSync(
    process.execPath,
    [callScript, entry({ MOCK_API_TOKEN: "abc" }), "a(b"],
    { encoding: "utf8" },
  );
  assert.equal(run.status, 1);
  assert.match(JSON.parse(run.stdout).detail, /no tool named a\(b/);
});

test("fails when the server refuses to start without its token", () => {
  const run = spawnSync(
    process.execPath,
    [callScript, entry({ MOCK_API_TOKEN: "" }), "check_secret"],
    {
      encoding: "utf8",
    },
  );
  assert.equal(run.status, 1);
  assert.match(JSON.parse(run.stdout).detail, /exited/);
});

test("fails for a tool that is not listed", () => {
  const run = spawnSync(
    process.execPath,
    [callScript, entry({ MOCK_API_TOKEN: "abc" }), "missing_tool"],
    {
      encoding: "utf8",
    },
  );
  assert.equal(run.status, 1);
  assert.match(JSON.parse(run.stdout).detail, /no tool named missing_tool/);
});

test("passes arguments through", () => {
  const out = execFileSync(
    process.execPath,
    [
      callScript,
      entry({ MOCK_API_TOKEN: "abc" }),
      "echo",
      JSON.stringify({ message: "hi" }),
    ],
    { encoding: "utf8" },
  );
  assert.equal(JSON.parse(out).result, "hi");
});
