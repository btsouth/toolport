import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  BINARY_MARKER,
  LaunchError,
  PLATFORMS,
  binaryName,
  findOnPath,
  resolveBinary,
} from "../lib/launcher.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const launcher = path.join(here, "..", "bin", "toolport.js");
const root = mkdtempSync(path.join(tmpdir(), "toolport-cli-test-"));
after(() => rmSync(root, { recursive: true, force: true }));

const unix = process.platform !== "win32";

/** A fake native binary: a shell script that records its arguments and exits with a code. */
function fakeBinary(dir, { code = 0, marker = true, name = "toolport" } = {}) {
  mkdirSync(dir, { recursive: true });
  const file = path.join(dir, name);
  const record = path.join(dir, "args.txt");
  writeFileSync(
    file,
    `#!/bin/sh\n# ${marker ? BINARY_MARKER : "some other program"}\nprintf '%s\\n' "$@" > '${record}'\nexit ${code}\n`,
  );
  chmodSync(file, 0o755);
  return { file, record };
}

function fakePlatformPackage(base, key) {
  const pkg = PLATFORMS[key];
  const dir = path.join(base, "node_modules", ...pkg.split("/"));
  mkdirSync(path.join(dir, "bin"), { recursive: true });
  writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify({ name: pkg, version: "0.0.0" }),
  );
  const platform = key.split("-")[0];
  const bin = path.join(dir, "bin", binaryName(platform));
  writeFileSync(bin, BINARY_MARKER);
  return bin;
}

describe("resolveBinary", () => {
  it("prefers TOOLPORT_CLI_BIN over everything", () => {
    const { file } = fakeBinary(path.join(root, "override"));
    assert.equal(resolveBinary({ env: { TOOLPORT_CLI_BIN: file, PATH: "" } }), file);
  });

  it("reports a TOOLPORT_CLI_BIN that doesn't exist", () => {
    assert.throws(
      () => resolveBinary({ env: { TOOLPORT_CLI_BIN: path.join(root, "missing") } }),
      (e) => e instanceof LaunchError && /doesn't exist/.test(e.message),
    );
  });

  for (const key of Object.keys(PLATFORMS)) {
    it(`finds the ${key} platform package`, () => {
      const base = path.join(root, `pkg-${key}`);
      const bin = fakePlatformPackage(base, key);
      const [platform, arch] = key.split("-");
      assert.equal(
        resolveBinary({ platform, arch, env: { PATH: "" }, paths: [base] }),
        bin,
      );
    });
  }

  it("explains a missing optional dependency", () => {
    assert.throws(
      () =>
        resolveBinary({
          platform: "linux",
          arch: "x64",
          env: { PATH: "" },
          paths: [path.join(root, "empty")],
        }),
      (e) =>
        e instanceof LaunchError &&
        /optional/.test(e.message) &&
        /cli-linux-x64/.test(e.message),
    );
  });

  it("explains an unsupported platform and lists the supported ones", () => {
    assert.throws(
      () => resolveBinary({ platform: "freebsd", arch: "riscv64", env: { PATH: "" } }),
      (e) =>
        e instanceof LaunchError &&
        /freebsd-riscv64/.test(e.message) &&
        /linux-x64/.test(e.message) &&
        /TOOLPORT_CLI_BIN/.test(e.message),
    );
  });
});

describe("PATH fallback", { skip: !unix }, () => {
  it("uses a native toolport on PATH", () => {
    const { file } = fakeBinary(path.join(root, "path-native"));
    assert.equal(findOnPath({ PATH: path.dirname(file) }, "linux"), file);
  });

  it("never runs a different program named toolport, such as the desktop app", () => {
    const { file } = fakeBinary(path.join(root, "path-desktop"), { marker: false });
    assert.equal(findOnPath({ PATH: path.dirname(file) }, "linux"), null);
    assert.throws(
      () =>
        resolveBinary({
          platform: "freebsd",
          arch: "x64",
          env: { PATH: path.dirname(file) },
        }),
      LaunchError,
    );
  });

  it("never runs another copy of this launcher", () => {
    const dir = path.join(root, "path-shim");
    mkdirSync(dir, { recursive: true });
    const shim = path.join(dir, "toolport");
    writeFileSync(
      shim,
      `#!/usr/bin/env node\n// @toolport/cli launcher ${BINARY_MARKER}\n`,
    );
    chmodSync(shim, 0o755);
    assert.equal(findOnPath({ PATH: dir }, "linux"), null);
  });
});

describe("launcher process", { skip: !unix }, () => {
  it("forwards arguments, stdio and the exact exit code", () => {
    for (const code of [0, 2, 7]) {
      const { file, record } = fakeBinary(path.join(root, `exit-${code}`), { code });
      const result = spawnSync(process.execPath, [launcher, "scan", "--json", "a b"], {
        env: { ...process.env, TOOLPORT_CLI_BIN: file },
        encoding: "utf8",
      });
      assert.equal(result.status, code, result.stderr);
      const args = spawnSync("cat", [record], { encoding: "utf8" }).stdout;
      assert.equal(args, "scan\n--json\na b\n");
    }
  });

  it("prints a clear error and exits 1 when no binary can be found", () => {
    const result = spawnSync(process.execPath, [launcher, "--version"], {
      env: { PATH: "", TOOLPORT_CLI_BIN: path.join(root, "nope") },
      encoding: "utf8",
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /^error: TOOLPORT_CLI_BIN points at/);
  });
});
