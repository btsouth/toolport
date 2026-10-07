import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { ProbeResult, Registry } from "@/lib/types";

const probeServers = vi.fn();
const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();
const setServerEnabled = vi.fn();
const setAllEnabled = vi.fn();

// Captures the props App hands to the (mocked) Onboarding wizard so the test can
// invoke onProbe exactly the way the Done step does.
const captured: {
  onProbe: (() => Promise<ProbeResult[]>) | null;
  onFinish?: () => void;
} = { onProbe: null };

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
  probeServers: (...a: unknown[]) => probeServers(...a),
  removeServer: vi.fn(),
  setAllEnabled: (...a: unknown[]) => setAllEnabled(...a),
  setSecret: vi.fn(),
  setServerEnabled: (...a: unknown[]) => setServerEnabled(...a),
  takeRegistryRecoveryNotice: (...a: unknown[]) => takeRegistryRecoveryNotice(...a),
  teamSyncWait: vi.fn(),
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

vi.mock("@/components/AppSidebar", () => ({
  AppSidebar: ({
    registry,
    onRegistryChange,
    onSelectView,
  }: {
    registry: Registry | null;
    onRegistryChange: (registry: Registry) => void;
    onSelectView: (view: "clients") => void;
  }) => (
    <>
      <button type="button" onClick={() => onSelectView("clients")}>
        Clients
      </button>
      {registry?.profiles.some((profile) => profile.id === "work") && (
        <button
          type="button"
          onClick={() =>
            onRegistryChange({ ...registry, defaultAccessProfileId: "work" })
          }
        >
          Change default access
        </button>
      )}
    </>
  ),
}));
vi.mock("@/components/PendingApprovals", () => ({ PendingApprovals: () => null }));
vi.mock("@/components/QuarantineAlert", () => ({ QuarantineAlert: () => null }));

vi.mock("@/components/Onboarding", () => ({
  Onboarding: (props: {
    onProbe: () => Promise<ProbeResult[]>;
    onFinish: () => void;
  }) => {
    captured.onProbe = props.onProbe;
    captured.onFinish = props.onFinish;
    return null;
  },
}));

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
  captured.onProbe = null;
  getRegistry.mockResolvedValue({
    version: 3,
    servers: [],
    profiles: [],
    activeProfileId: null,
  });
  detectClients.mockResolvedValue([]);
  takeRegistryRecoveryNotice.mockResolvedValue(null);
});

describe("App onboarding probe wiring", () => {
  // SBS-720 / CodeRev: after Connect, load() already kicked off a probe. The Done
  // step's verification must JOIN that in-flight probe (reprobe), not queue a second
  // full probeServers pass behind it (reprobeAfterMutation) — each pass is bounded
  // at 90s per server, so stacking two can hold "Checking" for minutes.
  it("joins the in-flight health probe instead of starting a second one", async () => {
    const results: ProbeResult[] = [
      { serverId: "s1", ok: true, toolCount: 3, error: null, authRequired: false },
    ];
    const inFlight = deferred<ProbeResult[]>();
    probeServers.mockReturnValueOnce(inFlight.promise).mockResolvedValue([]);

    render(<App />);

    // Fresh install (no servers, no connected clients) opens onboarding, and the
    // initial load has already started a silent health probe that is still pending.
    await waitFor(() => expect(captured.onProbe).not.toBeNull());
    await waitFor(() => expect(probeServers).toHaveBeenCalledTimes(1));

    let probePromise!: Promise<ProbeResult[]>;
    act(() => {
      probePromise = captured.onProbe!();
    });

    await act(async () => {
      inFlight.resolve(results);
    });

    // The Done step gets the authoritative in-flight result...
    await expect(probePromise).resolves.toEqual(results);
    // ...and no trailing probeServers pass was queued behind it.
    await act(async () => {});
    expect(probeServers).toHaveBeenCalledTimes(1);
  });
});

describe("App health visibility", () => {
  it("keeps the backend warning visible after navigating away from Servers", async () => {
    localStorage.setItem("toolport.onboarded", "1");
    probeServers.mockRejectedValue(new Error("backend unavailable"));

    render(<App />);

    expect(await screen.findByText(/backend didn't respond/i)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Clients" }));
    expect(screen.getByText(/backend didn't respond/i)).toBeInTheDocument();
  });

  it("keeps an enabled server with no health result out of Ready", async () => {
    localStorage.setItem("toolport.onboarded", "1");
    getRegistry.mockResolvedValue({
      version: 3,
      servers: [
        {
          id: "server-1",
          enabled: true,
          name: "Unchecked server",
          transport: "stdio",
          command: "example",
          args: [],
          env: [],
          url: null,
          source: "manual",
        },
      ],
      profiles: [{ id: "default", name: "Default", enabledServerIds: ["server-1"] }],
      activeProfileId: "default",
    });
    probeServers.mockResolvedValue([]);

    render(<App />);

    expect(
      await screen.findByRole("button", { name: /checking 1/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ready/i })).not.toBeInTheDocument();
  });

  it("drops stale health when a server is re-enabled", async () => {
    localStorage.setItem("toolport.onboarded", "1");
    const server = {
      id: "server-1",
      enabled: true,
      name: "Example",
      transport: "stdio",
      command: "example",
      args: [],
      env: [],
      url: null,
      source: "manual",
    };
    const enabledRegistry = {
      version: 3,
      servers: [server],
      profiles: [{ id: "default", name: "Default", enabledServerIds: ["server-1"] }],
      activeProfileId: "default",
    };
    const disabledRegistry = {
      ...enabledRegistry,
      servers: [{ ...server, enabled: false }],
    };
    const nextProbe = deferred<ProbeResult[]>();
    probeServers
      .mockResolvedValueOnce([
        {
          serverId: "server-1",
          ok: true,
          toolCount: 1,
          error: null,
          authRequired: false,
        },
      ])
      .mockReturnValueOnce(nextProbe.promise);
    getRegistry.mockResolvedValue(enabledRegistry);
    setServerEnabled
      .mockResolvedValueOnce(disabledRegistry)
      .mockResolvedValueOnce(enabledRegistry);

    render(<App />);

    expect(await screen.findByRole("button", { name: /ready 1/i })).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Toggle Example"));
    await waitFor(() =>
      expect(setServerEnabled).toHaveBeenCalledWith("default", "server-1", false, false),
    );

    await userEvent.click(screen.getByRole("button", { name: /disabled 1/i }));
    await userEvent.click(screen.getByLabelText("Toggle Example"));

    expect(
      await screen.findByRole("button", { name: /checking 1/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ready/i })).not.toBeInTheDocument();
    expect(
      screen.queryByText(/1 of 1 enabled servers reachable/i),
    ).not.toBeInTheDocument();
  });

  it("drops stale health when all servers are re-enabled", async () => {
    localStorage.setItem("toolport.onboarded", "1");
    const server = {
      id: "server-1",
      enabled: true,
      name: "Example",
      transport: "stdio",
      command: "example",
      args: [],
      env: [],
      url: null,
      source: "manual",
    };
    const enabledRegistry = {
      version: 3,
      servers: [server],
      profiles: [{ id: "default", name: "Default", enabledServerIds: ["server-1"] }],
      activeProfileId: "default",
    };
    const disabledRegistry = {
      ...enabledRegistry,
      servers: [{ ...server, enabled: false }],
    };
    const nextProbe = deferred<ProbeResult[]>();
    probeServers
      .mockResolvedValueOnce([
        {
          serverId: "server-1",
          ok: true,
          toolCount: 1,
          error: null,
          authRequired: false,
        },
      ])
      .mockReturnValueOnce(nextProbe.promise);
    getRegistry.mockResolvedValue(enabledRegistry);
    setAllEnabled
      .mockResolvedValueOnce(disabledRegistry)
      .mockResolvedValueOnce(enabledRegistry);

    render(<App />);

    expect(await screen.findByRole("button", { name: /ready 1/i })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "More actions" }));
    await userEvent.click(screen.getByText("Disable all"));
    await waitFor(() => expect(setAllEnabled).toHaveBeenCalledWith("default", false));

    await userEvent.click(screen.getByRole("button", { name: "More actions" }));
    await userEvent.click(screen.getByText("Enable all"));

    expect(
      await screen.findByRole("button", { name: /checking 1/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ready/i })).not.toBeInTheDocument();
  });

  it("invalidates health and probes after changing default access", async () => {
    localStorage.setItem("toolport.onboarded", "1");
    const server = (id: string, name: string) => ({
      id,
      enabled: id === "server-1",
      name,
      transport: "stdio",
      command: "example",
      args: [],
      env: [],
      url: null,
      source: "manual",
    });
    getRegistry.mockResolvedValue({
      version: 3,
      servers: [server("server-1", "Default server"), server("server-2", "Work server")],
      profiles: [
        { id: "default", name: "Default", enabledServerIds: ["server-1"] },
        { id: "work", name: "Work", enabledServerIds: ["server-2"] },
      ],
      activeProfileId: "default",
    });
    const nextProbe = deferred<ProbeResult[]>();
    probeServers
      .mockResolvedValueOnce([
        {
          serverId: "server-1",
          ok: true,
          toolCount: 1,
          error: null,
          authRequired: false,
        },
      ])
      .mockReturnValueOnce(nextProbe.promise);

    render(<App />);

    expect(await screen.findByRole("button", { name: /ready 1/i })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Change default access" }));

    await waitFor(() => expect(probeServers).toHaveBeenCalledTimes(2));
    expect(
      await screen.findByRole("button", { name: /checking 1/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ready/i })).not.toBeInTheDocument();
  });
});

describe("server list stability and automatic prompts", () => {
  it("keeps adjacent toggle targets in place when enabled state changes", async () => {
    const servers = ["Zulu", "Alpha", "Beta"].map((name) => ({
      id: name,
      name,
      transport: "stdio",
      command: "fixture",
      args: [],
      env: [],
      url: null,
      source: "manual",
    }));
    const registry = {
      version: 1,
      servers,
      profiles: [{ id: "p", name: "Default", enabledServerIds: [] as string[] }],
      activeProfileId: "p",
    } as Registry;
    getRegistry.mockResolvedValue(registry);
    probeServers.mockResolvedValue([]);
    setServerEnabled.mockImplementation((_profile, id, enabled) =>
      Promise.resolve({
        ...registry,
        profiles: [{ ...registry.profiles[0], enabledServerIds: enabled ? [id] : [] }],
      }),
    );
    render(<App />);
    await screen.findByRole("switch", { name: "Toggle Alpha" });
    const order = () =>
      screen.getAllByRole("switch").map((row) => row.getAttribute("aria-label"));
    const before = order();
    await userEvent.click(screen.getByRole("switch", { name: "Toggle Alpha" }));
    await waitFor(() => expect(setServerEnabled).toHaveBeenCalled());
    expect(order()).toEqual(before);
    expect(before).toEqual(["Toggle Alpha", "Toggle Beta", "Toggle Zulu"]);
  });
  it("never asks for a star after finishing onboarding or returning", async () => {
    probeServers.mockResolvedValue([]);
    const { unmount } = render(<App />);
    await waitFor(() => expect(captured.onFinish).toBeDefined());
    act(() => captured.onFinish!());
    vi.useFakeTimers();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(16000);
    });
    expect(screen.queryByText(/Star.*GitHub/i)).not.toBeInTheDocument();
    unmount();
    render(<App />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(16000);
    });
    expect(screen.queryByText(/Star.*GitHub/i)).not.toBeInTheDocument();
    vi.useRealTimers();
  });
});
