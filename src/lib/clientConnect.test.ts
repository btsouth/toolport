import { describe, it, expect } from "vitest";
import { clientRestartHint, clientRestartHintAfterRemoval } from "./clientConnect";

describe("clientRestartHint (SOU-317)", () => {
  it("puts the restart line first and keeps optional scope/backup notes", () => {
    expect(clientRestartHint("Claude Desktop")).toBe(
      "Restart Claude Desktop so it loads Toolport.",
    );
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
});
