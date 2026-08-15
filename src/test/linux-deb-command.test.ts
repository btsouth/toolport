// SBS-846: the Linux .deb must put a `toolport` command on PATH. The crate
// binary stays `conduit` (compat alias); AppImage install is already `toolport`.
// These are file-content + prefix-install assertions so CI can check the
// packaging hook without running `tauri build`.
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const repoRoot = process.cwd();
const wrapperPath = join(repoRoot, "packaging", "linux", "deb", "toolport");
const installShPath = join(repoRoot, "scripts", "install.sh");
const tauriConfPath = join(repoRoot, "src-tauri", "tauri.conf.json");
const cargoTomlPath = join(repoRoot, "src-tauri", "Cargo.toml");

function linuxAptBranch(script: string): string {
  const marker =
    "if command -v dpkg >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then";
  const start = script.indexOf(marker);
  expect(start, "install.sh is missing the apt/dpkg .deb branch").toBeGreaterThan(-1);
  const after = script.slice(start + marker.length);
  const end = after.search(/\n {4}return\n/);
  expect(end, "could not find the apt/dpkg branch return").toBeGreaterThan(-1);
  return after.slice(0, end);
}

function sayLines(branch: string): string[] {
  return [...branch.matchAll(/^\s*say\s+"([^"]*)"/gm)].map((m) => m[1]);
}

describe("install.sh apt path (SBS-846)", () => {
  const script = readFileSync(installShPath, "utf8");
  const aptBranch = linuxAptBranch(script);

  it("tells apt users to run toolport", () => {
    const says = sayLines(aptBranch);
    expect(says.some((line) => /\brun: toolport\b/.test(line))).toBe(true);
  });

  it("does not tell apt users to run conduit after the deb path", () => {
    const says = sayLines(aptBranch);
    expect(says.some((line) => /\bconduit\b/.test(line))).toBe(false);
    expect(aptBranch).not.toMatch(/run:\s*conduit\b/);
  });

  it("still installs the AppImage as bindir/toolport", () => {
    expect(script).toContain('curl -fsSL "$url" -o "$bindir/toolport"');
    expect(script).toContain('chmod +x "$bindir/toolport"');
    expect(script).toContain("Installed the AppImage to $bindir/toolport");
  });
});

describe("Linux .deb toolport packaging hook (SBS-846)", () => {
  it("ships a /usr/bin/toolport wrapper in the .deb only", () => {
    const conf = JSON.parse(readFileSync(tauriConfPath, "utf8")) as {
      identifier?: string;
      mainBinaryName?: string;
      bundle?: {
        linux?: {
          deb?: { files?: Record<string, string> };
          appimage?: { files?: Record<string, string> };
        };
      };
    };
    expect(conf.identifier).toBe("com.tsout.conduit");
    expect(conf.mainBinaryName).toBeUndefined();
    expect(conf.bundle?.linux?.deb?.files?.["/usr/bin/toolport"]).toBe(
      "../packaging/linux/deb/toolport",
    );
    expect(conf.bundle?.linux?.appimage).toBeUndefined();
  });

  it("keeps the Cargo package and app bin named conduit", () => {
    const cargo = readFileSync(cargoTomlPath, "utf8");
    expect(cargo).toMatch(/^name = "conduit"$/m);
    expect(cargo).toMatch(/^default-run = "conduit"$/m);
    const binBlock = cargo.slice(cargo.indexOf("[[bin]]"));
    const nextTable = binBlock.indexOf("\n[");
    const firstBin = nextTable === -1 ? binBlock : binBlock.slice(0, nextTable);
    expect(firstBin).toMatch(/^name = "conduit"$/m);
    expect(firstBin).toMatch(/^path = "src\/main\.rs"$/m);
  });

  // Actually runs the wrapper, so it needs a POSIX shell and a real exec bit.
  // On Windows `chmodSync` is a no-op and execFileSync on an extensionless
  // `#!/bin/sh` file is ENOENT, so this one case is Linux/macOS only. CI runs
  // the suite on ubuntu, so the coverage that matters is unaffected; this only
  // stops `npm test` failing on a Windows dev box.
  it.skipIf(process.platform === "win32")(
    "installs a toolport command that execs the conduit binary",
    () => {
      const prefix = mkdtempSync(join(tmpdir(), "sbs-846-deb-"));
      try {
        writeFileSync(
          join(prefix, "conduit"),
          '#!/bin/sh\nprintf "conduit-ran"\n[ $# -gt 0 ] && printf " %s" "$@"\nprintf "\\n"\n',
        );
        chmodSync(join(prefix, "conduit"), 0o755);
        copyFileSync(wrapperPath, join(prefix, "toolport"));
        chmodSync(join(prefix, "toolport"), 0o755);

        const viaAbs = execFileSync(join(prefix, "toolport"), ["--version"], {
          encoding: "utf8",
        });
        expect(viaAbs).toBe("conduit-ran --version\n");

        const viaPath = execFileSync("toolport", ["launch"], {
          encoding: "utf8",
          env: { ...process.env, PATH: prefix },
        });
        expect(viaPath).toBe("conduit-ran launch\n");
      } finally {
        rmSync(prefix, { recursive: true, force: true });
      }
    },
  );
});
