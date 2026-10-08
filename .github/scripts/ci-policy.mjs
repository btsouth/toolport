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
];

function skipsRust(file) {
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
          "scripts/render-aur.sh",
          "scripts/ci-apt-install.sh",
          "scripts/toolport-preview-rollback.sh",
          "src/test/linux-packaging.test.ts",
        ].includes(file),
    )
  );
}

export function requireResults(needs) {
  for (const job of [
    "changes",
    "frontend",
    "installer-script",
    "installer-script-bash",
    "pinned-install-urls",
  ]) {
    if (needs[job]?.result !== "success") throw new Error(`${job} did not pass`);
  }
  const selected = needs.changes.outputs?.rust;
  if (selected !== "true" && selected !== "false")
    throw new Error("Rust selection is missing or invalid");
  for (const job of ["build-test", "cross-platform-rust", "linux-native", "chaos"]) {
    const expected = selected === "true" ? "success" : "skipped";
    if (needs[job]?.result !== expected)
      throw new Error(`${job}: expected ${expected}, got ${needs[job]?.result}`);
  }
  const packages = needs.changes.outputs?.linux_packages;
  if (packages !== "true" && packages !== "false")
    throw new Error("Linux package selection is missing or invalid");
  const expected = packages === "true" ? "success" : "skipped";
  if (needs["linux-packages"]?.result !== expected)
    throw new Error(
      `linux-packages: expected ${expected}, got ${needs["linux-packages"]?.result}`,
    );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv[2] === "gate") {
    requireResults(JSON.parse(process.env.CI_NEEDS));
    console.log("All selected checks passed");
  } else if (process.argv[2] === "select") {
    let files = [];
    const event = process.env.CI_EVENT;
    if (event === "pull_request" || event === "push") {
      const refs = [process.env.CI_BASE_SHA, process.env.CI_HEAD_SHA];
      if (refs.some((ref) => !/^[a-f0-9]{40}$/.test(ref ?? "")))
        throw new Error("Invalid source revision");
      // The first push has no before commit. Select checks conservatively.
      if (refs[0] !== "0".repeat(40)) {
        files = execFileSync("git", ["diff", "--name-only", "-z", ...refs], {
          encoding: "utf8",
        })
          .split("\0")
          .filter(Boolean);
      }
    }
    const rust = needsRust(process.env.CI_EVENT, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `rust=${rust}\n`);
    const packages = needsLinuxPackages(event, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `linux_packages=${packages}\n`);
    console.log(`Linux packages ${packages ? "selected" : "unchanged"}`);
    console.log(`Native checks ${rust ? "selected" : "unchanged"}`);
  } else throw new Error("Expected select or gate");
}
