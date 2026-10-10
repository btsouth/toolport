import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { assertManifest } from "./package-manifest.mjs";
import process from "node:process";
import console from "node:console";
import { pathToFileURL } from "node:url";

const allowed = new Set([
  "conduit",
  "toolport",
  "Toolport",
  "toolport-gtk",
  "toolport-gateway",
  "conduit-gateway",
  "conduit.exe",
  "toolport-gateway.exe",
  "uninstall.exe",
  "AppRun",
]);
export function assertContents(paths, { nsisInstaller = false, appImage = false } = {}) {
  for (const path of paths) {
    if (/mock-mcp-server|\/deps\/|\/examples\//i.test(path))
      throw new Error(`Test artifact shipped: ${path}`);
    // The installer extracts its incoming gateway here before replacing files.
    if (nsisInstaller && path === "$PLUGINSDIR/toolport-preflight.exe") continue;
    // Tauri adds this runtime helper for the app's deep-link protocols.
    if (appImage && path === "squashfs-root/usr/bin/xdg-mime") continue;
    const name = path.split("/").at(-1);
    if (
      name &&
      (/\.exe$/i.test(name) || /\/(?:s?bin|MacOS)\/[^/]+$/.test(path)) &&
      !allowed.has(name)
    )
      throw new Error(`Unexpected packaged binary: ${path}`);
  }
}
const run = (command, args) =>
  execFileSync(command, args, {
    encoding: "utf8",
    timeout: 120000,
    maxBuffer: 32 * 1024 * 1024,
  });
export function appImageOffset(bytes) {
  let offset = -1;
  // The runtime can contain the magic as a machine-code constant. Check the
  // superblock too, so only the appended SquashFS filesystem is selected.
  for (
    let candidate = bytes.indexOf("hsqs");
    candidate >= 0;
    candidate = bytes.indexOf("hsqs", candidate + 1)
  ) {
    if (candidate + 96 > bytes.length) continue;
    const blockSize = bytes.readUInt32LE(candidate + 12);
    const used = bytes.readBigUInt64LE(candidate + 40);
    if (
      bytes.readUInt16LE(candidate + 28) === 4 &&
      bytes.readUInt16LE(candidate + 30) === 0 &&
      blockSize >= 4096 &&
      (blockSize & (blockSize - 1)) === 0 &&
      used >= 96n &&
      used <= BigInt(bytes.length - candidate)
    ) {
      offset = candidate;
      break;
    }
  }
  return offset;
}
export function listContents(file) {
  if (file.endsWith(".deb")) {
    return run("bash", [
      "-o",
      "pipefail",
      "-c",
      'dpkg-deb --fsys-tarfile "$1" | tar -tf -',
      "contents",
      file,
    ]).split(/\r?\n/);
  }
  if (file.endsWith(".rpm")) return run("rpm", ["-qlp", file]).split(/\r?\n/);
  if (/\.(?:tar\.gz|pkg\.tar\.zst)$/.test(file))
    return run("tar", ["-tf", file]).split(/\r?\n/);
  if (file.endsWith(".app")) {
    const entries = [];
    const walk = (directory) => {
      for (const entry of readdirSync(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name);
        entries.push(path);
        if (entry.isDirectory()) walk(path);
      }
    };
    if (!statSync(file).isDirectory()) throw new Error("App bundle is not a directory");
    walk(file);
    return entries;
  }
  if (file.endsWith(".msi")) {
    return JSON.parse(
      run("powershell", [
        "-NoProfile",
        "-NonInteractive",
        "-File",
        ".github/scripts/list-msi.ps1",
        file,
      ]),
    );
  }
  if (file.endsWith(".exe"))
    return run("7z", ["l", "-slt", file])
      .split(/\r?\n/)
      .filter((line) => line.startsWith("Path = "))
      .slice(1)
      .map((line) => line.slice(7).replaceAll("\\", "/"));
  if (file.endsWith(".AppImage")) {
    const bytes = readFileSync(file);
    const offset = appImageOffset(bytes);
    if (offset < 0) throw new Error("No AppImage squashfs found");
    return run("unsquashfs", ["-o", String(offset), "-l", file])
      .split(/\r?\n/)
      .filter((line) => line.startsWith("squashfs-root/"));
  }
  if (file.endsWith(".dmg")) {
    const mount = `${process.env.RUNNER_TEMP}/toolport-payload-${process.pid}`;
    run("hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mount, file]);
    try {
      return run("find", [mount, "-type", "f"])
        .split(/\r?\n/)
        .map((path) => path.slice(mount.length + 1));
    } finally {
      run("hdiutil", ["detach", mount]);
    }
  }
  throw new Error(`Unsupported artifact: ${file}`);
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.length < 3) throw new Error("Provide package archives");
  for (const file of process.argv.slice(2)) {
    const paths = listContents(file);
    if (!paths.some((path) => /(?:^|\/)toolport-gateway(?:\.exe)?$/.test(path)))
      throw new Error(`Gateway missing: ${file}`);
    console.log(`${file}\n${paths.join("\n")}`);
    assertContents(paths, {
      nsisInstaller: file.endsWith(".exe"),
      appImage: file.endsWith(".AppImage"),
    });
    const kind = file.endsWith(".exe")
      ? "nsis"
      : file.endsWith(".msi")
        ? "msi"
        : file.endsWith(".AppImage")
          ? "appimage"
          : /\.(?:app|dmg|tar\.gz)$/.test(file)
            ? "mac"
            : file.endsWith(".pkg.tar.zst")
              ? "pacman"
              : paths.some((path) => path.endsWith("/toolport-gtk"))
                ? "native"
                : "tauri-deb";
    assertManifest(paths, kind);
    console.log(`PASS: ${file} matches the ${kind} payload manifest`);
  }
}
