// Normalize and compare what the GTK and React shells show for one scenario.
// Pure functions, no dependencies; tested by diff.test.mjs (node --test).
//
// A node is { region, kind, value, on?, items? } in reading order. Regions are
// "header", "body" and "row:<server id>". Differences are reported, never
// enforced: the parity tool is report-only.

const SHORTCUT_HINT = /\s*\((?:Ctrl\+|⌘|Cmd\+|Alt\+|Shift\+|\/ or )[^()]*\)$/;

/** Text as a user reads it: spacing, ellipses and keyboard hints do not count. */
export function normalizeText(text) {
  return String(text ?? "")
    .replace(/[\u200b-\u200d\ufeff]/g, "")
    .replace(/\.\.\./g, "…")
    .replace(/\s+/g, " ")
    .trim()
    .replace(SHORTCUT_HINT, "")
    .trim();
}

export function normalizeNodes(nodes) {
  return nodes
    .map((node) => ({
      ...node,
      value: normalizeText(node.value),
      ...(node.items ? { items: node.items.map(normalizeText).filter(Boolean) } : {}),
    }))
    .filter((node) => node.value !== "" || node.kind !== "text");
}

/** Longest common subsequence over `key`, as [gtkIndex, reactIndex] pairs. */
export function lcsPairs(left, right, key) {
  const a = left.map(key);
  const b = right.map(key);
  const width = b.length + 1;
  const table = new Uint32Array((a.length + 1) * width);
  for (let i = a.length - 1; i >= 0; i--)
    for (let j = b.length - 1; j >= 0; j--)
      table[i * width + j] =
        a[i] === b[j]
          ? table[(i + 1) * width + j + 1] + 1
          : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
  const pairs = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      pairs.push([i, j]);
      i++;
      j++;
    } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) i++;
    else j++;
  }
  return pairs;
}

const nodeKey = (node) => `${node.kind}\u0000${node.value}`;
const show = (node) => (node ? `${node.kind} "${node.value}"` : null);
/** Runs longer than this are summarized as one block (for example 140 extra rows). */
export const BLOCK = 12;

function regionsInOrder(gtk, react) {
  const seen = [];
  for (const node of [...gtk, ...react])
    if (!seen.includes(node.region)) seen.push(node.region);
  return seen;
}

function itemDiff(gtkItems = [], reactItems = []) {
  const pairs = lcsPairs(gtkItems, reactItems, (item) => item);
  const keptGtk = new Set(pairs.map(([i]) => i));
  const keptReact = new Set(pairs.map(([, j]) => j));
  return {
    onlyGtk: gtkItems.filter((_, i) => !keptGtk.has(i)),
    onlyReact: reactItems.filter((_, j) => !keptReact.has(j)),
  };
}

function hunkEntries(base, gtkRun, reactRun) {
  const out = [];
  if (gtkRun.length > BLOCK || reactRun.length > BLOCK) {
    out.push({
      ...base,
      type: "block",
      kind: "block",
      gtk: gtkRun.length ? `${gtkRun.length} items, starting ${show(gtkRun[0])}` : null,
      react: reactRun.length
        ? `${reactRun.length} items, starting ${show(reactRun[0])}`
        : null,
      count: { gtk: gtkRun.length, react: reactRun.length },
    });
    return out;
  }
  // Pair what is left by kind and position: a changed label, a renamed switch.
  const reactLeft = [...reactRun];
  for (const node of gtkRun) {
    const match = reactLeft.findIndex((other) => other.kind === node.kind);
    if (match === -1) {
      out.push({
        ...base,
        type: "only-gtk",
        kind: node.kind,
        gtk: node.value,
        react: null,
      });
      continue;
    }
    const [other] = reactLeft.splice(match, 1);
    out.push({
      ...base,
      type: "changed",
      kind: node.kind,
      gtk: node.value,
      react: other.value,
    });
    // A renamed switch: still compare whether it is on, or a known label
    // difference would hide a state difference.
    if (node.on !== undefined && other.on !== undefined && node.on !== other.on)
      out.push({
        ...base,
        type: "state",
        kind: node.kind,
        gtk: `${node.value}: ${node.on ? "on" : "off"}`,
        react: `${other.value}: ${other.on ? "on" : "off"}`,
      });
    // A renamed menu: still compare what is in it.
    if (node.kind === "menu") {
      const { onlyGtk, onlyReact } = itemDiff(node.items, other.items);
      if (onlyGtk.length || onlyReact.length)
        out.push({
          ...base,
          context: `${node.value}" / "${other.value}`,
          type: "items",
          kind: "menu",
          gtk: (node.items ?? []).join(" | "),
          react: (other.items ?? []).join(" | "),
          onlyGtk,
          onlyReact,
        });
    }
  }
  for (const node of reactLeft)
    out.push({
      ...base,
      type: "only-react",
      kind: node.kind,
      gtk: null,
      react: node.value,
    });
  return out;
}

/** Every difference between the two extractions, region by region. */
export function diffNodes(gtkNodes, reactNodes) {
  const gtk = normalizeNodes(gtkNodes);
  const react = normalizeNodes(reactNodes);
  const entries = [];
  for (const region of regionsInOrder(gtk, react)) {
    const left = gtk.filter((node) => node.region === region);
    const right = react.filter((node) => node.region === region);
    const pairs = lcsPairs(left, right, nodeKey);
    let i = 0;
    let j = 0;
    let context = null;
    const flush = (untilI, untilJ) => {
      const gtkRun = left.slice(i, untilI);
      const reactRun = right.slice(j, untilJ);
      if (gtkRun.length || reactRun.length)
        entries.push(...hunkEntries({ region, context }, gtkRun, reactRun));
    };
    for (const [pi, pj] of [...pairs, [left.length, right.length]]) {
      flush(pi, pj);
      if (pi < left.length) {
        const a = left[pi];
        const b = right[pj];
        if (a.on !== undefined && b.on !== undefined && a.on !== b.on)
          entries.push({
            region,
            context,
            type: "state",
            kind: a.kind,
            gtk: `${a.value}: ${a.on ? "on" : "off"}`,
            react: `${b.value}: ${b.on ? "on" : "off"}`,
          });
        if (a.kind === "menu") {
          const { onlyGtk, onlyReact } = itemDiff(a.items, b.items);
          if (onlyGtk.length || onlyReact.length)
            entries.push({
              region,
              context: a.value,
              type: "items",
              kind: "menu",
              gtk: (a.items ?? []).join(" | "),
              react: (b.items ?? []).join(" | "),
              onlyGtk,
              onlyReact,
            });
        }
        if (a.kind === "text") context = a.value;
      }
      i = pi + 1;
      j = pj + 1;
    }
  }
  return entries;
}

function globToRegExp(pattern) {
  const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`, "s");
}

function matchesValue(pattern, value) {
  if (pattern === undefined) return true;
  if (pattern === null) return value === null || value === undefined;
  if (value === null || value === undefined) return false;
  return globToRegExp(normalizeText(pattern)).test(value);
}

/** Does a reviewed known difference cover this entry? */
export function knownMatches(known, entry) {
  return (
    matchesValue(known.screen, entry.screen) &&
    matchesValue(known.scenario, entry.scenario) &&
    matchesValue(known.region, entry.region) &&
    matchesValue(known.kind, entry.kind) &&
    matchesValue(known.type, entry.type) &&
    matchesValue(known.gtk, entry.gtk) &&
    matchesValue(known.react, entry.react)
  );
}

/** The same text shown by the other shell in the same place: a moved line, not a missing one. */
function hasCounterpart(entry, entries) {
  const otherType = entry.type === "only-gtk" ? "only-react" : "only-gtk";
  const text = entry.type === "only-gtk" ? entry.gtk : entry.react;
  return entries.some(
    (other) =>
      other.type === otherType &&
      other.screen === entry.screen &&
      other.scenario === entry.scenario &&
      other.region === entry.region &&
      other.kind === entry.kind &&
      (otherType === "only-gtk" ? other.gtk : other.react) === text,
  );
}

/** Split entries into drift and known differences; list known entries that matched nothing.
 * A known entry with `moved: true` covers a line only one shell shows when the other shell
 * shows the same text elsewhere in that region, so a line one shell drops is still drift. */
export function applyKnown(entries, knownList) {
  const used = new Set();
  const drift = [];
  const known = [];
  for (const entry of entries) {
    const index = knownList.findIndex(
      (item) =>
        knownMatches(item, entry) && (!item.moved || hasCounterpart(entry, entries)),
    );
    if (index === -1) drift.push(entry);
    else {
      used.add(index);
      known.push({ ...entry, reason: knownList[index].reason });
    }
  }
  const stale = knownList.filter((_, index) => !used.has(index));
  return { drift, known, stale };
}

export function validateKnown(list) {
  const problems = [];
  if (!Array.isArray(list)) return ["known-differences.json must be a list"];
  list.forEach((item, index) => {
    if (!item || typeof item !== "object")
      problems.push(`entry ${index} is not an object`);
    else if (!item.reason || typeof item.reason !== "string")
      problems.push(`entry ${index} needs a reason`);
    else if (item.gtk === undefined && item.react === undefined)
      problems.push(
        `entry ${index} needs gtk or react (null means the shell shows nothing)`,
      );
  });
  return problems;
}

function quote(value) {
  return value === null || value === undefined ? "nothing" : `"${value}"`;
}

/** One line per difference: screen, region, element, both values. */
export function formatEntry(entry) {
  const where = `${entry.region}${entry.context ? ` (after "${entry.context}")` : ""}`;
  switch (entry.type) {
    case "only-gtk":
      return `${where}  ${entry.kind} only in GTK: ${quote(entry.gtk)}`;
    case "only-react":
      return `${where}  ${entry.kind} only in React: ${quote(entry.react)}`;
    case "items":
      return [
        `${entry.region}  menu "${entry.context}" items differ`,
        entry.onlyGtk.length ? `      only in GTK: ${entry.onlyGtk.join(" | ")}` : null,
        entry.onlyReact.length
          ? `      only in React: ${entry.onlyReact.join(" | ")}`
          : null,
      ]
        .filter(Boolean)
        .join("\n");
    case "block":
      return `${where}  GTK shows ${entry.count.gtk} items, React ${entry.count.react}: GTK ${quote(entry.gtk)} | React ${quote(entry.react)}`;
    default:
      return `${where}  ${entry.kind}: GTK ${quote(entry.gtk)} | React ${quote(entry.react)}`;
  }
}

export function formatReport(report) {
  const lines = [];
  const failed = report.scenarios.filter((scenario) => scenario.error).length;
  lines.push(
    `Shell parity report: GTK vs React, ${report.scenarios.length} scenarios, ${report.drift.length} differences, ${report.known.length} known${failed ? `, ${failed} not compared` : ""} (${report.seconds} s)`,
  );
  for (const scenario of report.scenarios) {
    const drift = report.drift.filter((entry) => entry.scenario === scenario.name);
    lines.push("");
    // A scenario that failed to extract was never compared; saying "no
    // differences" for it would hide the failure.
    if (scenario.error) {
      lines.push(
        `${scenario.screen} / ${scenario.name}: not compared: ${scenario.error}`,
      );
      continue;
    }
    lines.push(
      `${scenario.screen} / ${scenario.name}: ${drift.length ? `${drift.length} differences` : "no differences"}`,
    );
    for (const entry of drift)
      lines.push(`  ${formatEntry(entry).replace(/\n/g, "\n  ")}`);
    for (const note of scenario.notes ?? []) lines.push(`  note: ${note}`);
  }
  const fallback = [
    ...new Set(report.scenarios.flatMap((scenario) => scenario.fallback ?? [])),
  ].sort();
  if (fallback.length) {
    lines.push("");
    lines.push(
      `React also read these from the default fixture, not a scenario (app-wide state; add a command to a scenario's calls if it changes a covered screen): ${fallback.join(", ")}`,
    );
  }
  if (report.known.length) {
    lines.push("");
    lines.push("Known differences (intended, not drift):");
    for (const entry of report.known)
      lines.push(
        `  ${entry.scenario}  ${formatEntry(entry)}\n      why: ${entry.reason}`,
      );
  }
  if (report.stale.length) {
    lines.push("");
    lines.push(
      "Notes: these known differences matched nothing in this run (fixed, or out of date):",
    );
    for (const item of report.stale)
      lines.push(
        `  ${item.screen ?? "*"} ${item.region ?? "*"} ${item.kind ?? "*"}: GTK ${quote(item.gtk)} | React ${quote(item.react)}`,
      );
  }
  return lines.join("\n");
}
