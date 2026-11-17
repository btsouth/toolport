// Logic tests for the parity differ: node --test scripts/parity/diff.test.mjs
// (npm run parity runs them first).
import assert from "node:assert/strict";
import test from "node:test";
import {
  applyKnown,
  diffNodes,
  formatEntry,
  normalizeText,
  validateKnown,
  formatReport,
} from "./diff.mjs";

const text = (value, region = "body") => ({ region, kind: "text", value });

test("normalization ignores spacing, ellipses and shortcut hints only", () => {
  assert.equal(normalizeText("  Loading...\n now "), "Loading… now");
  assert.equal(normalizeText("Check servers again (Ctrl+R)"), "Check servers again");
  assert.equal(normalizeText("Add server (⌘N)"), "Add server");
  assert.equal(normalizeText("Search servers (/ or Ctrl+F)"), "Search servers");
  assert.equal(normalizeText("Ready · 25 tools"), "Ready · 25 tools");
  assert.notEqual(normalizeText("Ready, 25 tools"), normalizeText("Ready · 25 tools"));
});

test("a changed label names the region, the text before it and both values", () => {
  const gtk = [
    text("Servers"),
    text("All 3 enabled servers are ready."),
    { region: "row:a", kind: "switch", value: "Disable A", on: true },
  ];
  const react = [
    text("Servers"),
    text("3 enabled servers reachable"),
    { region: "row:a", kind: "switch", value: "Toggle A", on: true },
  ];
  const entries = diffNodes(gtk, react);
  assert.deepEqual(
    entries.map((entry) => [
      entry.region,
      entry.type,
      entry.kind,
      entry.context,
      entry.gtk,
      entry.react,
    ]),
    [
      [
        "body",
        "changed",
        "text",
        "Servers",
        "All 3 enabled servers are ready.",
        "3 enabled servers reachable",
      ],
      ["row:a", "changed", "switch", null, "Disable A", "Toggle A"],
    ],
  );
  assert.equal(
    formatEntry(entries[0]),
    'body (after "Servers")  text: GTK "All 3 enabled servers are ready." | React "3 enabled servers reachable"',
  );
});

test("one-sided nodes, switch state and menu items are reported", () => {
  const menu = (items) => ({ region: "header", kind: "menu", value: "More", items });
  const entries = diffNodes(
    [
      menu(["Import", "Enable all"]),
      { region: "row:a", kind: "switch", value: "A", on: true },
      { region: "row:a", kind: "button", value: "Retry" },
    ],
    [
      menu(["Import", "Disable all"]),
      { region: "row:a", kind: "switch", value: "A", on: false },
    ],
  );
  assert.deepEqual(
    entries.map((entry) => entry.type),
    ["items", "state", "only-gtk"],
  );
  assert.deepEqual(entries[0].onlyGtk, ["Enable all"]);
  assert.deepEqual(entries[0].onlyReact, ["Disable all"]);
});

test("a renamed menu still has its items compared", () => {
  const entries = diffNodes(
    [
      {
        region: "header",
        kind: "menu",
        value: "Import and server actions",
        items: ["Import", "Disable all servers"],
      },
    ],
    [{ region: "header", kind: "menu", value: "More actions", items: ["Import"] }],
  );
  assert.deepEqual(
    entries.map((entry) => entry.type),
    ["changed", "items"],
  );
  assert.deepEqual(entries[1].onlyGtk, ["Disable all servers"]);
  assert.equal(
    formatEntry(entries[1]).split("\n")[0],
    'header  menu "Import and server actions" / "More actions" items differ',
  );
});

test("identical extractions have no differences", () => {
  const nodes = [
    text("Servers", "header"),
    { region: "header", kind: "menu", value: "M", items: ["x"] },
  ];
  assert.deepEqual(diffNodes(nodes, structuredClone(nodes)), []);
});

test("long one-sided runs become one block with both counts", () => {
  const rows = (count) =>
    Array.from({ length: count }, (_, index) => text(`row ${index}`));
  const entries = diffNodes(rows(10), rows(150));
  assert.equal(entries.length, 1);
  assert.equal(entries[0].type, "block");
  assert.deepEqual(entries[0].count, { gtk: 0, react: 140 });
});

test("known differences move out of drift; unmatched ones become notes", () => {
  const entries = [
    {
      scenario: "s",
      screen: "servers",
      region: "row:a",
      type: "changed",
      kind: "switch",
      gtk: "Disable A",
      react: "Toggle A",
    },
    {
      scenario: "s",
      screen: "servers",
      region: "body",
      type: "only-react",
      kind: "text",
      gtk: null,
      react: "Hi",
    },
  ];
  const known = [
    {
      screen: "servers",
      region: "row:*",
      kind: "switch",
      gtk: "Disable *",
      react: "Toggle *",
      reason: "agreed",
    },
    { screen: "activity", gtk: "x", react: null, reason: "old" },
  ];
  const { drift, known: matched, stale } = applyKnown(entries, known);
  assert.equal(drift.length, 1);
  assert.equal(drift[0].react, "Hi");
  assert.equal(matched[0].reason, "agreed");
  assert.deepEqual(stale, [known[1]]);
  assert.deepEqual(validateKnown([{ gtk: "x" }]), ["entry 0 needs a reason"]);
  assert.deepEqual(validateKnown([{ reason: "r" }]), [
    "entry 0 needs gtk or react (null means the shell shows nothing)",
  ]);
});

test("a scenario that failed to extract is reported as not compared", () => {
  const text = formatReport({
    scenarios: [
      {
        screen: "servers",
        name: "servers-empty",
        error: "React: the React page did not settle in 30 s",
      },
      { screen: "servers", name: "servers-all-ready", error: null },
    ],
    drift: [],
    known: [],
    stale: [],
    seconds: 1,
  });
  assert.match(text, /1 not compared/);
  assert.match(text, /servers-empty: not compared: React: the React page did not settle/);
  assert.doesNotMatch(text, /servers-empty: no differences/);
  assert.match(text, /servers-all-ready: no differences/);
});
