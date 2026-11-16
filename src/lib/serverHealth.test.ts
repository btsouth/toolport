import { describe, expect, it } from "vitest";
import { serverFailureLabel } from "./serverHealth";
import type { ProbeResult } from "./types";

const failed = (error: string): ProbeResult => ({
  serverId: "jira",
  ok: false,
  toolCount: 0,
  error,
  authRequired: false,
});

describe("serverFailureLabel", () => {
  it("gives a launch error without a transport code the GTK app's short words", () => {
    expect(
      serverFailureLabel(
        failed(
          "Executable '/opt/atlassian/bin/mcp-atlassian' not found: No such file or directory",
        ),
      ),
    ).toBe("Command or file not found");
    expect(serverFailureLabel(failed("downstream closed stdout"))).toBe("Server stopped");
    expect(serverFailureLabel(failed("something odd happened"))).toBe(
      "Connection failed",
    );
  });
});
