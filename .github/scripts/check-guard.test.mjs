import assert from "node:assert/strict";
import test from "node:test";
import { classify, explanation } from "./check-guard.mjs";

const reasons = (files) => classify(files).map((f) => `${f.file}: ${f.reason}`);

test("ordinary source and test changes pass", () => {
  assert.deepEqual(
    reasons([
      { filename: "src/App.tsx", status: "modified", patch: "@@\n-old\n+new" },
      {
        filename: "src/App.catalog.test.tsx",
        status: "modified",
        patch: '@@\n+  it("shows the new row", () => {})',
      },
      {
        filename: "src-tauri/tests/catalog_launch.rs",
        status: "added",
        patch: "@@\n+#[test]\n+fn launches() {}",
      },
    ]),
    [],
  );
});

test("any change to check selection or review rules is flagged", () => {
  for (const filename of [
    ".github/scripts/ci-policy.mjs",
    ".github/scripts/check-guard.mjs",
    ".github/workflows/check-guard.yml",
    "REVIEW.md",
  ])
    assert.equal(
      classify([{ filename, status: "modified", patch: "@@\n+one more line" }]).length,
      1,
      filename,
    );
});

test("check files are flagged only when they shrink", () => {
  for (const filename of [
    ".github/workflows/ci.yml",
    "scripts/verify.mjs",
    "scripts/browser-smoke.mjs",
    "scripts/check-locales.mjs",
    "benchmark/bundle.mjs",
    "vitest.config.ts",
    "eslint.config.mjs",
    "AGENTS.md",
    ".claude/skills/verify/SKILL.md",
    ".agents/skills/verify/SKILL.md",
  ]) {
    assert.deepEqual(
      classify([{ filename, status: "modified", patch: "@@\n-a\n+a2\n+b" }]),
      [],
      filename,
    );
    assert.equal(
      classify([{ filename, status: "modified", patch: "@@\n-a\n-b\n+c" }]).length,
      1,
      filename,
    );
    assert.equal(classify([{ filename, status: "removed" }]).length, 1, filename);
  }
});

test("deleted or moved test files are flagged", () => {
  assert.deepEqual(reasons([{ filename: "src/lib/api.test.ts", status: "removed" }]), [
    "src/lib/api.test.ts: deletes a test file",
  ]);
  assert.deepEqual(
    reasons([
      {
        filename: "src/lib/api.old.ts",
        previous_filename: "src/lib/api.test.ts",
        status: "renamed",
      },
    ]),
    ["src/lib/api.old.ts: moves test file src/lib/api.test.ts out of the test paths"],
  );
});

test("skips, retries and swallowed failures are flagged", () => {
  const patch = [
    "@@ -1,2 +1,6 @@",
    '+  it.skip("flaky", () => {})',
    "+  retries: 2,",
    "+        continue-on-error: true",
    "+        run: npm test || true",
  ].join("\n");
  assert.deepEqual(
    reasons([{ filename: ".github/workflows/x.yml", status: "modified", patch }]).sort(),
    [
      ".github/workflows/x.yml: ignores a failing command in CI",
      ".github/workflows/x.yml: lets a CI step fail without failing the job",
      ".github/workflows/x.yml: retries failing tests",
      ".github/workflows/x.yml: skips or narrows tests",
    ],
  );
  assert.deepEqual(
    reasons([
      {
        filename: "scripts/release.sh",
        status: "modified",
        patch: "@@\n+rm -f x || true",
      },
    ]),
    [],
    "|| true outside workflows is ordinary shell",
  );
});

test("ignoring an existing Rust test is flagged, a new desktop-only test is not", () => {
  const existing = "@@\n     #[test]\n+    #[ignore]\n     fn old() {}";
  assert.deepEqual(
    reasons([
      { filename: "src-tauri/src/router.rs", status: "modified", patch: existing },
    ]),
    ["src-tauri/src/router.rs: ignores an existing Rust test"],
  );
  const added =
    '@@\n+    #[test]\n+    #[ignore = "requires an isolated GTK desktop; run in omabox"]\n+    fn fits() {}';
  assert.deepEqual(
    reasons([
      { filename: "src-tauri/src/linux_native/mod.rs", status: "modified", patch: added },
    ]),
    [],
  );
});

test("a net loss of test cases across the PR is flagged, a move or rewrite is not", () => {
  const lost = '@@\n-  it("a", () => {})\n-  test("b", () => {})\n+  it("a2", () => {})';
  assert.deepEqual(
    reasons([{ filename: "src/x.test.ts", status: "modified", patch: lost }]),
    ["src/x.test.ts: removes 1 test case overall (from src/x.test.ts)"],
  );
  const rust =
    "@@\n-    #[test]\n-    fn old() {}\n-    #[tokio::test]\n-    async fn other() {}";
  assert.deepEqual(
    reasons([{ filename: "src-tauri/src/router.rs", status: "modified", patch: rust }]),
    [
      "src-tauri/src/router.rs: removes 2 test cases overall (from src-tauri/src/router.rs)",
    ],
  );
  const moved = [
    {
      filename: "src-tauri/src/oauth.rs",
      status: "modified",
      patch: "@@\n-    #[test]\n-    fn reaped() {}",
    },
    {
      filename: "src-tauri/src/hostenv.rs",
      status: "modified",
      patch: "@@\n+    #[test]\n+    fn reaped() {}",
    },
  ];
  assert.deepEqual(reasons(moved), []);
  const rewrite = '@@\n-  it("a", () => {})\n+  it("a, renamed", () => {})';
  assert.deepEqual(
    reasons([{ filename: "src/x.test.ts", status: "modified", patch: rewrite }]),
    [],
  );
});

test("the PR body explanation is found in common forms", () => {
  assert.equal(
    explanation("## Notes\nCheck changes: CI moved to Node 24"),
    "CI moved to Node 24",
  );
  assert.equal(
    explanation("- **Check changes:** dropped a duplicate smoke step"),
    "dropped a duplicate smoke step",
  );
  assert.equal(explanation("check changes:   "), null);
  assert.equal(explanation("No checks changed here."), null);
  assert.equal(explanation(null), null);
});
