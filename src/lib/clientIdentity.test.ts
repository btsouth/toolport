import { expect, it } from "vitest";
import { activityClientName, trustedClientName } from "./clientIdentity";

it("keeps raw IDs out of names and keeps version reports explicitly untrusted", () => {
  expect(trustedClientName({})).toBe("An AI client");
  expect(
    activityClientName({ clientName: "Claude Code", clientLabel: "Claude Code 2.1" }),
  ).toBe('Claude Code (reports "Claude Code 2.1")');
  expect(activityClientName({ clientName: "Claude Code", clientLabel: "Other" })).toBe(
    'Claude Code (reports "Other")',
  );
  expect(
    activityClientName({ clientName: "Claude Code", clientLabel: "Claude Code" }),
  ).toBe("Claude Code");
});
