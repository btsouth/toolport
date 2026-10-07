#!/usr/bin/env node
// INST-06: gate a release on the tag matching every embedded version source.
//
// A `v*` tag starts the release workflow, but nothing compared the tag against
// the version baked into the artifacts. One forgotten bump ships a release whose
// filename says 1.23.6 while the binaries still say 1.23.5, which produces a
// false or repeated update. This runs as the first step of release.yml and fails
// before any build when a source does not match the tag.
//
// Usage:
//   node scripts/check-release-version.mjs <tag> [--root <dir>]
//   GITHUB_REF_NAME=v2.0.0 node scripts/check-release-version.mjs
//
// Preview tags map to each tool's embedded form: v2.0.0-preview.1 is
// 2.0.0-preview.1 in package.json/Cargo/tauri/plugin/homebrew, and
// 2.0.0preview.1 in the Arch PKGBUILD (Arch pkgver forbids '-').
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const TAG_PATTERN = /^v(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*))?$/;

/**
 * Parse a release tag. Returns the embedded version (tag without `v`) and the
 * Arch pkgver form. Throws on anything that is not a `vX.Y.Z` or a
 * `vX.Y.Z-prerelease` tag, so a malformed tag fails before any build.
 */
export function parseTag(raw) {
  const tag = String(raw ?? "").trim();
  const match = TAG_PATTERN.exec(tag);
  if (!match) {
    throw new Error(
      `not a release tag: ${JSON.stringify(tag)} (expected vX.Y.Z or vX.Y.Z-prerelease)`,
    );
  }
  const [, major, minor, patch, prerelease] = match;
  const version = prerelease
    ? `${major}.${minor}.${patch}-${prerelease}`
    : `${major}.${minor}.${patch}`;
  return {
    tag,
    version,
    prerelease: Boolean(prerelease),
    // Arch pkgver forbids '-', so the prerelease separator is dropped.
    pkgbuildVersion: version.split("-").join(""),
  };
}

/** Read a JSON file's top-level `version`. */
function jsonVersion(root, file) {
  const value = JSON.parse(readFileSync(path.join(root, file), "utf8"));
  return value.version;
}

/** Extract the package version from a Cargo.lock `name = "..."` block. */
function cargoLockVersion(text, crate) {
  const match = new RegExp(`name = "${crate}"\\nversion = "([^"]+)"`).exec(text);
  return match?.[1];
}

/**
 * The version sources the tag must equal, with the expected value for each.
 * `stableOnly` marks a source that only tracks stable releases (see the PKGBUILD
 * entry): it is skipped for a prerelease tag rather than reported as a mismatch.
 */
export function expectedChecks(parsed) {
  return [
    {
      file: "package.json",
      expected: parsed.version,
      read: (root) => jsonVersion(root, "package.json"),
    },
    {
      file: "package-lock.json",
      expected: parsed.version,
      read: (root) => {
        const lock = JSON.parse(
          readFileSync(path.join(root, "package-lock.json"), "utf8"),
        );
        return [lock.version, lock.packages?.[""]?.version];
      },
    },
    {
      file: "src-tauri/Cargo.toml",
      expected: parsed.version,
      read: (root) => {
        const text = readFileSync(path.join(root, "src-tauri/Cargo.toml"), "utf8");
        return /^version = "([^"]+)"/m.exec(text)?.[1];
      },
    },
    {
      file: "src-tauri/Cargo.lock",
      expected: parsed.version,
      read: (root) =>
        cargoLockVersion(
          readFileSync(path.join(root, "src-tauri/Cargo.lock"), "utf8"),
          "conduit",
        ),
    },
    {
      file: "src-tauri/tauri.conf.json",
      expected: parsed.version,
      read: (root) => jsonVersion(root, "src-tauri/tauri.conf.json"),
    },
    {
      file: "packaging/agent-plugin/toolport/plugin.json",
      expected: parsed.version,
      read: (root) => jsonVersion(root, "packaging/agent-plugin/toolport/plugin.json"),
    },
    {
      file: "packaging/agent-plugin/toolport/.claude-plugin/plugin.json",
      expected: parsed.version,
      read: (root) =>
        jsonVersion(root, "packaging/agent-plugin/toolport/.claude-plugin/plugin.json"),
    },
    {
      file: "packaging/homebrew/toolport.rb",
      expected: parsed.version,
      read: (root) => {
        const text = readFileSync(
          path.join(root, "packaging/homebrew/toolport.rb"),
          "utf8",
        );
        return /^ {2}version "([^"]+)"/m.exec(text)?.[1];
      },
    },
    {
      // The native Arch PKGBUILD pins a stable source tarball: its `source=` URL is
      // built from `v$pkgver`, and arch-repo.yml refuses prereleases and checks
      // pkgver == tag before building. A preview is not packaged from this file, so
      // only a stable tag is held to it here.
      file: "packaging/linux/native/PKGBUILD",
      expected: parsed.pkgbuildVersion,
      stableOnly: true,
      read: (root) => {
        const text = readFileSync(
          path.join(root, "packaging/linux/native/PKGBUILD"),
          "utf8",
        );
        return /^pkgver=(.+)$/m.exec(text)?.[1];
      },
    },
  ];
}

/**
 * Compare every source under `root` to `parsed`. Returns `{ mismatches, skipped }`,
 * where a mismatch is `{ file, expected, actual }` and `actual` may be undefined
 * when the file is missing or the value cannot be read.
 */
export function runCheck(root, parsed) {
  const mismatches = [];
  const skipped = [];
  for (const check of expectedChecks(parsed)) {
    if (check.stableOnly && parsed.prerelease) {
      skipped.push(check.file);
      continue;
    }
    let actual;
    try {
      actual = check.read(root);
    } catch {
      actual = undefined;
    }
    const values = Array.isArray(actual) ? actual : [actual];
    for (const value of values) {
      if (value !== check.expected) {
        mismatches.push({ file: check.file, expected: check.expected, actual: value });
      }
    }
  }
  return { mismatches, skipped };
}

function main(argv) {
  const args = argv.filter((arg) => arg !== "--root");
  const rootIndex = argv.indexOf("--root");
  const root = rootIndex === -1 ? repoRoot : path.resolve(argv[rootIndex + 1] ?? ".");
  const tag =
    args.find((arg) => !arg.startsWith("-") && arg !== root) ??
    process.env.GITHUB_REF_NAME;

  let parsed;
  try {
    parsed = parseTag(tag);
  } catch (error) {
    console.error(`check-release-version: ${error.message}`);
    return 1;
  }

  const { mismatches, skipped } = runCheck(root, parsed);
  for (const file of skipped) {
    console.log(`skip ${file} (stable-only; tag ${parsed.tag} is a prerelease)`);
  }
  for (const { file, expected, actual } of mismatches) {
    console.error(
      `check-release-version: ${file} is ${actual === undefined ? "missing" : JSON.stringify(actual)}, expected ${JSON.stringify(expected)} for ${parsed.tag}`,
    );
  }
  if (mismatches.length > 0) {
    console.error(
      `check-release-version: ${mismatches.length} version source(s) do not match ${parsed.tag}`,
    );
    return 1;
  }
  console.log(
    `check-release-version: every version source matches ${parsed.tag} (${parsed.version})`,
  );
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  process.exitCode = main(process.argv.slice(2));
}
