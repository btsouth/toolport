import { Buffer } from "node:buffer";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import process from "node:process";
import console from "node:console";
import { pathToFileURL } from "node:url";

const allowed = new Set([
  "conduit",
  "toolport",
  "Toolport",
  "toolport-gtk",
  "toolport-gateway",
  "conduit.exe",
  "toolport-gateway.exe",
  "uninstall.exe",
  "AppRun",
]);
export function assertContents(paths) {
  for (const path of paths) {
    if (/mock-mcp-server|\/deps\/|\/examples\//i.test(path))
      throw new Error(`Test artifact shipped: ${path}`);
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
export function listContents(file) {
  if (file.endsWith(".deb")) {
    const tar = execFileSync("dpkg-deb", ["--fsys-tarfile", file], {
      timeout: 120000,
      maxBuffer: 128 * 1024 * 1024,
    });
    return execFileSync("tar", ["-tf", "-"], {
      input: tar,
      encoding: "utf8",
      timeout: 120000,
    }).split("\n");
  }
  if (file.endsWith(".rpm")) return run("rpm", ["-qlp", file]).split("\n");
  if (/\.(?:tar\.gz|pkg\.tar\.zst)$/.test(file))
    return run("tar", ["-tf", file]).split("\n");
  if (file.endsWith(".exe"))
    return run("7z", ["l", "-slt", file])
      .split("\n")
      .filter((line) => line.startsWith("Path = "))
      .slice(1)
      .map((line) => line.slice(7).replaceAll("\\", "/"));
  if (file.endsWith(".AppImage")) {
    const offset = readFileSync(file).indexOf(Buffer.from("hsqs"));
    if (offset < 0) throw new Error("No AppImage squashfs found");
    return run("unsquashfs", ["-o", String(offset), "-l", file])
      .split("\n")
      .filter((line) => line.startsWith("squashfs-root/"));
  }
  if (file.endsWith(".dmg")) {
    const mount = `${process.env.RUNNER_TEMP}/toolport-payload-${process.pid}`;
    run("hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mount, file]);
    try {
      return run("find", [mount, "-type", "f"]).split("\n");
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
    assertContents(paths);
    console.log(`PASS: ${file} contains only intended app binaries`);
  }
}
