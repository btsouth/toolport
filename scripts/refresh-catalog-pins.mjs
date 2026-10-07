// Refresh curated package pins without executing upstream code. Review the diff
// and run catalog tests before committing. Existing user servers are untouched.
import { readFile, writeFile } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const pinsUrl = new URL("src-tauri/catalog-pins.json", root);
const catalogUrl = new URL("src-tauri/src/catalog.rs", root);
const pins = JSON.parse(await readFile(pinsUrl, "utf8"));
let source = await readFile(catalogUrl, "utf8");

async function getJson(url) {
  const response = await fetch(url, { signal: AbortSignal.timeout(30000) });
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return response.json();
}

for (const [key, old] of Object.entries(pins)) {
  const [runner, name] = [
    key.slice(0, key.indexOf(":")),
    key.slice(key.indexOf(":") + 1),
  ];
  let pin;
  if (runner === "npx") {
    const metadata = await getJson(
      `https://registry.npmjs.org/${encodeURIComponent(name)}/latest`,
    );
    if (!/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(metadata.version))
      throw new Error(`Invalid npm version: ${name}`);
    if (!metadata.dist.integrity?.startsWith("sha512-"))
      throw new Error(`Missing npm integrity: ${name}`);
    pin = {
      version: metadata.version,
      integrity: metadata.dist.integrity,
      url: metadata.dist.tarball,
    };
  } else {
    const metadata = await getJson(
      `https://pypi.org/pypi/${encodeURIComponent(name)}/json`,
    );
    const wheel = metadata.urls.find(
      (file) => file.filename.endsWith("-py3-none-any.whl") && !file.yanked,
    );
    if (!wheel || !/^[a-f0-9]{64}$/.test(wheel.digests.sha256))
      throw new Error(`Missing portable wheel: ${name}`);
    pin = {
      version: metadata.info.version,
      integrity: `sha256-${wheel.digests.sha256}`,
      url: wheel.url,
    };
  }
  if (old.version === pin.version && old.integrity && old.integrity !== pin.integrity) {
    throw new Error(`Artifact changed without a version change: ${name}`);
  }
  pins[key] = pin;
}

// Match only curated cmd() declarations. Neither remote entries nor user
// commands pass through this script or the runtime catalog mapper.
source = source.replace(
  /cmd\("([^"]+)", "([^"]+)", "(npx|uvx)", &\[([^\]]+)\]/g,
  (match, title, description, runner, raw) => {
    const args = JSON.parse(`[${raw}]`);
    const index = ["-y", "--from"].includes(args[0]) ? 1 : 0;
    let name;
    if (runner === "uvx" && args[0] === "--from" && args[1].startsWith("https://")) {
      name = args[2];
    } else {
      name = args[index].replace(/==.*$/, "").replace(/(?<!^)@[^@]+$/, "");
    }
    const pin = pins[`${runner}:${name}`];
    if (!pin) throw new Error(`Missing pin: ${runner}:${name}`);
    args[index] =
      `${name}${runner === "uvx" && args[0] === "--from" ? "==" : "@"}${pin.version}`;
    return `cmd(${JSON.stringify(title)}, ${JSON.stringify(description)}, ${JSON.stringify(runner)}, &[${args.map((arg) => JSON.stringify(arg)).join(", ")}]`;
  },
);
await writeFile(catalogUrl, source);
await writeFile(pinsUrl, JSON.stringify(pins, null, 2) + "\n");
console.log(
  `Refreshed ${Object.keys(pins).length} curated package pins. Review before committing.`,
);
