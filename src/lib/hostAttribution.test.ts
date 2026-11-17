import { describe, expect, it } from "vitest";
import type { DetectedClient, T3CodeOther } from "./types";
import {
  compareClientNames,
  connectAllOutcome,
  connectAllSummary,
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
      title:
        "Work is on in T3 Code\nHome is off in T3 Code. Connect all skips it; you can still connect it here.",
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
        other("Notes", "OpenCode", "missingHome"),
      ]),
    ).toEqual([
      "Toolport can't tell which home folder Grok uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
      "Toolport won't use the home folder Away (Codex) uses: it isn't written as a plain folder inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it.",
      "Toolport won't use the home folder Linked (Claude) (off) uses: it, or its config file, is a link or not a plain folder or file. Point it at the real folder in T3 Code's provider settings.",
      "Toolport can't connect Work (Claude), Notes (OpenCode) yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients.",
    ]);
    expect(
      t3CodeGapLines([
        other("A", "Codex", "outsideHome"),
        other("B", "Claude", "outsideHome"),
        other("C", "Claude", "linkedHome"),
        other("D", "Pi", "linkedHome"),
        other("E", "Grok", "missingHome"),
        other("F", "Codex", "missingHome", false),
        other("G", "OpenCode", "missingHome", false),
      ]),
    ).toEqual([
      "Toolport won't use the home folders A (Codex), B (Claude) use: they aren't written as plain folders inside your home folder. Toolport only manages configs inside it; add each by hand as a client location if you trust it.",
      "Toolport won't use the home folders C (Claude), D (Pi) use: they, or their config files, are links or not plain folders or files. Point each at the real folder in T3 Code's provider settings.",
      "Toolport can't connect E (Grok), F (Codex) (off), G (OpenCode) (off) yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients.",
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

describe("connectAllSummary", () => {
  it("says only the count when every instance connected", () => {
    expect(
      connectAllSummary({ connected: 3, total: 3, stopped: false, notConnected: [] }),
    ).toEqual({
      text: "Connected 3 of 3 T3 Code instances.",
      title: "Connected 3 of 3 T3 Code instances.",
      detail: "",
      success: true,
      tone: "success",
    });
    expect(
      connectAllSummary({ connected: 1, total: 1, stopped: false, notConnected: [] }),
    ).toEqual({
      text: "Connected 1 of 1 T3 Code instance.",
      title: "Connected 1 of 1 T3 Code instance.",
      detail: "",
      success: true,
      tone: "success",
    });
  });

  it("names the rows it left out last, and is not a full success then", () => {
    expect(
      connectAllSummary({
        connected: 1,
        total: 1,
        stopped: false,
        notConnected: [],
        leftOut: ["F uses Kiro, which isn't installed. Install Kiro, then connect it."],
      }),
    ).toEqual({
      text: "Connected 1 of 1 T3 Code instance. F uses Kiro, which isn't installed. Install Kiro, then connect it.",
      title: "Connected 1 of 1 T3 Code instance.",
      detail: "F uses Kiro, which isn't installed. Install Kiro, then connect it.",
      success: false,
      tone: "warning",
    });
  });

  it("names skipped rows in step order without the stopped prefix", () => {
    expect(
      connectAllSummary({
        connected: 2,
        total: 4,
        stopped: false,
        notConnected: ["Cursor", "Claude Code (claude_personal)"],
      }),
    ).toEqual({
      text: "Connected 2 of 4 T3 Code instances. Not connected: Cursor, Claude Code (claude_personal).",
      title: "Connected 2 of 4 T3 Code instances.",
      detail: "Not connected: Cursor, Claude Code (claude_personal).",
      success: false,
      tone: "warning",
    });
  });

  it("says when the user stopped it", () => {
    expect(
      connectAllSummary({
        connected: 0,
        total: 5,
        stopped: true,
        notConnected: ["Aider", "Codex"],
      }),
    ).toEqual({
      text: "Connect all stopped. Connected 0 of 5 T3 Code instances. Not connected: Aider, Codex.",
      title: "Connect all stopped.",
      detail: "Connected 0 of 5 T3 Code instances. Not connected: Aider, Codex.",
      success: false,
      tone: "warning",
    });
  });

  it("is neutral, not a success, when nothing was left to count", () => {
    expect(
      connectAllSummary({ connected: 0, total: 0, stopped: false, notConnected: [] }),
    ).toEqual({
      text: "Connected 0 of 0 T3 Code instances.",
      title: "Connected 0 of 0 T3 Code instances.",
      detail: "",
      success: true,
      tone: "info",
    });
  });
});

describe("connectAllOutcome", () => {
  it("names instances, not rows, and counts only what is still on when the batch ends", () => {
    const clients = [
      client({
        id: "claude",
        name: "Claude Code (claude_personal)",
        gatewayInstalled: true,
        t3Code: [use("Claude Personal")],
      }),
      client({
        id: "opencode",
        name: "OpenCode",
        t3Code: [use("OpenCode Go"), use("OpenCode Zen", false)],
      }),
      // Turned off in T3 Code while the batch ran.
      client({ id: "cursor", name: "Cursor", t3Code: [use("Cursor Work", false)] }),
      client({ id: "kiro", name: "Kiro", appPresent: false, t3Code: [use("F")] }),
    ];
    // Removed from T3 Code while the batch ran: "gone" no longer exists.
    expect(
      connectAllOutcome(clients, ["claude"], ["opencode", "cursor", "gone"]),
    ).toEqual({
      connected: 1,
      total: 2,
      notConnected: ["OpenCode Go"],
      leftOut: ["F uses Kiro, which isn't installed. Install Kiro, then connect it."],
    });
  });

  it("does not count a customized row as connected, as its row does not", () => {
    const clients = [
      client({
        id: "codex",
        name: "Codex",
        gatewayInstalled: true,
        entryState: "customized",
        t3Code: [use("Codex Home")],
      }),
    ];
    expect(connectAllOutcome(clients, [], ["codex"])).toEqual({
      connected: 0,
      total: 1,
      notConnected: ["Codex Home"],
      leftOut: [],
    });
  });

  it("counts a row the rescan shows connected, even when its step reported a failure", () => {
    const clients = [
      client({
        id: "codex",
        name: "Codex",
        gatewayInstalled: true,
        t3Code: [use("Codex Home")],
      }),
      client({ id: "aider", name: "Aider", t3Code: [use("Aider Work")] }),
    ];
    expect(connectAllOutcome(clients, [], ["codex", "aider"])).toEqual({
      connected: 1,
      total: 2,
      notConnected: ["Aider Work"],
      leftOut: [],
    });
  });

  it("leaves a batch row out of the left-out lines even when it can no longer be read", () => {
    const clients = [
      client({
        id: "codex",
        name: "Codex",
        error: "denied",
        t3Code: [use("Codex Home")],
      }),
    ];
    expect(connectAllOutcome(clients, [], ["codex"])).toEqual({
      connected: 0,
      total: 1,
      notConnected: ["Codex Home"],
      leftOut: [],
    });
  });
});

describe("compareClientNames", () => {
  it("orders by Unicode lowercase name, then id, as GTK's client_order does", () => {
    const rows = [
      { id: "zed", name: "Zed" },
      { id: "ecran", name: "Écran" },
      { id: "codex@b", name: "codex" },
      { id: "codex@a", name: "Codex" },
      { id: "emoji", name: "\u{1F600} Smile" },
      { id: "private", name: "\uFF21 Wide" },
    ];
    // Code point order: the astral emoji sorts after U+FF21, unlike UTF-16 order.
    expect([...rows].sort(compareClientNames).map((row) => row.id)).toEqual([
      "codex@a",
      "codex@b",
      "zed",
      "ecran",
      "private",
      "emoji",
    ]);
  });
});
