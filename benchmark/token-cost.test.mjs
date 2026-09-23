import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

test("catalog report labels parsed JSON size rather than captured byte size", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "toolport-token-cost-"));
  try {
    const full = path.join(dir, "full.json");
    const lazy = path.join(dir, "lazy.json");
    const rawArray = String.raw`[{"name":"caf\u00e9","inputSchema":{"type":"object"}}]`;
    const raw = `{"tools":${rawArray}}`;
    writeFileSync(full, raw);
    writeFileSync(lazy, '{"tools":[]}');
    const canonical = Buffer.byteLength(JSON.stringify(JSON.parse(raw).tools));
    expect(canonical).not.toBe(Buffer.byteLength(rawArray));
    const script = fileURLToPath(new URL("./token-cost.mjs", import.meta.url));
    const run = spawnSync(process.execPath, [script, full, lazy], {
      encoding: "utf8",
    });
    expect(run.status, run.stderr).toBe(0);
    expect(run.stdout).toMatch(
      new RegExp(`${canonical} canonical reserialized UTF-8 bytes`),
    );
    expect(run.stdout).not.toMatch(/exact.*bytes/i);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("extra exposure reports canonical reserialized bytes", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "toolport-token-cost-extra-"));
  try {
    const full = path.join(dir, "full.json");
    const exposed = path.join(dir, "exposed.json");
    const escaped = String.raw`[{"name":"caf\u00e9","description":"extra"}]`;
    writeFileSync(full, '{"tools":[]}');
    writeFileSync(exposed, `{"tools":${escaped}}`);
    const canonicalExtra = Buffer.byteLength(JSON.stringify(JSON.parse(escaped))) - 2;
    expect(canonicalExtra).not.toBe(Buffer.byteLength(escaped) - 2);
    const script = fileURLToPath(new URL("./token-cost.mjs", import.meta.url));
    const run = spawnSync(process.execPath, [script, full, exposed], {
      encoding: "utf8",
    });
    expect(run.status, run.stderr).toBe(0);
    expect(run.stdout).toContain(
      `Extra exposure: ${canonicalExtra} canonical reserialized bytes`,
    );
    expect(run.stdout).not.toMatch(/exact.*bytes/i);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
