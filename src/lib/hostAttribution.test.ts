import { describe, expect, it } from "vitest";
import {
  hostName,
  hostThreadChip,
  hostedClientName,
  shortThread,
  t3CodeActivitySegment,
  t3CodeBadge,
} from "./hostAttribution";

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

const use = (displayName: string, enabled = true) => ({
  instanceId: displayName.toLowerCase(),
  displayName,
  enabled,
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
