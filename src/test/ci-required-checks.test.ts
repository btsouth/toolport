// Pins SBS-874: branch protection required contexts are only the check named
// "Build + test". If that name sits on the Linux-only job again, a red
// Windows keyring run or a red install.ps1 Pester job cannot block merge.
// These are file-content assertions against .github/workflows/ci.yml so a
// rename or a dropped `needs` fails CI instead of silently reopening the hole.
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const repoRoot = process.cwd();
const ciYaml = readFileSync(join(repoRoot, ".github", "workflows", "ci.yml"), "utf8");

/** Job blocks under `jobs:` keyed by id. Keys are the 2-space identifiers. */
function parseJobs(yaml: string): Record<string, string> {
  const marker = "\njobs:\n";
  const jobsIdx = yaml.indexOf(marker);
  if (jobsIdx < 0) {
    throw new Error("ci.yml has no jobs: block");
  }
  const body = yaml.slice(jobsIdx + marker.length);
  const re = /^[ ]{2}([a-z][a-z0-9-]*):$/gm;
  const matches = [...body.matchAll(re)];
  const jobs: Record<string, string> = {};
  for (let i = 0; i < matches.length; i++) {
    const id = matches[i][1];
    const start = matches[i].index ?? 0;
    const end =
      i + 1 < matches.length ? (matches[i + 1].index ?? body.length) : body.length;
    jobs[id] = body.slice(start, end);
  }
  return jobs;
}

function jobDisplayName(block: string): string | undefined {
  const match = block.match(/^[ ]{4}name:\s*(.+)$/m);
  return match?.[1]?.trim();
}

function jobNeeds(block: string): string[] {
  const inline = block.match(/^[ ]{4}needs:\s*\[([^\]]+)\]/m);
  if (inline) {
    return inline[1]
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  }
  const list: string[] = [];
  const lines = block.split("\n");
  let inNeeds = false;
  for (const line of lines) {
    if (/^[ ]{4}needs:\s*$/.test(line)) {
      inNeeds = true;
      continue;
    }
    if (inNeeds) {
      const item = line.match(/^[ ]{6}-\s+(\S+)\s*$/);
      if (item) {
        list.push(item[1]);
        continue;
      }
      if (/^[ ]{4}\S/.test(line) || /^[ ]{2}[a-z]/.test(line)) {
        break;
      }
    }
  }
  return list;
}

describe("CI required merge gate (SBS-874)", () => {
  const jobs = parseJobs(ciYaml);

  it("puts the required check name on a gate that needs Windows rust and install.ps1", () => {
    const namedBuildPlusTest = Object.entries(jobs).filter(
      ([, block]) => jobDisplayName(block) === "Build + test",
    );
    expect(namedBuildPlusTest).toHaveLength(1);
    const [id, block] = namedBuildPlusTest[0];
    expect(id).not.toBe("build-test");
    expect(block).toMatch(/^[ ]{4}if:\s*always\(\)\s*$/m);
    const needs = jobNeeds(block);
    expect(needs).toEqual(
      expect.arrayContaining(["build-test", "cross-platform-rust", "installer-script"]),
    );
    expect(needs).not.toContain("clippy");
  });

  it("keeps the Linux suite under a different check name", () => {
    expect(jobDisplayName(jobs["build-test"])).toBe("Linux build + test");
  });

  it("still runs install.ps1 Pester as Installer script tests", () => {
    const block = jobs["installer-script"];
    expect(block).toBeDefined();
    expect(jobDisplayName(block)).toBe("Installer script tests");
    expect(block).toContain("scripts/install.Tests.ps1");
  });

  it("still runs Windows keyring tests on windows-latest", () => {
    const block = jobs["cross-platform-rust"];
    expect(block).toBeDefined();
    expect(block).toMatch(/windows-latest/);
    expect(block).toContain("scripts/test-rust-windows.ps1");
  });
});
