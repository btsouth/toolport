import assert from "node:assert/strict";
import test from "node:test";
import {
  DraftRun,
  fullRun,
  headlessMatrix,
  needsInstallers,
  needsLinuxPackages,
  needsRust,
  requireResults,
} from "./ci-policy.mjs";

test("frontend-only PRs skip unchanged native code", () => {
  for (const files of [
    ["src/App.tsx"],
    ["src/locales/de.ts", "src/locales/GLOSSARY.md", "src/lib/api.ts"],
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
    ["parity/scenarios/servers-empty.json", "parity/known-differences.json"],
  ])
    assert.equal(needsRust("pull_request", files), false, files.join());
});

test("native, shared, unknown, empty diffs and main pushes run native checks", () => {
  for (const file of [
    "src/locales/shell/en.json",
    "src/locales/native/de.json",
    "src/lib/personalSync.ts",
    "src/lib/secretRefs.ts",
    "src/lib/teamUrl.ts",
    "src/lib/teamsPlan.ts",
    "docs/clients.md",
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

const RUST_JOBS = [
  "clippy",
  "rustfmt",
  "build-test",
  "cross-platform-rust",
  "linux-native",
  "chaos",
];
const FULL_ONLY = ["windows-installer", "macos-payload", "linux-appimage"];

function results(selected, packages = "true", installers = "true", full = "true") {
  const expected = (job) => {
    if (FULL_ONLY.includes(job))
      return full === "true" && installers === "true" ? "success" : "skipped";
    if (job === "installer-script") return full === "false" ? "skipped" : "success";
    if (job === "linux-packages") return packages === "false" ? "skipped" : "success";
    if (RUST_JOBS.includes(job)) return selected === "false" ? "skipped" : "success";
    return "success";
  };
  return Object.fromEntries(
    [
      "changes",
      "frontend",
      "linux-packages",
      ...FULL_ONLY,
      "installer-script",
      "installer-script-bash",
      "pinned-install-urls",
      ...RUST_JOBS,
    ].map((job) => [
      job,
      {
        result: expected(job),
        outputs:
          job === "changes"
            ? {
                rust: selected,
                linux_packages: packages,
                installers,
                full,
                headless_matrix: JSON.stringify(headlessMatrix(full !== "false")),
              }
            : {},
      },
    ]),
  );
}

test("Linux packages only run for relevant PR and push changes", () => {
  for (const event of ["pull_request", "push"]) {
    for (const file of ["README.md", "docs/design.md", "src/App.tsx", "public/logo.svg"])
      assert.equal(needsLinuxPackages(event, [file]), false, `${event}: ${file}`);
    for (const file of [
      "src-tauri/src/linux_native/settings.rs",
      "src-tauri/build.rs",
      "src-tauri/Cargo.toml",
      "src-tauri/Cargo.lock",
      "Cargo.toml",
      "packaging/linux/native/nfpm.yaml",
      "packaging/agent-plugin/toolport/a.txt",
      "scripts/build-linux-packages.sh",
      "scripts/test-linux-packages.sh",
      "scripts/test-uninstall-hooks.py",
      "scripts/test-package-removal.sh",
      "scripts/package-removal-fixture.py",
      "scripts/test-nsis-hooks.sh",
      "scripts/install-nfpm.sh",
      "scripts/ci-apt-install.sh",
      ".github/actions/apt-install/action.yml",
      "scripts/toolport-preview-rollback.sh",
      ".github/workflows/ci.yml",
      ".github/workflows/linux-packages.yml",
      ".github/workflows/release.yml",
      ".github/scripts/ci-policy.mjs",
      ".github/scripts/ci-policy.test.mjs",
      "src/test/linux-packaging.test.ts",
    ])
      assert.equal(
        needsLinuxPackages(event, ["README.md", file]),
        true,
        `${event}: ${file}`,
      );
    assert.equal(needsLinuxPackages(event, []), true);
  }
  assert.equal(needsLinuxPackages("release", ["README.md"]), true);
});

test("only an explicit draft pull request skips the full set", () => {
  assert.equal(fullRun("pull_request", "true"), false);
  for (const draft of ["false", "", undefined, "TRUE", "1"])
    assert.equal(fullRun("pull_request", draft), true, String(draft));
  for (const event of ["push", "workflow_dispatch", "merge_group", undefined])
    assert.equal(fullRun(event, "true"), true, String(event));
});

test("full runs test macOS once and Linux and Windows split; drafts test Linux", () => {
  const full = headlessMatrix(true);
  assert.deepEqual(
    full.map(({ os, group }) => `${os}/${group}`),
    [
      "ubuntu-22.04/core",
      "ubuntu-22.04/integration",
      "macos-latest/all",
      "windows-latest/core",
      "windows-latest/integration",
    ],
  );
  const targets = (os) =>
    full
      .filter((entry) => entry.os === os)
      .map((entry) => entry.targets)
      .join(" ");
  for (const os of ["ubuntu-22.04", "macos-latest", "windows-latest"])
    assert.equal(targets(os), "--lib --bins --test '*'", os);
  assert.deepEqual(headlessMatrix(false), full.slice(0, 2));
});

test("a draft gate stays red after its Linux checks pass", () => {
  for (const rust of ["true", "false"])
    for (const packages of ["true", "false"])
      for (const installers of ["true", "false"])
        assert.throws(
          () => requireResults(results(rust, packages, installers, "false")),
          (error) => error instanceof DraftRun && /ready for review/.test(error.message),
        );
});

test("a draft gate still reports a failing Linux check first", () => {
  const needs = results("true", "true", "true", "false");
  needs["build-test"].result = "failure";
  assert.throws(
    () => requireResults(needs),
    (error) => !(error instanceof DraftRun) && /build-test/.test(error.message),
  );
});

test("a full run cannot pass without every headless OS in its matrix", () => {
  for (const matrix of [headlessMatrix(false), [], undefined, "[]"]) {
    const needs = results("true");
    needs.changes.outputs.headless_matrix =
      typeof matrix === "string" || matrix === undefined
        ? matrix
        : JSON.stringify(matrix);
    assert.throws(() => requireResults(needs), /matrix/);
  }
});

test("gate accepts successful selected checks and intentional skips", () => {
  for (const rust of ["true", "false"])
    for (const packages of ["true", "false"])
      for (const installers of ["true", "false"])
        requireResults(results(rust, packages, installers));
});

test("any failed, canceled, missing or unexpectedly skipped required check blocks", () => {
  for (const selected of ["true", "false"]) {
    for (const packages of ["true", "false"]) {
      for (const installers of ["true", "false"]) {
        for (const job of Object.keys(results(selected, packages, installers))) {
          const expected = results(selected, packages, installers)[job].result;
          for (const result of [
            "success",
            "failure",
            "cancelled",
            undefined,
            "skipped",
          ]) {
            if (result === expected) continue;
            const needs = results(selected, packages, installers);
            needs[job].result = result;
            assert.throws(() => requireResults(needs));
          }
        }
      }
    }
  }
  for (const job of Object.keys(results("true", "true", "true", "false"))) {
    const expected = results("true", "true", "true", "false")[job].result;
    for (const result of ["success", "failure", "cancelled", undefined, "skipped"]) {
      if (result === expected) continue;
      const needs = results("true", "true", "true", "false");
      needs[job].result = result;
      assert.throws(
        () => requireResults(needs),
        (error) => !(error instanceof DraftRun),
        `${job}: ${result}`,
      );
    }
  }
  for (const invalid of [undefined, "maybe"]) {
    assert.throws(() => requireResults(results(invalid)));
    const fullNeeds = results("true");
    fullNeeds.changes.outputs.full = invalid;
    assert.throws(() => requireResults(fullNeeds));
    const needs = results("true");
    needs.changes.outputs.linux_packages = invalid;
    assert.throws(() => requireResults(needs));
    const installerNeeds = results("true");
    installerNeeds.changes.outputs.installers = invalid;
    assert.throws(() => requireResults(installerNeeds));
  }
});

test("installers only run for packaging inputs on PRs and pushes", () => {
  for (const event of ["pull_request", "push"]) {
    for (const file of ["src/App.tsx", "public/logo.svg", "README.md", "docs/design.md"])
      assert.equal(needsInstallers(event, [file]), false, `${event}: ${file}`);
    for (const file of [
      "src-tauri/src/lib.rs",
      "src-tauri/tauri.bundle.conf.json",
      "src-tauri/nsis-hooks.nsh",
      "src-tauri/Cargo.toml",
      "src-tauri/Cargo.lock",
      "Cargo.lock",
      "package.json",
      "package-lock.json",
      "scripts/prepare-sidecar.mjs",
      "scripts/ci-apt-install.sh",
      ".github/actions/apt-install/action.yml",
      "scripts/patch-appimage.sh",
      "scripts/test-windows-installer.py",
      "scripts/test-nsis-hooks.sh",
      ".github/scripts/package-contents.mjs",
      ".github/scripts/ci-msi-config.mjs",
      ".github/scripts/ci-policy.mjs",
      ".github/workflows/ci.yml",
      ".github/workflows/release.yml",
      "packaging/windows/hooks.nsh",
    ])
      assert.equal(
        needsInstallers(event, ["src/App.tsx", file]),
        true,
        `${event}: ${file}`,
      );
    assert.equal(needsInstallers(event, []), true);
  }
  assert.equal(needsInstallers("workflow_dispatch", ["README.md"]), true);
});
