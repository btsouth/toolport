import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { Registry } from "@/lib/types";

const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();

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
  setServerEnabled: vi.fn(),
  takeRegistryRecoveryNotice: (...a: unknown[]) => takeRegistryRecoveryNotice(...a),
  teamSyncWait: vi.fn(),
  testServer: vi.fn(),
  updateServer: vi.fn(),
  listStacks: vi.fn(() => Promise.resolve([])),
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

vi.mock("@/components/AppSidebar", () => ({ AppSidebar: () => null }));
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
});

describe("Servers header catalog entry point", () => {
  // The sidebar dropped Catalog, so with servers already present the Servers
  // header must still offer a way to browse the catalog.
  it("shows Browse catalog with servers present and opens the catalog view", async () => {
    render(<App />);

    const browse = await screen.findByRole("button", { name: /Browse catalog/ });
    await userEvent.click(browse);

    expect(
      await screen.findByRole("heading", { name: "Browse catalog" }),
    ).toBeInTheDocument();
  });
});
