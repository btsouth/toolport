// Pins server.json (the official MCP registry manifest) to package.json and to
// the label the registry reads from the gateway image, so a release bump that
// forgets it fails CI on main, before scripts/check-release-version.mjs would
// stop the tag. docker-publish.yml publishes this file after each release.
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Vitest's cwd is the repo root (see homebrew-cask.test.ts for why not import.meta.url).
const repoRoot = process.cwd();
const read = (file: string) => readFileSync(join(repoRoot, file), "utf8");

const pkg = JSON.parse(read("package.json")) as { version: string };
const server = JSON.parse(read("server.json")) as {
  name: string;
  description: string;
  version: string;
  packages: { registryType: string; identifier: string }[];
};

describe("MCP registry manifest (server.json)", () => {
  it("stays in version lockstep with package.json and the image tag", () => {
    expect(server.version).toBe(pkg.version);
    expect(server.packages.map((p) => [p.registryType, p.identifier])).toEqual([
      ["oci", `ghcr.io/btsouth/toolport-gateway:v${pkg.version}`],
    ]);
  });

  it("names the server the way both Dockerfiles label the image", () => {
    for (const file of ["Dockerfile", "Dockerfile.source"]) {
      const labels = [
        ...read(file).matchAll(
          /^LABEL io\.modelcontextprotocol\.server\.name="([^"]+)"$/gm,
        ),
      ];
      expect(
        labels.map((m) => m[1]),
        file,
      ).toEqual([server.name]);
    }
  });

  it("keeps the description within the registry's 100 characters", () => {
    expect(server.description.length).toBeLessThanOrEqual(100);
    expect(server.description).not.toMatch(/[\u2013\u2014]/);
  });
});
