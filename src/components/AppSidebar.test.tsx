import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";

import { TooltipProvider } from "@/components/ui/tooltip";
import type { DetectedClient } from "@/lib/types";
import { AppSidebar } from "./AppSidebar";

const getSavingsSummary = vi.fn();
const listQuarantined = vi.fn();
const checkForUpdate = vi.fn();
const installUpdate = vi.fn();
const toastInfo = vi.fn();
const toastError = vi.fn();
const openDataDir = vi.fn();
const eventListeners = new Map<string, (event: { payload: unknown }) => void>();

vi.mock("sonner", () => ({
  toast: {
    info: (...args: unknown[]) => toastInfo(...args),
    success: vi.fn(),
  },
}));

vi.mock("@/lib/toast", () => ({
  toastError: (...args: unknown[]) => toastError(...args),
}));

vi.mock("@/lib/api", () => ({
  gatherDiagnostics: vi.fn(),
  getSavingsSummary: (...args: unknown[]) => getSavingsSummary(...args),
  listQuarantined: (...args: unknown[]) => listQuarantined(...args),
  openDataDir: (...args: unknown[]) => openDataDir(...args),
}));

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("1.0.0"),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: (event: { payload: unknown }) => void) => {
    eventListeners.set(event, handler);
    return Promise.resolve(() => eventListeners.delete(event));
  }),
}));

vi.mock("@/lib/updater", () => ({
  checkForUpdate: (...args: unknown[]) => checkForUpdate(...args),
  installUpdate: (...args: unknown[]) => installUpdate(...args),
}));

vi.mock("@/components/ShareDialog", () => ({
  ShareDialog: ({ trigger }: { trigger: ReactNode }) => trigger,
}));

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

beforeEach(() => {
  getSavingsSummary.mockReset();
  listQuarantined.mockReset();
  checkForUpdate.mockReset();
  installUpdate.mockReset();
  toastInfo.mockReset();
  toastError.mockReset();
  openDataDir.mockReset();
  eventListeners.clear();
  checkForUpdate.mockResolvedValue({ kind: "current" });
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    listLoads: 0,
    peakCatalog: 0,
    sinceTs: 0,
  });
  listQuarantined.mockResolvedValue([]);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("AppSidebar accessibility", () => {
  it("names both navigation landmarks and exposes client selection state", () => {
    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId="cursor"
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    expect(screen.getByRole("navigation", { name: "Views" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Clients" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /cursor/i })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("exposes whether the not-detected client list is expanded", async () => {
    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client(), client({ id: "zed", name: "Zed", appPresent: false })]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    const toggle = screen.getByRole("button", { name: /not detected/i });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("button", { name: /zed/i })).toBeInTheDocument();
  });

  it("rechecks updates when a stale tray-hidden window is shown", async () => {
    let now = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await waitFor(() => expect(checkForUpdate).toHaveBeenCalledTimes(1));
    now += 24 * 60 * 60 * 1000;
    act(() => {
      eventListeners.get("team-window-visible")?.({ payload: true });
    });
    await waitFor(() => expect(checkForUpdate).toHaveBeenCalledTimes(2));
  });

  it("retries a quiet update check after a transient error", async () => {
    checkForUpdate
      .mockResolvedValueOnce({ kind: "error", message: "offline" })
      .mockResolvedValueOnce({ kind: "current" });

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await waitFor(() => expect(checkForUpdate).toHaveBeenCalledTimes(1));
    act(() => {
      eventListeners.get("team-window-visible")?.({ payload: true });
    });
    await waitFor(() => expect(checkForUpdate).toHaveBeenCalledTimes(2));
  });

  it("shows byte-based download progress while installing", async () => {
    const update = { version: "1.1.0", body: "Release notes" };
    checkForUpdate.mockResolvedValue({ kind: "update", update });
    installUpdate.mockImplementation(
      async (_update: unknown, onProgress: (progress: unknown) => void) => {
        onProgress({
          phase: "downloading",
          downloadedBytes: 5,
          totalBytes: 10,
        });
        await new Promise(() => {});
      },
    );

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    const updateButton = await screen.findByRole("button", { name: /update to v1.1.0/i });
    await userEvent.click(updateButton);
    await userEvent.click(screen.getByRole("button", { name: /install and restart/i }));

    expect((await screen.findAllByText("Downloading 50%")).length).toBeGreaterThan(0);
  });

  it("shows updater recovery guidance without losing the install error", async () => {
    const update = { version: "1.1.0", body: "Release notes" };
    checkForUpdate.mockResolvedValue({ kind: "update", update });
    installUpdate.mockRejectedValue(
      Object.assign(new Error("package signature rejected"), {
        recoveryAdvice:
          "Restart Cursor.exe to recreate its Toolport connection. Toolport restored its HTTP endpoint on port 8765.",
      }),
    );

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: /update to v1.1.0/i }),
    );
    await userEvent.click(screen.getByRole("button", { name: /install and restart/i }));

    await waitFor(() =>
      expect(toastError).toHaveBeenCalledWith(
        "Update failed: package signature rejected",
        expect.objectContaining({
          description: expect.stringContaining(
            "Restart Cursor.exe to recreate its Toolport connection",
          ),
        }),
      ),
    );
  });

  it("turns an in-flight quiet check into an announced tray result", async () => {
    const update = { version: "1.1.0", body: "Release notes" };
    let resolveCheck!: (result: unknown) => void;
    checkForUpdate.mockReturnValue(
      new Promise((resolve) => {
        resolveCheck = resolve;
      }),
    );

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await waitFor(() => expect(checkForUpdate).toHaveBeenCalledTimes(1));
    act(() => {
      eventListeners.get("tray-check-updates")?.({ payload: undefined });
    });
    await act(async () => {
      resolveCheck({ kind: "update", update });
    });

    expect(checkForUpdate).toHaveBeenCalledTimes(1);
    expect(
      await screen.findByRole("heading", { name: "Update available: v1.1.0" }),
    ).toBeInTheDocument();
  });

  it("acknowledges an explicit tray check while an update is installing", async () => {
    const update = { version: "1.1.0", body: "Release notes" };
    checkForUpdate.mockResolvedValue({ kind: "update", update });
    installUpdate.mockReturnValue(new Promise(() => {}));

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: /update to v1.1.0/i }),
    );
    await userEvent.click(screen.getByRole("button", { name: /install and restart/i }));
    act(() => {
      eventListeners.get("tray-check-updates")?.({ payload: undefined });
    });

    expect(toastInfo).toHaveBeenCalledWith("An update is already in progress");
    expect(checkForUpdate).toHaveBeenCalledTimes(1);
  });
});

describe("AppSidebar quarantine badge", () => {
  function renderSidebar() {
    return render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );
  }

  function quarantinedTool(over: Partial<import("@/lib/api").QuarantinedTool> = {}) {
    return {
      server: "linear",
      tool: "linear__save_issue",
      reason: "a destructive tool's definition changed",
      ts: Date.now(),
      profile: "",
      ...over,
    };
  }

  it("shows the blocked-tool count once the poll confirms a count", async () => {
    listQuarantined.mockResolvedValue([
      quarantinedTool({ tool: "a" }),
      quarantinedTool({ tool: "b" }),
    ]);

    renderSidebar();

    expect(await screen.findByLabelText("2 tools blocked")).toBeInTheDocument();
    expect(screen.queryByLabelText("Quarantine status unknown")).not.toBeInTheDocument();
  });

  it("shows no badge on a confirmed clean state", async () => {
    listQuarantined.mockResolvedValue([]);

    renderSidebar();

    await waitFor(() => expect(listQuarantined).toHaveBeenCalled());
    expect(screen.queryByLabelText("Quarantine status unknown")).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/tool\s?blocked/)).not.toBeInTheDocument();
  });

  it("shows no badge before the first poll answers (#742)", async () => {
    let resolvePoll!: (q: import("@/lib/api").QuarantinedTool[]) => void;
    listQuarantined.mockReturnValue(
      new Promise((resolve) => {
        resolvePoll = resolve;
      }),
    );

    renderSidebar();

    await waitFor(() => expect(listQuarantined).toHaveBeenCalled());
    expect(screen.queryByLabelText("Quarantine status unknown")).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/tool\s?blocked/)).not.toBeInTheDocument();

    await act(async () => {
      resolvePoll([quarantinedTool({ tool: "a" })]);
    });

    expect(await screen.findByLabelText("1 tool blocked")).toBeInTheDocument();
  });

  it("does not present a failed poll as an all-clear (#741)", async () => {
    listQuarantined.mockRejectedValue(new Error("gateway not reachable"));

    renderSidebar();

    expect(await screen.findByLabelText("Quarantine status unknown")).toBeInTheDocument();
    expect(screen.queryByLabelText(/tool\s?blocked/)).not.toBeInTheDocument();
  });

  it("keeps surfacing the unknown state across repeated failures (#741)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      listQuarantined.mockRejectedValue(new Error("gateway not reachable"));

      renderSidebar();

      expect(
        await screen.findByLabelText("Quarantine status unknown"),
      ).toBeInTheDocument();

      const callsBeforeNextTick = listQuarantined.mock.calls.length;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });

      expect(listQuarantined.mock.calls.length).toBeGreaterThan(callsBeforeNextTick);
      expect(screen.getByLabelText("Quarantine status unknown")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a confirmed count on a failed poll and marks it stale (#742)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      listQuarantined
        .mockResolvedValueOnce([
          quarantinedTool({ tool: "a" }),
          quarantinedTool({ tool: "b" }),
          quarantinedTool({ tool: "c" }),
        ])
        .mockRejectedValueOnce(new Error("gateway not reachable"));

      renderSidebar();

      const badge = await screen.findByLabelText("3 tools blocked");
      expect(badge).toBeInTheDocument();
      expect(
        screen.queryByLabelText("Quarantine status unknown"),
      ).not.toBeInTheDocument();

      const callsBeforeNextTick = listQuarantined.mock.calls.length;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });

      expect(listQuarantined.mock.calls.length).toBeGreaterThan(callsBeforeNextTick);
      // The confirmed count survives the failed poll instead of degrading to "?".
      expect(screen.getByLabelText("3 tools blocked")).toBeInTheDocument();
      expect(
        screen.queryByLabelText("Quarantine status unknown"),
      ).not.toBeInTheDocument();
      // ...but the badge says the number may be stale.
      expect(screen.getByLabelText("3 tools blocked")).toHaveAttribute(
        "title",
        expect.stringContaining("stale"),
      );
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("AppSidebar open data folder", () => {
  it("shows an error toast when opening the data folder fails", async () => {
    openDataDir.mockRejectedValue(new Error("no such directory"));
    const user = userEvent.setup();

    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await user.click(await screen.findByLabelText("Open data folder"));

    await waitFor(() => {
      expect(toastError).toHaveBeenCalledWith("Couldn't open data folder");
    });
  });
});
