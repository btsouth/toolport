import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DetectedClient, McpServer, T3CodeOther } from "@/lib/types";
import { useRef, useState } from "react";
import { ClientsView, type T3CodeSeen } from "./ClientsView";
import {
  getClientSessions,
  listClientRecoveries,
  migrateClient,
  previewClientSetup,
  refreshClientRecoveryStatus,
  t3CodeOtherInstances,
  t3CodeRevision,
} from "@/lib/api";
import { toast } from "sonner";
vi.mock("@/lib/api", () => ({
  getClientSessions: vi.fn(),
  previewClientSetup: vi.fn(),
  migrateClient: vi.fn(),
  listClientRecoveries: vi.fn().mockResolvedValue([]),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
  discardClientRecovery: vi.fn(),
  refreshClientRecoveryStatus: vi.fn().mockResolvedValue(undefined),
  t3CodeOtherInstances: vi.fn(),
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
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }),
}));
// A summary that lists what was not connected stays until it is dismissed.
const lasting = expect.objectContaining({ duration: Infinity, closeButton: true });
beforeEach(() => {
  visibility.value = true;
  vi.mocked(getClientSessions).mockResolvedValue([]);
  vi.mocked(t3CodeRevision).mockResolvedValue(null);
  vi.mocked(t3CodeOtherInstances).mockResolvedValue([]);
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

    it("rescans on return from a client's details when T3 Code's settings changed meanwhile", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      // As in App: the scanned clients and the revision they match live above the page,
      // which unmounts while a client's details are open.
      function Harness() {
        const [detail, setDetail] = useState(false);
        const [rows, setRows] = useState([client({ t3Code: [on("Work")] })]);
        const seen = useRef<T3CodeSeen>(null);
        return (
          <>
            <button onClick={() => setDetail((value) => !value)}>toggle</button>
            {detail ? (
              <p>details</p>
            ) : (
              <ClientsView
                clients={rows}
                registry={null}
                onSelectClient={vi.fn()}
                t3Seen={seen}
                onChanged={async () => {
                  onChanged();
                  setRows([client({ t3Code: [on("Renamed")] })]);
                }}
              />
            )}
          </>
        );
      }
      const onChanged = vi.fn();
      render(<Harness />);
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(screen.getByText("Used by T3 Code: Work")).toBeInTheDocument();
      act(() => screen.getByText("toggle").click());
      vi.mocked(t3CodeRevision).mockResolvedValue("b");
      await act(() => vi.advanceTimersByTimeAsync(20_000));
      act(() => screen.getByText("toggle").click());
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(onChanged).toHaveBeenCalledTimes(1);
      expect(screen.getByText("Used by T3 Code: Renamed")).toBeInTheDocument();
      // Unchanged since: returning again does not rescan.
      act(() => screen.getByText("toggle").click());
      act(() => screen.getByText("toggle").click());
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(onChanged).toHaveBeenCalledTimes(1);
    });

    const card = () => screen.getByText("T3 Code").closest("[data-slot=status-card]");
    const other = (
      instanceId: string,
      providerName: string,
      displayName: string,
      gap: T3CodeOther["gap"],
      enabled = true,
    ): T3CodeOther => ({ instanceId, providerName, displayName, enabled, gap });

    it("lists instances Toolport cannot connect, with why, and reads them again on change", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValueOnce("a").mockResolvedValue("b");
      vi.mocked(t3CodeOtherInstances)
        .mockResolvedValueOnce([
          other("muse", "Muse Code", "Muse Code", "unsupported"),
          other("fx", "fx", "My fx", "unsupported", false),
          other("antigravity", "Antigravity", "Antigravity", "privateHome"),
        ])
        .mockResolvedValue([]);
      render(
        <ClientsView
          clients={[client()]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      // Only instances Toolport cannot connect: the card shows, without a status line.
      expect(screen.getByRole("group", { name: "T3 Code" })).toBeVisible();
      expect(card()).toHaveTextContent(
        "Not supported by Toolport yet: Muse Code, My fx (fx) (off).",
      );
      expect(card()).toHaveTextContent(
        "Toolport can't connect Antigravity yet: T3 Code gives it a private settings folder.",
      );
      expect(card()).not.toHaveTextContent(/not connected|is connected/);
      expect(within(card() as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
      await act(() => vi.advanceTimersByTimeAsync(5_000));
      expect(t3CodeOtherInstances).toHaveBeenCalledTimes(2);
      expect(screen.queryByText(/T3 Code/)).not.toBeInTheDocument();
    });

    it("tries a failed read of the instances again on the next check", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      vi.mocked(t3CodeOtherInstances)
        .mockRejectedValueOnce(new Error("ipc"))
        .mockResolvedValueOnce([other("muse", "Muse Code", "Muse Code", "unsupported")])
        .mockResolvedValue([]);
      render(
        <ClientsView
          clients={[client()]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(screen.queryByRole("group", { name: "T3 Code" })).not.toBeInTheDocument();
      // The revision has not changed, but the failed read is tried again.
      await act(() => vi.advanceTimersByTimeAsync(5_000));
      expect(card()).toHaveTextContent("Not supported by Toolport yet: Muse Code.");
      // Once a read has worked, an unchanged revision reads nothing more.
      const reads = vi.mocked(t3CodeOtherInstances).mock.calls.length;
      await act(() => vi.advanceTimersByTimeAsync(5_000));
      expect(t3CodeOtherInstances).toHaveBeenCalledTimes(reads);
    });

    it("reads the instances again after a scan, and not without T3 Code", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      vi.mocked(t3CodeOtherInstances)
        .mockResolvedValueOnce([other("w", "Claude", "Work", "missingHome")])
        .mockResolvedValue([]);
      const { rerender } = render(
        <ClientsView clients={[client()]} registry={null} onSelectClient={vi.fn()} />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(card()).toHaveTextContent(
        "Toolport can't connect Work (Claude) yet: its home folder doesn't exist yet. Start it once in T3 Code, then refresh Clients.",
      );
      // A scan (a new client list) reads them again: the folder may exist now.
      rerender(
        <ClientsView clients={[client()]} registry={null} onSelectClient={vi.fn()} />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(t3CodeOtherInstances).toHaveBeenCalledTimes(2);
      expect(screen.queryByText(/T3 Code/)).not.toBeInTheDocument();
    });

    it("says nothing is connected or not when every instance is off", () => {
      render(
        <ClientsView
          clients={[
            client({ t3Code: [off("Work")] }),
            client({ id: "codex", name: "Codex", t3Code: [off("Home")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(card()).toBeInTheDocument();
      expect(card()).not.toHaveTextContent(/is connected|not connected/);
    });

    it("lists the instances it can't use once T3 Code appears and the page rescans", async () => {
      vi.useFakeTimers();
      // T3 Code is absent at first, so there is no timer.
      vi.mocked(t3CodeRevision).mockResolvedValue(null);
      vi.mocked(t3CodeOtherInstances).mockResolvedValue([
        other("fx1", "fx", "Fx One", "unsupported"),
      ]);
      const onChanged = vi.fn(async () => {});
      const { rerender } = render(
        <ClientsView
          clients={[client()]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(20_000));
      expect(t3CodeOtherInstances).not.toHaveBeenCalled();
      // It is installed, then the user refreshes: the scan labels a row.
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      rerender(
        <ClientsView
          clients={[client({ t3Code: [on("Work")] })]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(card()).toHaveTextContent("Not supported by Toolport yet: Fx One (fx).");
      expect(card()).toHaveTextContent("2 enabled T3 Code instances are not connected.");
    });

    it("says T3 Code's settings can't be read once a revision exists", async () => {
      vi.useFakeTimers();
      // A broken settings link still has a revision, so the card reads the instances.
      vi.mocked(t3CodeRevision).mockResolvedValue("link:broken");
      vi.mocked(t3CodeOtherInstances).mockResolvedValue([
        other("", "", "~/.t3/userdata/settings.json", "unreadable"),
      ]);
      render(
        <ClientsView clients={[client()]} registry={null} onSelectClient={vi.fn()} />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(t3CodeOtherInstances).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("group", { name: "T3 Code" })).toBeVisible();
      expect(card()).toHaveTextContent(
        "Toolport can't read T3 Code's settings in ~/.t3/userdata/settings.json, so it can't list T3 Code's instances.",
      );
      // Not counted as an unconnected instance, and no status line without rows.
      expect(card()).not.toHaveTextContent(/not connected|is connected/);
    });

    it("says to add an OpenCode, Grok or Pi home no row covers by hand", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      vi.mocked(t3CodeOtherInstances).mockResolvedValue([
        other("notes", "OpenCode", "Notes", "unlistedOpenCodeHome"),
      ]);
      render(
        <ClientsView clients={[client()]} registry={null} onSelectClient={vi.fn()} />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      expect(card()).toHaveTextContent(
        "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once Notes (OpenCode)'s config folder exists, add the opencode folder inside it by hand as a client location to connect it.",
      );
    });

    it("counts enabled instances on rows that still need connecting", () => {
      const { rerender } = render(
        <ClientsView
          clients={[client({ t3Code: [on("Work")] })]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(card()).toHaveTextContent(
        "T3 Code runs these clients as providers. Each reads its own MCP config, so Toolport connects to them, not to T3 Code.",
      );
      expect(card()).toHaveTextContent("1 enabled T3 Code instance is not connected.");

      rerender(
        <ClientsView
          clients={[client({ t3Code: [on("Work"), on("Home"), off("Old")] })]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(card()).toHaveTextContent("2 enabled T3 Code instances are not connected.");

      rerender(
        <ClientsView
          clients={[
            client({
              gatewayInstalled: true,
              entryState: "managed",
              t3Code: [on("Work")],
            }),
            client({ id: "codex", name: "Codex", t3Code: [off("Home")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(card()).toHaveTextContent("Every enabled T3 Code instance is connected.");
    });

    it("leaves out connected, customized, unreadable and absent rows", () => {
      render(
        <ClientsView
          clients={[
            client({
              id: "a",
              name: "A",
              gatewayInstalled: true,
              entryState: "managed",
              t3Code: [on("A")],
            }),
            client({ id: "b", name: "B", entryState: "customized", t3Code: [on("B")] }),
            client({ id: "c", name: "C", error: "bad json", t3Code: [on("C")] }),
            client({ id: "d", name: "D", appPresent: false, t3Code: [on("D")] }),
            client({ id: "e", name: "E", t3Code: [off("E")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      // B, C and D are not connected, so the card does not say everything is; it names
      // each with why and what to do.
      expect(card()).not.toHaveTextContent(
        "Every enabled T3 Code instance is connected.",
      );
      expect(card()).toHaveTextContent("3 enabled T3 Code instances are not connected.");
      const lines = within(card() as HTMLElement)
        .getAllByText(/^(B|C|D) uses/)
        .map((line) => line.textContent);
      expect(lines).toEqual([
        "B uses B, whose Toolport entry was changed by hand. Open B below and reset its Toolport entry.",
        "C uses C, but Toolport can't read its config. Open C below to see why, then connect it.",
        "D uses D, which isn't installed. Install D, then connect it.",
      ]);
    });

    it("counts instances Toolport can't use and says why for each", async () => {
      vi.useFakeTimers();
      vi.mocked(t3CodeRevision).mockResolvedValue("a");
      vi.mocked(t3CodeOtherInstances).mockResolvedValue([
        other("pi_hidden", "Pi", "Pi Hidden", "hiddenHome"),
        other("text_entry", "", "text_entry", "malformed"),
        other("away", "Codex", "Away", "outsideHome"),
        other("linked", "Claude", "Linked", "linkedHome", false),
      ]);
      render(
        <ClientsView
          clients={[
            client({
              id: "a",
              name: "A",
              gatewayInstalled: true,
              entryState: "managed",
              t3Code: [on("A")],
            }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      await act(() => vi.advanceTimersByTimeAsync(0));
      // The off one is listed but not counted.
      expect(card()).toHaveTextContent("3 enabled T3 Code instances are not connected.");
      expect(card()).toHaveTextContent(
        "Toolport can't read text_entry in T3 Code's settings. Fix this instance in T3 Code's settings.",
      );
      expect(card()).toHaveTextContent(
        "Toolport can't see which home folder Pi Hidden (Pi) uses: it is marked sensitive or isn't plain text. Set its home folder as plain text in T3 Code's provider settings.",
      );
      expect(card()).toHaveTextContent(
        "Toolport won't use the home folder Away (Codex) uses: it isn't written as a plain folder inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it.",
      );
      expect(card()).toHaveTextContent(
        "Toolport won't use the home folder Linked (Claude) (off) uses: it, or its config file, is a link or not a plain folder or file.",
      );
    });

    function mockConnect() {
      vi.mocked(previewClientSetup).mockImplementation(async (id) => ({
        configPath: `/fixture/${id}`,
        backupDir: "/fixture/backups",
        revision: `rev-${id}`,
        items: [],
      }));
      vi.mocked(migrateClient).mockResolvedValue({
        registry: { servers: [] } as never,
        imported: 0,
        moved: [],
        outcome: { path: "/fixture/config", backup: null },
        tools: [],
        servers: [],
      } as never);
    }

    it("offers Connect all only while a row it can connect is not connected", () => {
      const { rerender } = render(
        <ClientsView
          clients={[client({ t3Code: [on("Work")] })]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(screen.getByRole("button", { name: "Connect all" })).toBeVisible();
      rerender(
        <ClientsView
          clients={[
            client({ gatewayInstalled: true, entryState: "managed", t3Code: [on("A")] }),
            client({ id: "b", name: "B", entryState: "customized", t3Code: [on("B")] }),
            client({ id: "c", name: "C", error: "bad json", t3Code: [on("C")] }),
            client({ id: "d", name: "D", appPresent: false, t3Code: [on("D")] }),
            client({ id: "e", name: "E", t3Code: [off("E")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
        />,
      );
      expect(
        screen.queryByRole("button", { name: "Connect all" }),
      ).not.toBeInTheDocument();
    });

    it("connects each row in turn with a step line and reports the total", async () => {
      mockConnect();
      const onChanged = vi.fn(async () => {});
      render(
        <ClientsView
          clients={[
            client({ id: "zed", name: "Zed", t3Code: [on("Two")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("One"), on("Home")] }),
            client({ id: "skip", name: "Aider", t3Code: [off("Off")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Review and connect Codex")).toBeVisible();
      expect(screen.getByText("Step 1 of 2: One, Home")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      expect(migrateClient).toHaveBeenLastCalledWith(
        "codex",
        undefined,
        undefined,
        [],
        "rev-codex",
      );
      await userEvent.click(await screen.findByRole("button", { name: "Next" }));
      expect(await screen.findByText("Review and connect Zed")).toBeVisible();
      expect(screen.getByText("Step 2 of 2: Two")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      await userEvent.click(await screen.findByRole("button", { name: "Done" }));
      expect(previewClientSetup).not.toHaveBeenCalledWith("skip");
      expect(toast.success).toHaveBeenCalledWith("Connected 3 of 3 T3 Code instances.");
      expect(onChanged).toHaveBeenCalled();
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    it("names the rows it left out in a summary that stays", async () => {
      mockConnect();
      render(
        <ClientsView
          clients={[
            client({ id: "codex", name: "Codex", t3Code: [on("One")] }),
            client({
              id: "kiro",
              name: "Kiro",
              appPresent: false,
              t3Code: [on("Work"), on("Home")],
            }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Step 1 of 1: One")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      await userEvent.click(await screen.findByRole("button", { name: "Done" }));
      expect(toast.success).not.toHaveBeenCalled();
      expect(toast).toHaveBeenCalledWith(
        "Connected 1 of 1 T3 Code instance. Work, Home use Kiro, which isn't installed. Install Kiro, then connect it.",
        lasting,
      );
    });

    it("stops on Escape and names what it did not reach", async () => {
      mockConnect();
      const onChanged = vi.fn(async () => {});
      render(
        <ClientsView
          clients={[
            client({ id: "codex", name: "Codex", t3Code: [on("One")] }),
            client({ id: "zed", name: "Zed", t3Code: [on("Two")] }),
            client({ id: "aider", name: "Aider", t3Code: [on("Three")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={onChanged}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Review and connect Aider")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      await userEvent.click(await screen.findByRole("button", { name: "Next" }));
      expect(await screen.findByText("Review and connect Codex")).toBeVisible();
      await userEvent.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(previewClientSetup).not.toHaveBeenCalledWith("zed");
      expect(migrateClient).toHaveBeenCalledTimes(1);
      expect(toast).toHaveBeenCalledWith(
        "Connect all stopped. Connected 1 of 3 T3 Code instances. Not connected: One, Two.",
        lasting,
      );
      expect(onChanged).toHaveBeenCalled();
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Connect all" })).toHaveFocus(),
      );
    });

    it("stops on Escape from a step's success screen", async () => {
      mockConnect();
      render(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      await userEvent.click(
        await screen.findByRole("button", { name: "Connect to Toolport" }),
      );
      expect(await screen.findByRole("button", { name: "Next" })).toBeVisible();
      await userEvent.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(previewClientSetup).not.toHaveBeenCalledWith("codex");
      expect(toast).toHaveBeenCalledWith(
        "Connect all stopped. Connected 1 of 2 T3 Code instances. Not connected: Two.",
        lasting,
      );
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Connect all" })).toHaveFocus(),
      );
    });

    it("offers Skip and Retry after a failure and names what was not connected", async () => {
      mockConnect();
      vi.mocked(migrateClient)
        .mockRejectedValueOnce("Permission denied")
        .mockRejectedValueOnce("Permission denied");
      render(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two"), on("Three")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Review and connect Aider")).toBeVisible();
      expect(screen.queryByRole("button", { name: "Skip" })).not.toBeInTheDocument();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      expect(await screen.findByText("Permission denied")).toBeVisible();
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Retry" })).toHaveFocus(),
      );
      expect(screen.getByRole("button", { name: "Cancel" })).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Retry" }));
      expect(await screen.findByText("Permission denied")).toBeVisible();
      expect(migrateClient).toHaveBeenCalledTimes(2);
      await userEvent.click(screen.getByRole("button", { name: "Skip" }));
      expect(await screen.findByText("Review and connect Codex")).toBeVisible();
      expect(screen.getByText("Step 2 of 2: Two, Three")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      await userEvent.click(await screen.findByRole("button", { name: "Done" }));
      expect(toast).toHaveBeenCalledWith(
        "Connected 2 of 3 T3 Code instances. Not connected: One.",
        lasting,
      );
      expect(toast.success).not.toHaveBeenCalled();
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Connect all" })).toHaveFocus(),
      );
    });

    it("finishes when the last step is skipped after its config could not be read", async () => {
      mockConnect();
      vi.mocked(previewClientSetup)
        .mockImplementationOnce(async () => ({
          configPath: "/fixture/aider",
          backupDir: "/fixture/backups",
          revision: "rev-aider",
          items: [],
        }))
        .mockRejectedValueOnce("unreadable");
      render(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      await userEvent.click(
        await screen.findByRole("button", { name: "Connect to Toolport" }),
      );
      await userEvent.click(await screen.findByRole("button", { name: "Next" }));
      expect(await screen.findByText("unreadable")).toBeVisible();
      await userEvent.click(screen.getByRole("button", { name: "Skip" }));
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(toast).toHaveBeenCalledWith(
        "Connected 1 of 2 T3 Code instances. Not connected: Two.",
        lasting,
      );
    });

    it("moves focus to the T3 Code card when Connect all goes away after the batch", async () => {
      mockConnect();
      const props = {
        registry: null,
        onSelectClient: vi.fn(),
        onChanged: vi.fn(async () => {}),
      };
      const { rerender } = render(
        <ClientsView
          clients={[client({ id: "aider", name: "Aider", t3Code: [on("One")] })]}
          {...props}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      await userEvent.click(
        await screen.findByRole("button", { name: "Connect to Toolport" }),
      );
      await userEvent.click(await screen.findByRole("button", { name: "Done" }));
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Connect all" })).toHaveFocus(),
      );
      // The rescan after the batch finds Aider connected.
      rerender(
        <ClientsView
          clients={[
            client({
              id: "aider",
              name: "Aider",
              gatewayInstalled: true,
              entryState: "managed",
              t3Code: [on("One")],
            }),
          ]}
          {...props}
        />,
      );
      expect(
        screen.queryByRole("button", { name: "Connect all" }),
      ).not.toBeInTheDocument();
      expect(screen.getByRole("group", { name: "T3 Code" })).toHaveFocus();
    });

    it("works out the summary from the settings as they are when the batch ends", async () => {
      mockConnect();
      const props = {
        registry: null,
        onSelectClient: vi.fn(),
        onChanged: vi.fn(async () => {}),
      };
      const { rerender } = render(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two"), on("Three")] }),
            client({ id: "zed", name: "Zed", t3Code: [on("Four")] }),
          ]}
          {...props}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Review and connect Aider")).toBeVisible();
      // T3 Code's settings change while the dialog is open: Three is turned off and
      // Zed's instance is removed.
      rerender(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two"), off("Three")] }),
            client({ id: "zed", name: "Zed" }),
          ]}
          {...props}
        />,
      );
      await userEvent.keyboard("{Escape}");
      expect(toast).toHaveBeenCalledWith(
        "Connect all stopped. Connected 0 of 2 T3 Code instances. Not connected: One, Two.",
        lasting,
      );
    });

    it("works out the summary from the rescan after the batch, as GTK does", async () => {
      mockConnect();
      const before = [
        client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
        client({ id: "codex", name: "Codex", t3Code: [on("Two"), on("Three")] }),
      ];
      // The rescan after the batch finds Aider connected and Three turned off.
      const after = [
        client({
          id: "aider",
          name: "Aider",
          gatewayInstalled: true,
          entryState: "managed",
          t3Code: [on("One")],
        }),
        client({ id: "codex", name: "Codex", t3Code: [on("Two"), off("Three")] }),
      ];
      function Page() {
        const [clients, setClients] = useState(before);
        return (
          <ClientsView
            clients={clients}
            registry={null}
            onSelectClient={vi.fn()}
            onChanged={async () => {
              await Promise.resolve();
              setClients(after);
            }}
          />
        );
      }
      render(<Page />);
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      await userEvent.click(
        await screen.findByRole("button", { name: "Connect to Toolport" }),
      );
      await userEvent.click(await screen.findByRole("button", { name: "Next" }));
      expect(await screen.findByText("Review and connect Codex")).toBeVisible();
      await userEvent.keyboard("{Escape}");
      await waitFor(() =>
        expect(toast).toHaveBeenCalledWith(
          "Connect all stopped. Connected 1 of 2 T3 Code instances. Not connected: Two.",
          lasting,
        ),
      );
      expect(toast).toHaveBeenCalledTimes(1);
    });

    it("names each step's instances as they were when Connect all started, as GTK does", async () => {
      mockConnect();
      const props = {
        registry: null,
        onSelectClient: vi.fn(),
        onChanged: vi.fn(async () => {}),
      };
      const { rerender } = render(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({ id: "codex", name: "Codex", t3Code: [on("Two"), on("Three")] }),
          ]}
          {...props}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      expect(await screen.findByText("Step 1 of 2: One")).toBeVisible();
      // T3 Code's settings change during the batch: Three is turned off, Four added.
      rerender(
        <ClientsView
          clients={[
            client({ id: "aider", name: "Aider", t3Code: [on("One")] }),
            client({
              id: "codex",
              name: "Codex",
              t3Code: [on("Two"), off("Three"), on("Four")],
            }),
          ]}
          {...props}
        />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
      await userEvent.click(await screen.findByRole("button", { name: "Next" }));
      expect(await screen.findByText("Step 2 of 2: Two, Three")).toBeVisible();
    });

    it("orders the steps by lowercase name then id, as GTK does", async () => {
      mockConnect();
      render(
        <ClientsView
          clients={[
            client({ id: "zed", name: "Zed", t3Code: [on("Z")] }),
            client({ id: "ecran", name: "Écran", t3Code: [on("E")] }),
            client({ id: "codex@b", name: "codex", t3Code: [on("B")] }),
            client({ id: "codex@a", name: "Codex", t3Code: [on("A")] }),
          ]}
          registry={null}
          onSelectClient={vi.fn()}
          onChanged={vi.fn(async () => {})}
        />,
      );
      const steps: string[] = [];
      await userEvent.click(screen.getByRole("button", { name: "Connect all" }));
      for (let index = 1; index <= 4; index++) {
        steps.push(
          (await screen.findByText(new RegExp(`^Step ${index} of 4: `))).textContent!,
        );
        await userEvent.click(
          screen.getByRole("button", { name: "Connect to Toolport" }),
        );
        await userEvent.click(
          await screen.findByRole("button", { name: index < 4 ? "Next" : "Done" }),
        );
      }
      expect(steps).toEqual([
        "Step 1 of 4: A",
        "Step 2 of 4: B",
        "Step 3 of 4: Z",
        "Step 4 of 4: E",
      ]);
    });
  });
});

it("keeps a T3 Code home row's label beside its recovery cards", async () => {
  const id = "codex@t3-0123456789abcdef";
  const name = "Codex (~/.t3/cx/codex)";
  const base = {
    clientId: id,
    clientName: name,
    path: "/fixture/.t3/cx/codex/config.toml",
    backupPath: "/fixture/backups/codex/original.toml",
    dismissed: false,
    discardValues: [],
    discardStarted: false,
    discardAction: "Discard these saved values",
    cleanup: false,
    instructions: "Unlock your keyring, then choose Retry.",
    retention: "The recovery backup does not expire.",
    details: "fixture",
  };
  vi.mocked(listClientRecoveries).mockResolvedValueOnce([
    {
      ...base,
      id: "owed",
      missing: [{ server: "server-a", key: "API_KEY" }],
      message: `${name} is disconnected.\n\nThe config has placeholders for these values for now.`,
      description: "Its original config is back. These values still have placeholders:",
      valueLines: ["API_KEY for server-a: keyring locked"],
      error: null,
      canDiscard: true,
      canRetry: true,
    },
    {
      ...base,
      id: "receipt",
      missing: [],
      message: `${name} recovery needs attention.\n\nToolport could not read this recovery file.`,
      description: "Toolport could not read this recovery file. Check its permissions.",
      valueLines: [],
      error: "Toolport could not read this recovery file. Check its permissions.",
      canDiscard: false,
      canRetry: true,
    },
  ] as Awaited<ReturnType<typeof listClientRecoveries>>);
  render(
    <ClientsView
      clients={[
        client({
          id,
          name,
          t3Code: [{ instanceId: "cx", displayName: "Codex Extra", enabled: false }],
        }),
      ]}
      registry={null}
      onSelectClient={vi.fn()}
      onChanged={vi.fn(async () => {})}
    />,
  );
  expect(await screen.findAllByRole("region")).toHaveLength(2);
  expect(screen.getByText(`${name} is disconnected.`)).toBeInTheDocument();
  expect(screen.getByText(`${name} recovery needs attention.`)).toBeInTheDocument();
  expect(screen.getByText("Used by T3 Code: Codex Extra (off)")).toBeInTheDocument();
});
