import { describe, it, expect } from "vitest";
import {
  clientRestartHint,
  clientRestartHintAfterRemoval,
  connectSuccessDescription,
  toolportStudioClientBlurb,
} from "./clientConnect";

describe("clientRestartHint / connectSuccessDescription (SOU-317)", () => {
  it("puts the restart line first and keeps optional scope/backup notes", () => {
    expect(clientRestartHint("Claude Desktop")).toBe(
      "Restart Claude Desktop so it loads Toolport.",
    );
    expect(
      connectSuccessDescription("Claude Desktop", [
        'Scoped to the "Work" profile.',
        false,
        null,
      ]),
    ).toBe('Restart Claude Desktop so it loads Toolport. Scoped to the "Work" profile.');
    expect(connectSuccessDescription("Claude Desktop")).toBe(
      "Restart Claude Desktop so it loads Toolport.",
    );
  });

  it("uses a session-scoped hint for Toolport Studio", () => {
    expect(clientRestartHint("Toolport Studio", "toolport-studio")).toBe(
      "Start a new conversation in Toolport Studio so it picks up this scope.",
    );
    expect(clientRestartHint("Toolport Studio", "other-client")).toBe(
      "Restart Toolport Studio so it loads Toolport.",
    );
    expect(
      connectSuccessDescription(
        "Toolport Studio",
        ['Scoped to the "Work" profile.'],
        "toolport-studio",
      ),
    ).toBe(
      'Start a new conversation in Toolport Studio so it picks up this scope. Scoped to the "Work" profile.',
    );
  });

  it("explains zero-config tools vs Connect for Studio", () => {
    expect(toolportStudioClientBlurb()).toMatch(/discovers Toolport automatically/);
    expect(toolportStudioClientBlurb()).toMatch(/pin a profile/);
  });
});

describe("clientRestartHintAfterRemoval (SBS-336 review)", () => {
  it("says the client stops loading Toolport, not that it loads it", () => {
    // The connect wording on a disconnect reads as though the disconnect failed.
    expect(clientRestartHintAfterRemoval("Claude Desktop")).toBe(
      "Restart Claude Desktop so it stops loading Toolport.",
    );
    expect(clientRestartHintAfterRemoval("Claude Desktop")).not.toContain(
      "so it loads Toolport",
    );
  });

  it("keeps Studio's new-conversation wording", () => {
    expect(clientRestartHintAfterRemoval("Toolport Studio", "toolport-studio")).toBe(
      "Start a new conversation in Toolport Studio so it stops using Toolport.",
    );
    // A different client id must not pick up the Studio phrasing.
    expect(clientRestartHintAfterRemoval("Other", "other-client")).toBe(
      "Restart Other so it stops loading Toolport.",
    );
  });
});
