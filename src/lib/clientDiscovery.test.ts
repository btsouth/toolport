import { describe, expect, it } from "vitest";
import { chosenDiscoveryMode, clientDiscovery } from "@/lib/clientDiscovery";
import type { DetectedClient, Registry } from "@/lib/types";

const codex: DetectedClient = {
  id: "codex",
  name: "Codex",
  usesConnectors: false,
  configPath: "/tmp/codex.toml",
  configExists: true,
  appPresent: true,
  servers: [],
  pluginServers: [],
  gatewayInstalled: true,
  entryState: "managed",
  error: null,
};

function registry(over: Partial<Registry> = {}): Registry {
  return { version: 1, servers: [], profiles: [], activeProfileId: null, ...over };
}

describe("clientDiscovery", () => {
  it("names every mode the backend accepts, code-mode spellings included", () => {
    for (const mode of ["full", "lazy", "grouped", "code-mode"]) {
      expect(chosenDiscoveryMode(mode)).toBe(mode);
      expect(chosenDiscoveryMode(` ${mode.toUpperCase()} `)).toBe(mode);
    }
    for (const alias of ["codemode", "code_mode", "code", "Code-Mode"]) {
      expect(chosenDiscoveryMode(alias)).toBe("code-mode");
    }
    for (const auto of ["", "inherit", "typo", null, undefined]) {
      expect(chosenDiscoveryMode(auto)).toBe("");
    }
  });

  it("resolves a client's code-mode override and a global code-mode choice", () => {
    expect(
      clientDiscovery(codex, registry({ clientDiscovery: { codex: "code-mode" } })),
    ).toEqual({
      autoMode: "lazy",
      autoFullMax: null,
      clientMode: "code-mode",
      effectiveMode: "code-mode",
    });
    expect(
      clientDiscovery(codex, registry({ clientDiscovery: { codex: "codemode" } }))
        .effectiveMode,
    ).toBe("code-mode");
    const global = clientDiscovery(
      {
        ...codex,
        discovery: {
          autoMode: "lazy",
          autoFullMaxTools: 120,
          nativeToolSearch: true,
          toolsListChanged: null,
          evidence: "test",
        },
      },
      registry({ discoveryMode: "code_mode" }),
    );
    expect(global).toEqual({
      autoMode: "code-mode",
      autoFullMax: null,
      clientMode: "",
      effectiveMode: "code-mode",
    });
  });
});
