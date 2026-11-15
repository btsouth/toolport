import { describe, expect, it } from "vitest";
import { t3CodeBadge } from "./t3Code";

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
