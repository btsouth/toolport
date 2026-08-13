// Conformance checks for packaging/agent-plugin/toolport against the Agent
// Plugins 1.0 spec (https://github.com/agentplugins/agent-plugins-spec) and
// the Claude Code plugin layout. These are file-content assertions, not a
// runtime test: they exist so a rename, a moved launcher, or a version bump
// that skips the plugin manifest fails CI instead of shipping a broken zip.
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, posix, win32 } from "node:path";
import {
  gatewayCandidates,
  spawnFirst,
  validateGatewayOverride,
} from "../../packaging/agent-plugin/toolport/bin/launch-gateway.mjs";

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
const releaseWorkflow = readFileSync(
  join(repoRoot, ".github", "workflows", "release.yml"),
  "utf8",
);

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
      expect(server.args).toContain("${PLUGIN_ROOT}/bin/launch-gateway.mjs");
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
      expect(server.args).toContain("${CLAUDE_PLUGIN_ROOT}/bin/launch-gateway.mjs");
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

describe("agent plugin gateway launcher", () => {
  const blindFs = {
    readFileSync: () => {
      throw new Error("hidden");
    },
    readdirSync: () => {
      throw new Error("hidden");
    },
    statSync: () => {
      throw new Error("hidden");
    },
  };

  it("tries constructed Windows paths even when filesystem probes are hidden", () => {
    const candidates = gatewayCandidates({
      platform: "win32",
      home: "C:\\Users\\me",
      env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
      fsOps: blindFs,
      version: pkg.version,
    });
    expect(candidates).toContain(
      win32.join("L:\\Local", "Toolport", "toolport-gateway.exe"),
    );
    expect(candidates).toContain(
      win32.join("R:\\Roaming", "Toolport", "bin", `toolport-gateway-${pkg.version}.exe`),
    );
  });

  it("keeps legacy Conduit gateway layouts on every desktop OS", () => {
    expect(
      gatewayCandidates({
        platform: "win32",
        home: "C:\\Users\\me",
        env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
        fsOps: blindFs,
      }),
    ).toContain(win32.join("L:\\Local", "Conduit", "conduit-gateway.exe"));
    expect(
      gatewayCandidates({
        platform: "darwin",
        home: "/Users/me",
        env: {},
        fsOps: blindFs,
      }),
    ).toContain(
      posix.join("/Applications", "Conduit.app", "Contents", "MacOS", "conduit-gateway"),
    );
    expect(
      gatewayCandidates({
        platform: "linux",
        home: "/home/me",
        env: {},
        fsOps: blindFs,
      }),
    ).toContain(posix.join("/home/me", ".config", "Conduit", "bin", "conduit-gateway"));
  });

  it("finds an older versioned Conduit gateway when its manifest is unreadable", () => {
    const legacyBin = win32.join("R:\\Roaming", "Conduit", "bin");
    const fsOps = {
      readFileSync: () => {
        throw new Error("unreadable manifest");
      },
      readdirSync: (path: string) =>
        path === legacyBin ? ["conduit-gateway-1.11.0.exe"] : [],
      statSync: () => ({ mtimeMs: 1 }),
    };
    expect(
      gatewayCandidates({
        platform: "win32",
        home: "C:\\Users\\me",
        env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
        fsOps,
      }),
    ).toContain(win32.join(legacyBin, "conduit-gateway-1.11.0.exe"));
  });

  it("prefers the newest published gateway over its own pinned version", () => {
    // The zip is never auto-updated, so an installed plugin's version goes stale
    // while the app publishes newer gateways. Guessing the pinned filename ahead
    // of a real directory scan would spawn old gateway code that the app's prune
    // deliberately kept (referenced by another client, or recent enough).
    const binDir = win32.join("R:\\Roaming", "Toolport", "bin");
    const newer = win32.join(binDir, "toolport-gateway-1.13.0.exe");
    const pinned = win32.join(binDir, "toolport-gateway-1.12.0.exe");
    const candidates = gatewayCandidates({
      platform: "win32",
      home: "C:\\Users\\me",
      env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
      fsOps: {
        // No manifest, so the fallbacks are all that is left to order.
        readFileSync: () => {
          throw new Error("unreadable manifest");
        },
        readdirSync: (path: string) =>
          path === binDir
            ? ["toolport-gateway-1.12.0.exe", "toolport-gateway-1.13.0.exe"]
            : [],
        statSync: (path: string) => ({ mtimeMs: path === newer ? 2 : 1 }),
      },
      version: "1.12.0",
    });
    expect(candidates).toContain(newer);
    expect(candidates.indexOf(newer)).toBeLessThan(candidates.indexOf(pinned));
  });

  it("only scans versioned gateway images, not lookalike neighbours", () => {
    // Explorer's duplicate names and hand-made backups sit in the same directory
    // and usually have the newest mtime. Only one scanned path is returned, so
    // letting one of these win would hide the real newest binary entirely. The
    // publisher cannot produce these names (gateway_publish.rs's version suffix
    // is digit-led, dotted, and `[A-Za-z0-9._+-]` only).
    const binDir = win32.join("R:\\Roaming", "Toolport", "bin");
    const impostors = [
      "toolport-gateway-1.12.0 - Copy.exe",
      "toolport-gateway-1.12.0 (1).exe",
      "toolport-gateway-backup.exe",
      "toolport-gateway-1.13.0.exe.sig",
    ];
    const candidates = gatewayCandidates({
      platform: "win32",
      home: "C:\\Users\\me",
      env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
      fsOps: {
        readFileSync: () => {
          throw new Error("unreadable manifest");
        },
        readdirSync: (path: string) =>
          path === binDir ? [...impostors, "toolport-gateway-1.13.0.exe"] : [],
        // Every impostor is newer than the real image.
        statSync: (path: string) => ({
          mtimeMs: path.endsWith("toolport-gateway-1.13.0.exe") ? 1 : 9,
        }),
      },
      version: "1.12.0",
    });
    for (const impostor of impostors) {
      expect(candidates, impostor).not.toContain(win32.join(binDir, impostor));
    }
    // The real image still beats this plugin's own pinned guess.
    expect(
      candidates.indexOf(win32.join(binDir, "toolport-gateway-1.13.0.exe")),
    ).toBeLessThan(candidates.indexOf(win32.join(binDir, "toolport-gateway-1.12.0.exe")));
  });

  it("still accepts content-addressed published names", () => {
    // `<gateway>-<version>-<digest><exe>` is a real publisher output and must
    // survive the stricter scan.
    const binDir = win32.join("R:\\Roaming", "Toolport", "bin");
    const addressed = "toolport-gateway-1.13.0-a1b2c3d4e5f6.exe";
    const candidates = gatewayCandidates({
      platform: "win32",
      home: "C:\\Users\\me",
      env: { APPDATA: "R:\\Roaming", LOCALAPPDATA: "L:\\Local" },
      fsOps: {
        readFileSync: () => {
          throw new Error("unreadable manifest");
        },
        readdirSync: (path: string) => (path === binDir ? [addressed] : []),
        statSync: () => ({ mtimeMs: 1 }),
      },
      version: "1.12.0",
    });
    expect(candidates).toContain(win32.join(binDir, addressed));
  });

  it("falls through an unspawnable candidate to a working binary", async () => {
    await expect(
      spawnFirst([join(repoRoot, "missing-gateway"), process.execPath], {
        args: ["-e", "process.exit(0)"],
        stdio: "ignore",
      }),
    ).resolves.toBe(0);
  });

  it("reports a missing candidate and rejects relative overrides", async () => {
    await expect(
      spawnFirst([join(repoRoot, "missing-gateway")], { stdio: "ignore" }),
    ).rejects.toMatchObject({ code: "ENOENT" });
    expect(() => validateGatewayOverride("./gateway")).toThrow(
      "TOOLPORT_GATEWAY must be an absolute path",
    );
  });

  it("packages and uploads the zip independently of native builds", () => {
    const job = releaseWorkflow
      .split("\n  agent-plugin:\n")[1]
      ?.split("\n  updater-manifest:\n")[0];
    expect(job).toBeDefined();
    expect(job).not.toMatch(/^\s+needs:/m);
    expect(job).toContain("toolport-agent-plugin.zip");
    expect(job).toContain("Upload the agent plugin");
  });
});
