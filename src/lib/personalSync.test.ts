import { describe, expect, it } from "vitest";
import { riskySyncEnv, syncsByDefault } from "./personalSync";

describe("sync defaults for plain values", () => {
  it("syncs ordinary settings", () => {
    expect(syncsByDefault("REGION", "west")).toBe(true);
    expect(syncsByDefault("BASE_URL", "https://api.example.com/v1")).toBe(true);
    expect(syncsByDefault("MODE", "")).toBe(true);
  });

  it("keeps paths, tokens, credential URLs and risky names local", () => {
    for (const value of [
      "/home/me",
      "~/notes",
      "./data",
      "..",
      "\\\\share",
      "C:\\x",
      "D:/x",
    ]) {
      expect(syncsByDefault("DIR", value)).toBe(false);
    }
    expect(syncsByDefault("ID", "abc123def456ghi789jk")).toBe(false);
    expect(syncsByDefault("URL", "https://user:pw@example.com")).toBe(false);
    expect(syncsByDefault("URL", "https://example.com/?api_key=1")).toBe(false);
    expect(syncsByDefault("node_options", "--inspect")).toBe(false);
  });

  it("matches risky names by exact name and prefix, ignoring case", () => {
    expect(riskySyncEnv(" path ")).toBe(true);
    expect(riskySyncEnv("GIT_CONFIG_KEY_0")).toBe(true);
    expect(riskySyncEnv("dyld_insert_libraries")).toBe(true);
    expect(riskySyncEnv("PATHS")).toBe(false);
    expect(riskySyncEnv("REGION")).toBe(false);
  });
});
