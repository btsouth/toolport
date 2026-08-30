import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const root = process.cwd();

function readIfPresent(...parts: string[]): string {
  const path = join(root, ...parts);
  return existsSync(path) ? readFileSync(path, "utf8") : "";
}

describe("vendored logo provenance", () => {
  it.each(["anythingllm", "boltai", "continue", "droid", "omp"])(
    "keeps the unverified %s client mark on the neutral fallback",
    (slug) => {
      expect(readIfPresent("src", "components", "ClientLogo.tsx")).not.toContain(
        `"${slug}"`,
      );
      expect(
        readIfPresent("src-tauri", "src", "linux_native", "branding.rs"),
      ).not.toContain(`"${slug}"`);
      expect(existsSync(join(root, "src", "assets", "client-logos", `${slug}.svg`))).toBe(
        false,
      );
      expect(
        existsSync(join(root, "src-tauri", "icons", "client-logos", `${slug}.png`)),
      ).toBe(false);
    },
  );

  it.each(["amazonwebservices", "slack", "twilio"])(
    "keeps the restricted %s server mark on the neutral fallback",
    (slug) => {
      expect(readIfPresent("src", "lib", "serverLogo.ts")).not.toContain(`"${slug}"`);
      expect(
        readIfPresent("src-tauri", "src", "linux_native", "branding.rs"),
      ).not.toContain(`"${slug}"`);
      expect(existsSync(join(root, "src", "assets", "server-logos", `${slug}.svg`))).toBe(
        false,
      );
      expect(
        existsSync(join(root, "src-tauri", "icons", "server-logos", `${slug}.png`)),
      ).toBe(false);
    },
  );
});
