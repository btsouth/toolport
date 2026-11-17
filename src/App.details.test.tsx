import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { DetectedClient, Registry } from "@/lib/types";

const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();
const uninstallGateway = vi.fn();
const setServerEnabled = vi.fn();
const removeServer = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api")>()),
  teamPairState: vi.fn(() => Promise.resolve(null)),
  teamPairCancel: vi.fn(),
  addServer: vi.fn(),
  detectClients: (...a: unknown[]) => detectClients(...a),
  getRegistry: (...a: unknown[]) => getRegistry(...a),
  importServers: vi.fn(),
  mainWindowVisible: vi.fn(() => Promise.resolve(true)),
  parseServerSnippet: vi.fn(),
  previewImportServers: vi.fn(),
  probeServers: vi.fn(() => Promise.resolve([])),
  uninstallGateway: (...a: unknown[]) => uninstallGateway(...a),
  recentClientActivity: vi.fn(() => Promise.resolve([])),
  listClientRecoveries: vi.fn(() => Promise.resolve([])),
  removeServer: (...a: unknown[]) => removeServer(...a),
  setAllEnabled: vi.fn(),
  setSecret: vi.fn(),
  setServerEnabled: (...a: unknown[]) => setServerEnabled(...a),
  takeRegistryRecoveryNotice: (...a: unknown[]) => takeRegistryRecoveryNotice(...a),
  teamSyncWait: vi.fn(),
  testServer: vi.fn(),
  updateServer: vi.fn(),
  popularCatalog: vi.fn(() => Promise.resolve([])),
  searchCatalog: vi.fn(() => Promise.resolve([])),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/lib/trayApprovals", () => ({
  subscribeToTrayApprovals: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/lib/theme", () => ({
  useTheme: () => ({ resolved: "light" }),
}));

vi.mock("@/components/AppSidebar", () => ({
  AppSidebar: ({
    onSelectView,
    onReplayOnboarding,
  }: {
    onSelectView: (view: "clients" | "servers") => void;
    onReplayOnboarding: () => void;
  }) => (
    <nav>
      <button onClick={() => onSelectView("clients")}>Clients</button>
      <button onClick={() => onSelectView("servers")}>Servers</button>
      <button onClick={onReplayOnboarding}>Replay onboarding</button>
    </nav>
  ),
}));
vi.mock("@/components/Onboarding", () => ({
  Onboarding: ({ onOpenTools }: { onOpenTools: () => void }) => (
    <button onClick={onOpenTools}>Open onboarding tools</button>
  ),
}));
vi.mock("@/components/PendingApprovals", () => ({ PendingApprovals: () => null }));
vi.mock("@/components/QuarantineAlert", () => ({ QuarantineAlert: () => null }));

/** A registry that already has a server, so the Servers view renders its header
 * actions instead of the empty state (which has its own catalog link). */
function registryWithServer(): Registry {
  return {
    version: 1,
    servers: [
      {
        id: "files",
        name: "Files",
        transport: "stdio",
        command: "npx",
        args: ["-y", "mcp-files"],
        env: [],
        url: null,
        source: null,
      },
    ],
    profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
    activeProfileId: "default",
  } as Registry;
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  getRegistry.mockResolvedValue(registryWithServer());
  detectClients.mockResolvedValue([]);
  takeRegistryRecoveryNotice.mockResolvedValue(null);
  uninstallGateway.mockResolvedValue({ restored: [], warnings: [] });
});

const profileClient = (id: string, connected = true): DetectedClient => ({
  id,
  name: "Claude Code",
  appPresent: true,
  configExists: true,
  configPath: `/fixture/${id}/config.json`,
  gatewayInstalled: connected,
  entryState: connected ? "managed" : "absent",
  error: null,
  usesConnectors: false,
  pluginServers: [],
  servers: [],
});

describe("pushed detail navigation", () => {
  it("opens a server page, keeps sheets transient, and returns keyboard focus to its row", async () => {
    const user = userEvent.setup();
    render(<App />);
    const open = await screen.findByRole("button", { name: "Show Files details" });
    const listViewport = document.querySelector<HTMLElement>(
      'main [data-slot="scroll-area-viewport"]',
    )!;
    listViewport.scrollTop = 400;
    open.focus();
    await user.keyboard("{Enter}");
    expect(
      await screen.findByRole("heading", { level: 1, name: "Files" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Browse catalog/ }),
    ).not.toBeInTheDocument();
    expect(
      document.querySelector<HTMLElement>('main [data-slot="scroll-area-viewport"]')
        ?.scrollTop,
    ).toBe(0);
    expect(screen.getByRole("tab", { name: "Overview" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "Files" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Back to servers" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Show Files details" })).toHaveFocus(),
    );
  });

  it("restores the live detail switch after reviewed enable failure, cancel and success", async () => {
    const user = userEvent.setup();
    const registry = registryWithServer();
    registry.servers[0].source = "team:work";
    getRegistry.mockResolvedValue(registry);
    const enabled = structuredClone(registry);
    enabled.profiles[0].enabledServerIds = ["files"];
    setServerEnabled
      .mockRejectedValueOnce(new Error("Definition needs review"))
      .mockResolvedValueOnce(enabled);
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Show Files details" }));
    const toggle = screen.getByRole("switch", { name: /^(Enable|Disable) Files$/ });
    toggle.focus();
    await user.keyboard(" ");
    let dialog = within(await screen.findByRole("dialog"));
    await waitFor(() =>
      expect(dialog.getByRole("button", { name: "Enable" })).toBeEnabled(),
    );
    await user.click(dialog.getByRole("button", { name: "Enable" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent("Definition needs review");
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await user.click(dialog.getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(
        screen.getByRole("switch", { name: /^(Enable|Disable) Files$/ }),
      ).toHaveFocus(),
    );
    await user.keyboard(" ");
    dialog = within(await screen.findByRole("dialog"));
    await waitFor(() =>
      expect(dialog.getByRole("button", { name: "Enable" })).toBeEnabled(),
    );
    await user.click(dialog.getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const live = screen.getByRole("switch", { name: /^(Enable|Disable) Files$/ });
    expect(live).toHaveAttribute("aria-checked", "true");
    await waitFor(() => expect(live).toHaveFocus());
    expect(live).toBeInTheDocument();
  });

  it("refreshes a selected server and gives a safe missing state after removal", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Show Files details" }));
    const next = registryWithServer();
    next.servers[0].args = ["new-argument"];
    getRegistry.mockResolvedValue(next);
    await user.click(
      screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
    );
    expect(await screen.findByText("npx new-argument")).toBeInTheDocument();
    getRegistry.mockResolvedValue({ ...next, servers: [] });
    await user.click(
      screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
    );
    expect(await screen.findByText("Server is no longer available")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByText("Couldn't reach the backend")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Back to servers" })).toHaveLength(2);
  });

  it("targets the selected client instance and refreshes detail after disconnect", async () => {
    const user = userEvent.setup();
    const first = profileClient("claude-code@personal");
    const second = profileClient("claude-code@work");
    detectClients.mockResolvedValue([first, second]);
    render(<App />);
    await user.click(screen.getByRole("button", { name: "Clients" }));
    await waitFor(() =>
      expect(
        document.querySelector(`[data-client-id="${second.id}"]`),
      ).toBeInTheDocument(),
    );
    await user.click(
      document.querySelector<HTMLButtonElement>(`[data-client-id="${second.id}"]`)!,
    );
    expect(await screen.findByText(second.configPath)).toBeInTheDocument();
    detectClients.mockResolvedValue([first, profileClient(second.id, false)]);
    await user.click(screen.getByRole("button", { name: "Disconnect" }));
    await user.click(screen.getByRole("button", { name: "Disconnect" }));
    await waitFor(() => expect(uninstallGateway).toHaveBeenCalledWith(second.id));
    expect(
      await screen.findByRole("button", { name: "Connect to Toolport" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Still connected until restart")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Back to clients" }));
    await waitFor(() =>
      expect(document.querySelector(`[data-client-id="${second.id}"]`)).toHaveFocus(),
    );
  });

  it("does not leave stale client actions after a missing instance or scan error", async () => {
    const user = userEvent.setup();
    const selected = profileClient("codex@work");
    detectClients.mockResolvedValue([selected]);
    render(<App />);
    await user.click(screen.getByRole("button", { name: "Clients" }));
    await waitFor(() =>
      expect(
        document.querySelector(`[data-client-id="${selected.id}"]`),
      ).toBeInTheDocument(),
    );
    await user.click(
      document.querySelector<HTMLButtonElement>(`[data-client-id="${selected.id}"]`)!,
    );
    detectClients.mockRejectedValue(new Error("scan failed"));
    await user.click(
      screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
    );
    expect(
      await screen.findByText("Error: scan failed", { exact: true }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Disconnect" })).not.toBeInTheDocument();
    detectClients.mockResolvedValue([]);
    await act(async () => {
      await user.click(
        screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
      );
    });
    expect(await screen.findByText("Client is no longer available")).toBeInTheDocument();
    expect(screen.queryByText("Couldn't reach the backend")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Servers" }));
    expect(
      await screen.findByRole("heading", { level: 1, name: "Servers" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Back to clients" }),
    ).not.toBeInTheDocument();
  });
  it.each(["servers", "clients"] as const)(
    "opens %s detail after a list refresh error without reusing hidden error",
    async (view) => {
      const user = userEvent.setup();
      const selected = profileClient("codex@work");
      detectClients.mockResolvedValue([selected]);
      render(<App />);
      if (view === "clients")
        await user.click(screen.getByRole("button", { name: "Clients" }));
      const selector =
        view === "servers"
          ? '[data-detail-open="files"]'
          : `[data-client-id="${selected.id}"]`;
      await waitFor(() => expect(document.querySelector(selector)).toBeInTheDocument());
      (view === "servers" ? getRegistry : detectClients).mockRejectedValueOnce(
        new Error("list refresh failed"),
      );
      await user.click(
        screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
      );
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
        ).toBeEnabled(),
      );
      await user.click(document.querySelector<HTMLButtonElement>(selector)!);
      expect(
        await screen.findByRole("button", {
          name: view === "servers" ? "Edit" : "Disconnect",
        }),
      ).toBeInTheDocument();
      expect(screen.queryByText("Couldn't reach the backend")).not.toBeInTheDocument();
      // A failure after opening detail must still hide stale mutation controls.
      (view === "servers" ? getRegistry : detectClients).mockRejectedValueOnce(
        new Error("detail refresh failed"),
      );
      await user.click(
        screen.getByRole("button", { name: /^(Refresh|Check servers again)$/ }),
      );
      expect(
        await screen.findByText("Error: detail refresh failed", { exact: true }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("button", {
          name: view === "servers" ? "Edit" : "Disconnect",
        }),
      ).not.toBeInTheDocument();
    },
  );

  it("keeps a failed removal in detail and returns to the list with focus only after success", async () => {
    const user = userEvent.setup();
    const empty = { ...registryWithServer(), servers: [] };
    removeServer
      .mockRejectedValueOnce(new Error("remove denied"))
      .mockResolvedValueOnce(empty);
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Show Files details" }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await user.click(
      within(screen.getByRole("dialog")).getByRole("button", { name: "Remove" }),
    );
    await waitFor(() => expect(removeServer).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("heading", { level: 1, name: "Files" })).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await user.click(
      within(screen.getByRole("dialog")).getByRole("button", { name: "Remove" }),
    );
    expect(
      await screen.findByRole("heading", { level: 1, name: "Servers" }),
    ).toBeInTheDocument();
    expect(removeServer).toHaveBeenLastCalledWith("files");
    expect(screen.queryByText("Server is no longer available")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(document.querySelector("main")).toContainElement(
        document.activeElement as HTMLElement,
      ),
    );
    expect(document.activeElement).not.toBe(document.body);
  });

  it.each([true, false])(
    "onboarding tools excludes a first enabled gateway, valid server present=%s",
    async (hasServer) => {
      const user = userEvent.setup();
      const registry = registryWithServer();
      const gateway = {
        ...registry.servers[0],
        id: "gateway",
        name: "Toolport",
        command: "toolport-gateway",
      };
      registry.servers = hasServer ? [gateway, ...registry.servers] : [gateway];
      registry.profiles[0].enabledServerIds = ["gateway", "files"];
      getRegistry.mockResolvedValue(registry);
      render(<App />);
      await screen.findByRole("heading", { level: 1, name: "Servers" });
      await user.click(screen.getByRole("button", { name: "Replay onboarding" }));
      await user.click(
        await screen.findByRole("button", { name: "Open onboarding tools" }),
      );
      expect(
        screen.queryByRole("heading", { level: 1, name: "Toolport" }),
      ).not.toBeInTheDocument();
      if (hasServer) {
        expect(
          await screen.findByRole("heading", { level: 1, name: "Files" }),
        ).toBeInTheDocument();
        expect(screen.getByRole("tab", { name: "Tools" })).toHaveAttribute(
          "aria-selected",
          "true",
        );
      } else {
        expect(
          screen.getByRole("heading", { level: 1, name: "Servers" }),
        ).toBeInTheDocument();
        expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
        expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();
      }
    },
  );
});
