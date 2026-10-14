import assert from "node:assert/strict";
import test from "node:test";
import {
  needsInstallers,
  needsLinuxPackages,
  needsRust,
  requireResults,
} from "./ci-policy.mjs";

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

function results(selected, packages = "true", installers = "true") {
  return Object.fromEntries(
    [
      "changes",
      "frontend",
      "linux-packages",
      "windows-installer",
      "macos-payload",
      "installer-script",
      "installer-script-bash",
      "pinned-install-urls",
      "clippy",
      "rustfmt",
      "build-test",
      "cross-platform-rust",
      "linux-native",
      "chaos",
    ].map((job) => [
      job,
      {
        result:
          ["windows-installer", "macos-payload"].includes(job) && installers === "false"
            ? "skipped"
            : job === "linux-packages" && packages === "false"
              ? "skipped"
              : selected === "false" &&
                  [
                    "clippy",
                    "rustfmt",
                    "build-test",
                    "cross-platform-rust",
                    "linux-native",
                    "chaos",
                  ].includes(job)
                ? "skipped"
                : "success",
        outputs:
          job === "changes"
            ? { rust: selected, linux_packages: packages, installers }
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
      "scripts/render-aur.sh",
      "scripts/ci-apt-install.sh",
      "scripts/toolport-preview-rollback.sh",
      ".github/workflows/ci.yml",
      ".github/workflows/linux-packages.yml",
      ".github/workflows/release.yml",
      ".github/workflows/aur.yml",
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
  for (const invalid of [undefined, "maybe"]) {
    assert.throws(() => requireResults(results(invalid)));
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
