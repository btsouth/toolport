import assert from "node:assert/strict";
import test from "node:test";
import { needsRust, requireResults } from "./ci-policy.mjs";

test("frontend-only PRs skip unchanged native code", () => {
  for (const files of [
    ["src/App.tsx"],
    ["public/logo.svg"],
    ["index.html", "vite.config.ts", "tsconfig.app.json"],
  ])
    assert.equal(needsRust("pull_request", files), false);
});

test("docs, packaging and non-CI workflow PRs skip native checks", () => {
  for (const files of [
    ["docs/design.md"],
    ["README.md", "src/lib/README.md", "LICENSE"],
    [".github/ISSUE_TEMPLATE/bug_report.yml", ".github/PULL_REQUEST_TEMPLATE.md"],
    [".coderabbit.yaml", ".vscode/settings.json"],
    ["packaging/linux/native/PKGBUILD", "packaging/homebrew/toolport.rb"],
    [".github/workflows/release.yml"],
    [".github/workflows/docker-publish.yml", ".github/workflows/winget.yml"],
  ])
    assert.equal(needsRust("pull_request", files), false, files.join());
});

test("native, shared, unknown, empty diffs and main pushes run native checks", () => {
  for (const file of [
    "src-tauri/src/lib.rs",
    "package-lock.json",
    "package.json",
    "scripts/install.sh",
    ".github/workflows/ci.yml",
    ".github/scripts/ci-policy.mjs",
    ".github/workflows/nested/x.yml",
    "new-file",
  ])
    assert.equal(needsRust("pull_request", ["src/App.tsx", file]), true);
  assert.equal(needsRust("pull_request", []), true);
  assert.equal(
    needsRust("pull_request", ["docs/a.md", ".github/workflows/ci.yml"]),
    true,
  );
  assert.equal(needsRust("pull_request", ["packaging/x", "scripts/install.sh"]), true);
  assert.equal(needsRust("push", ["docs/a.md"]), true);
  assert.equal(needsRust("push", ["src/App.tsx"]), true);
});

function results(selected) {
  return Object.fromEntries(
    [
      "changes",
      "frontend",
      "installer-script",
      "installer-script-bash",
      "pinned-install-urls",
      "build-test",
      "cross-platform-rust",
      "linux-native",
    ].map((job) => [
      job,
      {
        result:
          selected === "false" &&
          ["build-test", "cross-platform-rust", "linux-native"].includes(job)
            ? "skipped"
            : "success",
        outputs: job === "changes" ? { rust: selected } : {},
      },
    ]),
  );
}

test("gate accepts successful selected checks and only intentional native skips", () => {
  requireResults(results("true"));
  requireResults(results("false"));
});

test("any failed, canceled, missing or unexpectedly skipped required check blocks", () => {
  for (const selected of ["true", "false"]) {
    for (const job of Object.keys(results(selected))) {
      for (const result of ["failure", "cancelled", undefined, "skipped"]) {
        if (
          selected === "false" &&
          ["build-test", "cross-platform-rust", "linux-native"].includes(job) &&
          result === "skipped"
        )
          continue;
        const needs = results(selected);
        needs[job].result = result;
        assert.throws(() => requireResults(needs));
      }
    }
  }
  assert.throws(() => requireResults(results(undefined)));
  assert.throws(() => requireResults(results("maybe")));
  const needs = results("false");
  needs["build-test"].result = "success";
  assert.throws(() => requireResults(needs));
});
