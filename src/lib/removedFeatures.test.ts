import { describe, expect, it } from "vitest";
import { removedFeaturesIssueUrl, removedFeaturesMessage } from "./removedFeatures";

describe("removed features notice", () => {
  it("matches the GTK wording and issue link", () => {
    const features = ["agentRules", "routines"];
    expect(removedFeaturesMessage(features)).toBe(
      "Toolport 2.0 no longer includes Agent rules and Routines, which you used in 1.x. Your settings for them are saved in the exports folder, and files Toolport wrote for them were left as they were. Tell us if you need one back, or go back to 1.24.",
    );
    expect(removedFeaturesIssueUrl(features)).toBe(
      "https://github.com/btsouth/toolport/issues/new?title=I%20need%20Agent%20rules%2C%20Routines%20in%20Toolport%202.0&body=Toolport%202.0%20removed%3A%20Agent%20rules%2C%20Routines.%0A%0AWhat%20I%20used%20it%20for%3A%0A%0A",
    );
  });
});
