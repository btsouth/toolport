import { describe, expect, it } from "vitest";
import type { DetectedClient, T3CodeOther } from "./types";
import {
  hostName,
  hostThreadChip,
  hostedClientName,
  shortThread,
  t3CodeActivitySegment,
  t3CodeBadge,
  t3CodeGapLines,
  t3CodeInstanceCount,
  t3CodeLeftOut,
  t3CodeLeftOutLines,
  t3CodeStatus,
  t3CodeTargets,
  t3CodeUnconnectedCount,
} from "./hostAttribution";

function client(overrides: Partial<DetectedClient> = {}): DetectedClient {
  return {
    id: "codex",
    name: "Codex",
    usesConnectors: false,
    configPath: "/tmp/config.toml",
    configExists: true,
    appPresent: true,
    servers: [],
    pluginServers: [],
    gatewayInstalled: false,
    entryState: "absent",
    error: null,
    ...overrides,
  };
}
const use = (displayName: string, enabled = true) => ({
  instanceId: displayName.toLowerCase(),
  displayName,
  enabled,
});

describe("host attribution", () => {
  it("names only T3 Code", () => {
    expect(hostName("t3-code")).toBe("T3 Code");
    expect(hostName("other")).toBeNull();
    expect(hostName(undefined)).toBeNull();
    expect(hostedClientName("Codex", { host: "t3-code" })).toBe("Codex via T3 Code");
    expect(hostedClientName("Codex", { host: "other" })).toBe("Codex");
    expect(hostedClientName("Codex", {})).toBe("Codex");
  });

  it("shortens thread ids to 8 characters without the thread: prefix", () => {
    expect(shortThread("0f8e2c1a-9b7d-4e3f")).toBe("0f8e2c1a");
    expect(shortThread("thread:abcdef0123")).toBe("abcdef01");
    expect(shortThread("abc")).toBe("abc");
  });

  it("builds a thread chip only for T3 Code with a thread", () => {
    expect(hostThreadChip({ host: "t3-code", hostThread: "thread:abcdef0123" })).toEqual({
      text: "Thread abcdef01",
      title: "T3 Code thread thread:abcdef0123",
      id: "thread:abcdef0123",
      label: "Copy thread id thread:abcdef0123",
    });
    expect(hostThreadChip({ host: "t3-code" })).toBeNull();
    expect(hostThreadChip({ host: "other", hostThread: "abc" })).toBeNull();
  });
  it("targets rows that can be connected now and counts their enabled instances", () => {
    const rows = [
      client({ id: "a", t3Code: [use("One"), use("Two"), use("Off", false)] }),
      client({ id: "b", t3Code: [use("Off", false)] }),
      client({
        id: "c",
        t3Code: [use("C")],
        gatewayInstalled: true,
        entryState: "managed",
      }),
      client({ id: "d", t3Code: [use("D")], entryState: "customized" }),
      client({ id: "e", t3Code: [use("E")], error: "bad" }),
      client({ id: "f", t3Code: [use("F")], appPresent: false }),
      client({ id: "g" }),
      client({ id: "h", t3Code: [use("H")] }),
    ];
    const targets = t3CodeTargets(rows);
    expect(targets.map((row) => row.id)).toEqual(["a", "h"]);
    expect(t3CodeInstanceCount(targets)).toBe(3);
  });

  it("names the unconnected rows left out, with why and what to do", () => {
    const rows = [
      client({ id: "a", name: "A", t3Code: [use("A")] }),
      client({
        id: "c",
        name: "C",
        t3Code: [use("C")],
        gatewayInstalled: true,
        entryState: "managed",
      }),
      client({
        id: "d",
        name: "Cursor",
        t3Code: [use("Work"), use("Home"), use("Old", false)],
        entryState: "customized",
      }),
      client({ id: "e", name: "Codex", t3Code: [use("E")], error: "bad" }),
      client({ id: "f", name: "Kiro", t3Code: [use("F")], appPresent: false }),
      client({ id: "g", name: "Pi", t3Code: [use("G", false)], appPresent: false }),
    ];
    const left = t3CodeLeftOut(rows);
    expect(left.map((row) => [row.client.id, row.reason, row.names])).toEqual([
      ["d", "customized", ["Work", "Home"]],
      ["e", "unreadable", ["E"]],
      ["f", "missing", ["F"]],
    ]);
    expect(t3CodeLeftOutLines(left)).toEqual([
      "Work, Home use Cursor, whose Toolport entry was changed by hand. Open Cursor below and reset its Toolport entry.",
      "E uses Codex, but Toolport can't read its config. Open Codex below to see why, then connect it.",
      "F uses Kiro, which isn't installed. Install Kiro, then connect it.",
    ]);
    // A, the left-out rows' four, and the enabled instance Toolport can't connect; not
    // the off one, nor the unreadable-file notice.
    const others = [
      {
        instanceId: "m",
        providerName: "Muse Code",
        displayName: "Muse Code",
        enabled: true,
        gap: "unsupported" as const,
      },
      {
        instanceId: "x",
        providerName: "fx",
        displayName: "fx",
        enabled: false,
        gap: "unsupported" as const,
      },
      {
        instanceId: "",
        providerName: "",
        displayName: "~/f",
        enabled: true,
        gap: "unreadable" as const,
      },
    ];
    expect(t3CodeUnconnectedCount(t3CodeTargets(rows), left, others)).toBe(6);
    expect(t3CodeUnconnectedCount([], [], [])).toBe(0);
  });

  it("words the card status for one, several and none", () => {
    expect(t3CodeStatus(1)).toBe("1 enabled T3 Code instance is not connected.");
    expect(t3CodeStatus(3)).toBe("3 enabled T3 Code instances are not connected.");
    expect(t3CodeStatus(0)).toBe("Every enabled T3 Code instance is connected.");
  });
});

describe("t3CodeActivitySegment", () => {
  it("says how much of a client's activity came through T3 Code", () => {
    expect(t3CodeActivitySegment({})).toBeNull();
    expect(t3CodeActivitySegment({ viaT3Code: false, t3CodeSessions: 2 })).toBeNull();
    expect(t3CodeActivitySegment({ viaT3Code: true, t3CodeSessions: 0 })).toBe(
      "Calls via T3 Code",
    );
    expect(t3CodeActivitySegment({ viaT3Code: true, t3CodeSessions: 1 })).toBe(
      "1 session via T3 Code",
    );
    expect(t3CodeActivitySegment({ viaT3Code: true, t3CodeSessions: 3 })).toBe(
      "3 sessions via T3 Code",
    );
  });
});

describe("hostThreadChip validation", () => {
  it("shows only thread ids that pass the same check as storage", () => {
    expect(hostThreadChip({ host: "t3-code", hostThread: "bad id" })).toBeNull();
    expect(hostThreadChip({ host: "t3-code", hostThread: "a".repeat(129) })).toBeNull();
    expect(hostThreadChip({ host: "t3-code", hostThread: "<img>" })).toBeNull();
    expect(hostThreadChip({ host: "t3-code", hostThread: "thread:ab-1_2.3" })?.text).toBe(
      "Thread ab-1_2.3",
    );
  });
});

describe("t3CodeBadge", () => {
  it("names every instance, marks the ones that are off, and says so per line", () => {
    expect(t3CodeBadge([])).toBeNull();
    expect(t3CodeBadge([use("Work"), use("Home", false)])).toEqual({
      text: "Used by T3 Code: Work, Home (off)",
      title: "Work is on in T3 Code\nHome is off in T3 Code",
    });
    expect(t3CodeBadge([use("Claude")])?.text).toBe("Used by T3 Code: Claude");
  });
});

describe("t3CodeGapLines", () => {
  it("says when T3 Code's settings can't be read, naming only the file", () => {
    expect(
      t3CodeGapLines([
        {
          instanceId: "",
          providerName: "",
          displayName: "~/.t3/userdata/settings.json",
          enabled: true,
          gap: "unreadable",
        },
      ]),
    ).toEqual([
      "Toolport can't read T3 Code's settings in ~/.t3/userdata/settings.json, so it can't list T3 Code's instances.",
    ]);
  });
});

describe("t3CodeGapLines for instances Toolport can't use", () => {
  const other = (
    instanceId: string,
    providerName: string,
    displayName: string,
    gap: "malformed" | "hiddenHome" | "relativeHome" | "unsupported",
    enabled = true,
  ) => ({ instanceId, providerName, displayName, enabled, gap });

  it("names each one with why and what to do, in a fixed order", () => {
    expect(
      t3CodeGapLines([
        other("text_entry", "", "text_entry", "malformed"),
        other("pi_hidden", "Pi", "Pi Hidden", "hiddenHome"),
        other("rel", "Codex", "Relative", "relativeHome", false),
        other("bad driver", "", "bad driver", "unsupported", false),
      ]),
    ).toEqual([
      "Not supported by Toolport yet: bad driver (off).",
      "Toolport can't read text_entry in T3 Code's settings. Fix this instance in T3 Code's settings.",
      "Toolport can't see which home folder Pi Hidden (Pi) uses: it is marked sensitive or isn't plain text. Set its home folder as plain text in T3 Code's provider settings.",
      "Toolport can't tell which home folder Relative (Codex) (off) uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
    ]);
  });

  it("words several of a kind in the plural", () => {
    expect(
      t3CodeGapLines([
        other("a", "Claude", "Claude", "malformed"),
        other("b", "Grok", "Broken", "malformed"),
        other("c", "Grok", "Grok", "hiddenHome"),
        other("d", "OpenCode", "Hidden", "hiddenHome"),
        other("e", "Codex", "One", "relativeHome"),
        other("f", "Claude", "Two", "relativeHome"),
      ]),
    ).toEqual([
      "Toolport can't read Claude, Broken (Grok) in T3 Code's settings. Fix these instances in T3 Code's settings.",
      "Toolport can't see which home folders Grok, Hidden (OpenCode) use: they are marked sensitive or aren't plain text. Set each home folder as plain text in T3 Code's provider settings.",
      "Toolport can't tell which home folders One (Codex), Two (Claude) use: they are relative paths. Set each to a full path in T3 Code's provider settings.",
    ]);
  });
});

describe("t3CodeGapLines for homes Toolport won't use", () => {
  const other = (
    displayName: string,
    providerName: string,
    gap: T3CodeOther["gap"],
    enabled = true,
  ): T3CodeOther => ({
    instanceId: displayName.toLowerCase(),
    providerName,
    displayName,
    enabled,
    gap,
  });

  // GTK's `t3_code_gap_lines_say_why_a_home_is_not_used_and_what_to_do` checks the same.
  it("names each by its instance only, with why and what to do", () => {
    expect(
      t3CodeGapLines([
        other("Away", "Codex", "outsideHome"),
        other("Linked", "Claude", "linkedHome", false),
        other("Work", "Claude", "missingHome"),
        other("Grok", "Grok", "relativeHome"),
        other("Notes", "OpenCode", "unlistedHome"),
      ]),
    ).toEqual([
      "Toolport can't tell which home folder Grok uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
      "Toolport won't use the home folder Away (Codex) uses: it isn't inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it.",
      "Toolport won't use the home folder Linked (Claude) (off) uses: it, or its config file, is a link or not a plain folder or file. Point it at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it.",
      "Toolport can't connect Work (Claude) yet: its home folder doesn't exist yet. Start it once in T3 Code, then refresh Clients.",
      "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once Notes (OpenCode)'s home folder exists, add it by hand as a client location to connect it.",
    ]);
    expect(
      t3CodeGapLines([
        other("A", "Codex", "outsideHome"),
        other("B", "Claude", "outsideHome"),
        other("C", "Claude", "linkedHome"),
        other("D", "Pi", "linkedHome"),
        other("E", "Grok", "missingHome"),
        other("F", "Codex", "missingHome", false),
        other("G", "OpenCode", "unlistedHome"),
        other("H", "Pi", "unlistedHome", false),
      ]),
    ).toEqual([
      "Toolport won't use the home folders A (Codex), B (Claude) use: they aren't inside your home folder. Toolport only manages configs inside it; add each by hand as a client location if you trust it.",
      "Toolport won't use the home folders C (Claude), D (Pi) use: they, or their config files, are links or not plain folders or files. Point each at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it.",
      "Toolport can't connect E (Grok), F (Codex) (off) yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients.",
      "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once the home folders G (OpenCode), H (Pi) (off) use exist, add each by hand as a client location to connect it.",
    ]);
    // Every enabled one counts as not connected; an off one does not.
    expect(
      t3CodeUnconnectedCount(
        [],
        [],
        [
          other("Away", "Codex", "outsideHome"),
          other("Linked", "Claude", "linkedHome", false),
          other("Work", "Claude", "missingHome"),
        ],
      ),
    ).toBe(2);
  });
});
