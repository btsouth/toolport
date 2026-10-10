import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { Registry } from "@/lib/types";

const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();
const setServerEnabled = vi.fn();
const { eventHandlers } = vi.hoisted(() => ({
  eventHandlers: new Map<string, (event: { payload: Registry }) => void>(),
}));

vi.mock("@/lib/api", () => ({
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
  removeServer: vi.fn(),
  setAllEnabled: vi.fn(),
  setSecret: vi.fn(),
  setServerEnabled: (...a: unknown[]) => setServerEnabled(...a),
  takeRegistryRecoveryNotice: (...a: unknown[]) => takeRegistryRecoveryNotice(...a),
  testServer: vi.fn(),
  updateServer: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: (event: { payload: Registry }) => void) => {
    eventHandlers.set(name, handler);
    return Promise.resolve(() => eventHandlers.delete(name));
  }),
}));

vi.mock("@/lib/trayApprovals", () => ({
  subscribeToTrayApprovals: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/lib/theme", () => ({
  useTheme: () => ({ resolved: "light" }),
}));

vi.mock("@/components/AppSidebar", () => ({ AppSidebar: () => null }));
vi.mock("@/components/PendingApprovals", () => ({ PendingApprovals: () => null }));
vi.mock("@/components/QuarantineAlert", () => ({ QuarantineAlert: () => null }));

function registryWith(args: string[]): Registry {
  return {
    version: 1,
    servers: [
      {
        id: "team-tool",
        name: "Team tool",
        transport: "stdio",
        command: "npx",
        args,
        env: [],
        url: null,
        source: "team:team-1",
      },
    ],
    profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
    activeProfileId: "default",
    team: {
      serverUrl: "https://teams.toolport.app",
      teamId: "team-1",
      role: "member",
      lastVersion: 1,
    },
  } as Registry;
}

beforeEach(() => {
  vi.clearAllMocks();
  eventHandlers.clear();
  localStorage.clear();
  getRegistry.mockResolvedValue(registryWith(["-y", "old-tool"]));
  detectClients.mockResolvedValue([]);
  takeRegistryRecoveryNotice.mockResolvedValue(null);
});

describe("team enable review dialog", () => {
  it("shows all personal execution inputs as escaped text with masked secrets", async () => {
    const registry = registryWith(["-y", "old-tool"]);
    registry.team!.role = "admin";
    registry.team!.accountStatus = {
      personalSync: true,
      plan: "pro",
      trialActive: false,
      trialEndsAt: null,
      freeSyncGraceEndsAt: null,
      deviceId: "device",
      canReceiveConfig: true,
      reason: null,
    };
    const server = registry.servers[0];
    server.command = "npx\u{202e}";
    server.cwd = "/work\n\u{200b}";
    server.inheritEnv = false;
    server.env = [
      { key: "REGION", secret: false, value: "west" },
      { key: "TOKEN", secret: true, value: "hidden-env-secret" },
    ];
    server.launch = {
      inputs: [
        {
          key: "project",
          label: "Project",
          required: false,
          secret: false,
          value: "<img src=x onerror=evil()>",
        },
        {
          key: "auth",
          label: "Auth",
          required: false,
          secret: true,
          value: "hidden-input-secret",
        },
      ],
      bindings: [{ index: 1, parts: [{ kind: "input", key: "project" }] }],
    };
    getRegistry.mockResolvedValue(registry);
    render(<App />);
    await userEvent.click(
      await screen.findByRole("switch", { name: "Toggle Team tool" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    expect(await dialog.findByText("Command: npx\\u{202E}")).toBeInTheDocument();
    expect(
      dialog.getByText("Working folder: /work\\u{000A}\\u{200B}"),
    ).toBeInTheDocument();
    expect(dialog.getByText("Uses this machine's environment: no")).toBeInTheDocument();
    expect(dialog.getByText("Environment: REGION = west")).toBeInTheDocument();
    expect(dialog.getByText("Environment: TOKEN = <masked secret>")).toBeInTheDocument();
    expect(
      dialog.getByText("Input: project = <img src=x onerror=evil()>"),
    ).toBeInTheDocument();
    expect(dialog.getByText("Input: auth = <masked secret>")).toBeInTheDocument();
    expect(dialog.getByText(/Argument values:/)).toBeInTheDocument();
    expect(screen.getByRole("dialog").querySelector("img")).toBeNull();
    expect(screen.getByRole("dialog")).not.toHaveTextContent("hidden-env-secret");
    expect(screen.getByRole("dialog")).not.toHaveTextContent("hidden-input-secret");
  });
  it("highlights only changed fields and offers the full definition", async () => {
    const registry = registryWith(["-y", "old-tool"]);
    const { executionReviewFields } = await import("@/lib/executionReview");
    registry.servers[0].syncExecutionReview = executionReviewFields(registry.servers[0]);
    registry.servers[0].args = ["-y", "new-tool"];
    getRegistry.mockResolvedValue(registry);
    render(<App />);
    await userEvent.click(
      await screen.findByRole("switch", { name: "Toggle Team tool" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    await dialog.findByText("Show full definition");
    expect(
      dialog
        .getAllByText(/^Arguments changed:\s+2\. new-tool \(was old-tool\)$/)
        .find((node) => !node.closest("details")),
    ).toHaveClass("bg-amber-500/10");
    expect(dialog.getByText("Show full definition")).toBeInTheDocument();
    expect(dialog.queryByText("New server")).toBeNull();
    expect(dialog.getByText("Command: npx").closest("details")).not.toHaveAttribute(
      "open",
    );
    await userEvent.click(dialog.getByText("Show full definition"));
    expect(dialog.getByText("Command: npx").closest("details")).toHaveAttribute("open");
  });
  it("keeps a rejected enable visible and clears the error on a new review", async () => {
    const message = "Plain values needs NOTES_DIR. Open server setup to add it.";
    const registry = registryWith(["-y", "old-tool"]);
    registry.servers[0].name = "Plain values";
    getRegistry.mockResolvedValue(registry);
    setServerEnabled.mockRejectedValueOnce(message);
    render(<App />);
    const toggle = await screen.findByRole("switch", { name: "Toggle Plain values" });
    await userEvent.click(toggle);
    const dialog = within(await screen.findByRole("dialog"));
    await waitFor(() =>
      expect(dialog.getByRole("button", { name: "Enable" })).toBeEnabled(),
    );
    await userEvent.click(dialog.getByRole("button", { name: "Enable" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent(message);
    expect(dialog.getByRole("alert").textContent).toBe(message);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(toggle).toHaveAttribute("aria-checked", "false");
    await userEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    await userEvent.click(toggle);
    expect(within(await screen.findByRole("dialog")).queryByRole("alert")).toBeNull();
  });
  it("closes after a successful retry and enables the server", async () => {
    const registry = registryWith(["-y", "old-tool"]);
    registry.profiles[0].enabledServerIds = ["team-tool"];
    setServerEnabled
      .mockRejectedValueOnce(
        new Error("Plain values needs NOTES_DIR. Open server setup to add it."),
      )
      .mockResolvedValueOnce(registry);
    render(<App />);
    const toggle = await screen.findByRole("switch", { name: "Toggle Team tool" });
    await userEvent.click(toggle);
    const dialog = within(await screen.findByRole("dialog"));
    const enable = dialog.getByRole("button", { name: "Enable" });
    await waitFor(() => expect(enable).toBeEnabled());
    await userEvent.click(enable);
    await dialog.findByRole("alert");
    await userEvent.click(enable);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.queryByRole("alert")).toBeNull();
    expect(toggle).toHaveAttribute("aria-checked", "true");
    expect(setServerEnabled).toHaveBeenLastCalledWith(
      "default",
      "team-tool",
      true,
      true,
      expect.objectContaining({ id: "team-tool" }),
    );
  });
  // CodeRev on SBS-786: a team push landing while the confirm is open swaps the
  // definition. The handler re-opens review on the live entry, but a normal
  // return let ConfirmDialog close and null it out, so the promised in-place
  // re-review never appeared. The handler must reject to hold the dialog open.
  it(
    "stays open showing the new command when the definition changes mid-review",
    { timeout: 20000 },
    async () => {
      render(<App />);
      const toggle = await screen.findByRole("switch", { name: "Toggle Team tool" });
      await userEvent.click(toggle);
      expect(await screen.findByText("Command: npx")).toBeInTheDocument();
      expect(
        screen.getByText(/^Arguments:\s+1\. -y\s+2\. old-tool$/),
      ).toBeInTheDocument();

      // The push lands while the member is reading the dialog.
      await act(async () => {
        const onSync = eventHandlers.get("team-sync-registry");
        expect(onSync).toBeDefined();
        onSync!({ payload: registryWith(["-y", "new-tool"]) });
      });

      await userEvent.click(screen.getByRole("button", { name: "Enable" }));

      // Nothing enabled, dialog still up, and it now shows the changed command.
      expect(setServerEnabled).not.toHaveBeenCalled();
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      await waitFor(() =>
        expect(
          screen.getByText(/^Arguments:\s+1\. -y\s+2\. new-tool$/),
        ).toBeInTheDocument(),
      );
    },
  );
});
