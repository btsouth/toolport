import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamPairEvent } from "@/lib/api";

const api = vi.hoisted(() => ({
  teamPairState: vi.fn(),
  teamPairCancel: vi.fn(),
}));
vi.mock("@/lib/api", () => api);

const events = vi.hoisted(() => ({
  handler: null as null | ((event: { payload: unknown }) => void),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    if (name === "team-pair") events.handler = handler;
    return Promise.resolve(vi.fn());
  }),
}));

const toast = vi.hoisted(() => Object.assign(vi.fn(), { success: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

import { TeamPairingDialog } from "./TeamPairingDialog";

const pending: TeamPairEvent = { state: "pending", check: "4b6db433", message: null };
const APPROVE = "Approve this device in your browser";

function emit(event: Partial<TeamPairEvent> & Pick<TeamPairEvent, "state">) {
  act(() => events.handler?.({ payload: { check: null, message: null, ...event } }));
}

async function mounted(onConnected = vi.fn()) {
  render(<TeamPairingDialog onConnected={onConnected} />);
  await waitFor(() => expect(events.handler).not.toBeNull());
  return onConnected;
}

describe("TeamPairingDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    events.handler = null;
    api.teamPairState.mockResolvedValue(null);
    api.teamPairCancel.mockResolvedValue(undefined);
  });

  it("closes the approval prompt when pairing succeeds and opens Teams", async () => {
    const onConnected = await mounted();
    emit(pending);
    expect(await screen.findByText(APPROVE)).toBeInTheDocument();
    expect(screen.getByText("4b6db433")).toBeInTheDocument();

    emit({ state: "connected" });
    await waitFor(() => expect(screen.queryByText(APPROVE)).toBeNull());
    expect(onConnected).toHaveBeenCalledTimes(1);
    expect(toast.success).toHaveBeenCalledWith("Toolport is signed in to sync.");
  });

  it.each([
    "server returned 502: bad gateway",
    "Connection request expired. Choose Connect Toolport again.",
  ])("replaces the prompt with the reason when pairing ends: %s", async (message) => {
    const onConnected = await mounted();
    emit(pending);
    await screen.findByText(APPROVE);
    emit({ state: "failed", message });
    expect(await screen.findByText("Connection not completed")).toBeInTheDocument();
    expect(screen.queryByText(APPROVE)).toBeNull();
    expect(screen.getByText(message, { exact: false })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() =>
      expect(screen.queryByText("Connection not completed")).toBeNull(),
    );
    expect(onConnected).not.toHaveBeenCalled();
  });

  it("keeps pairing when hidden and brings the prompt back for a repeated link", async () => {
    const onConnected = await mounted();
    emit(pending);
    await userEvent.click(await screen.findByRole("button", { name: "Hide" }));
    await waitFor(() => expect(screen.queryByText(APPROVE)).toBeNull());
    expect(api.teamPairCancel).not.toHaveBeenCalled();

    emit(pending);
    expect(await screen.findByText(APPROVE)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hide" }));
    emit({ state: "connected" });
    await waitFor(() => expect(onConnected).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(APPROVE)).toBeNull();
  });

  it("cancels the request and closes without an error", async () => {
    await mounted();
    emit(pending);
    await userEvent.click(await screen.findByRole("button", { name: "Cancel request" }));
    expect(api.teamPairCancel).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Cancelling…" })).toBeDisabled();
    emit({ state: "cancelled" });
    await waitFor(() => expect(screen.queryByText(APPROVE)).toBeNull());
    expect(screen.queryByText("Connection not completed")).toBeNull();
    expect(toast).toHaveBeenCalledWith("Connection request cancelled.");
  });

  it("shows a prompt that was already waiting when the view mounted", async () => {
    api.teamPairState.mockResolvedValue(pending);
    render(<TeamPairingDialog onConnected={vi.fn()} />);
    expect(await screen.findByText(APPROVE)).toBeInTheDocument();
  });
});
