import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  listen: vi.fn(),
  takePendingTrayApprovals: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => mocks.listen(...args),
}));
vi.mock("@/lib/api", () => ({
  takePendingTrayApprovals: (...args: unknown[]) =>
    mocks.takePendingTrayApprovals(...args),
}));

import { subscribeToTrayApprovals } from "./trayApprovals";

describe("tray approvals navigation", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.listen.mockResolvedValue(vi.fn());
    mocks.takePendingTrayApprovals.mockResolvedValue(false);
  });

  it("replays an approvals request captured before the frontend mounted", async () => {
    mocks.takePendingTrayApprovals.mockResolvedValue(true);
    const openApprovals = vi.fn();

    await subscribeToTrayApprovals(openApprovals);

    expect(mocks.listen).toHaveBeenCalledWith(
      "tray-open-approvals",
      expect.any(Function),
    );
    expect(mocks.listen.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.takePendingTrayApprovals.mock.invocationCallOrder[0],
    );
    expect(openApprovals).toHaveBeenCalledTimes(1);
  });

  it("opens once when a ready frontend receives the live event", async () => {
    let handler!: () => void;
    mocks.listen.mockImplementation(async (_event: string, callback: () => void) => {
      handler = callback;
      return vi.fn();
    });
    const openApprovals = vi.fn();

    await subscribeToTrayApprovals(openApprovals);
    handler();

    expect(mocks.takePendingTrayApprovals).toHaveBeenCalledTimes(1);
    expect(openApprovals).toHaveBeenCalledTimes(1);
  });
});
