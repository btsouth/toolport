// Tests for the INST-06 release version gate. Run with:
//   node --test scripts/check-release-version.test.mjs
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { parseTag, runCheck } from "./check-release-version.mjs";

const scriptPath = fileURLToPath(new URL("./check-release-version.mjs", import.meta.url));

/** Write a fixture repo tree whose sources all carry the given versions. */
function writeFixture(root, { version, pkgbuildVersion }) {
  const files = {
    "package.json": JSON.stringify({ name: "toolport", version }),
    "package-lock.json": JSON.stringify({ version, packages: { "": { version } } }),
    "src-tauri/Cargo.toml": `[package]\nname = "conduit"\nversion = "${version}"\n`,
    "src-tauri/Cargo.lock": `[[package]]\nname = "conduit"\nversion = "${version}"\n`,
    "src-tauri/tauri.conf.json": JSON.stringify({ version }),
    "packaging/agent-plugin/toolport/plugin.json": JSON.stringify({ version }),
    "packaging/agent-plugin/toolport/.claude-plugin/plugin.json": JSON.stringify({
      version,
    }),
    "packaging/homebrew/toolport.rb": `cask "toolport" do\n  version "${version}"\nend\n`,
    "packaging/linux/native/PKGBUILD": `pkgver=${pkgbuildVersion}\n`,
  };
  for (const [file, contents] of Object.entries(files)) {
    const full = path.join(root, file);
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, contents);
  }
}

function withFixture(tag, versions, body) {
  const root = mkdtempSync(path.join(tmpdir(), "toolport-version-check-"));
  try {
    const parsed = parseTag(tag);
    writeFixture(root, versions);
    return body(root, parsed);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("stable tag maps to itself", () => {
  assert.deepEqual(parseTag("v2.0.0"), {
    tag: "v2.0.0",
    version: "2.0.0",
    prerelease: false,
    pkgbuildVersion: "2.0.0",
  });
});

test("preview tag maps to the embedded and PKGBUILD forms", () => {
  assert.deepEqual(parseTag("v2.0.0-preview.1"), {
    tag: "v2.0.0-preview.1",
    version: "2.0.0-preview.1",
    prerelease: true,
    pkgbuildVersion: "2.0.0preview.1",
  });
});

test("malformed tags are rejected", () => {
  for (const tag of [
    "2.0.0",
    "v2.0",
    "v2",
    "v2.0.0-",
    "release-2.0.0",
    "v2.0.0+meta",
    "",
  ]) {
    assert.throws(
      () => parseTag(tag),
      /not a release tag/,
      `accepted ${JSON.stringify(tag)}`,
    );
  }
});

test("matching stable tag passes every source", () => {
  withFixture(
    "v2.0.0",
    { version: "2.0.0", pkgbuildVersion: "2.0.0" },
    (root, parsed) => {
      const { mismatches } = runCheck(root, parsed);
      assert.deepEqual(mismatches, []);
    },
  );
});

test("matching preview tag passes and skips the stable-only PKGBUILD", () => {
  withFixture(
    "v2.0.0-preview.1",
    { version: "2.0.0-preview.1", pkgbuildVersion: "2.0.0preview.1" },
    (root, parsed) => {
      const { mismatches, skipped } = runCheck(root, parsed);
      assert.deepEqual(mismatches, []);
      assert.deepEqual(skipped, ["packaging/linux/native/PKGBUILD"]);
    },
  );
});

test("a mismatched source fails with the file and both values", () => {
  withFixture(
    "v2.0.0",
    { version: "2.0.0", pkgbuildVersion: "2.0.0" },
    (root, parsed) => {
      writeFileSync(
        path.join(root, "src-tauri/tauri.conf.json"),
        JSON.stringify({ version: "1.2.3" }),
      );
      const { mismatches } = runCheck(root, parsed);
      assert.deepEqual(mismatches, [
        {
          file: "src-tauri/tauri.conf.json",
          expected: "2.0.0",
          actual: "1.2.3",
        },
      ]);
    },
  );
});

test("a mismatched stable PKGBUILD fails and a missing file counts as a mismatch", () => {
  withFixture(
    "v2.0.0",
    { version: "2.0.0", pkgbuildVersion: "1.24.0" },
    (root, parsed) => {
      const { mismatches } = runCheck(root, parsed);
      assert.deepEqual(mismatches, [
        {
          file: "packaging/linux/native/PKGBUILD",
          expected: "2.0.0",
          actual: "1.24.0",
        },
      ]);
    },
  );

  withFixture(
    "v2.0.0",
    { version: "2.0.0", pkgbuildVersion: "2.0.0" },
    (root, parsed) => {
      rmSync(path.join(root, "package-lock.json"));
      const { mismatches } = runCheck(root, parsed);
      assert.equal(mismatches.length, 1);
      assert.equal(mismatches[0].file, "package-lock.json");
      assert.equal(mismatches[0].actual, undefined);
    },
  );
});

test("the CLI exits non-zero on a malformed tag and zero on an exact match", () => {
  const malformed = spawnSync(process.execPath, [scriptPath, "v2.0"], {
    encoding: "utf8",
  });
  assert.equal(malformed.status, 1);
  assert.match(malformed.stderr, /not a release tag/);

  withFixture("v2.0.0", { version: "2.0.0", pkgbuildVersion: "2.0.0" }, (root) => {
    const ok = spawnSync(process.execPath, [scriptPath, "v2.0.0", "--root", root], {
      encoding: "utf8",
    });
    assert.equal(ok.status, 0, ok.stderr);
    assert.match(ok.stdout, /every version source matches v2\.0\.0/);
  });
});
