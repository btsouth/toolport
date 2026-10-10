import { describe, expect, it } from "vitest";
import { keptSafetySummary } from "./keptSafety";

describe("keptSafetySummary", () => {
  it("matches the GTK wording", () => {
    expect(keptSafetySummary(undefined)).toBeNull();
    expect(keptSafetySummary({})).toBeNull();
    expect(keptSafetySummary({ holdUntrusted: true })).toBe(
      "Kept from 1.x: Toolport also asks before calls from shared or registry servers.",
    );
    expect(keptSafetySummary({ holdUntrusted: true, blockOnInjection: true })).toBe(
      "Kept from 1.x: Toolport also asks before calls from shared or registry servers and blocks results that look like prompt injection.",
    );
    expect(
      keptSafetySummary({ denyDestructive: true, quarantineOnDrift: true, blockOnInjection: true }),
    ).toBe(
      "Kept from 1.x: Toolport also hides destructive tools, pauses tools whose definitions change, and blocks results that look like prompt injection.",
    );
  });
});
