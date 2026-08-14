import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { Registry } from "@/lib/types";

const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();
const setServerEnabled = vi.fn();
const teamSyncWait = vi.fn();

vi.mock("@/lib/api", () => ({
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
  teamSyncWait: (...a: unknown[]) => teamSyncWait(...a),
  testServer: vi.fn(),
  updateServer: vi.fn(),
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  getRegistry.mockResolvedValue(registryWith(["-y", "old-tool"]));
  detectClients.mockResolvedValue([]);
  takeRegistryRecoveryNotice.mockResolvedValue(null);
});

describe("team enable review dialog", () => {
  // CodeRev on SBS-786: a team push landing while the confirm is open swaps the
  // definition. The handler re-opens review on the live entry, but a normal
  // return let ConfirmDialog close and null it out, so the promised in-place
  // re-review never appeared. The handler must reject to hold the dialog open.
  it(
    "stays open showing the new command when the definition changes mid-review",
    { timeout: 20000 },
    async () => {
      const sync = deferred<Registry>();
      // First long-poll delivers the mutated registry when we choose; later polls park.
      teamSyncWait
        .mockReturnValueOnce(sync.promise)
        .mockReturnValue(new Promise(() => {}));

      render(<App />);
      // The (only) server is off, so it sits in the collapsed Disabled group.
      await userEvent.click(
        await screen.findByRole("button", { name: /Disabled/ }, { timeout: 3000 }),
      );
      const toggle = await screen.findByRole("switch", { name: "Toggle Team tool" });
      await userEvent.click(toggle);
      expect(await screen.findByText(/npx -y old-tool/)).toBeInTheDocument();

      // The push lands while the member is reading the dialog.
      await act(async () => {
        sync.resolve(registryWith(["-y", "new-tool"]));
      });

      await userEvent.click(screen.getByRole("button", { name: "Enable" }));

      // Nothing enabled, dialog still up, and it now shows the changed command.
      expect(setServerEnabled).not.toHaveBeenCalled();
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      await waitFor(() =>
        expect(screen.getByText(/npx -y new-tool/)).toBeInTheDocument(),
      );
    },
  );
});
