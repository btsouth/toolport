import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DetectedClient, McpServer } from "@/lib/types";
import { ClientsView } from "./ClientsView";
import { getClientSessions } from "@/lib/api";
vi.mock("@/lib/api", () => ({
  getClientSessions: vi.fn(),
  listClientRecoveries: vi.fn().mockResolvedValue([]),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
}));
beforeEach(() => vi.mocked(getClientSessions).mockResolvedValue([]));

function client(overrides: Partial<DetectedClient> = {}): DetectedClient {
  return {
    id: "cursor",
    name: "Cursor",
    usesConnectors: false,
    configPath: "/tmp/cursor.json",
    configExists: true,
    appPresent: true,
    servers: [],
    pluginServers: [],
    gatewayInstalled: false,
    entryState: "absent",
    error: null,
    ...overrides,
  };
}

function server(name: string, command: string): McpServer {
  return { name, transport: "stdio", command, args: [], envKeys: [], url: null };
}

describe("ClientsView", () => {
  it("keeps observed identity ahead of the self-reported label without config actions", async () => {
    vi.mocked(getClientSessions).mockResolvedValueOnce([
      {
        sessionId: "opaque",
        clientName: "Unknown app (via Cursor)",
        clientLabel: "kt 1",
        clientType: "unknown",
        gatewayVersion: "2",
        phase: "close",
        reason: "client_disconnect",
        transport: "stdio",
        toolsListCount: 3,
        listChangedCount: 1,
        firstCatalogSize: 4,
        firstCatalogRevision: 1,
        catalogRevision: 2,
        contentChanged: true,
      },
    ]);
    render(<ClientsView clients={[]} registry={null} onSelectClient={vi.fn()} />);
    const toggle = await screen.findByRole("button", { name: /Recent client activity/ });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Unknown app (via Cursor)")).not.toBeInTheDocument();
    await userEvent.click(toggle);
    const heading = screen.getByText("Unknown app (via Cursor)");
    expect(heading).toHaveAttribute(
      "title",
      expect.stringContaining("Reports itself as: kt 1"),
    );
    expect(screen.getByText(/Last active time unavailable/)).toHaveTextContent(
      "Last saw 4 tools",
    );
    expect(screen.queryByText(/tool lists/)).not.toBeInTheDocument();
  });
  it("reports session history read failures", async () => {
    vi.mocked(getClientSessions).mockRejectedValueOnce(new Error("failed"));
    render(<ClientsView clients={[]} registry={null} onSelectClient={vi.fn()} />);
    expect(
      await screen.findByText("Client session history could not be read."),
    ).toBeInTheDocument();
  });
  it("groups connected clients before clients available to connect", () => {
    render(
      <ClientsView
        clients={[
          client({ id: "cursor", name: "Cursor" }),
          client({
            id: "claude-desktop",
            name: "Claude Desktop",
            gatewayInstalled: true,
            entryState: "managed",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.getByText("Connected to Toolport")).toBeInTheDocument();
    expect(screen.getByText("Available to connect")).toBeInTheDocument();
    const rows = screen.getAllByRole("button");
    expect(rows[0]).toHaveTextContent("Claude Desktop");
    expect(rows[0]).toHaveTextContent("Connected");
    expect(rows[1]).toHaveTextContent("Cursor");
    expect(rows[1]).toHaveTextContent("Ready to connect");
  });

  it("lists Claude Code profiles under one entry with copies behind a note", async () => {
    const onSelectClient = vi.fn();
    const profile = (
      id: string,
      claudeProfile: NonNullable<DetectedClient["claudeProfile"]>,
      overrides: Partial<DetectedClient> = {},
    ) =>
      client({
        id,
        name: `Claude Code (${id})`,
        configPath: `/home/me/${claudeProfile.folder.slice(2)}/.claude.json`,
        claudeProfile,
        ...overrides,
      });
    const renderGroup = () =>
      render(
        <ClientsView
          clients={[
            profile("claude-code@claude3", {
              command: "claude3",
              account: "third@example.com",
              folder: "~/.claude3",
              lastUsedMs: null,
              sameAs: null,
            }),
            profile(
              "claude-code",
              {
                command: "claude",
                account: "me@example.com",
                folder: "~/.claude.json",
                lastUsedMs: null,
                sameAs: null,
              },
              { gatewayInstalled: true, entryState: "managed" },
            ),
            profile("claude-code@claude-api", {
              command: null,
              account: null,
              folder: "~/.claude-api",
              lastUsedMs: Date.now(),
              sameAs: null,
            }),
            profile("claude-code@claude", {
              command: null,
              account: "me@example.com",
              folder: "~/.claude",
              lastUsedMs: null,
              sameAs: "claude-code",
            }),
            client({ id: "cursor", name: "Cursor" }),
          ]}
          registry={null}
          onSelectClient={onSelectClient}
        />,
      );
    const { unmount } = renderGroup();

    const group = screen.getByRole("button", { name: "Claude Code profiles" });
    expect(group).toHaveAttribute("aria-expanded", "true");
    expect(group).toHaveTextContent("3 profiles");
    expect(group).toHaveTextContent("1 of 3 connected");
    // The group sits with the connected clients; Available counts only Cursor.
    expect(screen.getByText("Available to connect").parentElement).toHaveTextContent("1");
    const rows = screen
      .getAllByRole("button")
      .filter((row) => /Claude Code|claude/.test(row.textContent ?? ""));
    expect(rows.map((row) => row.textContent)).toEqual([
      expect.stringContaining("Claude Code"),
      expect.stringMatching(
        /^claudeme@example\.com~\/\.claude\.json · Search, then call tools/,
      ),
      expect.stringMatching(/^claude3third@example\.com~\/\.claude3Ready to connect/),
      expect.stringMatching(/^~\/\.claude-apiNo claude\.ai sign-inLast active just now/),
    ]);
    expect(
      screen.getByText(/Also found ~\/\.claude: the same account as claude\./),
    ).toBeVisible();

    await userEvent.click(screen.getByRole("button", { name: "Show" }));
    await userEvent.click(
      screen.getByRole("button", { name: /^~\/\.claude\s*me@example/ }),
    );
    expect(onSelectClient).toHaveBeenCalledWith("claude-code@claude");
    await userEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(
      screen.queryByRole("button", { name: /^~\/\.claude\s*me@example/ }),
    ).toBeNull();

    await userEvent.click(group);
    expect(group).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("claude3")).toBeNull();
    // Opening a client and coming back keeps the group as it was left.
    unmount();
    renderGroup();
    expect(screen.getByRole("button", { name: "Claude Code profiles" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    await userEvent.click(screen.getByRole("button", { name: "Claude Code profiles" }));
  });

  it("shows one real profile as an ordinary row with its copy behind the note", () => {
    render(
      <ClientsView
        clients={[
          client({
            id: "claude-code",
            name: "Claude Code",
            claudeProfile: {
              command: "claude",
              account: "me@example.com",
              folder: "~/.claude.json",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
          client({
            id: "claude-code@claude",
            name: "Claude Code (claude)",
            claudeProfile: {
              command: null,
              account: "me@example.com",
              folder: "~/.claude",
              lastUsedMs: null,
              sameAs: "claude-code",
            },
          }),
          client({
            id: "claude-code@claude2",
            name: "Claude Code (claude2)",
            error: "invalid JSON",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: "Claude Code profiles" })).toBeNull();
    expect(
      screen.getByRole("button", { name: /^Claude Code\s*No local MCP/ }),
    ).toBeVisible();
    expect(
      screen.getByText(/Also found ~\/\.claude: the same account as claude\./),
    ).toBeVisible();
    expect(screen.getByText("Available to connect").parentElement).toHaveTextContent("2");
  });

  it("keeps an unconnected Claude Code group with the clients available to connect", () => {
    render(
      <ClientsView
        clients={[
          client({
            id: "claude-code",
            name: "Claude Code",
            claudeProfile: {
              command: "claude",
              account: null,
              folder: "~/.claude.json",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
          client({
            id: "claude-code@claude2",
            name: "Claude Code (claude2)",
            claudeProfile: {
              command: "claude2",
              account: null,
              folder: "~/.claude2",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.queryByText("Connected to Toolport")).toBeNull();
    expect(screen.getByText("Available to connect").parentElement).toHaveTextContent("2");
    expect(
      screen.getByRole("button", { name: "Claude Code profiles" }),
    ).toHaveTextContent("0 of 2 connected");
  });

  it("puts each section in one full-width row group, profile groups included", () => {
    const { container } = render(
      <ClientsView
        clients={[
          client({ id: "cursor", name: "Cursor" }),
          client({ id: "opencode", name: "OpenCode" }),
          client({
            id: "codex",
            name: "Codex",
            claudeProfile: {
              command: "codex",
              account: null,
              folder: "~/.codex",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
          client({
            id: "codex@.codex2",
            name: "Codex (.codex2)",
            claudeProfile: {
              command: null,
              account: null,
              folder: "~/.codex2",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );
    // Clients lines up with the other list pages: no centered column.
    expect(container.querySelector('[class*="max-w-[1080px]"]')).toBeNull();
    const groups = container.querySelectorAll(".toolport-list");
    expect(groups).toHaveLength(1);
    const group = groups[0];
    expect(group).toContainElement(
      screen.getByRole("button", { name: "Codex profiles" }),
    );
    expect(group).toContainElement(screen.getByRole("button", { name: /^Cursor/ }));
    expect(group).toContainElement(screen.getByRole("button", { name: /^OpenCode/ }));
    expect(container.querySelector(".rounded-lg.border")).toBeNull();
  });

  it("lists Codex homes in their own group, apart from Claude Code profiles", async () => {
    const codex = (
      id: string,
      command: string | null,
      folder: string,
      connected = false,
    ) =>
      client({
        id,
        name: id === "codex" ? "Codex" : `Codex (${folder})`,
        configPath: `/home/me/${folder.slice(2)}/config.toml`,
        gatewayInstalled: connected,
        entryState: connected ? "managed" : "absent",
        claudeProfile: { command, account: null, folder, lastUsedMs: null, sameAs: null },
      });
    const onSelectClient = vi.fn();
    render(
      <ClientsView
        clients={[
          codex("codex", "codex", "~/.codex", true),
          codex("codex@Codex-Work%2F.codex", "codex-work", "~/Codex-Work/.codex"),
          codex("codex@.codex2", null, "~/.codex2"),
          client({
            id: "claude-code",
            name: "Claude Code",
            claudeProfile: {
              command: "claude",
              account: "me@work.example",
              folder: "~/.claude.json",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
          client({
            id: "claude-code@claude2",
            name: "Claude Code (claude2)",
            claudeProfile: {
              command: "claude2",
              account: null,
              folder: "~/.claude2",
              lastUsedMs: null,
              sameAs: null,
            },
          }),
        ]}
        registry={null}
        onSelectClient={onSelectClient}
      />,
    );

    const group = screen.getByRole("button", { name: "Codex profiles" });
    expect(group).toHaveTextContent("3 profiles");
    expect(group).toHaveTextContent("1 of 3 connected");
    // Connected Codex sits with the connected clients; Claude Code waits to connect.
    expect(screen.getByText("Connected to Toolport").closest("section")).toContainElement(
      group,
    );
    expect(screen.getByText("Available to connect").closest("section")).toContainElement(
      screen.getByRole("button", { name: "Claude Code profiles" }),
    );
    // Codex has no account to show; Claude Code profiles still show theirs.
    expect(screen.queryAllByText("No claude.ai sign-in")).toHaveLength(1);
    expect(screen.getByText("me@work.example")).toBeInTheDocument();
    await userEvent.click(screen.getByText("codex-work"));
    expect(onSelectClient).toHaveBeenCalledWith("codex@Codex-Work%2F.codex");
    expect(screen.getByText("~/.codex2")).toBeInTheDocument();
  });

  it("describes each client by its servers and discovery, with the config path on hover", () => {
    render(
      <ClientsView
        clients={[
          client({
            servers: [server("github", "npx")],
          }),
          client({
            id: "codex",
            name: "Codex",
            gatewayInstalled: true,
            entryState: "managed",
            discovery: {
              autoMode: "lazy",
              autoFullMaxTools: 120,
              nativeToolSearch: true,
              toolsListChanged: null,
              evidence: "test",
            },
            servers: [server("toolport", "toolport-gateway"), server("github", "npx")],
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    const cursor = screen.getByText("1 local MCP server");
    expect(cursor).toHaveAttribute("title", expect.stringContaining("/tmp/cursor.json"));
    expect(
      screen.getByText("1 other server · Auto: full up to 120 tools, then lazy"),
    ).toBeInTheDocument();
  });

  it("does not describe a customized gateway entry as connected", () => {
    render(
      <ClientsView
        clients={[
          client({
            gatewayInstalled: true,
            entryState: "customized",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.getByText("Custom configuration")).toBeInTheDocument();
    expect(screen.getByText("Available to connect")).toBeInTheDocument();
    expect(
      screen.queryByText("Your installed clients are connected to Toolport."),
    ).not.toBeInTheDocument();
  });

  it("mentions config-read errors alongside connected clients", () => {
    render(
      <ClientsView
        clients={[
          client({
            id: "claude-desktop",
            name: "Claude Desktop",
            gatewayInstalled: true,
            entryState: "managed",
          }),
          client({
            id: "cursor",
            name: "Cursor",
            error: "invalid JSON",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.getByText("Connected to Toolport")).toBeInTheDocument();
    expect(screen.getByText("Available to connect")).toBeInTheDocument();
    expect(
      screen.queryByText("Your installed clients are connected to Toolport."),
    ).not.toBeInTheDocument();
  });

  it("opens the selected client", async () => {
    const onSelectClient = vi.fn();
    render(
      <ClientsView
        clients={[client()]}
        registry={null}
        onSelectClient={onSelectClient}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: /cursor/i }));
    expect(onSelectClient).toHaveBeenCalledWith("cursor");
  });

  it("keeps clients that are not installed behind a disclosure", async () => {
    render(
      <ClientsView
        clients={[
          client(),
          client({
            id: "zed",
            name: "Zed",
            appPresent: false,
            configExists: false,
            configPath: "",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    const toggle = screen.getByRole("button", { name: /not installed/i });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("button", { name: /zed/i })).not.toBeInTheDocument();

    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("button", { name: /zed/i })).toHaveTextContent(
      "Not installed",
    );
  });

  it("shows an honest empty state when detection returns nothing", () => {
    render(<ClientsView clients={[]} registry={null} onSelectClient={vi.fn()} />);

    expect(screen.getByText("No AI clients detected")).toBeInTheDocument();
    expect(screen.getByText(/install claude desktop/i)).toBeInTheDocument();
  });

  it("shows loading placeholders before client detection completes", () => {
    render(<ClientsView clients={[]} registry={null} loading onSelectClient={vi.fn()} />);

    expect(screen.getByLabelText("Loading AI clients")).toBeInTheDocument();
    expect(screen.queryByText("No AI clients detected")).not.toBeInTheDocument();
  });

  it("explains an inventory containing only clients that are not installed", () => {
    render(
      <ClientsView
        clients={[
          client({
            id: "zed",
            name: "Zed",
            appPresent: false,
            configExists: false,
            configPath: "",
          }),
        ]}
        registry={null}
        onSelectClient={vi.fn()}
      />,
    );

    expect(screen.getByText("No supported clients installed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /not installed/i })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });
});
