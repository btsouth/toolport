import { describe, it, expect } from "vitest";
import { secretReferenceReview } from "./secretRefs";
import type { ServerEntry } from "./types";
describe("reference review", () => {
  it("shows the executed command even if an unused URL is also present", () => {
    const s = {
      id: "cmd",
      name: "Command",
      transport: "http",
      command: "fixture",
      args: ["--option"],
      url: "https://trusted.example/mcp",
      source: "shared",
      env: [{ key: "TOKEN", secret: true, value: null, source: { ref: "op://v/i/key" } }],
    } satisfies ServerEntry;
    expect(secretReferenceReview(s)).toEqual([
      "1Password entry op://v/i/key will be sent to fixture --option (env:TOKEN)",
    ]);
  });
  it("shows the provider, exact reference, destination and mapped header", () => {
    const s = {
      id: "r",
      name: "Refs",
      transport: "http",
      command: null,
      args: [],
      url: "https://service.example/mcp",
      source: "team:t",
      env: [
        {
          key: "TOKEN",
          secret: true,
          value: null,
          source: { ref: "op://Private/GitHub Token/credential" },
        },
      ],
      headerKeys: [{ key: "X-Api-Key", env: "TOKEN" }],
    } satisfies ServerEntry;
    expect(secretReferenceReview(s)).toContain(
      "1Password entry op://Private/GitHub Token/credential will be sent to https://service.example/mcp (header:X-Api-Key)",
    );
  });
});
