import process from "node:process";
import console from "node:console";
import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

// Only a narrow allowlist can skip unchanged native code: the frontend, docs,
// packaging and release workflows other than ci.yml (none feed the Rust jobs;
// the frontend job still tests packaging/ and always runs). Unknown paths,
// dependencies, build scripts, ci.yml, the CI scripts and main pushes run all tests.
const SKIP_RUST_FILES = new Set([
  "index.html",
  "LICENSE",
  ".coderabbit.yaml",
  ".github/PULL_REQUEST_TEMPLATE.md",
]);
const SKIP_RUST_PREFIXES = [
  "src/",
  "public/",
  "docs/",
  "packaging/",
  ".vscode/",
  ".github/ISSUE_TEMPLATE/",
  // Parity scenarios and known differences: read only by the on-demand parity
  // report (npm run parity), never by a CI test.
  "parity/",
];

// Files under those prefixes that the Rust crate reads anyway: the shell
// strings it compiles in (i18n.rs) and the sources its parity tests compare.
const RUST_READS_PREFIXES = ["shell", "connections", "workflows", "common", "native"].map(
  (dir) => `src/locales/${dir}/`,
);
const RUST_READS_FILES = new Set([
  "src/lib/personalSync.ts",
  "src/lib/secretRefs.ts",
  "src/lib/teamUrl.ts",
  "src/lib/teamsPlan.ts",
  "docs/clients.md",
]);

function skipsRust(file) {
  if (RUST_READS_FILES.has(file) || RUST_READS_PREFIXES.some((p) => file.startsWith(p)))
    return false;
  return (
    SKIP_RUST_FILES.has(file) ||
    SKIP_RUST_PREFIXES.some((prefix) => file.startsWith(prefix)) ||
    file.endsWith(".md") ||
    /^(vite\.config|tsconfig[^/]*)\.(ts|json)$/.test(file) ||
    (/^\.github\/workflows\/[^/]+\.yml$/.test(file) &&
      file !== ".github/workflows/ci.yml")
  );
}

export function needsRust(event, files) {
  return (
    event !== "pull_request" ||
    files.length === 0 ||
    files.some((file) => !skipsRust(file))
  );
}

export function needsLinuxPackages(event, files) {
  return (
    !["pull_request", "push"].includes(event) ||
    files.length === 0 ||
    files.some(
      (file) =>
        /^src-tauri\/.*\.rs$/.test(file) ||
        /(^|\/)Cargo\.(toml|lock)$/.test(file) ||
        file.startsWith("packaging/") ||
        file.startsWith(".github/workflows/") ||
        file.startsWith(".github/scripts/ci-policy.") ||
        [
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
          "src/test/linux-packaging.test.ts",
        ].includes(file),
    )
  );
}

// Frontend and docs edits do not change the native installer machinery. Empty
// diffs and manual/unknown events stay conservative, like Linux packages.
export function needsInstallers(event, files) {
  return (
    !["pull_request", "push"].includes(event) ||
    files.length === 0 ||
    files.some(
      (file) =>
        file.startsWith("src-tauri/") ||
        file.startsWith("packaging/") ||
        file.startsWith(".github/workflows/") ||
        file.startsWith(".github/scripts/ci-") ||
        file.startsWith(".github/scripts/package-") ||
        /(^|\/)Cargo\.(toml|lock)$/.test(file) ||
        [
          "package.json",
          "package-lock.json",
          "scripts/ci-apt-install.sh",
          ".github/actions/apt-install/action.yml",
          "scripts/macos-build-dmg.sh",
          "scripts/macos-package-ci.sh",
          "scripts/test-homebrew-cask-lifecycle.sh",
          "scripts/patch-appimage.sh",
          "scripts/prepare-sidecar.mjs",
          "scripts/test-windows-installer.py",
          "scripts/test-nsis-hooks.sh",
        ].includes(file),
    )
  );
}

// Draft pull requests run the Linux checks only. A push to a ready PR, marking a
// PR ready for review, main pushes, dispatch and any unknown event run the full
// set. Anything but an explicit draft flag on a pull_request counts as ready.
export function fullRun(event, draft) {
  return !(event === "pull_request" && draft === "true");
}

// macOS runs core and integration in one job: one runner and one compile of the
// shared library instead of two. Linux and Windows stay split.
const HEADLESS_LINUX = [
  { os: "ubuntu-22.04", group: "core", targets: "--lib --bins" },
  { os: "ubuntu-22.04", group: "integration", targets: "--test '*'" },
];
const HEADLESS_OTHER = [
  { os: "macos-latest", group: "all", targets: "--lib --bins --test '*'" },
  { os: "windows-latest", group: "core", targets: "--lib --bins" },
  { os: "windows-latest", group: "integration", targets: "--test '*'" },
];

export function headlessMatrix(full) {
  return full ? [...HEADLESS_LINUX, ...HEADLESS_OTHER] : [...HEADLESS_LINUX];
}

// Jobs that drafts skip: every macOS and Windows job and the three bundle
// validations. Their result is checked against the selection below.
const FULL_ONLY_INSTALLERS = ["windows-installer", "macos-payload", "linux-appimage"];

export class DraftRun extends Error {}

export function requireResults(needs) {
  for (const job of [
    "changes",
    "frontend",
    "installer-script-bash",
    "pinned-install-urls",
  ]) {
    if (needs[job]?.result !== "success") throw new Error(`${job} did not pass`);
  }
  const fullOutput = needs.changes.outputs?.full;
  if (fullOutput !== "true" && fullOutput !== "false")
    throw new Error("Full run selection is missing or invalid");
  const full = fullOutput === "true";
  const windowsScript = full ? "success" : "skipped";
  if (needs["installer-script"]?.result !== windowsScript)
    throw new Error(
      `installer-script: expected ${windowsScript}, got ${needs["installer-script"]?.result}`,
    );
  const selected = needs.changes.outputs?.rust;
  if (selected !== "true" && selected !== "false")
    throw new Error("Rust selection is missing or invalid");
  for (const job of [
    "clippy",
    "rustfmt",
    "build-test",
    "cross-platform-rust",
    "linux-native",
    "chaos",
  ]) {
    const expected = selected === "true" ? "success" : "skipped";
    if (needs[job]?.result !== expected)
      throw new Error(`${job}: expected ${expected}, got ${needs[job]?.result}`);
  }
  // The headless matrix is data from the select job; a full run must have had
  // every OS in it, so a matrix that lost macOS or Windows cannot pass.
  if (
    selected === "true" &&
    needs.changes.outputs?.headless_matrix !== JSON.stringify(headlessMatrix(full))
  )
    throw new Error("Headless Rust matrix does not match the selection");
  const packages = needs.changes.outputs?.linux_packages;
  if (packages !== "true" && packages !== "false")
    throw new Error("Linux package selection is missing or invalid");
  const expected = packages === "true" ? "success" : "skipped";
  if (needs["linux-packages"]?.result !== expected)
    throw new Error(
      `linux-packages: expected ${expected}, got ${needs["linux-packages"]?.result}`,
    );
  const installers = needs.changes.outputs?.installers;
  if (installers !== "true" && installers !== "false")
    throw new Error("Installer selection is missing or invalid");
  for (const job of FULL_ONLY_INSTALLERS) {
    const expected = full && installers === "true" ? "success" : "skipped";
    if (needs[job]?.result !== expected)
      throw new Error(`${job}: expected ${expected}, got ${needs[job]?.result}`);
  }
  // A skipped or successful gate would count as passing branch protection, and
  // this SHA keeps its check until the ready-for-review run replaces it. So a
  // draft run stays red even when everything it ran passed.
  if (!full)
    throw new DraftRun(
      "Draft pull request: the Linux checks passed, but the macOS, Windows and installer jobs did not run. Mark the PR ready for review to run the full set.",
    );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv[2] === "gate") {
    try {
      requireResults(JSON.parse(process.env.CI_NEEDS));
    } catch (error) {
      console.log(`::error title=Build + test::${error.message}`);
      process.exit(1);
    }
    console.log("All selected checks passed, full set");
  } else if (process.argv[2] === "select") {
    let files = [];
    const event = process.env.CI_EVENT;
    if (event === "pull_request" || event === "push") {
      const refs = [process.env.CI_BASE_SHA, process.env.CI_HEAD_SHA];
      if (refs.some((ref) => !/^[a-f0-9]{40}$/.test(ref ?? "")))
        throw new Error("Invalid source revision");
      // The first push has no before commit. Select checks conservatively.
      if (refs[0] !== "0".repeat(40)) {
        files = execFileSync(
          "git",
          ["diff", "--name-only", "--no-renames", "-z", ...refs],
          {
            encoding: "utf8",
          },
        )
          .split("\0")
          .filter(Boolean);
      }
    }
    const rust = needsRust(process.env.CI_EVENT, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `rust=${rust}\n`);
    const packages = needsLinuxPackages(event, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `linux_packages=${packages}\n`);
    const installers = needsInstallers(event, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `installers=${installers}\n`);
    const full = fullRun(event, process.env.CI_DRAFT);
    appendFileSync(process.env.GITHUB_OUTPUT, `full=${full}\n`);
    appendFileSync(
      process.env.GITHUB_OUTPUT,
      `headless_matrix=${JSON.stringify(headlessMatrix(full))}\n`,
    );
    console.log(
      full ? "Full run" : "Draft PR: macOS, Windows and installer jobs skipped",
    );
    console.log(`Installers ${installers ? "selected" : "unchanged"}`);
    console.log(`Linux packages ${packages ? "selected" : "unchanged"}`);
    console.log(`Native checks ${rust ? "selected" : "unchanged"}`);
  } else throw new Error("Expected select or gate");
}
