// Conformance checks for packaging/agent-plugin/toolport against the Agent
// Plugins 1.0 spec (https://github.com/agentplugins/agent-plugins-spec) and
// the Claude Code plugin layout. These are file-content assertions, not a
// runtime test: they exist so a rename, a moved launcher, or a version bump
// that skips the plugin manifest fails CI instead of shipping a broken zip.
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

// Not import.meta.url: under the jsdom environment vitest serves modules from
// an http:// URL, so file-relative resolution breaks. Vitest's cwd is the repo
// root (where vite.config.ts lives).
const repoRoot = process.cwd();
const pluginRoot = join(repoRoot, "packaging", "agent-plugin", "toolport");

const readJson = (...segments: string[]) =>
  JSON.parse(readFileSync(join(pluginRoot, ...segments), "utf8"));

const manifest = readJson("plugin.json");
const mcp = readJson("mcp.json");
const claudeManifest = readJson(".claude-plugin", "plugin.json");
const claudeMcp = readJson(".mcp.json");
const pkg = JSON.parse(readFileSync(join(repoRoot, "package.json"), "utf8"));

// Spec 1.0.0: name is 1-64 chars of [a-z0-9.-], alphanumeric at both ends, no
// consecutive hyphens or periods.
const NAME_PATTERN = /^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/;

interface McpServerEntry {
  type?: string;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
}

describe("agent plugin manifest (plugin.json)", () => {
  it("declares the 1.0.0 schema and a spec-valid name", () => {
    expect(manifest.$schema).toBe(
      "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
    );
    expect(manifest.name).toBe("toolport");
    expect(manifest.name).toMatch(NAME_PATTERN);
    expect(manifest.name.length).toBeLessThanOrEqual(64);
  });

  it("only uses top-level fields the closed schema allows", () => {
    const allowed = new Set([
      "$schema",
      "name",
      "version",
      "description",
      "author",
      "homepage",
      "repository",
      "license",
      "keywords",
      "extensions",
    ]);
    for (const key of Object.keys(manifest)) {
      expect(allowed, `unexpected top-level field "${key}"`).toContain(key);
    }
    // Components live in fixed locations, never in the manifest.
    expect(manifest.mcpServers).toBeUndefined();
    expect(manifest.skills).toBeUndefined();
  });

  it("stays in version lockstep with package.json", () => {
    // RELEASING.md step 1 bumps this file with the rest; this is the backstop.
    expect(manifest.version).toBe(pkg.version);
    expect(claudeManifest.version).toBe(pkg.version);
  });

  it("mirrors the Claude Code manifest", () => {
    expect(claudeManifest.name).toBe(manifest.name);
    expect(claudeManifest.description).toBe(manifest.description);
  });
});

describe("agent plugin MCP config (mcp.json)", () => {
  it("declares the matching 1.0.0 MCP schema", () => {
    // A schema-version mismatch between plugin.json and mcp.json makes
    // conformant clients skip MCP loading entirely.
    expect(mcp.$schema).toBe("https://agent-plugins.org/schemas/1.0.0/mcp.schema.json");
  });

  it("defines a stdio server whose launcher exists inside the plugin", () => {
    const servers = Object.entries(mcp.mcpServers as Record<string, McpServerEntry>);
    expect(servers.length).toBeGreaterThan(0);
    for (const [id, server] of servers) {
      expect(server.type).toBe("stdio");
      // The spec requires a single bare token or ./relative path — no
      // placeholders and no embedded arguments.
      expect(server.command).toBe("node");
      expect(server.command).not.toMatch(/[\s$]/);
      for (const arg of server.args ?? []) {
        const m = /^\$\{PLUGIN_ROOT\}\/(.+)$/.exec(arg);
        if (m) {
          expect(existsSync(join(pluginRoot, m[1])), `${id}: missing ${m[1]}`).toBe(true);
        }
        expect(arg).not.toContain("${PLUGIN_DATA}"); // launcher takes no data-dir args
      }
      // Expansion never happens in env *keys*, and these two are reserved.
      expect(Object.keys(server.env ?? {})).not.toContain("PLUGIN_ROOT");
      expect(Object.keys(server.env ?? {})).not.toContain("PLUGIN_DATA");
    }
  });

  it("keeps the Claude Code MCP config pointing at the same launcher", () => {
    const servers = Object.values(claudeMcp.mcpServers as Record<string, McpServerEntry>);
    expect(servers.length).toBeGreaterThan(0);
    for (const server of servers) {
      const launcherArg = (server.args ?? []).find((a) =>
        a.startsWith("${CLAUDE_PLUGIN_ROOT}/"),
      );
      expect(launcherArg).toBeDefined();
      const relative = launcherArg!.replace("${CLAUDE_PLUGIN_ROOT}/", "");
      expect(existsSync(join(pluginRoot, relative))).toBe(true);
    }
  });
});

describe("agent plugin components", () => {
  it("ships at least one skill with valid SKILL.md frontmatter", () => {
    const skillsDir = join(pluginRoot, "skills");
    const skillDirs = readdirSync(skillsDir).filter((entry) =>
      statSync(join(skillsDir, entry)).isDirectory(),
    );
    expect(skillDirs.length).toBeGreaterThan(0);
    for (const dir of skillDirs) {
      const skillFile = join(skillsDir, dir, "SKILL.md");
      expect(existsSync(skillFile), `${dir} has no SKILL.md`).toBe(true);
      const text = readFileSync(skillFile, "utf8");
      const frontmatter = /^---\n([\s\S]+?)\n---/.exec(text);
      expect(frontmatter, `${dir}: SKILL.md has no frontmatter`).not.toBeNull();
      expect(frontmatter![1]).toMatch(/^name:\s*\S+/m);
      expect(frontmatter![1]).toMatch(/^description:\s*\S+/m);
    }
  });

  it("has a syntactically valid launcher", () => {
    // `node --check` parses without executing; catches a broken edit before
    // the zip ships. process.execPath is the node running vitest.
    execFileSync(process.execPath, [
      "--check",
      join(pluginRoot, "bin", "launch-gateway.mjs"),
    ]);
  });
});
