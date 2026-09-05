#!/usr/bin/env node
// Measure all statically imported startup JS, not just a renamed entry chunk.
import { readFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";
import { fileURLToPath } from "node:url";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(
  await readFile(path.join(root, "dist/.vite/manifest.json"), "utf8"),
);
const visited = new Set();
function visit(key) {
  if (visited.has(key)) return;
  visited.add(key);
  for (const child of manifest[key].imports || []) visit(child);
}
for (const [key, item] of Object.entries(manifest)) if (item.isEntry) visit(key);
let bytes = 0;
let gzipBytes = 0;
for (const key of visited) {
  const content = await readFile(path.join(root, "dist", manifest[key].file));
  bytes += content.length;
  gzipBytes += gzipSync(content).length;
}
const result = {
  startupChunks: visited.size,
  startupJsBytes: bytes,
  startupGzipBytes: gzipBytes,
};
console.log(JSON.stringify(result, null, 2));
// Baseline before externalizing logos: 649,253 bytes / 210,862 gzip.
if (bytes > 580_000 || gzipBytes > 185_000) {
  console.error(
    "Startup bundle exceeds budget (580 kB raw / 185 kB gzip). Inspect eager imports before changing the budget.",
  );
  process.exitCode = 1;
}
