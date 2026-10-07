import { describe, expect, it } from "vitest";
import {
  importableServers,
  hasLegacyBearerArgv,
  isGatewayDetected,
  isGatewayServer,
  type DetectedClient,
  type McpServer,
  type ServerEntry,
} from "./types";

describe("gateway identity", () => {
  it("recognizes the legacy toolport ID and name without relying on the command", () => {
    const server: ServerEntry = {
      id: "TOOLPORT",
      name: "Toolport",
      transport: "stdio",
      command: "manual-wrapper",
      args: [],
      env: [],
      url: null,
      source: null,
    };

    expect(isGatewayServer(server)).toBe(true);
  });

  it("never surfaces a legacy toolport entry as an import candidate", () => {
    const client: DetectedClient = {
      id: "claude-code",
      name: "Claude Code",
      usesConnectors: false,
      configPath: "config.json",
      configExists: true,
      appPresent: true,
      servers: [
        {
          name: "toolport",
          transport: "stdio",
          command: "manual-wrapper",
          args: [],
          envKeys: [],
          url: null,
        },
      ],
      pluginServers: [],
      gatewayInstalled: true,
      entryState: "managed",
      error: null,
    };

    expect(importableServers(client, null)).toEqual([]);
  });

  it("recognizes a detected entry under both the current and pre-rename names", () => {
    const detected = (name: string): McpServer => ({
      name,
      transport: "stdio",
      command: null,
      args: [],
      envKeys: [],
      url: null,
    });
    // Current name and the pre-rename `conduit` name both count as the gateway, so
    // the UI hides it during the migration window regardless of which one is on disk.
    expect(isGatewayDetected(detected("toolport"))).toBe(true);
    expect(isGatewayDetected(detected("conduit"))).toBe(true);
    expect(isGatewayDetected(detected("linear"))).toBe(false);
  });
});

describe("legacy bearer argv", () => {
  const remote: McpServer = {
    name: "toolport",
    command: "npx",
    args: [
      "-y",
      "mcp-remote",
      "http://127.0.0.1/mcp",
      "--header",
      "Authorization: Bearer fixture-canary",
    ],
    transport: "stdio",
    envKeys: [],
    url: null,
  };
  it("detects literal credentials only on the preserved gateway shim", () => {
    expect(hasLegacyBearerArgv(remote)).toBe(true);
    expect(hasLegacyBearerArgv({ ...remote, name: "custom remote" })).toBe(false);
    expect(
      hasLegacyBearerArgv({
        ...remote,
        args: ["mcp-remote", "Authorization: Bearer ${TOKEN}"],
      }),
    ).toBe(false);
    expect(hasLegacyBearerArgv({ ...remote, args: ["mcp-remote"] })).toBe(false);
  });
});
