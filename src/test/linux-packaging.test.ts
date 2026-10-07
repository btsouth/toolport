// Pins the two Linux packaging fixes for modern Wayland / rolling distros.
//
// 1. The AppImage's `GDK_BACKEND=x11` must be a DEFAULT, not an OVERRIDE.
//    linuxdeploy-plugin-gtk writes it into a hook AppRun sources AFTER the
//    caller's environment, so `GDK_BACKEND=wayland` is silently ignored. On a
//    Wayland session whose Xwayland cannot survive the app that is fatal: the
//    first launch kills Xwayland session-wide and every launch after it blocks
//    forever on the orphaned X socket with no window and no error.
// 2. The AppImage must not bundle libwayland-*. AppRun puts the bundle on
//    LD_LIBRARY_PATH, so the HOST's Mesa - which is deliberately not bundled -
//    resolves against it, and libEGL_mesa fails to load with
//    `undefined symbol: wl_fixes_interface`. The window then never paints.
// GTK system packages replace the Tauri .deb; the AppImage remains a fallback.
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parse } from "yaml";

interface Step {
  name?: string;
  if?: string;
  run?: string;
  env?: Record<string, string>;
}
interface Job {
  if?: string;
  env?: Record<string, string>;
  steps?: Step[];
}
interface Workflow {
  on?: unknown;
  jobs?: Record<string, Job>;
}

function read(...parts: string[]): string {
  return readFileSync(join(process.cwd(), ...parts), "utf8");
}
function workflow(name: string): Workflow {
  return parse(read(".github", "workflows", name)) as Workflow;
}

const PATCH_SCRIPT = "scripts/patch-appimage.sh";
const patchScript = read(...PATCH_SCRIPT.split("/"));

// The exact line linuxdeploy-plugin-gtk writes, trailing comment and all.
const LINUXDEPLOY_LINE =
  "export GDK_BACKEND=x11 # Crash with Wayland backend on Wayland - We tested it" +
  " without it and ended up with this: https://github.com/tauri-apps/tauri/issues/8541";

// Run the script's ACTUAL sed line rather than a JS transliteration of it: the
// pattern is a POSIX BRE and the first version of it looked right in JS while
// being wrong in sed. Skipped where GNU sed is unavailable (BSD sed on a mac dev
// box); CI runs the frontend tests on ubuntu-22.04, which always has it.
const SED_LINE = patchScript.match(/^sed -i .*"\$hook"$/m)?.[0];

function hasGnuSed(): boolean {
  try {
    return execFileSync("bash", ["-c", "sed --version"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).includes("GNU sed");
  } catch {
    return false;
  }
}
const gnuSed = hasGnuSed();

function applyScriptSed(input: string): string {
  const dir = mkdtempSync(join(tmpdir(), "toolport-gdk-"));
  const file = join(dir, "hook.sh");
  try {
    writeFileSync(file, input + "\n");
    execFileSync("bash", ["-c", `hook=${JSON.stringify(file)}; ${SED_LINE}`], {
      stdio: ["ignore", "ignore", "pipe"],
    });
    return readFileSync(file, "utf8").replace(/\n$/, "");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

describe("AppImage GDK_BACKEND is a default, not an override", () => {
  it("has a substitution line to test", () => {
    expect(SED_LINE, `no 'sed -i' line found in ${PATCH_SCRIPT}`).toBeDefined();
  });

  it.skipIf(!gnuSed)(
    "rewrites linuxdeploy's line to an overridable assignment, comment intact",
    () => {
      const out = applyScriptSed(LINUXDEPLOY_LINE);
      expect(out).toContain('export GDK_BACKEND="${GDK_BACKEND:-x11}"');
      // The trailing comment carries the reason linuxdeploy forced it; keep it.
      expect(out).toContain("tauri-apps/tauri/issues/8541");
      // Nothing may still assign x11 unconditionally.
      expect(out).not.toMatch(/^export GDK_BACKEND=x11(\s|$)/m);
    },
  );

  it.skipIf(!gnuSed)("leaves an unrelated GDK_BACKEND assignment alone", () => {
    expect(applyScriptSed("export GDK_BACKEND=wayland")).toBe(
      "export GDK_BACKEND=wayland",
    );
    // Not anchored to a substring of a longer value.
    expect(applyScriptSed("export GDK_BACKEND=x11,wayland")).toBe(
      "export GDK_BACKEND=x11,wayland",
    );
  });

  it("fails loudly when linuxdeploy stops writing the line it expects", () => {
    // A silent no-op here would ship an AppImage that looks patched and is not.
    expect(patchScript).toMatch(/if ! grep -qF "\$old_line" "\$hook"; then/);
    expect(patchScript).toMatch(/exit 1/);
  });

  it("verifies the repack before overwriting the release artifact", () => {
    expect(patchScript).toContain("--appimage-extract");
    // The repacked image is re-extracted and re-checked, and the file count is
    // compared, so a repack that quietly dropped files cannot ship.
    expect(patchScript).toContain("does not contain the patched hook");
    expect(patchScript).toContain("the repack changed the file count");
    // The original runtime is reused rather than a downloaded appimagetool's.
    expect(patchScript).toContain("--appimage-offset");
  });

  it("refuses to repack an image whose xattrs it cannot carry over", () => {
    // --appimage-extract does not restore xattrs, and the file-count check
    // cannot see a dropped capability bit, so the source is checked instead.
    expect(patchScript).toMatch(/xattrs are \(present\|stored\)/);
    expect(patchScript).toContain("cannot restore");
    // And no -no-xattrs, which would throw away anything that did survive.
    expect(patchScript).not.toContain("-no-xattrs");
  });
});

describe("the AppImage does not bundle the host's wayland libraries", () => {
  it("removes every libwayland-* it finds in the AppDir", () => {
    // Searched across the whole AppDir rather than one fixed path, so a
    // linuxdeploy layout change cannot make the removal silently miss them.
    expect(patchScript).toMatch(
      /find "\$appdir" \\\( -type f -o -type l \\\) -name 'libwayland-\*\.so\*'/,
    );
  });

  it("fails the release if any survive the repack", () => {
    // A silent miss here ships an AppImage that opens a grey window on every
    // Mesa driver, which is the bug this whole step exists to prevent.
    expect(patchScript).toMatch(
      /find "\$repacked" \\\( -type f -o -type l \\\) -name 'libwayland-\*\.so\*'/,
    );
    expect(patchScript).toContain("survived the repack");
  });

  it("matches symlinks, not just regular files", () => {
    // linuxdeploy copies these four as plain files today, but it does emit `.so`
    // symlinks elsewhere in the same AppDir. Were the SONAME ever a link,
    // `-type f` would remove the target, ship a dangling `libwayland-client.so.0`
    // that the loader still finds by SONAME, and pass verification. Asserted on
    // BOTH searches: a one-sided fix is the silent-success case again.
    const searches =
      patchScript.match(/find "\$(?:appdir|repacked)"[^|\n]*libwayland[^|\n]*/g) ?? [];
    expect(searches).toHaveLength(2);
    for (const search of searches) {
      expect(search).toContain("-type l");
      expect(search).not.toMatch(/-type f -name/);
    }
  });

  it("leaves the rest of the bundle alone", () => {
    // Not a general "unbundle system libraries" pass: the payload still needs
    // its own GTK and WebKitGTK. Only the wayland family goes.
    const removals = patchScript.match(/^\s*rm -f .*$/gm) ?? [];
    expect(removals.length).toBeGreaterThan(0);
    for (const line of removals) {
      expect(line).toContain("$lib");
    }
  });
});

describe("release.yml runs the AppImage patch and re-signs", () => {
  const build = workflow("release.yml").jobs?.build;
  const step = (build?.steps ?? []).find((s) => s.run?.includes(PATCH_SCRIPT));

  it("has the patch step, gated to the Linux matrix leg", () => {
    expect(step, "no release.yml step runs " + PATCH_SCRIPT).toBeDefined();
    expect(step!.if).toBe("matrix.os == 'ubuntu-22.04'");
  });

  it("re-signs, because the patch invalidates the updater signature", () => {
    // Rewriting the AppImage changes the bytes `tauri build` signed. Shipping the
    // stale .sig would break auto-update for every Linux user.
    expect(step!.run).toContain("tauri signer sign");
    expect(step!.env?.TAURI_SIGNING_PRIVATE_KEY).toContain(
      "secrets.TAURI_SIGNING_PRIVATE_KEY",
    );
    expect(step!.env?.TAURI_SIGNING_PRIVATE_KEY_PASSWORD).toContain(
      "secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
    );
  });

  it("installs squashfs-tools, which the repack needs", () => {
    const deps = (build?.steps ?? []).find(
      (s) => s.name === "Install Linux build dependencies",
    );
    expect(deps?.run).toContain("squashfs-tools");
  });
});

describe("install.sh installs the pacman package on Arch", () => {
  const installer = read("scripts", "install.sh");

  it("does not reach for an AUR helper on the user's behalf", () => {
    // It mattered most under `curl ... | bash`, where a helper's PKGBUILD
    // review prompt reads stdin - which IS the rest of this script.
    for (const helper of ["paru", "yay", "pamac", "pikaur", "trizen"]) {
      expect(installer).not.toMatch(new RegExp(`^\\s*"?\\$?${helper}\\b.*-S`, "m"));
    }
    expect(installer).not.toContain("for helper in");
    expect(installer).not.toContain("--skipreview");
  });

  it("adds the signed repository and installs the package", () => {
    expect(installer).toContain("install_arch_repo");
    expect(installer).toContain("[toolport]");
    expect(installer).toContain("pacman-key --lsign-key");
    // A full upgrade, never `pacman -Sy <pkg>`: a partial upgrade can leave the
    // system with a newer toolport linked against older libraries.
    expect(installer).toMatch(/pacman -Syu .*\btoolport\b/);
    expect(installer).not.toMatch(/pacman -Sy\s/);
  });

  it("pins the signing key instead of trusting whatever the URL serves", () => {
    // --lsign-key only trusts the fingerprint it is handed, so a key swapped at
    // the host cannot become trusted. The pin is what makes that true.
    expect(installer).toMatch(/^REPO_SIGNING_KEY="[A-F0-9]{40}"$/m);
    expect(installer).toContain('--lsign-key "$repo_key_id"');
  });

  it("detects Arch from os-release, not from pacman being present", () => {
    // A Debian box can have pacman installed; os-release is the honest signal,
    // and ID_LIKE is what catches Omarchy, EndeavourOS and Manjaro.
    expect(installer).toMatch(/\^\(ID\|ID_LIKE\)=.*arch/);
  });

  it("still falls through to the AppImage everywhere else", () => {
    expect(installer).toContain("Installed the AppImage");
  });
});
