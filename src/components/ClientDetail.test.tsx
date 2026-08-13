import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ClientDetail } from "./ClientDetail";
import type { DetectedClient, Registry } from "@/lib/types";

const installGateway = vi.fn();
const uninstallGateway = vi.fn();
const migrateClient = vi.fn();
const toastSuccess = vi.fn();
const toastError = vi.fn();

vi.mock("@/lib/api", () => ({
  installGateway: (...a: unknown[]) => installGateway(...a),
  uninstallGateway: (...a: unknown[]) => uninstallGateway(...a),
  migrateClient: (...a: unknown[]) => migrateClient(...a),
  setClientDiscovery: vi.fn(),
  addServer: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: {
    success: (...a: unknown[]) => toastSuccess(...a),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@/lib/toast", () => ({
  toastError: (...a: unknown[]) => toastError(...a),
}));

function client(over: Partial<DetectedClient> = {}): DetectedClient {
  return {
    id: "claude-desktop",
    name: "Claude Desktop",
    usesConnectors: false,
    configPath: "C:\\Users\\me\\Claude\\claude_desktop_config.json",
    configExists: true,
    gatewayInstalled: false,
    entryState: "absent",
    appPresent: true,
    servers: [],
    pluginServers: [],
    error: null,
    ...over,
  };
}

function emptyRegistry(): Registry {
  return {
    version: 1,
    servers: [],
    profiles: [],
    activeProfileId: null,
  };
}

beforeEach(() => {
  installGateway.mockReset();
  uninstallGateway.mockReset();
  migrateClient.mockReset();
  toastSuccess.mockReset();
  toastError.mockReset();
});

describe("ClientDetail detection errors", () => {
  it("shows the client error in the main panel", () => {
    render(
      <ClientDetail
        client={client({ error: "Couldn't parse config: unexpected token" })}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Couldn't parse config: unexpected token",
    );
    expect(screen.getByRole("button", { name: /connect to toolport/i })).toBeEnabled();
  });
});

describe("ClientDetail customized entry (SOU-406)", () => {
  function customizedClient() {
    return client({
      gatewayInstalled: true,
      entryState: "customized",
      servers: [
        {
          name: "toolport",
          transport: "stdio",
          command: "npx",
          args: ["-y", "mcp-remote", "http://localhost:8765/mcp"],
          envKeys: [],
          url: null,
        },
      ],
    });
  }

  it("shows custom configuration badge and Reset to default", () => {
    render(
      <ClientDetail
        client={customizedClient()}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    expect(screen.getAllByText(/custom configuration/i).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /reset to default/i })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /connect to toolport/i }),
    ).not.toBeInTheDocument();
    // Managed onboarding copy must not claim scope is reachable for a customized entry.
    expect(screen.queryByText(/connect .* once and it reaches/i)).not.toBeInTheDocument();
  });

  it("calls installGateway with force=true after confirming Reset to default", async () => {
    installGateway.mockResolvedValue({ backup: false });
    render(
      <ClientDetail
        client={customizedClient()}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    // Open confirm dialog, then click the dialog's confirm action (last match).
    await userEvent.click(screen.getByRole("button", { name: /reset to default/i }));
    const confirms = screen.getAllByRole("button", { name: /reset to default/i });
    await userEvent.click(confirms[confirms.length - 1]!);

    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith(
        "claude-desktop",
        undefined,
        true,
        "stdio",
      ),
    );
  });

  it("passes live sharedHttp transport when applying scope (WS3-2)", async () => {
    // Regression: installGateway defaults missing transport to stdio and would
    // silently rewrite a Shared HTTP client as a stdio spawn.
    installGateway.mockResolvedValue({ backup: false });
    const reg = emptyRegistry();
    // Profile picker only renders when profiles.length > 1.
    reg.profiles = [
      { id: "p1", name: "Work", enabledServerIds: [] },
      { id: "p2", name: "Home", enabledServerIds: [] },
    ];
    reg.clientScopes = { "claude-desktop": "Work" };
    reg.clientManagedEntries = {
      "claude-desktop": {
        command: "",
        args: [],
        env: {},
        transport: "sharedHttp",
        url: "http://127.0.0.1:8765/mcp",
        updatedAt: 1,
      },
    };
    const connected = {
      ...client(),
      gatewayInstalled: true,
      entryState: "managed" as const,
    };
    render(
      <ClientDetail
        client={connected}
        registry={reg}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    // Change scope so Apply scope is enabled (profile !== currentScope).
    // Scope select is the first combobox in the header (w-52); discovery is lower.
    const scopeSelect = screen.getAllByRole("combobox")[0]!;
    await userEvent.click(scopeSelect);
    const home = await screen.findByRole("option", { name: /Only: Home/i });
    await userEvent.click(home);

    const apply = await screen.findByRole("button", { name: /apply scope/i });
    await userEvent.click(apply);

    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith(
        "claude-desktop",
        "p2",
        false,
        "sharedHttp",
      ),
    );
  });

  it("selects a scope stored as a profile id", async () => {
    const reg = emptyRegistry();
    reg.profiles = [
      { id: "p1", name: "Work", enabledServerIds: [] },
      { id: "p2", name: "Home", enabledServerIds: [] },
    ];
    reg.clientScopes = { "claude-desktop": "p1" };
    const connected = {
      ...client(),
      gatewayInstalled: true,
      entryState: "managed" as const,
    };
    render(
      <ClientDetail
        client={connected}
        registry={reg}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    expect(screen.getAllByRole("combobox")[0]).toHaveTextContent("Only: Work");
  });

  it("passes a stable profile id from the migrate dialog", async () => {
    const reg = emptyRegistry();
    reg.profiles = [
      { id: "p1", name: "Work", enabledServerIds: [] },
      { id: "p2", name: "Home", enabledServerIds: [] },
    ];
    migrateClient.mockResolvedValue({ registry: reg, moved: ["calendar"] });
    render(
      <ClientDetail
        client={client({
          servers: [
            {
              name: "calendar",
              transport: "stdio",
              command: "calendar-mcp",
              args: [],
              envKeys: [],
              url: null,
            },
          ],
        })}
        registry={reg}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /move into gateway/i }));
    const dialog = screen.getByRole("dialog");
    const scopeSelect = dialog.querySelector('[role="combobox"]');
    expect(scopeSelect).not.toBeNull();
    await userEvent.click(scopeSelect!);
    await userEvent.click(await screen.findByRole("option", { name: /Only: Home/i }));
    await userEvent.click(screen.getByRole("button", { name: /move 1 into toolport/i }));

    await waitFor(() =>
      expect(migrateClient).toHaveBeenCalledWith(
        "claude-desktop",
        "p2",
        undefined,
        "stdio",
      ),
    );
  });
});

describe("ClientDetail connect toast (SOU-317)", () => {
  it("tells the user to restart the client after a successful Connect", async () => {
    // Without this, the UI says "Connected" while Claude Desktop (and most peers)
    // still has the old MCP config in memory and Toolport looks broken.
    installGateway.mockResolvedValue({ backup: false });
    render(
      <ClientDetail
        client={client()}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));

    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith(
        "claude-desktop",
        undefined,
        false,
        "stdio",
      ),
    );
    expect(toastSuccess).toHaveBeenCalledWith(
      "Connected Toolport to Claude Desktop",
      expect.objectContaining({
        description: "Restart Claude Desktop so it loads Toolport.",
      }),
    );
  });

  it("includes scope detail after the restart nudge when connecting with a profile", async () => {
    installGateway.mockResolvedValue({ backup: false });
    // Seed clientScopes so the component's profile state initializes to Work.
    const reg = emptyRegistry();
    reg.profiles = [{ id: "p1", name: "Work", enabledServerIds: [] }];
    reg.clientScopes = { "claude-desktop": "Work" };

    render(
      <ClientDetail
        client={client()}
        registry={reg}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );

    // Already connected would show Disconnect; for connect we need uninstalled.
    // clientScopes still pre-fills the profile picker for a fresh connect.
    await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));

    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith("claude-desktop", "p1", false, "stdio"),
    );
    expect(toastSuccess).toHaveBeenCalledWith(
      "Connected Toolport to Claude Desktop",
      expect.objectContaining({
        description:
          'Restart Claude Desktop so it loads Toolport. Scoped to the "Work" profile.',
      }),
    );
  });
});
