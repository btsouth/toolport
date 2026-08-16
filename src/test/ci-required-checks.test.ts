// Pins SBS-874: branch protection required contexts are only the check named
// "Build + test". If that name sits on the Linux-only job again, a red Windows
// keyring run or a red install.ps1 Pester job cannot block merge.
//
// These assertions run against the PARSED .github/workflows/ci.yml, not raw
// text. Substring checks were not enough: `windows-latest` also appears in
// step `if:` guards, so dropping it from `strategy.matrix.os` used to keep a
// text-matching test green while the Windows cell never ran. Likewise the gate
// job carries `if: always()`, so a step that only echoes the dependency
// results would keep the required check green while Linux, the Rust matrix, or
// install.ps1 was red. Each helper below is exercised against a fixture that
// reopens the hole, so a helper that stops biting fails its own test.
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parse } from "yaml";

/** The single required context configured in branch protection. */
const REQUIRED_CHECK_NAME = "Build + test";

/** Jobs that must be green before the required check can be green. */
const GATED_JOB_IDS = ["build-test", "cross-platform-rust", "installer-script"];

interface WorkflowStep {
  name?: string;
  if?: string;
  run?: string;
  uses?: string;
  shell?: string;
  "continue-on-error"?: boolean;
}

interface WorkflowJob {
  name?: string;
  if?: string;
  needs?: string | string[];
  "runs-on"?: string;
  "continue-on-error"?: boolean;
  strategy?: { "fail-fast"?: boolean; matrix?: Record<string, unknown> };
  steps?: WorkflowStep[];
}

interface Workflow {
  jobs?: Record<string, WorkflowJob>;
}

function parseJobs(source: string): Record<string, WorkflowJob> {
  const workflow = parse(source) as Workflow | null;
  const jobs = workflow?.jobs;
  if (!jobs || typeof jobs !== "object") {
    throw new Error("workflow has no jobs: block");
  }
  return jobs;
}

/** GitHub falls back to the job id when a job has no display `name:`. */
function jobsWithCheckName(
  jobs: Record<string, WorkflowJob>,
  checkName: string,
): [string, WorkflowJob][] {
  return Object.entries(jobs).filter(([id, job]) => (job.name ?? id) === checkName);
}

function needsOf(job: WorkflowJob): string[] {
  const needs = job.needs;
  if (needs === undefined) return [];
  return Array.isArray(needs) ? needs : [needs];
}

/** The `strategy.matrix.os` list, or undefined when it is absent or dynamic. */
function matrixOs(job: WorkflowJob): string[] | undefined {
  const os = job.strategy?.matrix?.os;
  if (!Array.isArray(os)) return undefined;
  return os.map(String);
}

/** Every `run:` script in the job, flattened, regardless of step guards. */
function runScripts(job: WorkflowJob): string[] {
  return (job.steps ?? []).flatMap((step) => (step.run ? [step.run] : []));
}

/**
 * Dependency ids that the job actually gates on: ids whose
 * `needs.<id>.result` is compared against `success` by a shell test that
 * aborts the step on mismatch.
 *
 * Only steps that always run and always count are considered, so neither
 * `continue-on-error: true` nor an `if:` guard that skips the step on failure
 * can be used to smuggle a check past this. A bare
 * `echo "${{ needs.x.result }}"` line does not count either: it never changes
 * the exit code.
 */
function failClosedResultChecks(job: WorkflowJob): string[] {
  if (job["continue-on-error"] === true) return [];
  const gatingSteps = (job.steps ?? []).filter((step) => {
    if (step["continue-on-error"] === true) return false;
    const guard = step.if?.trim();
    return guard === undefined || guard === "always()" || guard === "${{ always() }}";
  });
  // Matches `test "${{ needs.x.result }}" = "success"` and the `[ ... ]` /
  // `[[ ... ]]` spellings, with or without quotes, `=` or `==`.
  const comparison =
    /(?:^|\n|;|&&|\|\|)[ \t]*(?:test|\[\[?)[ \t]+"?\$\{\{[ \t]*needs\.([A-Za-z0-9_-]+)\.result[ \t]*\}\}"?[ \t]*={1,2}[ \t]*"?success"?/g;
  const gated = new Set<string>();
  for (const script of gatingSteps.flatMap((step) => (step.run ? [step.run] : []))) {
    for (const match of script.matchAll(comparison)) {
      gated.add(match[1]);
    }
  }
  return [...gated].sort();
}

const ciYaml = readFileSync(
  join(process.cwd(), ".github", "workflows", "ci.yml"),
  "utf8",
);
const jobs = parseJobs(ciYaml);

describe("CI required merge gate (SBS-874)", () => {
  it("puts the required check name on a gate that needs Windows rust and install.ps1", () => {
    const named = jobsWithCheckName(jobs, REQUIRED_CHECK_NAME);
    expect(named).toHaveLength(1);
    const [id, gate] = named[0];
    expect(id).not.toBe("build-test");
    // Without always() the gate is skipped when a dependency fails, and a
    // skipped required check does not block merge.
    expect(gate.if?.trim()).toBe("always()");
    expect(needsOf(gate)).toEqual(expect.arrayContaining(GATED_JOB_IDS));
    // Clippy is non-blocking until the existing warnings are cleared.
    expect(needsOf(gate)).not.toContain("clippy");
  });

  it("fails the required check unless every gated job reported success", () => {
    const [, gate] = jobsWithCheckName(jobs, REQUIRED_CHECK_NAME)[0];
    // The point of the gate: `if: always()` means the step runs on failed,
    // skipped, and cancelled dependencies, so it must compare each result
    // against `success` itself.
    expect(failClosedResultChecks(gate)).toEqual([...GATED_JOB_IDS].sort());
  });

  it("keeps the Linux suite under a different check name", () => {
    expect(jobs["build-test"]?.name).toBe("Linux build + test");
  });

  it("still runs install.ps1 Pester as Installer script tests", () => {
    const job = jobs["installer-script"];
    expect(job).toBeDefined();
    expect(job.name).toBe("Installer script tests");
    expect(job["runs-on"]).toBe("windows-latest");
    expect(runScripts(job).join("\n")).toContain("scripts/install.Tests.ps1");
  });

  it("still runs Windows keyring tests on a windows-latest matrix cell", () => {
    const job = jobs["cross-platform-rust"];
    expect(job).toBeDefined();
    // The list itself, not a substring of the block: `windows-latest` also
    // appears in step `if:` guards, so those must not stand in for a cell.
    expect(matrixOs(job)).toContain("windows-latest");
    expect(runScripts(job).join("\n")).toContain("scripts/test-rust-windows.ps1");
  });
});

// Fixtures that reopen the exact holes above. If a helper stops biting, these
// stop failing and this block goes red.
describe("the gate assertions reject a workflow that reopens the hole", () => {
  it("rejects a gate step that only echoes the dependency results", () => {
    const fixture = parseJobs(`
jobs:
  merge-gate:
    name: Build + test
    if: always()
    needs: [build-test, cross-platform-rust, installer-script]
    runs-on: ubuntu-22.04
    steps:
      - name: Require everything
        run: |
          echo "build-test=\${{ needs.build-test.result }}"
          echo "cross-platform-rust=\${{ needs.cross-platform-rust.result }}"
          echo "installer-script=\${{ needs.installer-script.result }}"
`);
    const gate = jobsWithCheckName(fixture, REQUIRED_CHECK_NAME)[0][1];
    // Passes the name / always() / needs assertions, but gates nothing.
    expect(needsOf(gate)).toEqual(expect.arrayContaining(GATED_JOB_IDS));
    expect(failClosedResultChecks(gate)).toEqual([]);
  });

  it("rejects result checks parked behind continue-on-error or a skip guard", () => {
    const fixture = parseJobs(`
jobs:
  merge-gate:
    name: Build + test
    if: always()
    needs: [build-test, cross-platform-rust, installer-script]
    runs-on: ubuntu-22.04
    steps:
      - name: Soft check
        continue-on-error: true
        run: test "\${{ needs.build-test.result }}" = "success"
      - name: Guarded check
        if: needs.cross-platform-rust.result == 'success'
        run: test "\${{ needs.cross-platform-rust.result }}" = "success"
      - name: Real check
        run: test "\${{ needs.installer-script.result }}" = "success"
`);
    const gate = jobsWithCheckName(fixture, REQUIRED_CHECK_NAME)[0][1];
    expect(failClosedResultChecks(gate)).toEqual(["installer-script"]);
  });

  it("rejects a matrix that mentions windows-latest only in step guards", () => {
    const fixture = parseJobs(`
jobs:
  cross-platform-rust:
    name: Headless Rust tests (\${{ matrix.os }})
    strategy:
      matrix:
        os: [ubuntu-22.04, macos-latest]
    runs-on: \${{ matrix.os }}
    steps:
      - name: Rust tests (Windows, no desktop)
        if: matrix.os == 'windows-latest'
        run: powershell -ExecutionPolicy Bypass -File scripts/test-rust-windows.ps1
`);
    const job = fixture["cross-platform-rust"];
    // The old substring pins both still pass on this workflow.
    expect(JSON.stringify(job)).toContain("windows-latest");
    expect(runScripts(job).join("\n")).toContain("scripts/test-rust-windows.ps1");
    // The parsed matrix does not.
    expect(matrixOs(job)).not.toContain("windows-latest");
  });
});
