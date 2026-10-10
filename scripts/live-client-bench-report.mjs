#!/usr/bin/env node
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { abstained, finalText, score } from "./live-client-bench.mjs";
import { tasksFrom } from "./live-client-bench-tasks.mjs";

const options = {};
for (let i = 2; i < process.argv.length; i += 2)
  options[process.argv[i].slice(2)] = process.argv[i + 1];
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
const lines = (file) => {
  try {
    return readFileSync(file, "utf8").split("\n").filter(Boolean).map(JSON.parse);
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
};
const catalog = json(options.catalog),
  tasks = tasksFrom(json(options.dev));
const cases = readdirSync(options.runs, { withFileTypes: true })
  .filter((e) => e.isDirectory())
  .flatMap((entry) => {
    const dir = path.join(options.runs, entry.name);
    let result;
    try {
      result = json(path.join(dir, "summary.json"));
    } catch (error) {
      if (error.code === "ENOENT") return [];
      throw error;
    }
    const task = tasks.find((t) => t.id === result.task);
    if (!task) return [];
    const calls = lines(path.join(dir, "downstream.jsonl")).filter(
      (e) => e.method === "tools/call",
    );
    const scored = score(task, calls, catalog, result.finished);
    const output = readFileSync(path.join(dir, "stdout.jsonl"), "utf8");
    if (!task.expected.length)
      scored.success &&= abstained(finalText(result.client, output));
    const wire = lines(path.join(dir, "wire.jsonl"));
    const searches = wire.filter(
      (e) =>
        e.direction === "client" &&
        e.method === "tools/call" &&
        (e.params.name === "toolport_search_tools" || e.params.name.startsWith("help_")),
    );
    return [
      {
        ...result,
        ...scored,
        searchRounds: searches.length,
        describeRounds: searches.filter((e) => e.params.arguments?.name).length,
        evidence: dir,
      },
    ];
  });
const mean = (xs) => (xs.length ? xs.reduce((n, x) => n + x, 0) / xs.length : null);
const median = (xs) => {
  if (!xs.length) return null;
  const a = [...xs].sort((a, b) => a - b),
    n = a.length;
  return (a[Math.floor((n - 1) / 2)] + a[Math.floor(n / 2)]) / 2;
};
const usage = (row) => {
  if (!row.usage) return null;
  const u = row.usage;
  const uncached = row.client === "codex" ? u.input - (u.cachedInput || 0) : u.input;
  return {
    uncached,
    cached: u.cachedInput || 0,
    cacheWrite: u.cacheWrite || 0,
    inputTotal: uncached + (u.cachedInput || 0) + (u.cacheWrite || 0),
    output: u.output || 0,
    reasoning: u.reasoning || u.reported?.reasoning_output_tokens || 0,
  };
};
const rows = [];
for (const client of ["claude-code", "codex", "opencode", "cursor"])
  for (const mode of ["full", "lazy", "grouped"]) {
    const group = cases.filter((x) => x.client === client && x.mode === mode),
      measured = group.map(usage).filter(Boolean);
    rows.push({
      client,
      mode,
      runs: group.length,
      success: group.filter((x) => x.success).length,
      wrongCalls: group.reduce((n, x) => n + x.wrongCalls, 0),
      finished: group.filter((x) => x.finished).length,
      usageRuns: measured.length,
      uncachedMean: mean(measured.map((u) => u.uncached)),
      cachedMean: mean(measured.map((u) => u.cached)),
      cacheWriteMean: mean(measured.map((u) => u.cacheWrite)),
      inputMean: mean(measured.map((u) => u.inputTotal)),
      outputMean: mean(measured.map((u) => u.output)),
      reasoningMean: mean(measured.map((u) => u.reasoning)),
      latencyMedianMs: median(group.map((x) => x.wallMs)),
      gatewaySearchMean: mean(group.map((x) => x.searchRounds)),
      describeMean: mean(group.map((x) => x.describeRounds)),
    });
  }
const totals = {
  runs: cases.length,
  input: cases
    .map(usage)
    .filter(Boolean)
    .reduce((n, u) => n + u.inputTotal, 0),
  output: cases
    .map(usage)
    .filter(Boolean)
    .reduce((n, u) => n + u.output, 0),
};
const analysis = { rows, totals, cases };
writeFileSync(options.out, JSON.stringify(analysis, null, 2));
console.log(JSON.stringify({ rows, totals }, null, 2));
