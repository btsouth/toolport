import { expect, it } from "vitest";
import {
  activityClientName,
  trustedClientName,
  clientIdentityTooltip,
  UNRECORDED_CLIENT_TOOLTIP,
} from "./clientIdentity";

it("keeps raw IDs out of names and keeps version reports explicitly untrusted", () => {
  expect(trustedClientName({})).toBe("Unrecorded client");
  expect(
    activityClientName({ clientName: "Claude Code", clientLabel: "Claude Code 2.1" }),
  ).toBe("Claude Code");
  expect(activityClientName({ clientName: "inbox", clientLabel: "inbox 1" })).toBe(
    "inbox",
  );
  expect(
    activityClientName({ clientName: "Codex", clientLabel: "codex-mcp-client 0.162.1" }),
  ).toBe('Codex (reports "codex-mcp-client 0.162.1")');
  expect(activityClientName({ clientName: "Claude Code", clientLabel: "Other" })).toBe(
    'Claude Code (reports "Other")',
  );
  expect(
    activityClientName({ clientName: "Claude Code", clientLabel: "Claude Code" }),
  ).toBe("Claude Code");
});

it("marks reported-only callers and explains legacy rows", () => {
  expect(trustedClientName({ clientLabel: "inbox" })).toBe("inbox (reported)");
  expect(
    activityClientName({ clientName: "Unknown client", clientLabel: "inbox 1" }),
  ).toBe("inbox 1 (reported)");
  expect(trustedClientName({ client: "adapter:unknown" })).toBe("Unknown client");
  expect(trustedClientName({ clientName: "inbox", clientLabel: "Other" })).toBe("inbox");
  expect(clientIdentityTooltip({})).toBe(UNRECORDED_CLIENT_TOOLTIP);
  expect(clientIdentityTooltip({ clientName: "An AI client" })).toBe(
    UNRECORDED_CLIENT_TOOLTIP,
  );
  expect(clientIdentityTooltip({ clientLabel: "inbox" })).toBeUndefined();
});
