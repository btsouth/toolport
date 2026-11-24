import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";

import { PLATFORMS as LAUNCHER_PLATFORMS } from "../packages/toolport-cli/lib/launcher.js";
import {
  PLATFORMS,
  check,
  mainManifest,
  packageName,
  platformManifest,
  readTemplate,
  stage,
} from "./stage-npm-cli.mjs";

const dir = mkdtempSync(path.join(tmpdir(), "stage-npm-cli-"));
after(() => rmSync(dir, { recursive: true, force: true }));

describe("stage-npm-cli", () => {
  it("knows the same platforms as the launcher", () => {
    assert.deepEqual(
      Object.keys(PLATFORMS).map(packageName).sort(),
      Object.values(LAUNCHER_PLATFORMS).sort(),
    );
  });

  it("the checked-in launcher package is consistent", () => {
    assert.deepEqual(check(), []);
  });

  it("flags a missing platform, a wrong pin and an install script", () => {
    const template = readTemplate();
    const broken = {
      ...template,
      scripts: { postinstall: "node x.js" },
      optionalDependencies: {
        ...template.optionalDependencies,
        "@toolport/cli-win32-x64": "0.0.1",
      },
    };
    delete broken.optionalDependencies["@toolport/cli-linux-arm64"];
    const problems = check(broken).join("\n");
    assert.match(problems, /optionalDependencies should be exactly/);
    assert.match(problems, /pinned to 0.0.1/);
    assert.match(problems, /install script/);
  });

  it("writes os, cpu and libc for each platform package", () => {
    const linux = platformManifest("linux-x64", "9.9.9");
    assert.deepEqual([linux.os, linux.cpu, linux.libc], [["linux"], ["x64"], ["glibc"]]);
    const mac = platformManifest("darwin-arm64", "9.9.9");
    assert.equal(mac.libc, undefined);
    assert.equal(platformManifest("win32-x64", "9.9.9").name, "@toolport/cli-win32-x64");
    assert.throws(() => platformManifest("plan9-x64", "1"));
    const main = mainManifest("9.9.9");
    assert.ok(Object.values(main.optionalDependencies).every((v) => v === "9.9.9"));
  });

  it("stages binaries and the launcher package", () => {
    const linuxBin = path.join(dir, "toolport-linux");
    const winBin = path.join(dir, "toolport.exe");
    writeFileSync(linuxBin, "linux binary");
    writeFileSync(winBin, "windows binary");
    const out = path.join(dir, "out");
    const { written, missing } = stage({
      out,
      version: "9.9.9",
      binaries: { "linux-x64": linuxBin, "win32-x64": winBin },
    });
    assert.ok(
      written.includes("@toolport/cli-linux-x64") && written.includes("@toolport/cli"),
    );
    assert.ok(missing.includes("darwin-arm64"));
    assert.equal(
      readFileSync(path.join(out, "cli-linux-x64/bin/toolport"), "utf8"),
      "linux binary",
    );
    assert.ok(existsSync(path.join(out, "cli-win32-x64/bin/toolport.exe")));
    if (process.platform !== "win32") {
      assert.ok(statSync(path.join(out, "cli-linux-x64/bin/toolport")).mode & 0o111);
    }
    const main = JSON.parse(readFileSync(path.join(out, "cli/package.json"), "utf8"));
    assert.equal(main.version, "9.9.9");
    assert.ok(existsSync(path.join(out, "cli/bin/toolport.js")));
    assert.ok(existsSync(path.join(out, "cli/lib/launcher.js")));
    assert.throws(
      () =>
        stage({
          out,
          version: "1",
          binaries: { "linux-x64": linuxBin },
          requireAll: true,
        }),
      /no binary for/,
    );
    assert.throws(
      () =>
        stage({ out, version: "1", binaries: { "linux-x64": path.join(dir, "nope") } }),
      /not found/,
    );
  });
});
