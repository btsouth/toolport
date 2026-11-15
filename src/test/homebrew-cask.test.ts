// packaging/homebrew/toolport.rb is the only source of the tap's cask: the
// tap's bump workflow copies it at each release tag and fills in version and
// sha256. These tests pin its version to package.json and its uninstall and
// zap behaviour to docs/uninstall.md. SBS-936.
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

  it("restores clients on uninstall, never on upgrade or reinstall", () => {
    const preflight = cask.match(/uninstall_preflight do([\s\S]*?)\n {2}end\n/)?.[1];
    expect(preflight).toBeDefined();
    // Homebrew runs this block for upgrades and reinstalls too. The call stack
    // and the brew command line tell them apart, and both must agree.
    for (const marker of [
      "/cask/upgrade.rb",
      "/cask/reinstall.rb",
      "start_upgrade",
      "uninstall_existing_cask",
      '"/bin/ps", args: ["-o", "command=", "-p", Process.pid.to_s]',
      "%r{/Homebrew/brew\\.rb\\s+(\\S+)}",
      "%w[upgrade reinstall install].include?(brew_command)",
      "%w[uninstall rm remove].include?(brew_command)",
      "if stack_replacing && command_replacing",
      "elsif stack_replacing || !command_uninstall",
      "Toolport could not tell whether Homebrew is upgrading or removing it",
    ]) {
      expect(preflight).toContain(marker);
    }
    // The test-only switches can only hide a signal, never invent one.
    expect(preflight).toContain(
      'stack_replacing = false if ENV["HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_STACK"] == "1"',
    );
    expect(preflight).toContain(
      'brew_command = nil if ENV["HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_COMMAND"] == "1"',
    );
    expect(preflight).toContain('args: ["--disconnect-all", "--uninstall"]');
    expect(preflight).toContain('args: ["--remove-data", "--confirm"]');
    expect(preflight).toContain('labels.include?("zap")');
    expect(cask).toContain('uninstall quit: "com.tsout.conduit"');
  });

  it("removes data only on zap, and data folders only once emptied", () => {
    const zap = cask.slice(cask.indexOf("  zap trash:"), cask.indexOf("  caveats"));
    const trash = zap.match(/zap trash: \[([\s\S]*?)\]/)?.[1];
    const rmdir = zap.match(/rmdir: \[([\s\S]*?)\]/)?.[1];
    const paths = (list?: string) =>
      [...(list ?? "").matchAll(/"([^"\n]+)"/g)].map((m) => m[1]);
    expect(paths(trash)).toEqual([
      "~/Library/Application Support/com.tsout.conduit",
      "~/Library/Caches/com.tsout.conduit",
      "~/Library/HTTPStorages/com.tsout.conduit",
      "~/Library/Preferences/com.tsout.conduit.plist",
      "~/Library/Saved Application State/com.tsout.conduit.savedState",
      "~/Library/WebKit/com.tsout.conduit",
    ]);
    // The data folders are never trashed: Toolport's own removal empties them
    // and keeps the registry when a credential could not be removed.
    expect(trash).not.toMatch(
      /Application Support\/(Toolport|Conduit)|registry|conduit-mcp/,
    );
    expect(paths(rmdir)).toEqual([
      "~/Library/Application Support/Conduit",
      "~/Library/Application Support/Toolport",
    ]);
  });

  it("matches what docs/uninstall.md says the cask does", () => {
    const docs = readFileSync(join(repoRoot, "docs", "uninstall.md"), "utf8");
    expect(docs).toContain("brew uninstall --cask toolport");
    expect(docs).toContain("Upgrades and reinstalls skip it");
    expect(docs).toContain("brew uninstall --cask --zap toolport");
    expect(docs).toContain("--remove-data --confirm");
    expect(docs).toContain("Plain `brew uninstall` never removes");
    expect(cask).toContain(
      "brew uninstall restores every client's config and keeps Toolport's data",
    );
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
