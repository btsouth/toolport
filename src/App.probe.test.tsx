import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, waitFor } from "@testing-library/react";
import App from "./App";
import type { ProbeResult } from "@/lib/types";

const probeServers = vi.fn();
const getRegistry = vi.fn();
const detectClients = vi.fn();
const takeRegistryRecoveryNotice = vi.fn();

// Captures the props App hands to the (mocked) Onboarding wizard so the test can
// invoke onProbe exactly the way the Done step does.
const captured: { onProbe: (() => Promise<ProbeResult[]>) | null } = { onProbe: null };

vi.mock("@/lib/api", () => ({
  addServer: vi.fn(),
  detectClients: (...a: unknown[]) => detectClients(...a),
  getRegistry: (...a: unknown[]) => getRegistry(...a),
  importServers: vi.fn(),
  mainWindowVisible: vi.fn(() => Promise.resolve(true)),
  parseServerSnippet: vi.fn(),
  previewImportServers: vi.fn(),
  probeServers: (...a: unknown[]) => probeServers(...a),
  removeServer: vi.fn(),
  setAllEnabled: vi.fn(),
  setSecret: vi.fn(),
  setServerEnabled: vi.fn(),
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

vi.mock("@/components/AppSidebar", () => ({ AppSidebar: () => null }));
vi.mock("@/components/PendingApprovals", () => ({ PendingApprovals: () => null }));
vi.mock("@/components/QuarantineAlert", () => ({ QuarantineAlert: () => null }));

vi.mock("@/components/Onboarding", () => ({
  Onboarding: (props: { onProbe: () => Promise<ProbeResult[]> }) => {
    captured.onProbe = props.onProbe;
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
    version: 1,
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
