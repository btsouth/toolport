// Tests for mcp-call.mjs against mock-mcp.mjs. Run: node --test scripts/macos-acceptance/mcp-call.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
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

test("calls the tool and returns the token digest", () => {
  const out = execFileSync(process.execPath, [callScript, entry({ MOCK_API_TOKEN: "abc" }), "check_secret"], {
    encoding: "utf8",
  });
  const report = JSON.parse(out);
  assert.equal(report.ok, true);
  assert.equal(report.calledVia, "check_secret");
  assert.equal(report.result, `sha256:${createHash("sha256").update("abc").digest("hex")}`);
});

test("fails when the server refuses to start without its token", () => {
  const run = spawnSync(process.execPath, [callScript, entry({ MOCK_API_TOKEN: "" }), "check_secret"], {
    encoding: "utf8",
  });
  assert.equal(run.status, 1);
  assert.match(JSON.parse(run.stdout).detail, /exited/);
});

test("fails for a tool that is not listed", () => {
  const run = spawnSync(process.execPath, [callScript, entry({ MOCK_API_TOKEN: "abc" }), "missing_tool"], {
    encoding: "utf8",
  });
  assert.equal(run.status, 1);
  assert.match(JSON.parse(run.stdout).detail, /no tool named missing_tool/);
});

test("passes arguments through", () => {
  const out = execFileSync(
    process.execPath,
    [callScript, entry({ MOCK_API_TOKEN: "abc" }), "echo", JSON.stringify({ message: "hi" })],
    { encoding: "utf8" },
  );
  assert.equal(JSON.parse(out).result, "hi");
});
