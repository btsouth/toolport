#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const tag = process.argv[2] ?? "";
const match = /^v([0-9]+\.[0-9]+\.[0-9]+)$/.exec(tag);

if (!match) {
  console.error(
    `Expected a stable release tag such as v1.18.0, got ${JSON.stringify(tag)}`,
  );
  process.exit(1);
}

const expected = match[1];
const errors = [];
const read = (path) => readFileSync(resolve(root, path), "utf8");
const json = (path) => JSON.parse(read(path));
const expectVersion = (path, actual) => {
  if (actual !== expected)
    errors.push(`${path}: expected ${expected}, found ${actual ?? "missing"}`);
};
const capture = (path, pattern, label) => {
  const value = pattern.exec(read(path))?.[1];
  if (!value) errors.push(`${path}: could not read ${label}`);
  return value;
};

const packageJson = json("package.json");
const packageLock = json("package-lock.json");
expectVersion("package.json", packageJson.version);
expectVersion("package-lock.json", packageLock.version);
expectVersion('package-lock.json packages[""]', packageLock.packages?.[""]?.version);
expectVersion("src-tauri/tauri.conf.json", json("src-tauri/tauri.conf.json").version);
expectVersion(
  "src-tauri/Cargo.toml",
  capture(
    "src-tauri/Cargo.toml",
    /^\[package\][\s\S]*?^version = "([^"]+)"/m,
    "package version",
  ),
);
expectVersion(
  "src-tauri/Cargo.lock conduit package",
  capture(
    "src-tauri/Cargo.lock",
    /^\[\[package\]\]\nname = "conduit"\nversion = "([^"]+)"/m,
    "conduit package version",
  ),
);
expectVersion(
  "packaging/agent-plugin/toolport/plugin.json",
  json("packaging/agent-plugin/toolport/plugin.json").version,
);
expectVersion(
  "packaging/agent-plugin/toolport/.claude-plugin/plugin.json",
  json("packaging/agent-plugin/toolport/.claude-plugin/plugin.json").version,
);
expectVersion(
  "packaging/homebrew/toolport.rb",
  capture("packaging/homebrew/toolport.rb", /^\s*version\s+"([^"]+)"/m, "cask version"),
);

const nativeRecipe = "packaging/linux/native/PKGBUILD";
const omarchyRecipe = "packaging/omarchy-pkgs/toolport/PKGBUILD";
const omarchyMetadata = json("packaging/omarchy-pkgs/toolport/.omarchy/package.json");
if (omarchyMetadata.source !== "local" || omarchyMetadata.release_ring !== "fast") {
  errors.push(
    "packaging/omarchy-pkgs/toolport/.omarchy/package.json: expected a local fast-ring package",
  );
}
for (const path of [nativeRecipe, omarchyRecipe]) {
  expectVersion(path, capture(path, /^pkgver=([^\s'"]+)/m, "pkgver"));
  const checksum = capture(path, /^sha256sums=\('([^']+)'\)$/m, "source checksum");
  if (
    checksum &&
    checksum !== `REPLACE_WITH_V${expected.replaceAll(".", "_")}_SOURCE_SHA256`
  ) {
    if (!/^[0-9a-f]{64}$/.test(checksum)) {
      errors.push(`${path}: source checksum is not 64 lowercase hex characters`);
    }
  }
}
if (read(nativeRecipe) !== read(omarchyRecipe)) {
  errors.push(`${nativeRecipe} and ${omarchyRecipe} differ`);
}

const changelog = read("CHANGELOG.md");
const heading = `## [${expected}]`;
const start = changelog.indexOf(heading);
if (start < 0) {
  errors.push(`CHANGELOG.md: missing ${heading}`);
} else {
  const bodyStart = changelog.indexOf("\n", start) + 1;
  const next = changelog.indexOf("\n## [", bodyStart);
  const body = changelog.slice(bodyStart, next < 0 ? undefined : next).trim();
  if (!body) errors.push(`CHANGELOG.md: ${heading} has no release notes`);
}

if (errors.length) {
  for (const error of errors) console.error(`ERROR: ${error}`);
  process.exit(1);
}

console.log(`Release metadata matches ${tag}.`);
if (read(nativeRecipe).includes("REPLACE_WITH_")) {
  console.log(
    "The Omarchy source checksum is intentionally pending until the tag archive exists; finalize it after publishing the tag.",
  );
}
