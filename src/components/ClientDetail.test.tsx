import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import { TooltipProvider } from "@/components/ui/tooltip";
import userEvent from "@testing-library/user-event";
import { ClientDetail } from "./ClientDetail";
import { setLanguage } from "@/lib/i18n";
import {
  resetClientNotices,
  setConnectNotice,
  setDetailNotice,
  useClientNotices,
} from "@/lib/clientNotice";
import {
  importServers,
  setClientDiscovery,
  previewImportServers,
  listClientRecoveries,
  retryClientRecovery,
  type ClientRecovery,
} from "@/lib/api";
import type { DetectedClient, Registry } from "@/lib/types";

const installGateway = vi.fn();
const uninstallGateway = vi.fn();
const migrateClient = vi.fn();
const removeClientLocation = vi.fn();
const previewGatewaySettings = vi.fn();
const toastSuccess = vi.fn();
const toastError = vi.fn();

vi.mock("@/lib/api", () => ({
  getUiLanguage: vi.fn().mockResolvedValue("en"),
  setUiLanguage: vi.fn().mockResolvedValue(undefined),
  listClientRecoveries: vi.fn().mockResolvedValue([]),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
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
  previewGatewaySettings: (...a: unknown[]) => previewGatewaySettings(...a),
  setClientDiscovery: vi.fn(),
  removeClientLocation: (...a: unknown[]) => removeClientLocation(...a),
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
  resetClientNotices();
  vi.mocked(listClientRecoveries).mockResolvedValue([]);
  vi.mocked(importServers).mockReset();
  vi.mocked(previewImportServers).mockReset().mockResolvedValue([]);
  installGateway.mockReset();
  uninstallGateway.mockReset();
  migrateClient.mockReset();
  previewGatewaySettings.mockReset().mockResolvedValue([]);
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
  expect(previewImportServers).toHaveBeenCalledWith("claude-desktop");
  expect(screen.getByRole("status")).toHaveTextContent("Imported selected servers");
  expect(toastSuccess).not.toHaveBeenCalled();
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
  // The same chevron fold marker as the connect dialogs and GTK's expander.
  expect(
    screen
      .getByText("How importing works")
      .closest("summary")!
      .querySelector('[data-slot="disclosure-chevron"]'),
  ).not.toBeNull();
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

it("lists backend import candidates by connection, not by registry name", async () => {
  const remote = {
    name: "github",
    transport: "http" as const,
    command: null,
    args: [],
    envKeys: [],
    url: "https://api.githubcopilot.com/mcp/",
  };
  const notes = {
    ...remote,
    name: "notes",
    transport: "stdio" as const,
    command: "notes-mcp",
    url: null,
  };
  const linear = { ...notes, name: "linear", command: "linear-mcp" };
  const registry = {
    ...emptyRegistry(),
    servers: [{ name: "github" }, { name: "my-notes" }, { name: "linear" }],
  } as unknown as Registry;
  vi.mocked(previewImportServers).mockResolvedValue([
    {
      ...remote,
      name: "github (2)",
      sourceName: "github",
      key: "remote",
      connection: "connection:remote",
      isNew: true,
    },
    {
      ...linear,
      name: "linear (2)",
      sourceName: "linear",
      key: "linear",
      connection: "connection:linear",
      isNew: true,
    },
  ]);
  vi.mocked(importServers).mockResolvedValue(emptyRegistry());
  const onChanged = vi.fn();
  render(
    <ClientDetail
      client={client({
        servers: [remote, notes],
        pluginServers: [linear],
        importableServers: [
          { ...remote, name: "github (2)" },
          { ...linear, name: "linear (2)" },
        ],
        importableKeys: ["connection:remote", "connection:linear"],
        managedServerNames: ["notes"],
      })}
      registry={registry}
      onChanged={onChanged}
      onRegistryChange={vi.fn()}
    />,
  );
  const card = (name: string) =>
    screen.getByText(name, { selector: "span.font-medium" }).closest("[data-slot=card]")!;
  expect(
    within(card("github (2)") as HTMLElement).getByRole("button", { name: /Import/ }),
  ).toBeEnabled();
  expect(within(card("linear (2)") as HTMLElement).getByText("plugin")).toBeVisible();
  expect(within(card("notes") as HTMLElement).getByText(/in Toolport/)).toBeVisible();
  expect(screen.queryByText("github", { selector: "span.font-medium" })).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: "Import all (2)" }));
  expect(await screen.findByText("Import as github (2)")).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Import 2 servers" }));
  await waitFor(() => expect(importServers).toHaveBeenCalledWith(["remote", "linear"]));
  expect(onChanged).toHaveBeenCalled();
});

it("reviews the row's own server when two servers redact alike", async () => {
  const twin = (name: string) => ({
    name,
    transport: "http" as const,
    command: null,
    args: [],
    envKeys: [],
    url: "https://example.test/mcp",
  });
  vi.mocked(previewImportServers).mockResolvedValue([
    { ...twin("personal"), key: "key-personal", connection: "c:a", isNew: true },
    { ...twin("work"), key: "key-work", connection: "c:b", isNew: true },
  ]);
  vi.mocked(importServers).mockResolvedValue(emptyRegistry());
  render(
    <ClientDetail
      client={client({
        servers: [twin("personal"), twin("work")],
        importableServers: [twin("personal"), twin("work")],
        importableKeys: ["c:a", "c:b"],
        managedServerNames: [],
      })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  const work = screen
    .getByText("work", { selector: "span.font-medium" })
    .closest("[data-slot=card]") as HTMLElement;
  await userEvent.click(within(work).getByRole("button", { name: /Import/ }));
  await userEvent.click(await screen.findByRole("button", { name: "Import 1 server" }));
  await waitFor(() => expect(importServers).toHaveBeenCalledWith(["key-work"]));
  await userEvent.click(screen.getByRole("button", { name: "Import all (2)" }));
  await userEvent.click(await screen.findByRole("button", { name: "Import 2 servers" }));
  await waitFor(() =>
    expect(importServers).toHaveBeenLastCalledWith(["key-personal", "key-work"]),
  );
});

it("lists servers only review can check and confirms them there", async () => {
  const docs = {
    name: "docs",
    transport: "http" as const,
    command: null,
    args: [],
    envKeys: [],
    url: "https://docs.example.test/mcp",
  };
  const context7 = {
    ...docs,
    name: "context7",
    url: "https://context7.example.test/mcp",
  };
  vi.mocked(previewImportServers).mockResolvedValue([]);
  render(
    <ClientDetail
      client={client({
        servers: [docs, context7],
        importableServers: [],
        importableKeys: [],
        managedServerNames: ["context7"],
        unconfirmedServers: [docs],
        unconfirmedKeys: ["c:docs"],
      })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  const card = screen
    .getByText("docs", { selector: "span.font-medium" })
    .closest("[data-slot=card]") as HTMLElement;
  expect(
    within(card).getByText(
      "Open the review to check whether this is already in Toolport.",
    ),
  ).toBeVisible();
  expect(within(card).queryByText(/in Toolport$/)).toBeNull();
  expect(screen.queryByText(/Everything here is already in Toolport/)).toBeNull();
  expect(screen.queryByRole("button", { name: /Import all/ })).toBeNull();
  await userEvent.click(within(card).getByRole("button", { name: "Review" }));
  expect(await screen.findByText("docs is already in Toolport.")).toBeVisible();
  // When review still lists it, the review opens on it.
  vi.mocked(previewImportServers).mockResolvedValue([
    {
      ...docs,
      key: "k:docs",
      connection: "c:docs",
      isNew: false,
      unsupported: "Could not compare docs.",
    },
  ]);
  await userEvent.click(within(card).getByRole("button", { name: "Review" }));
  expect(await screen.findByRole("dialog")).toBeVisible();
});

it("shows a refused import inside the review that is still open", async () => {
  const server = {
    name: "github",
    transport: "stdio" as const,
    command: "npx",
    args: ["-y", "@acme/github-mcp"],
    envKeys: [],
    url: null,
  };
  vi.mocked(previewImportServers).mockResolvedValue([
    { ...server, key: "k:github", connection: "c:a", isNew: true, sourceName: "github" },
  ]);
  vi.mocked(importServers).mockRejectedValue(
    "Server changed. Refresh the client and review again.",
  );
  render(
    <ClientDetail
      client={client({
        servers: [server],
        importableServers: [server],
        importableKeys: ["c:a"],
        managedServerNames: [],
      })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Import all (1)" }));
  await userEvent.click(await screen.findByRole("button", { name: "Import 1 server" }));
  const dialog = await screen.findByRole("dialog");
  expect(
    await within(dialog).findByText(
      "Server changed. Refresh the client and review again.",
    ),
  ).toBeVisible();
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
    expect(screen.getByRole("button", { name: /connect to toolport/i })).toBeDisabled();
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

  it("labels the connection in sentence case, as the Clients list does", () => {
    const view = render(
      <ClientDetail
        client={client({ gatewayInstalled: true, entryState: "managed" })}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    const badge = view.container.querySelector('[data-slot="badge"]');
    expect(badge).toHaveTextContent(/^Connected$/);
    expect(badge).toHaveAttribute("data-variant", "success");
    view.unmount();
    render(
      <ClientDetail
        client={customizedClient()}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    expect(screen.getByText("Custom configuration")).toBeInTheDocument();
  });

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

  it("lists the Codex settings a reset adds before it is confirmed", async () => {
    installGateway.mockResolvedValue({ backup: false });
    previewGatewaySettings.mockResolvedValue(["tool_timeout_sec = 600"]);
    render(
      <ClientDetail
        client={{ ...customizedClient(), id: "codex", name: "Codex" }}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: /reset to default/i }));
    const dialog = await screen.findByRole("dialog");
    expect(
      await within(dialog).findByText(/its Codex entry, each only if it is not set/),
    ).toBeVisible();
    expect(within(dialog).getByText("tool_timeout_sec = 600")).toBeVisible();
    expect(previewGatewaySettings).toHaveBeenCalledWith("codex");
    const confirms = within(dialog).getAllByRole("button", { name: /reset to default/i });
    await userEvent.click(confirms[confirms.length - 1]!);
    await waitFor(() =>
      expect(installGateway).toHaveBeenCalledWith("codex", undefined, true),
    );
  });

  it("lists no settings in the reset confirmation for a client that gets none", async () => {
    previewGatewaySettings.mockResolvedValue([]);
    render(
      <ClientDetail
        client={customizedClient()}
        registry={emptyRegistry()}
        onChanged={() => {}}
        onRegistryChange={() => {}}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: /reset to default/i }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(
        within(dialog)
          .getAllByRole("button", { name: /reset to default/i })
          .at(-1),
      ).toBeEnabled(),
    );
    expect(
      within(dialog).queryByText(/each only if it is not set/),
    ).not.toBeInTheDocument();
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
    /^Default access \(from Settings\)$/,
  );
  expect(installGateway).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("combobox", { name: "Access" }));
  // The default and the set named "Default" read as two different choices.
  expect(
    (await screen.findAllByRole("option")).map((option) => option.textContent),
  ).toEqual(["Default access (from Settings)", "All enabled servers", "Default"]);
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
  it("shows the small-catalog Full limit in Auto unless a global mode was chosen", () => {
    const discovery = {
      autoMode: "lazy" as const,
      autoFullMaxTools: 800,
      nativeToolSearch: true,
      toolsListChanged: true,
      evidence: "fixture",
    };
    const { unmount } = render(
      <ClientDetail
        client={client({ id: "claude-code", discovery })}
        registry={emptyRegistry()}
        onRegistryChange={vi.fn()}
        onChanged={vi.fn()}
      />,
    );
    expect(
      screen.getByText("Auto (full up to 800 tools, then lazy)"),
    ).toBeInTheDocument();
    unmount();
    render(
      <ClientDetail
        client={client({ id: "claude-code", discovery })}
        registry={{ ...emptyRegistry(), discoveryMode: "lazy" }}
        onRegistryChange={vi.fn()}
        onChanged={vi.fn()}
      />,
    );
    expect(screen.getByText("Auto (lazy)")).toBeInTheDocument();
  });
  it("shows the Lazy a 1.x upgrade kept instead of the client default", () => {
    render(
      <ClientDetail
        client={client({
          id: "codex",
          discovery: {
            autoMode: "full",
            nativeToolSearch: true,
            toolsListChanged: false,
            evidence: "fixture",
          },
        })}
        registry={{ ...emptyRegistry(), legacyLazyDiscovery: true }}
        onRegistryChange={vi.fn()}
        onChanged={vi.fn()}
      />,
    );
    expect(screen.getByText("Auto (lazy)")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Search, then call tools. Client per-tool permission rules need Full mode.",
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

it("removes a disconnected manual location through the confirmation flow", async () => {
  const user = userEvent.setup();
  const onChanged = vi.fn();
  removeClientLocation.mockResolvedValue(undefined);
  render(
    <ClientDetail
      client={client({ id: "codex@fixture", locationRemovable: true })}
      registry={emptyRegistry()}
      onChanged={onChanged}
      onRegistryChange={vi.fn()}
    />,
  );
  await user.click(screen.getByRole("button", { name: "Remove location" }));
  await user.click(screen.getByRole("button", { name: "Remove location" }));
  expect(removeClientLocation).toHaveBeenCalledWith("codex@fixture");
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
});

it("requires disconnect before forgetting a connected location", () => {
  render(
    <ClientDetail
      client={client({
        locationRemovable: true,
        gatewayInstalled: true,
        entryState: "managed",
      })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  expect(screen.getByRole("button", { name: "Remove location" })).toBeDisabled();
});

it("shows Remove location disabled with why while T3 Code still uses the folder", async () => {
  const user = userEvent.setup();
  const reason =
    "T3 Code still uses this folder. Remove it from T3 Code's provider settings first.";
  render(
    <TooltipProvider>
      <ClientDetail
        client={client({ id: "codex@t3-0123456789abcdef", locationKeptByT3Code: true })}
        registry={emptyRegistry()}
        onChanged={vi.fn()}
        onRegistryChange={vi.fn()}
      />
    </TooltipProvider>,
  );
  const remove = screen.getByRole("button", { name: "Remove location" });
  expect(remove).toBeDisabled();
  expect(remove).toHaveAccessibleDescription(reason);
  // Keyboard focus on the wrapper shows why.
  act(() => remove.parentElement!.focus());
  expect(await screen.findByRole("tooltip")).toHaveTextContent(reason);
  // Clicking does nothing: no confirmation, no error.
  await user.click(remove);
  expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("keeps the disabled Remove location's name and reason in the current language", async () => {
  render(
    <TooltipProvider>
      <ClientDetail
        client={client({ id: "codex@t3-0123456789abcdef", locationKeptByT3Code: true })}
        registry={emptyRegistry()}
        onChanged={vi.fn()}
        onRegistryChange={vi.fn()}
      />
    </TooltipProvider>,
  );
  await act(() => setLanguage("fr"));
  try {
    const remove = screen.getByRole("button", { name: "Supprimer l'emplacement" });
    expect(remove).toHaveAccessibleDescription(
      "T3 Code utilise encore ce dossier. Retirez-le d'abord des paramètres des fournisseurs de T3 Code.",
    );
  } finally {
    await act(() => setLanguage("en"));
  }
});

it("shows disconnect and restart feedback once inline without a duplicate toast", async () => {
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: ["calendar"],
    warnings: [],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const notice = await screen.findByRole("status");
  expect(notice).toHaveTextContent("Still connected until restart");
  expect(notice).toHaveTextContent("put back 1 server");
  expect(toastSuccess).not.toHaveBeenCalled();
  expect(toastError).not.toHaveBeenCalled();
});

it("ends the disconnect line before a kept-entry warning, as GTK does", async () => {
  const kept =
    "Toolport kept the toolport entry you edited. Remove it by hand if you uninstall Toolport.";
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: [],
    warnings: [kept],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "customized" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const notice = await screen.findByRole("status");
  expect(notice).toHaveTextContent(
    `Restart ${client().name} so it stops loading Toolport. Disconnected Toolport from ${client().name}. ${kept}`,
  );
});

it("shows the restart reminder as an info card with a Dismiss button", async () => {
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: [],
    warnings: [],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const notice = await screen.findByRole("status");
  expect(notice).toHaveAttribute("data-slot", "status-card");
  expect(notice).toHaveAttribute("data-variant", "info");
  expect(notice).not.toHaveClass("toolport-alert");
  const dismiss = within(notice).getByRole("button", { name: "Dismiss" });
  expect(dismiss).toHaveAttribute("data-variant", "ghost");
  await userEvent.click(dismiss);
  expect(screen.queryByText("Still connected until restart")).not.toBeInTheDocument();
});

it("lets the Discovery mode select show its whole label and wrap under its text", () => {
  render(
    <ClientDetail
      client={client()}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  const trigger = screen.getByRole("combobox", { name: "Discovery mode" });
  expect(trigger).not.toHaveClass("w-44");
  expect(trigger).toHaveClass("w-auto", "max-w-full");
  const row = trigger.parentElement!;
  expect(row).toHaveClass("flex", "flex-wrap");
  expect(row.firstElementChild).toHaveClass("min-w-[min(15rem,100%)]", "flex-1");
});

it("keeps a keychain warning next to the recovery notice after disconnect", async () => {
  const recovery = "Cursor needs a missing value restored.";
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: [],
    warnings: [
      `${recovery}\n\nChoose Retry.`,
      "config restored; could not remove the revoked keychain token: locked",
    ],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await waitFor(() => expect(listClientRecoveries).toHaveBeenCalled());
  const row: ClientRecovery = {
    id: "cursor-recovery",
    clientId: client().id,
    clientName: client().name,
    path: "/fixture/config",
    backupPath: "/fixture/backup",
    missing: [],
    dismissed: false,
    message: recovery,
    description: "",
    valueLines: [],
    discardValues: [],
    error: null,
    canDiscard: false,
    canRetry: true,
    discardStarted: false,
    discardAction: "Discard these saved values",
    cleanup: false,
    instructions: "Choose Retry.",
    retention: "",
    details: "",
  };
  vi.mocked(listClientRecoveries).mockResolvedValue([row]);
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const notice = await screen.findByText(/could not remove the revoked keychain token/);
  expect(notice).not.toHaveTextContent(recovery);
});

it("refreshes the committed disconnect even when recovery notices cannot be read", async () => {
  const onChanged = vi.fn();
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: [],
    warnings: [],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={onChanged}
      onRegistryChange={vi.fn()}
    />,
  );
  await waitFor(() => expect(listClientRecoveries).toHaveBeenCalled());
  vi.mocked(listClientRecoveries).mockRejectedValue(
    new Error("Recovery receipt is unreadable"),
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  await waitFor(() => expect(onChanged).toHaveBeenCalledOnce());
  expect(
    await screen.findByText("Error: Recovery receipt is unreadable"),
  ).toBeInTheDocument();
  expect(toastError).not.toHaveBeenCalled();
});

it("shows a refused disconnect inline with the client name, as GTK does", async () => {
  uninstallGateway.mockRejectedValue(
    "could not read /fixture/moved-servers.json: This recovery file is malformed.",
  );
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent(
    /^.+: could not read \/fixture\/moved-servers\.json: This recovery file is malformed\.$/,
  );
  expect(screen.queryByText("Still connected until restart")).not.toBeInTheDocument();
  expect(toastError).not.toHaveBeenCalled();
});

it("clears a refused disconnect once that client's recovery succeeds, as GTK does", async () => {
  uninstallGateway.mockRejectedValue("could not read its move record");
  vi.mocked(listClientRecoveries).mockResolvedValue([
    {
      id: "receipt-claude-desktop",
      clientId: "claude-desktop",
      clientName: "Claude Desktop",
      path: "/fixture/config.json",
      backupPath: "/fixture/backups/claude-desktop/original.json",
      dismissed: false,
      missing: [{ server: "keyring-echo", key: "API_KEY" }],
      message: "Claude Desktop is disconnected.",
      description: "Its original config is back.",
      valueLines: ["API_KEY for keyring-echo: keyring locked"],
      discardValues: ["API_KEY for keyring-echo"],
      error: null,
      canDiscard: true,
      canRetry: true,
      discardStarted: false,
      discardAction: "Discard these saved values",
      cleanup: false,
      instructions: "Unlock your keyring, then choose Retry.",
      retention: "Kept until recovery succeeds.",
      details: "",
    },
  ]);
  vi.mocked(retryClientRecovery).mockResolvedValue({
    id: "receipt-claude-desktop",
    clientId: "claude-desktop",
    clientName: "Claude Desktop",
    restored: 1,
    accounted: 0,
    remaining: 0,
    message: "Claude Desktop: saved values restored: 1.",
    pending: null,
    refreshed: false,
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Claude Desktop: could not read its move record",
  );
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
});

it("shows the recovery card a Disconnect leaves at once, without a rescan", async () => {
  uninstallGateway.mockResolvedValue({
    path: "/fixture/config",
    restored: [],
    warnings: [],
  });
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await waitFor(() => expect(listClientRecoveries).toHaveBeenCalled());
  expect(screen.queryByRole("region", { name: /Client config recovery/ })).toBeNull();
  const row: ClientRecovery = {
    id: "disconnect-recovery",
    clientId: client().id,
    clientName: client().name,
    path: "/fixture/config",
    backupPath: "/fixture/backup",
    missing: [{ server: "weather-demo", key: "WEATHER_API_KEY" }],
    dismissed: false,
    message: `${client().name} is disconnected.`,
    description:
      "Its original config is back, with temporary text where these values belong. Latest check of the saved values:",
    valueLines: ["WEATHER_API_KEY for weather-demo: keyring locked"],
    discardValues: ["WEATHER_API_KEY for weather-demo"],
    error: null,
    canDiscard: true,
    canRetry: true,
    discardStarted: false,
    discardAction: "Discard these saved values",
    cleanup: false,
    instructions: "If your keyring is locked, unlock it. Then choose Retry.",
    retention: "",
    details: "",
  };
  vi.mocked(listClientRecoveries).mockResolvedValue([row]);
  await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Disconnect" }),
  );
  const card = await screen.findByRole("region", {
    name: `Client config recovery: ${client().name}`,
  });
  expect(
    within(card).getByText(/WEATHER_API_KEY for weather-demo: keyring locked/),
  ).toBeVisible();
});

it("draws Disconnect as the tinted destructive trigger", () => {
  render(
    <ClientDetail
      client={client({ gatewayInstalled: true, entryState: "managed" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  const disconnect = screen.getByRole("button", { name: "Disconnect" });
  expect(disconnect).toHaveAttribute("data-variant", "destructive");
  expect(disconnect).not.toHaveAttribute("data-confirm");
  expect(disconnect).toHaveClass("bg-destructive/12", "text-destructive");
  expect(disconnect.className).not.toMatch(/dark:bg-destructive\/20(\s|$)/);
});

it("keeps a connect's restart reminder for the Clients list until it is dismissed", async () => {
  migrateClient.mockResolvedValue({
    registry: emptyRegistry(),
    tools: [],
    servers: [],
    outcome: { path: "/fixture/client.json", backup: null },
  });
  const store = renderHook(() => useClientNotices());
  const view = render(
    <ClientDetail
      client={client()}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("button", { name: /connect to toolport/i }));
  await userEvent.click(
    await within(await screen.findByRole("dialog")).findByRole("button", {
      name: /connect to toolport/i,
    }),
  );
  await userEvent.click(await screen.findByRole("button", { name: "Done" }));
  expect(await screen.findByText("Not live yet")).toBeVisible();
  expect(store.result.current.list).toMatchObject({
    clientId: "claude-desktop",
    title: "Not live yet",
  });
  // Leaving the page keeps it, for the list and for coming back.
  view.unmount();
  render(
    <ClientDetail
      client={client()}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  const card = screen.getByText("Not live yet").closest('[data-slot="status-card"]')!;
  await userEvent.click(
    within(card as HTMLElement).getByRole("button", { name: "Dismiss" }),
  );
  expect(screen.queryByText("Not live yet")).not.toBeInTheDocument();
  expect(store.result.current).toEqual({ detail: null, list: null });
});

it("shows a client's list reminder on its details after another client's feedback", () => {
  const reminder = {
    clientId: "claude-desktop",
    title: "Not live yet",
    text: "Restart Claude Desktop so it loads Toolport.",
  };
  setConnectNotice(reminder);
  // Codex's feedback replaces the details notice; the list still names Claude Desktop.
  setDetailNotice({ clientId: "codex", title: "refused", text: "" });
  render(
    <ClientDetail
      client={client()}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  expect(screen.getByText("Not live yet")).toBeVisible();
  expect(screen.queryByText("refused")).not.toBeInTheDocument();
});

it("drops discovery feedback that arrives after the details page closed", async () => {
  const reminder = {
    clientId: "cursor",
    title: "Not live yet",
    text: "Restart Cursor so it loads Toolport.",
  };
  setConnectNotice(reminder);
  let finish: (registry: Registry) => void = () => {};
  vi.mocked(setClientDiscovery).mockReturnValue(
    new Promise<Registry>((resolve) => {
      finish = resolve;
    }),
  );
  const store = renderHook(() => useClientNotices());
  const view = render(
    <ClientDetail
      client={client({ id: "codex", name: "Codex" })}
      registry={emptyRegistry()}
      onChanged={vi.fn()}
      onRegistryChange={vi.fn()}
    />,
  );
  await userEvent.click(screen.getByRole("combobox", { name: "Discovery mode" }));
  await userEvent.click(await screen.findByRole("option", { name: "Lazy · search" }));
  expect(setClientDiscovery).toHaveBeenCalledWith("codex", "lazy");
  view.unmount();
  await act(async () => finish(emptyRegistry()));
  expect(store.result.current).toEqual({ detail: reminder, list: reminder });
});
