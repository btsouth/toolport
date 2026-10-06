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
    (/^\.github\/workflows\/[^/]+\.yml$/.test(file) && file !== ".github/workflows/ci.yml")
  );
}

export function needsRust(event, files) {
  return (
    event !== "pull_request" ||
    files.length === 0 ||
    files.some((file) => !skipsRust(file))
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
  for (const job of ["build-test", "cross-platform-rust"]) {
    const expected = selected === "true" ? "success" : "skipped";
    if (needs[job]?.result !== expected)
      throw new Error(`${job}: expected ${expected}, got ${needs[job]?.result}`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv[2] === "gate") {
    requireResults(JSON.parse(process.env.CI_NEEDS));
    console.log("All selected checks passed");
  } else if (process.argv[2] === "select") {
    let files = [];
    if (process.env.CI_EVENT === "pull_request") {
      const refs = [process.env.CI_BASE_SHA, process.env.CI_HEAD_SHA];
      if (refs.some((ref) => !/^[a-f0-9]{40}$/.test(ref ?? "")))
        throw new Error("Invalid pull request source revision");
      files = execFileSync("git", ["diff", "--name-only", "-z", ...refs], {
        encoding: "utf8",
      })
        .split("\0")
        .filter(Boolean);
    }
    const rust = needsRust(process.env.CI_EVENT, files);
    appendFileSync(process.env.GITHUB_OUTPUT, `rust=${rust}\n`);
    console.log(`Native checks ${rust ? "selected" : "unchanged"}`);
  } else throw new Error("Expected select or gate");
}
