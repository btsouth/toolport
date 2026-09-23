#!/usr/bin/env node
// Compare canonical JSON serializations of captured tools/list arrays from the
// same client/configuration. JSON.parse/stringify may change escape spelling or
// number lexemes, so these are not byte-for-byte sizes of the capture file.
//
// Usage: node benchmark/token-cost.mjs <full-tools-list.json> <lazy-tools-list.json>
// A single cache/array file prints its own size only; it cannot establish savings.
// The files may contain a tools array, {tools}, or an MCP {result:{tools}} response.
import { readFileSync } from "node:fs";

function toolsFrom(path) {
  const parsed = JSON.parse(readFileSync(path, "utf8"));
  const tools = Array.isArray(parsed) ? parsed : (parsed.result?.tools ?? parsed.tools);
  if (!Array.isArray(tools)) throw new Error(`${path}: expected a tools array`);
  return tools;
}
function bytes(tools) {
  return Buffer.byteLength(JSON.stringify(tools), "utf8");
}
function estimate(bytes) {
  return Math.ceil(bytes / 4);
}
function number(n) {
  return n.toLocaleString("en-US");
}

const [fullPath, exposedPath] = process.argv.slice(2);
if (!fullPath) {
  console.error(
    "Usage: node benchmark/token-cost.mjs <full-tools-list.json> [lazy-tools-list.json]",
  );
  process.exit(2);
}
const full = toolsFrom(fullPath);
const fullBytes = bytes(full);
console.log(
  `Full tools array: ${number(full.length)} tool${full.length === 1 ? "" : "s"}, ${number(fullBytes)} canonical reserialized UTF-8 bytes, ≈${number(estimate(fullBytes))} token-equivalent (UTF-8 bytes / 4)`,
);
if (!exposedPath) {
  console.log(
    "Capture a lazy/grouped tools/list from the same client and pass it as the second file to measure exposure avoided.",
  );
  process.exit(0);
}
const exposed = toolsFrom(exposedPath);
const exposedBytes = bytes(exposed);
const avoided = Math.max(0, fullBytes - exposedBytes);
const extra = Math.max(0, exposedBytes - fullBytes);
console.log(
  `Exposed tools array: ${number(exposed.length)} tool${exposed.length === 1 ? "" : "s"}, ${number(exposedBytes)} canonical reserialized UTF-8 bytes, ≈${number(estimate(exposedBytes))} token-equivalent`,
);
console.log(
  `Catalog exposure avoided: ${number(avoided)} canonical reserialized bytes, ≈${number(estimate(avoided))} token-equivalent`,
);
if (extra > 0)
  console.log(`Extra exposure: ${number(extra)} canonical reserialized bytes`);
console.log(
  "MCP clients may transform/gate definitions and providers may cache them; these are not billed or model-consumed tokens. Discovery response overhead is separate and is not included here.",
);
