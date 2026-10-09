import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (file) => readFileSync(file, "utf8");

test("Cargo rejects a gateway without the bundled search model", () => {
  const gateway = read("src-tauri/Cargo.toml")
    .split("[[bin]]")
    .find((target) => target.includes('name = "toolport-gateway"'));
  assert.match(gateway, /required-features = \["search-static"\]/);
});

test("every production gateway builder includes the model without test hooks", () => {
  for (const file of [
    "packaging/linux/native/PKGBUILD",
    "scripts/build-linux-packages.sh",
    "scripts/stage-linux-native.sh",
    "scripts/prepare-sidecar.mjs",
    "Dockerfile.source",
    ".github/workflows/docker-publish.yml",
  ]) {
    const commands = read(file)
      .replace(/\\\r?\n/g, " ")
      .split("\n")
      .filter((line) => /cargo build/.test(line));
    assert.ok(commands.length, `${file}: no build command found`);
    for (const command of commands) {
      assert.match(command, /--features [\w,-]*\bsearch-static\b/, file);
      assert.doesNotMatch(command, /test-support/, file);
    }
  }
});

test("headless suites and smoke builders use the shipped model", () => {
  for (const file of [
    ".github/workflows/ci.yml",
    ".github/workflows/chaos-nightly.yml",
  ]) {
    for (const line of read(file).split("\n")) {
      if (/cargo (test|build|clippy).*--no-default-features.*--(bins|test)\b/.test(line))
        assert.match(line, /--features [\w,-]*\bsearch-static\b/, file);
    }
  }
  for (const file of ["scripts/verify.mjs", "scripts/test-rust-windows.ps1"])
    assert.match(read(file), /"test-support,search-static"/, file);
  const { scripts } = JSON.parse(read("package.json"));
  for (const name of ["build:gateway", "test:rust", "test:client-conformance"])
    assert.match(scripts[name], /--features [\w,-]*\bsearch-static\b/, name);
});
