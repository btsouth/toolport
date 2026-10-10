// Pins the in-repo Homebrew cask snapshot to package.json so a release
// bump that forgets packaging/homebrew/toolport.rb fails CI. brew install
// reads btsouth/homebrew-toolport, not this file; RELEASING.md is the
// step that updates the live tap. SBS-936.
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Not import.meta.url: under the jsdom environment vitest serves modules from
// an http:// URL, so file-relative resolution breaks. Vitest's cwd is the repo
// root (where vite.config.ts lives).
const repoRoot = process.cwd();
const cask = readFileSync(join(repoRoot, "packaging", "homebrew", "toolport.rb"), "utf8");
const pkg = JSON.parse(readFileSync(join(repoRoot, "package.json"), "utf8")) as {
  version: string;
};

const versionMatch = cask.match(/^\s*version\s+"([^"]+)"/m);
const armSha = cask.match(/on_arm do\s+sha256 "([0-9a-f]{64})"/);
const intelSha = cask.match(/on_intel do\s+sha256 "([0-9a-f]{64})"/);

describe("homebrew cask snapshot (packaging/homebrew/toolport.rb)", () => {
  it("stays in version lockstep with package.json", () => {
    // RELEASING.md bumps this file with the rest; this is the backstop.
    // The live tap is a different repo and is not visible to CI.
    expect(versionMatch?.[1]).toBe(pkg.version);
  });

  it("declares two distinct 64-hex sha256s for on_arm and on_intel", () => {
    expect(armSha?.[1], "on_arm sha256").toMatch(/^[0-9a-f]{64}$/);
    expect(intelSha?.[1], "on_intel sha256").toMatch(/^[0-9a-f]{64}$/);
    expect(armSha?.[1]).not.toBe(intelSha?.[1]);
  });

  it("keeps recovery data and directs users to explicit removal before uninstall", () => {
    expect(cask).not.toMatch(/^\s*zap\s/m);
    expect(cask).toContain("Uninstall and zap keep your data and credentials");
    expect(cask).toContain('"Remove Toolport data" in Settings before uninstalling');
    expect(cask).toContain("--disconnect-all");
  });

  it("points at the published darwin dmgs for this version", () => {
    expect(cask).toContain(
      "releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
    );
    expect(cask).toContain(
      "releases/download/v#{version}/Toolport_x86_64-apple-darwin.dmg",
    );
  });
});
