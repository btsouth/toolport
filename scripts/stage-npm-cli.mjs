#!/usr/bin/env node
// Stage the @toolport/cli npm packages from built binaries.
//
//   node scripts/stage-npm-cli.mjs --out dist/npm \
//     --bin linux-x64=target/x86_64-unknown-linux-gnu/release-cli/toolport \
//     --bin darwin-arm64=... [--version 2.3.8] [--all]
//   node scripts/stage-npm-cli.mjs --check
//
// Writes one folder per platform package (package.json with os, cpu and libc, plus
// bin/toolport) and the main @toolport/cli package with its optionalDependencies pinned
// to the same version. --all requires a binary for every platform, as a release does.
// --check verifies the checked-in launcher package lists every platform at its version.
// Staging only: publishing stays a separate, deliberate step.
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const templateDir = path.join(root, "packages", "toolport-cli");

export const PLATFORMS = {
  "darwin-arm64": { os: "darwin", cpu: "arm64" },
  "darwin-x64": { os: "darwin", cpu: "x64" },
  "linux-arm64": { os: "linux", cpu: "arm64", libc: "glibc" },
  "linux-x64": { os: "linux", cpu: "x64", libc: "glibc" },
  "win32-x64": { os: "win32", cpu: "x64" },
};

export function packageName(key) {
  return `@toolport/cli-${key}`;
}

export function readTemplate(dir = templateDir) {
  return JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8"));
}

/** package.json for one platform package. */
export function platformManifest(key, version, template = readTemplate()) {
  const platform = PLATFORMS[key];
  if (!platform) throw new Error(`unknown platform ${key}`);
  const manifest = {
    name: packageName(key),
    version,
    description: `The toolport CLI binary for ${key}. Install @toolport/cli instead.`,
    license: template.license,
    author: template.author,
    homepage: template.homepage,
    repository: template.repository,
    os: [platform.os],
    cpu: [platform.cpu],
    files: ["bin"],
    preferUnplugged: true,
  };
  if (platform.libc) manifest.libc = [platform.libc];
  return manifest;
}

/** The launcher package.json, with every platform pinned to `version`. */
export function mainManifest(version, template = readTemplate()) {
  const optionalDependencies = {};
  for (const key of Object.keys(PLATFORMS).sort()) {
    optionalDependencies[packageName(key)] = version;
  }
  return { ...template, version, optionalDependencies };
}

/** Problems with the checked-in launcher package, empty when it is consistent. */
export function check(template = readTemplate()) {
  const problems = [];
  const deps = template.optionalDependencies || {};
  const expected = Object.keys(PLATFORMS).map(packageName).sort();
  const actual = Object.keys(deps).sort();
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    problems.push(
      `optionalDependencies should be exactly ${expected.join(", ")}; found ${actual.join(", ")}`,
    );
  }
  for (const [name, version] of Object.entries(deps)) {
    if (version !== template.version) {
      problems.push(`${name} is pinned to ${version}, not ${template.version}`);
    }
  }
  if (template.bin?.toolport !== "bin/toolport.js")
    problems.push("bin.toolport should be bin/toolport.js");
  if (template.scripts?.postinstall || template.scripts?.install) {
    problems.push("the launcher package must not have an install script");
  }
  const rootVersion = JSON.parse(
    readFileSync(path.join(root, "package.json"), "utf8"),
  ).version;
  if (template.version !== rootVersion) {
    problems.push(
      `version ${template.version} doesn't match the app version ${rootVersion}`,
    );
  }
  return problems;
}

/** Write the packages under `out`. `binaries` maps platform keys to built binaries. */
export function stage({
  out,
  version,
  binaries,
  requireAll = false,
  template = readTemplate(),
}) {
  for (const key of Object.keys(binaries)) {
    if (!PLATFORMS[key])
      throw new Error(
        `unknown platform ${key}; use ${Object.keys(PLATFORMS).join(", ")}`,
      );
    if (!existsSync(binaries[key]))
      throw new Error(`binary for ${key} not found: ${binaries[key]}`);
  }
  const missing = Object.keys(PLATFORMS).filter((key) => !binaries[key]);
  if (requireAll && missing.length)
    throw new Error(`no binary for ${missing.join(", ")}`);
  rmSync(out, { recursive: true, force: true });
  const written = [];
  for (const [key, file] of Object.entries(binaries)) {
    const dir = path.join(out, `cli-${key}`);
    mkdirSync(path.join(dir, "bin"), { recursive: true });
    const name = PLATFORMS[key].os === "win32" ? "toolport.exe" : "toolport";
    copyFileSync(file, path.join(dir, "bin", name));
    chmodSync(path.join(dir, "bin", name), 0o755);
    writeFileSync(
      path.join(dir, "package.json"),
      `${JSON.stringify(platformManifest(key, version, template), null, 2)}\n`,
    );
    writeFileSync(
      path.join(dir, "README.md"),
      `# ${packageName(key)}\n\nThe toolport binary for ${key}. Install [@toolport/cli](https://www.npmjs.com/package/@toolport/cli) instead; it picks the right one.\n`,
    );
    written.push(packageName(key));
  }
  const main = path.join(out, "cli");
  for (const part of ["bin", "lib", "README.md"]) {
    cpSync(path.join(templateDir, part), path.join(main, part), { recursive: true });
  }
  writeFileSync(
    path.join(main, "package.json"),
    `${JSON.stringify(mainManifest(version, template), null, 2)}\n`,
  );
  chmodSync(path.join(main, "bin", "toolport.js"), 0o755);
  written.push(template.name);
  return { written, missing };
}

function parseArgs(argv) {
  const opts = { binaries: {}, requireAll: false, check: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const value = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`${arg} needs a value`);
      return v;
    };
    if (arg === "--out") opts.out = path.resolve(value());
    else if (arg === "--version") opts.version = value();
    else if (arg === "--all") opts.requireAll = true;
    else if (arg === "--check") opts.check = true;
    else if (arg === "--bin") {
      const [key, file] = value().split(/=(.*)/s);
      if (!file)
        throw new Error(
          "--bin takes platform=path, for example linux-x64=target/release/toolport",
        );
      opts.binaries[key] = path.resolve(file);
    } else throw new Error(`unknown option ${arg}`);
  }
  return opts;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const opts = parseArgs(process.argv.slice(2));
    if (opts.check) {
      const problems = check();
      if (problems.length) {
        for (const p of problems) console.error(`stage-npm-cli: ${p}`);
        process.exit(1);
      }
      console.log("stage-npm-cli: packages/toolport-cli is consistent");
    } else {
      if (!opts.out) throw new Error("--out is required");
      const template = readTemplate();
      const { written, missing } = stage({
        ...opts,
        version: opts.version || template.version,
        template,
      });
      console.log(`stage-npm-cli: wrote ${written.join(", ")} to ${opts.out}`);
      if (missing.length)
        console.log(
          `stage-npm-cli: no binary for ${missing.join(", ")} (pass --all to require them)`,
        );
    }
  } catch (error) {
    console.error(`stage-npm-cli: ${error.message}`);
    process.exit(1);
  }
}
