// Check guard: flags pull requests that change the checks that judge them.
//
// Runs from the base branch through pull_request_target, so a PR cannot edit
// the rules it is judged by. It never checks out or runs PR code: it reads the
// changed file list and patches from the API and the PR body from the event.
//
// A flagged PR passes once its body has a line starting "Check changes:" that
// says why the check had to change. Mode comes from CHECK_GUARD_MODE:
// off (skip), observe (report only, the default) or enforce (fail).

import console from "node:console";
import { appendFileSync, readFileSync } from "node:fs";
import process from "node:process";
import { pathToFileURL } from "node:url";

// Files that decide which checks run or how review is done. Any change to
// these needs a reason.
const GATE_PATHS = [
  /^\.github\/scripts\/(ci-policy|check-guard)\.mjs$/,
  /^\.github\/workflows\/check-guard\.yml$/,
  /^REVIEW\.md$/,
];

// Files that define checks. Adding coverage is routine, so these need a reason
// only when a change removes more than it adds, or deletes the file.
const CHECK_PATHS = [
  /^\.github\/workflows\//,
  /^\.github\/scripts\//,
  /^\.husky\//,
  /^scripts\/(verify|browser-smoke|smoke-headless|localization-smoke|check-[\w-]+)\.(mjs|ps1|py)$/,
  /^benchmark\/(bundle|token-budget)\.mjs$/,
  /^(vitest\.config\.ts|eslint\.config\.mjs|\.prettierrc[\w.]*|\.prettierignore)$/,
  /^(AGENTS|CLAUDE)\.md$/,
  /(^|\/)\.(claude|agents)\/skills\//,
];

const TEST_FILE =
  /(\.(test|spec)\.[cm]?[jt]sx?$)|(^|\/)tests?\/|(\.Tests\.(ps1|bash)$)|(^|\/)test[-_][\w.-]*\.(sh|py)$|_test\.rs$/;

// Added lines that switch a test or gate off.
const WEAKENING = [
  [/\b(it|test|describe)\.(skip|only|todo)\s*\(/, "skips or narrows tests"],
  [/\bx(it|describe)\s*\(/, "skips tests"],
  [/\btest\.fixme\s*\(/, "skips tests"],
  [/--passWithNoTests\b/, "lets an empty test run pass"],
  [/\bretries\s*:\s*[1-9]/, "retries failing tests"],
  [/continue-on-error\s*:\s*true/, "lets a CI step fail without failing the job"],
];

// A test declaration's first line: #[test], it("name"..., or a call whose name
// or table starts on a later line (it(, it.each([, it.each`).
const TEST_CASE =
  /^\s*(#\[(tokio::)?test\b|(it|test)(\.each\(.*?\))?\s*\(\s*["'`]|(it|test)(\.each)?\s*\(\s*(\[|$)|(it|test)\.each\s*`)/;
const RUST_TEST_ATTR = /^\s*#\[(tokio::)?test\b/;

// Walks a unified diff. Each entry is [kind, text] with kind "+", "-" or " ".
function patchLines(patch) {
  const lines = [];
  for (const line of (patch ?? "").split("\n")) {
    if (line.startsWith("@@") || line.startsWith("+++") || line.startsWith("---"))
      continue;
    if (/^[+ -]/.test(line)) lines.push([line[0], line.slice(1)]);
  }
  return lines;
}

export function classify(files) {
  const findings = [];
  const testCases = new Map(); // file -> net change in test cases
  for (const file of files) {
    const name = file.filename;
    const lines = patchLines(file.patch);
    const added = lines.filter(([k, t]) => k === "+" && t.trim()).length;
    const removed = lines.filter(([k, t]) => k === "-" && t.trim()).length;

    const guarded = (path) =>
      [...GATE_PATHS, ...CHECK_PATHS].some((re) => re.test(path ?? ""));
    if (file.status === "renamed" && guarded(file.previous_filename) && !guarded(name))
      findings.push({
        file: name,
        reason: `moves ${file.previous_filename} out of the checked paths`,
      });

    if (GATE_PATHS.some((re) => re.test(name)))
      findings.push({
        file: name,
        reason: "changes which checks run or how review is done",
      });
    else if (
      CHECK_PATHS.some((re) => re.test(name)) &&
      (file.status === "removed" || removed > added)
    )
      findings.push({
        file: name,
        reason: `shrinks a check or CI gate (${removed} lines out, ${added} in)`,
      });

    if (TEST_FILE.test(name) && file.status === "removed")
      findings.push({ file: name, reason: "deletes a test file" });
    if (
      file.status === "renamed" &&
      TEST_FILE.test(file.previous_filename ?? "") &&
      !TEST_FILE.test(name)
    )
      findings.push({
        file: name,
        reason: `moves test file ${file.previous_filename} out of the test paths`,
      });

    const isWorkflow = /^\.github\/workflows\//.test(name);
    const reasons = new Set();
    // The lines as they are after this PR, for looking at attribute groups.
    const after = lines.filter(([k]) => k !== "-");
    let at = -1;
    let net = 0;
    for (const [kind, text] of lines) {
      if (kind !== "-") at++;
      if (kind === "+") {
        for (const [re, reason] of WEAKENING) if (re.test(text)) reasons.add(reason);
        if (isWorkflow && /\|\|\s*true\b/.test(text))
          reasons.add("ignores a failing command in CI");
        // An ignore on a test this PR adds is how desktop-only tests are
        // written; an ignore on an existing test turns it off. Look through
        // the attribute group around it for an unchanged #[test].
        if (/#\[ignore\b/.test(text) && existingTestAttr(after, at))
          reasons.add("ignores an existing Rust test");
      }
      if (TEST_CASE.test(text)) net += kind === "+" ? 1 : kind === "-" ? -1 : 0;
    }
    for (const reason of reasons) findings.push({ file: name, reason });
    if (file.status !== "removed" && net) testCases.set(name, net);
  }

  // Tests often move between files, so count them across the whole PR.
  const lost = -[...testCases.values()].reduce((a, b) => a + b, 0);
  if (lost > 0) {
    const from = [...testCases].filter(([, n]) => n < 0).map(([f]) => f);
    findings.push({
      file: from[0],
      reason: `removes ${lost} test case${lost === 1 ? "" : "s"} overall (from ${from.join(", ")})`,
    });
  }
  return findings;
}

function existingTestAttr(after, at) {
  for (const step of [-1, 1])
    for (let i = at + step; i >= 0 && i < after.length; i += step) {
      const [kind, text] = after[i];
      if (!/^\s*#\[/.test(text)) break;
      if (kind === " " && RUST_TEST_ATTR.test(text)) return true;
    }
  return false;
}

// The reason must be on the same line as the label and contain a word.
export function explanation(body) {
  const match = /^[ \t>*_-]*check changes[ \t]*:(.*)$/im.exec(body ?? "");
  const reason = match?.[1].replace(/^[\s*_`]+|[\s*_`]+$/g, "");
  return reason && /\w/.test(reason) ? reason : null;
}

async function listFiles(repo, number, token) {
  const files = [];
  for (let page = 1; page <= 30; page++) {
    const res = await globalThis.fetch(
      `https://api.github.com/repos/${repo}/pulls/${number}/files?per_page=100&page=${page}`,
      {
        headers: {
          authorization: `Bearer ${token}`,
          accept: "application/vnd.github+json",
        },
      },
    );
    if (!res.ok)
      throw new Error(`listing PR files failed: ${res.status} ${await res.text()}`);
    const batch = await res.json();
    files.push(...batch);
    if (batch.length < 100) break;
  }
  return files;
}

async function main() {
  const mode = (process.env.CHECK_GUARD_MODE || "observe").toLowerCase();
  if (mode === "off") return console.log("Check guard is off.");

  const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
  const pr = event.pull_request;
  const files = await listFiles(
    process.env.GITHUB_REPOSITORY,
    pr.number,
    process.env.GH_TOKEN,
  );
  const findings = classify(files);
  const why = explanation(pr.body);

  const summary = [];
  if (!findings.length) summary.push("No changes to checks or tests found.");
  else {
    summary.push(`This PR changes how changes are checked (mode: ${mode}).`, "");
    for (const f of findings) summary.push(`- \`${f.file}\`: ${f.reason}`);
    summary.push(
      "",
      why
        ? `Explained in the PR body: ${why}`
        : 'Add a line starting "Check changes:" to the PR body saying why these checks had to change.',
    );
  }
  console.log(summary.join("\n"));
  if (process.env.GITHUB_STEP_SUMMARY)
    appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary.join("\n") + "\n");

  if (findings.length && !why) {
    const level = mode === "enforce" ? "error" : "warning";
    for (const f of findings)
      console.log(
        `::${level} file=${f.file}::Check guard: ${f.reason}. Explain it with "Check changes:" in the PR body.`,
      );
    if (mode === "enforce") process.exitCode = 1;
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) await main();
