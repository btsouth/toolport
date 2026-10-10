import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";

import type { Registry } from "@/lib/types";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppSidebar } from "./AppSidebar";
import { getVersion } from "@tauri-apps/api/app";

const getSavingsSummary = vi.fn();
const listQuarantined = vi.fn();
const checkForUpdate = vi.fn();
const installUpdate = vi.fn();
const toastInfo = vi.fn();
const toastError = vi.fn();
const openDataDir = vi.fn();
const openExternal = vi.fn();
const exitApp = vi.fn().mockResolvedValue(undefined);
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

vi.mock("@/lib/openUrl", () => ({
  openExternal: (...args: unknown[]) => openExternal(...args),
}));

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("1.0.0"),
}));

vi.mock("@tauri-apps/plugin-process", () => ({
  exit: (...args: unknown[]) => exitApp(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: (event: { payload: unknown }) => void) => {
    eventListeners.set(event, handler);
    return Promise.resolve(() => eventListeners.delete(event));
  }),
}));

vi.mock("@/lib/updater", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/updater")>()),
  checkForUpdate: (...args: unknown[]) => checkForUpdate(...args),
  installUpdate: (...args: unknown[]) => installUpdate(...args),
  releasePageUrl: (version: string) =>
    `https://github.com/btsouth/toolport/releases/tag/v${version}`,
}));

vi.mock("@/components/ShareDialog", () => ({
  ShareDialog: ({ trigger }: { trigger: ReactNode }) => trigger,
}));

function fakeUpdate(version = "1.1.0") {
  return { version, body: "Release notes", close: vi.fn().mockResolvedValue(undefined) };
}

/** The minimum registry a paired install hands the sidebar: one profile so the
 * ProfileBar renders, and a team connection so Team is a top-level row. */
function pairedRegistry(): Registry {
  return {
    profiles: [{ id: "default", name: "Default" }],
    activeProfileId: "default",
    team: { teamId: "team-1" },
  } as unknown as Registry;
}

beforeEach(() => {
  getSavingsSummary.mockReset();
  listQuarantined.mockReset();
  checkForUpdate.mockReset();
  installUpdate.mockReset();
  toastInfo.mockReset();
  toastError.mockReset();
  openDataDir.mockReset();
  openExternal.mockReset().mockResolvedValue(undefined);
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
  it("promotes Clients into the primary navigation", () => {
    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="clients"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    expect(screen.getByRole("navigation", { name: "Views" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Clients" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clients" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("shows the four top-level views and hides Team until paired", async () => {
    const onSelectView = vi.fn();
    const { rerender } = render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={onSelectView}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    const nav = screen.getByRole("navigation", { name: "Views" });
    expect(
      within(nav)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual(["Servers", "Clients", "Activity", "Settings"]);
    // Catalog, Playground, Agent rules, Agent activity and Team are not top-level.
    for (const name of [
      "Browse catalog",
      "Playground",
      "Agent rules",
      "Agent activity",
      "Team",
    ]) {
      expect(within(nav).queryByRole("button", { name })).not.toBeInTheDocument();
    }

    rerender(
      <TooltipProvider>
        <AppSidebar
          registry={pairedRegistry()}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={onSelectView}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await userEvent.click(screen.getByRole("button", { name: "Team" }));
    expect(onSelectView).toHaveBeenCalledWith("teams");
  });

  it("rechecks updates when a stale tray-hidden window is shown", async () => {
    let now = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);

    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
    const update = fakeUpdate();
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
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    const updateButton = await screen.findByRole("button", { name: /update to v1.1.0/i });
    await userEvent.click(updateButton);
    await userEvent.click(screen.getByRole("button", { name: /install and restart/i }));

    expect((await screen.findAllByText("Downloading 50%")).length).toBeGreaterThan(0);
  });

  it.each([
    ["deb", "sudo apt install ./<file>.deb"],
    ["rpm", "sudo dnf install ./<file>.rpm"],
    ["pacman", "sudo pacman -Syu"],
  ])("directs a .%s install to its package manager", async (systemPackage, command) => {
    const update = fakeUpdate();
    checkForUpdate.mockResolvedValue({ kind: "update", update, systemPackage });

    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: /update to v1.1.0/i }),
    );
    expect(screen.getByText((text) => text.includes(command))).toBeInTheDocument();
    if (systemPackage !== "pacman") {
      expect(screen.getByText(/download the new/i)).toBeInTheDocument();
    }
    expect(
      screen.queryByRole("button", { name: /install and restart/i }),
    ).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /open release page/i }));

    expect(openExternal).toHaveBeenCalledWith(
      "https://github.com/btsouth/toolport/releases/tag/v1.1.0",
    );
    expect(installUpdate).not.toHaveBeenCalled();
  });

  it("releases an update once a newer check or unmount replaces it", async () => {
    const first = fakeUpdate("1.1.0");
    const second = fakeUpdate("1.2.0");
    checkForUpdate
      .mockResolvedValueOnce({ kind: "update", update: first, systemPackage: "deb" })
      .mockResolvedValueOnce({ kind: "update", update: second, systemPackage: "deb" });

    const { unmount } = render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await screen.findByRole("button", { name: /update to v1.1.0/i });
    act(() => {
      eventListeners.get("tray-check-updates")?.({ payload: undefined });
    });
    await screen.findByRole("heading", { name: "Update available: v1.2.0" });

    expect(first.close).toHaveBeenCalledTimes(1);
    expect(second.close).not.toHaveBeenCalled();

    unmount();
    expect(second.close).toHaveBeenCalledTimes(1);
  });

  it("shows updater recovery guidance without losing the install error", async () => {
    const update = fakeUpdate();
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
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
    const update = fakeUpdate();
    let resolveCheck!: (result: unknown) => void;
    checkForUpdate.mockReturnValue(
      new Promise((resolve) => {
        resolveCheck = resolve;
      }),
    );

    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
    const update = fakeUpdate();
    checkForUpdate.mockResolvedValue({ kind: "update", update });
    installUpdate.mockReturnValue(new Promise(() => {}));

    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
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
  it("keeps Help and Quit reachable when the version lookup fails", async () => {
    vi.mocked(getVersion).mockRejectedValueOnce(new Error("unavailable"));
    const user = userEvent.setup();
    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );
    await user.click(screen.getByRole("button", { name: "Help" }));
    await user.click(screen.getByRole("button", { name: "Quit Toolport" }));
    expect(exitApp).toHaveBeenCalledWith(0);
  });

  it("shows an error toast when opening the data folder fails", async () => {
    openDataDir.mockRejectedValue(new Error("no such directory"));
    const user = userEvent.setup();

    render(
      <TooltipProvider>
        <AppSidebar
          registry={null}
          onRegistryChange={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onShortcuts={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    await user.click(await screen.findByRole("button", { name: /Help/ }));
    await user.click(await screen.findByRole("button", { name: "Open data folder" }));

    await waitFor(() => {
      expect(toastError).toHaveBeenCalledWith("Couldn't open data folder");
    });
  });
});

it("shows negative net savings with the tokenizer method and excludes legacy estimates", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: -12_340,
    tokenizedLoads: 1,
    listLoads: 99,
    peakCatalog: 80,
    sinceTs: 0,
    legacyEstimatedTokensAvoided: 1_000_000,
  });
  render(
    <TooltipProvider>
      <AppSidebar
        registry={null}
        onRegistryChange={vi.fn()}
        view="servers"
        onSelectView={vi.fn()}
        onShortcuts={vi.fn()}
        onReplayOnboarding={vi.fn()}
      />
    </TooltipProvider>,
  );
  const badge = await screen.findByRole("button", {
    name: "-12.3k catalog tokens avoided",
  });
  expect(badge).toHaveAttribute("title", expect.stringContaining("cl100k_base"));
  expect(badge).toHaveAttribute("title", expect.stringContaining("net of discovery"));
  expect(badge).toHaveAttribute("title", expect.stringContaining("once per session"));
  expect(badge).not.toHaveTextContent("1.0M");
});
