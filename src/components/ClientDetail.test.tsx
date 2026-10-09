import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ClientDetail } from "./ClientDetail";
import { importServers, previewImportServers } from "@/lib/api";
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
  previewClientSetup: vi.fn().mockResolvedValue({
    configPath: "/fixture/client.json",
    backupDir: "/fixture/backups",
    revision: "fixture-revision",
    items: [
      {
        key: "calendar",
        name: "calendar",
        transport: "stdio",
        command: "calendar-mcp",
        args: [],
        url: null,
        isNew: true,
      },
    ],
  }),
  setClientDiscovery: vi.fn(),
  importServers: vi.fn(),
  previewImportServers: vi.fn().mockResolvedValue([]),
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
  vi.mocked(importServers).mockReset();
  vi.mocked(previewImportServers).mockReset().mockResolvedValue([]);
  installGateway.mockReset();
  uninstallGateway.mockReset();
  migrateClient.mockReset();
  toastSuccess.mockReset();
  toastError.mockReset();
});

it("imports the reviewed definition when another client has the same server name", async () => {
  const server = {
    name: "calendar",
    transport: "stdio" as const,
    command: "calendar-mcp",
    args: ["--workspace", "reviewed"],
    envKeys: [],
    url: null,
  };
  vi.mocked(previewImportServers).mockResolvedValue([
    { ...server, args: ["--workspace", "other"], key: "other-client", isNew: true },
    { ...server, key: "reviewed-client", isNew: true },
  ]);
  vi.mocked(importServers).mockResolvedValue(emptyRegistry());
  render(
    <ClientDetail
      client={client({ servers: [server] })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: /^Import$/ }));
  await userEvent.click(screen.getByRole("button", { name: /^Import 1 server$/ }));
  await waitFor(() => expect(importServers).toHaveBeenCalledWith(["reviewed-client"]));
});

it("uses bulk review storage choices and leaves unsupported servers unchecked", async () => {
  const server = {
    name: "calendar",
    transport: "stdio" as const,
    command: "calendar-mcp",
    args: [],
    envKeys: ["PORT"],
    url: null,
  };
  const unsupported = { ...server, name: "unsupported", command: "custom-mcp" };
  vi.mocked(previewImportServers).mockResolvedValue([
    {
      ...server,
      key: "reviewed-safe",
      isNew: true,
      credentials: [{ key: "PORT", secret: false, present: true, required: true }],
    },
    {
      ...unsupported,
      key: "reviewed-unsupported",
      isNew: true,
      unsupported: "Custom HTTP headers",
    },
  ]);
  vi.mocked(importServers).mockResolvedValue(emptyRegistry());
  render(
    <ClientDetail
      client={client({ servers: [server, unsupported] })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Import all (2)" }));
  const choice = await screen.findByRole("checkbox", { name: /Keep PORT in keychain/ });
  expect(choice).not.toBeChecked();
  await userEvent.click(choice);
  await userEvent.click(screen.getByRole("button", { name: "Import 1 server" }));
  await waitFor(() =>
    expect(importServers).toHaveBeenCalledWith(["reviewed-safe"], {
      calendar: { PORT: true },
      unsupported: {},
    }),
  );
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
      expect(installGateway).toHaveBeenCalledWith("claude-desktop", undefined, true),
    );
  });

  it("keeps sharedHttp untouched on mount and uses stdio on explicit reconnect", async () => {
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

    expect(installGateway).not.toHaveBeenCalled();
    expect(migrateClient).not.toHaveBeenCalled();

    // Change scope so Apply scope is enabled (profile !== currentScope).
    // Scope select is the first combobox in the header (w-52); discovery is lower.
    const scopeSelect = screen.getAllByRole("combobox")[0]!;
    await userEvent.click(scopeSelect);
    const home = await screen.findByRole("option", { name: /^Home$/i });
    await userEvent.click(home);

    const apply = await screen.findByRole("button", { name: /apply access/i });
    await userEvent.click(apply);

    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith("claude-desktop", "p2", false),
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
    expect(screen.getAllByRole("combobox")[0]).toHaveTextContent("Work");
  });

  it("passes a stable profile id from the migrate dialog", async () => {
    const reg = emptyRegistry();
    reg.profiles = [
      { id: "p1", name: "Work", enabledServerIds: [] },
      { id: "p2", name: "Home", enabledServerIds: [] },
    ];
    migrateClient.mockResolvedValue({
      registry: reg,
      moved: ["calendar"],
      tools: [{ name: "calendar__read" }],
      servers: [{ name: "calendar", toolCount: 2, credentialState: "none" }],
      outcome: { path: "/fixture/client.json", backup: "/fixture/backups/previous.json" },
    });
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

    await userEvent.click(screen.getByRole("combobox", { name: "Access" }));
    await userEvent.click(await screen.findByRole("option", { name: /^Home$/i }));
    await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));
    await userEvent.click(
      await screen.findByRole("button", { name: /connect to toolport/i }),
    );

    await waitFor(() =>
      expect(migrateClient).toHaveBeenCalledWith(
        "claude-desktop",
        "p2",
        false,
        ["calendar"],
        "fixture-revision",
      ),
    );
  });
});

describe("ClientDetail reviewed connection", () => {
  it("requires review and shows actual tools with restart and backup paths", async () => {
    migrateClient.mockResolvedValue({
      registry: emptyRegistry(),
      imported: 1,
      moved: ["calendar"],
      tools: [{ name: "calendar__read" }],
      servers: [{ name: "calendar", toolCount: 2, credentialState: "none" }],
      outcome: { path: "/fixture/client.json", backup: "/fixture/backups/previous.json" },
    });
    render(
      <ClientDetail
        client={client()}
        registry={emptyRegistry()}
        onChanged={vi.fn()}
        onRegistryChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));
    await userEvent.click(await screen.findByText("Details"));
    expect(await screen.findByText(/Backups will be saved/)).toHaveTextContent(
      "/fixture/backups",
    );
    expect(migrateClient).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));
    await userEvent.click(await screen.findByText("What your agent sees"));
    expect(await screen.findByText("calendar__read")).toBeVisible();
    await userEvent.click(screen.getByText("Details"));
    expect(screen.getByText("Restart Claude Desktop to load Toolport.")).toBeVisible();
    expect(screen.getByText("Backup: /fixture/backups/previous.json")).toBeVisible();
    expect(installGateway).not.toHaveBeenCalled();
  });
});

it("shows a migrated default without rewriting the client and explicitly applies All", async () => {
  const reg = emptyRegistry();
  reg.version = 3;
  reg.profiles = [{ id: "default", name: "Default", enabledServerIds: ["on"] }];
  reg.defaultAccessProfileId = "default";
  reg.clientScopes = { "claude-desktop": "" };
  reg.servers = ["on", "off"].map((id) => ({
    id,
    name: id,
    enabled: id === "on",
    transport: "http",
    command: null,
    args: [],
    env: [],
    url: null,
    source: null,
  }));
  installGateway.mockResolvedValue({ backup: false });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={reg}
      onChanged={() => {}}
      onRegistryChange={() => {}}
    />,
  );
  expect(screen.getByRole("combobox", { name: "Access" })).toHaveTextContent(
    "Default access (Default)",
  );
  expect(installGateway).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("combobox", { name: "Access" }));
  await userEvent.click(
    await screen.findByRole("option", { name: "All enabled servers" }),
  );
  await userEvent.click(screen.getByRole("button", { name: /apply access/i }));
  await waitFor(() =>
    expect(installGateway).toHaveBeenCalledWith("claude-desktop", "@all-enabled", false),
  );
});

it("restores Default access with an empty scope and retains the default tool limits", async () => {
  const reg = emptyRegistry();
  reg.version = 3;
  reg.defaultAccessContextId = "default";
  reg.defaultAccessLegacyPolicy = true;
  reg.profiles = [
    {
      id: "default",
      name: "Kept tools",
      enabledServerIds: [],
      toolScope: { s: ["read"] },
    },
  ];
  reg.clientScopes = { "claude-desktop": "@all-enabled" };
  installGateway.mockResolvedValue({ backup: false });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={reg}
      onChanged={() => {}}
      onRegistryChange={() => {}}
    />,
  );
  expect(screen.getByRole("combobox", { name: "Access" })).toHaveTextContent(
    "All enabled servers",
  );
  await userEvent.click(screen.getByRole("combobox", { name: "Access" }));
  await userEvent.click(
    await screen.findByRole("option", { name: "Default access (Kept tools)" }),
  );
  await userEvent.click(screen.getByRole("button", { name: /apply access/i }));
  await waitFor(() =>
    expect(installGateway).toHaveBeenCalledWith("claude-desktop", undefined, false),
  );
  expect(reg.profiles[0]?.toolScope).toEqual({ s: ["read"] });
});

describe("ClientDetail legacy bearer migration", () => {
  it("preserves the connection through render, review and cancel; migrates only on confirmation", async () => {
    installGateway.mockResolvedValue({ backup: true });
    const legacy = client({
      gatewayInstalled: true,
      entryState: "customized",
      servers: [
        {
          name: "toolport",
          transport: "stdio",
          command: "npx",
          args: ["mcp-remote", "Authorization: Bearer fixture-canary"],
          envKeys: [],
          url: null,
        },
      ],
    });
    render(
      <ClientDetail
        client={legacy}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    expect(installGateway).not.toHaveBeenCalled();
    expect(screen.queryByText(/fixture-canary/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Review migration" }));
    expect(screen.getByText(/backs up the config/)).toBeInTheDocument();
    expect(installGateway).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(installGateway).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Review migration" }));
    await userEvent.click(screen.getByRole("button", { name: "Migrate to stdio" }));
    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith("claude-desktop", undefined, true),
    );
    expect(migrateClient).not.toHaveBeenCalled();
  });
});

describe("ClientDetail Auto discovery", () => {
  it.each([
    ["claude-code", "lazy"],
    ["codex", "full"],
    ["cursor", "full"],
    ["opencode", "lazy"],
  ] as const)("shows the backend Auto default before connect (%s=%s)", (id, autoMode) => {
    render(
      <ClientDetail
        client={client({
          id,
          discovery: {
            autoMode,
            nativeToolSearch: true,
            toolsListChanged: null,
            evidence: "fixture",
          },
        })}
        registry={emptyRegistry()}
        onRegistryChange={vi.fn()}
        onChanged={vi.fn()}
      />,
    );
    expect(screen.getByText(`Auto (${autoMode})`)).toBeInTheDocument();
    expect(
      screen.getByText(
        autoMode === "full"
          ? "Full tool list. Client per-tool permission rules need Full mode."
          : "Search, then call tools. Client per-tool permission rules need Full mode.",
      ),
    ).toBeInTheDocument();
  });
  it.each(["grouped", " GROUPED "])(
    "keeps an explicit override instead of Auto (%s)",
    (mode) => {
      render(
        <ClientDetail
          client={client({
            gatewayInstalled: true,
            entryState: "managed",
            discovery: {
              nativeToolSearch: true,
              toolsListChanged: true,
              evidence: "fixture",
            },
          })}
          registry={{
            ...emptyRegistry(),
            clientDiscovery: { "claude-desktop": mode },
          }}
          onRegistryChange={vi.fn()}
          onChanged={vi.fn()}
        />,
      );
      expect(screen.getByText("Grouped · per-server")).toBeInTheDocument();
      expect(
        screen.getByText(
          "Browse a server, then call tools. Client per-tool permission rules need Full mode.",
        ),
      ).toBeInTheDocument();
    },
  );
});
