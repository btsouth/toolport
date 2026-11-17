import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { Registry } from "@/lib/types";

const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();
const importServers = vi.fn();
const previewImportServers = vi.fn();

vi.mock("@/lib/api", () => ({
  teamPairState: vi.fn(() => Promise.resolve(null)),
  teamPairCancel: vi.fn(),
  addServer: vi.fn(),
  detectClients: (...a: unknown[]) => detectClients(...a),
  getRegistry: (...a: unknown[]) => getRegistry(...a),
  importServers: (...a: unknown[]) => importServers(...a),
  mainWindowVisible: vi.fn(() => Promise.resolve(true)),
  parseServerSnippet: vi.fn(),
  previewImportServers: (...a: unknown[]) => previewImportServers(...a),
  probeServers: vi.fn(() => Promise.resolve([])),
  removeServer: vi.fn(),
  setAllEnabled: vi.fn(),
  setSecret: vi.fn(),
  setServerEnabled: vi.fn(),
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

    expect(await screen.findByRole("heading", { name: "Catalog" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Back to servers" }));
    expect(
      await screen.findByRole("heading", { level: 1, name: "Servers" }),
    ).toBeInTheDocument();
  });
});

describe("Servers empty state import count", () => {
  it("counts distinct connections across clients, not names", async () => {
    getRegistry.mockResolvedValue({ ...registryWithServer(), servers: [] });
    // No servers and no connected client would open the first-run wizard. It loads
    // lazily, so it could open over the import review and hide it.
    localStorage.setItem("toolport.onboarded", "1");
    const github = (url: string) => ({
      name: "github",
      transport: "http",
      command: null,
      args: [],
      envKeys: [],
      url,
    });
    const client = (id: string, url: string, key: string) => ({
      id,
      name: id,
      usesConnectors: false,
      configPath: `${id}.json`,
      configExists: true,
      appPresent: true,
      servers: [github(url)],
      pluginServers: [],
      gatewayInstalled: false,
      entryState: "absent",
      error: null,
      importableServers: [github(url)],
      importableKeys: [key],
      managedServerNames: [],
    });
    detectClients.mockResolvedValue([
      client("cursor", "https://api.githubcopilot.com/mcp/", "connection:a"),
      client("claude-code", "https://example.test/github", "connection:b"),
      client("codex", "https://api.githubcopilot.com/mcp/", "connection:a"),
    ]);
    render(<App />);
    const importButton = await screen.findByRole("button", {
      name: /Import 2 from clients/,
    });
    expect(importButton).toBeVisible();

    // After importing, the clients are scanned again: counts come from detection.
    previewImportServers.mockResolvedValue([
      {
        ...github("https://api.githubcopilot.com/mcp/"),
        key: "connection:a",
        isNew: true,
      },
    ]);
    importServers.mockResolvedValue(registryWithServer());
    const scans = detectClients.mock.calls.length;
    await userEvent.click(importButton);
    await userEvent.click(await screen.findByRole("button", { name: "Import 1 server" }));
    await waitFor(() => expect(importServers).toHaveBeenCalledWith(["connection:a"]));
    await waitFor(() => expect(detectClients.mock.calls.length).toBeGreaterThan(scans));
  });
});
