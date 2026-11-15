import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DetectedClient, McpServer } from "@/lib/types";
import { ClientsView } from "./ClientsView";
import {
  getClientSessions,
  refreshClientRecoveryStatus,
  t3CodeRevision,
} from "@/lib/api";
vi.mock("@/lib/api", () => ({
  getClientSessions: vi.fn(),
  listClientRecoveries: vi.fn().mockResolvedValue([]),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
  discardClientRecovery: vi.fn(),
  refreshClientRecoveryStatus: vi.fn().mockResolvedValue(undefined),
  t3CodeRevision: vi.fn(),
}));
// Whether the window is on screen and focused, as `useWindowVisible` reports it.
const visibility = vi.hoisted(() => ({ value: true, listeners: new Set<() => void>() }));
vi.mock("@/lib/windowVisible", async () => {
  const { useSyncExternalStore } = await import("react");
  return {
    useWindowVisible: () =>
      useSyncExternalStore(
        (listener) => {
          visibility.listeners.add(listener);
          return () => visibility.listeners.delete(listener);
        },
        () => visibility.value,
      ),
  };
});
function setWindowVisible(value: boolean) {
  visibility.value = value;
  visibility.listeners.forEach((listener) => listener());
}
beforeEach(() => {
  visibility.value = true;
  vi.mocked(getClientSessions).mockResolvedValue([]);
  vi.mocked(t3CodeRevision).mockResolvedValue(null);
});
afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

it("waits for the saved-value check before completing an explicit refresh", async () => {
  let finish!: () => void;
  vi.mocked(refreshClientRecoveryStatus).mockClear();
  vi.mocked(refreshClientRecoveryStatus).mockImplementationOnce(
    () => new Promise<void>((resolve) => (finish = resolve)),
  );
  const changed = vi.fn().mockResolvedValue(undefined);
  render(
    <ClientsView
      clients={[]}
      registry={null}
      onSelectClient={vi.fn()}
      onChanged={changed}
    />,
  );
  const refresh = await screen.findByRole("button", { name: "Refresh" });
  await userEvent.click(refresh);
  await waitFor(() => expect(refreshClientRecoveryStatus).toHaveBeenCalledTimes(1));
  expect(refresh).toBeDisabled();
  expect(changed).not.toHaveBeenCalled();
  await act(async () => finish());
  await waitFor(() => expect(changed).toHaveBeenCalledTimes(1));
  expect(refresh).toBeEnabled();
});

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

  describe("T3 Code", () => {
    const on = (displayName: string, instanceId = displayName.toLowerCase()) => ({
      instanceId,
      displayName,
      enabled: true,
    });
    const off = (displayName: string) => ({ ...on(displayName), enabled: false });

    it("badges rows T3 Code uses, marking instances that are off", async () => {
      render(
        <ClientsView
          clients={[
            client({ id: "codex", name: "Codex", t3Code: [on("Work"), off("Home")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      const badge = screen.getByText("Used by T3 Code: Work, Home (off)");
      await userEvent.hover(badge);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(
        "Work is on in T3 Code Home is off in T3 Code",
      );
    });

    it("badges profile rows inside a group", () => {
      const profile = (id: string, folder: string, t3Code?: ReturnType<typeof on>[]) =>
        client({
          id,
          name: `Claude Code (${id})`,
          claudeProfile: {
            command: id,
            account: `${id}@example.com`,
            folder,
            lastUsedMs: null,
            sameAs: null,
          },
          t3Code,
        });
      render(
        <ClientsView
          clients={[
            profile("claude-code", "~/.claude.json", [on("Main")]),
            profile("claude-code@claude2", "~/.claude2"),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      const row = screen.getByText("Used by T3 Code: Main").closest("button");
      expect(row).toHaveAttribute("data-client-id", "claude-code");
      expect(within(row!).getByText("claude-code@example.com")).toBeVisible();
      expect(screen.getAllByText(/Used by T3 Code/)).toHaveLength(1);
    });

    it("adds nothing without T3 Code, and a null revision never rescans", async () => {
      vi.useFakeTimers();
      const onChanged = vi.fn(async () => {});
      render(
        <ClientsView
          clients={[client()]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(16_000));
      // One check when the page opens; no polling while T3 Code is absent.
      expect(t3CodeRevision).toHaveBeenCalledTimes(1);
      expect(onChanged).not.toHaveBeenCalled();
      // Focus checks again, once.
      act(() => setWindowVisible(false));
      act(() => setWindowVisible(true));
      await act(() => vi.advanceTimersByTimeAsync(16_000));
      expect(t3CodeRevision).toHaveBeenCalledTimes(2);
      expect(onChanged).not.toHaveBeenCalled();
      expect(screen.queryByText(/T3 Code/)).not.toBeInTheDocument();
    });

    it("rescans quietly when the T3 Code revision changes, null to a value on focus", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision)
        .mockResolvedValueOnce(null)
        .mockResolvedValueOnce("a")
        .mockResolvedValueOnce("a")
        .mockResolvedValue("b");
      const onChanged = vi.fn(async () => {});
      const { unmount } = render(
        <ClientsView
          clients={[client()]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(onChanged).not.toHaveBeenCalled();
      // No T3 Code yet: nothing polls. Installing it is noticed when the window
      // gets focus again, and from then on its settings are checked every 5 seconds.
      await act(() => vi.advanceTimersByTimeAsync(20_000));
      expect(t3CodeRevision).toHaveBeenCalledTimes(1);
      act(() => setWindowVisible(false));
      act(() => setWindowVisible(true));
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(onChanged).toHaveBeenCalledTimes(1);
      await act(() => vi.advanceTimersByTimeAsync(5_000));
      expect(onChanged).toHaveBeenCalledTimes(1);
      await act(() => vi.advanceTimersByTimeAsync(5_000));
      expect(onChanged).toHaveBeenCalledTimes(2);
      unmount();
      const calls = vi.mocked(t3CodeRevision).mock.calls.length;
      await act(() => vi.advanceTimersByTimeAsync(20_000));
      expect(t3CodeRevision).toHaveBeenCalledTimes(calls);
    });
  });
});
