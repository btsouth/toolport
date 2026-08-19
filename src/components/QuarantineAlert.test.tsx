import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QuarantineAlert } from "./QuarantineAlert";
import type { QuarantinedTool } from "@/lib/api";

const listQuarantined = vi.fn();
const releaseQuarantine = vi.fn();
const releaseAllQuarantine = vi.fn();
const toastError = vi.fn();
const toastSuccess = vi.fn();

vi.mock("@/lib/api", () => ({
  listQuarantined: (...a: unknown[]) => listQuarantined(...a),
  releaseQuarantine: (...a: unknown[]) => releaseQuarantine(...a),
  releaseAllQuarantine: (...a: unknown[]) => releaseAllQuarantine(...a),
}));
vi.mock("@/lib/toast", () => ({ toastError: (...a: unknown[]) => toastError(...a) }));
vi.mock("sonner", () => ({
  toast: { success: (...a: unknown[]) => toastSuccess(...a) },
}));

/** Open the bulk confirm and press its confirm button. */
async function confirmReapproveAll() {
  await userEvent.click(screen.getByRole("button", { name: /re-approve all \(/i }));
  const dialog = await screen.findByRole("dialog");
  await userEvent.click(
    within(dialog).getByRole("button", { name: /^re-approve all$/i }),
  );
}

function tool(over: Partial<QuarantinedTool> = {}): QuarantinedTool {
  return {
    server: "linear",
    tool: "linear__save_issue",
    reason: "a destructive tool's definition changed",
    ts: Date.now(),
    profile: "",
    ...over,
  };
}

beforeEach(() => {
  listQuarantined.mockReset();
  releaseQuarantine.mockReset();
  releaseAllQuarantine.mockReset();
  toastError.mockReset();
  toastSuccess.mockReset();
});

describe("QuarantineAlert bulk re-approval", () => {
  /** A lost integrity baseline blocks the whole catalog at once. A real install saw
   * 2,156 tools blocked in one shot, and the only recovery on offer was a per-tool
   * button, so the card was an unusable wall. */
  function catalog(n: number, profile = "default"): QuarantinedTool[] {
    return Array.from({ length: n }, (_, i) =>
      tool({
        tool: `clerk__call_api_key_${i}`,
        reason: "the integrity baseline was corrupt or tampered with",
        profile,
      }),
    );
  }

  it("clears a whole blocked catalog in one call per profile", async () => {
    listQuarantined.mockResolvedValueOnce(catalog(40));
    releaseAllQuarantine.mockResolvedValue({ released: 40, skipped: [] });
    listQuarantined.mockResolvedValue([]);

    render(<QuarantineAlert />);
    expect(
      await screen.findByRole("button", { name: /re-approve all \(40\)/i }),
    ).toBeInTheDocument();
    await confirmReapproveAll();

    // One call for the one profile, not one per tool.
    await waitFor(() => expect(releaseAllQuarantine).toHaveBeenCalledTimes(1));
    expect(releaseAllQuarantine).toHaveBeenCalledWith("default");
    expect(releaseQuarantine).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("region")).not.toBeInTheDocument());
    expect(toastSuccess).toHaveBeenCalledWith("Re-approved 40 tools");
  });

  it("clears every profile the card covers", async () => {
    // The same tool can be blocked in one profile and fine in another, and the store is
    // per-profile, so a single call would silently leave the other profile blocked.
    listQuarantined.mockResolvedValueOnce([
      ...catalog(2, "default"),
      ...catalog(2, "work"),
    ]);
    releaseAllQuarantine.mockResolvedValue({ released: 2, skipped: [] });
    listQuarantined.mockResolvedValue([]);

    render(<QuarantineAlert />);
    await screen.findByRole("region");
    await confirmReapproveAll();

    await waitFor(() => expect(releaseAllQuarantine).toHaveBeenCalledTimes(2));
    expect(releaseAllQuarantine).toHaveBeenCalledWith("default");
    expect(releaseAllQuarantine).toHaveBeenCalledWith("work");
  });

  it("does not release anything until the confirm is accepted", async () => {
    // Bulk-trusting definitions is a security decision, so it gets the same gate as
    // every other action that changes what clients may call.
    listQuarantined.mockResolvedValue(catalog(3));

    render(<QuarantineAlert />);
    await userEvent.click(
      await screen.findByRole("button", { name: /re-approve all \(3\)/i }),
    );

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(releaseAllQuarantine).not.toHaveBeenCalled();
  });

  it("reports tools it could not repair as still blocked, not as success", async () => {
    // Saying "re-approved" when some tools are still hidden would send the user away
    // believing the catalog is whole.
    listQuarantined.mockResolvedValue(catalog(3));
    releaseAllQuarantine.mockResolvedValue({
      released: 1,
      skipped: ["clerk__call_api_key_1", "clerk__call_api_key_2"],
    });

    render(<QuarantineAlert />);
    await screen.findByRole("region");
    await confirmReapproveAll();

    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(toastError.mock.calls[0][0]).toMatch(
      /Re-approved 1\. 2 could not be repaired/,
    );
    expect(toastSuccess).not.toHaveBeenCalled();
    // Still blocked, so the card must stay up.
    expect(screen.getByRole("region")).toBeInTheDocument();
  });

  it("keeps the card up and reports a failed bulk re-approval", async () => {
    listQuarantined.mockResolvedValue(catalog(3));
    releaseAllQuarantine.mockRejectedValue(new Error("store locked"));

    render(<QuarantineAlert />);
    await screen.findByRole("region");
    await confirmReapproveAll();

    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(toastSuccess).not.toHaveBeenCalled();
    expect(screen.getByRole("region")).toBeInTheDocument();
  });

  it("still counts and refreshes the profiles that succeeded when one fails", async () => {
    // One locked store must not discard the other profile's result. With Promise.all
    // the whole batch was thrown away: the tools that really were released stayed on
    // the card, and the count never came down.
    listQuarantined.mockResolvedValueOnce([
      ...catalog(2, "default"),
      ...catalog(2, "work"),
    ]);
    releaseAllQuarantine
      .mockResolvedValueOnce({ released: 2, skipped: [] })
      .mockRejectedValueOnce(new Error("store locked"));
    listQuarantined.mockResolvedValue(catalog(2, "work"));

    render(<QuarantineAlert />);
    await screen.findByRole("region");
    const pollsBefore = listQuarantined.mock.calls.length;
    await confirmReapproveAll();

    await waitFor(() => expect(toastError).toHaveBeenCalled());
    // The successful profile is reported, not silently dropped.
    expect(toastError.mock.calls[0][0]).toMatch(/Re-approved 2\./);
    expect(toastError.mock.calls[0][0]).toMatch(/1 profile could not be re-approved/);
    expect(toastSuccess).not.toHaveBeenCalled();
    // The refresh still ran, so the card drops the tools that did come free.
    expect(listQuarantined.mock.calls.length).toBeGreaterThan(pollsBefore);
    expect(screen.getByRole("region")).toBeInTheDocument();
  });
});

describe("QuarantineAlert", () => {
  it("renders nothing when no tool is quarantined", async () => {
    listQuarantined.mockResolvedValue([]);
    render(<QuarantineAlert />);
    await waitFor(() => expect(listQuarantined).toHaveBeenCalled());
    expect(screen.queryByRole("region")).not.toBeInTheDocument();
  });

  it("surfaces the blocked tool and the reason it was blocked", async () => {
    // The reason is the whole point of the surface: it is what makes re-approving an
    // informed decision rather than a reflex, so it must be on screen, not behind a click.
    listQuarantined.mockResolvedValue([tool()]);
    render(<QuarantineAlert />);

    expect(await screen.findByRole("region")).toBeInTheDocument();
    expect(screen.getByText("linear__save_issue")).toBeInTheDocument();
    expect(
      screen.getByText("a destructive tool's definition changed"),
    ).toBeInTheDocument();
  });

  it("prefers the concrete annotation detail when present (SOU-305)", async () => {
    listQuarantined.mockResolvedValue([
      tool({
        reason: "a tool dropped a readOnly/destructive safety annotation",
        detail: "readOnlyHint: true → false",
      }),
    ]);
    render(<QuarantineAlert />);
    expect(await screen.findByText("readOnlyHint: true → false")).toBeInTheDocument();
    // Generic reason stays as secondary context under the concrete delta.
    expect(
      screen.getByText("a tool dropped a readOnly/destructive safety annotation"),
    ).toBeInTheDocument();
  });

  it("re-approves through the profile-scoped API and re-reads the list", async () => {
    // Empty profile is the no-profile store; the backend maps it to None. Passing the
    // wrong profile would silently release nothing.
    listQuarantined.mockResolvedValueOnce([tool({ profile: "work" })]);
    releaseQuarantine.mockResolvedValue(undefined);
    listQuarantined.mockResolvedValue([]);

    render(<QuarantineAlert />);
    // Exact match: the footer also offers a bulk "Re-approve all" button.
    await userEvent.click(await screen.findByRole("button", { name: /^re-approve$/i }));

    expect(releaseQuarantine).toHaveBeenCalledWith("work", "linear__save_issue");
    await waitFor(() => expect(screen.queryByRole("region")).not.toBeInTheDocument());
  });

  it("keeps the card up and reports the error when re-approval fails", async () => {
    // Failing closed matters here: silently dropping the card would read as "unblocked"
    // when the tool is still blocked.
    listQuarantined.mockResolvedValue([tool()]);
    releaseQuarantine.mockRejectedValue(new Error("locked"));

    render(<QuarantineAlert />);
    // Exact match: the footer also offers a bulk "Re-approve all" button.
    await userEvent.click(await screen.findByRole("button", { name: /^re-approve$/i }));

    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(screen.getByRole("region")).toBeInTheDocument();
  });

  it("stays hidden after dismissal, but reopens when a NEW tool is quarantined", async () => {
    // Dismissal is scoped to the set that was on screen. A blanket "dismissed" flag would
    // hide a later, unrelated quarantine, which is exactly the silent-failure mode this
    // surface exists to remove.
    listQuarantined.mockResolvedValue([tool()]);
    render(<QuarantineAlert />);

    await userEvent.click(await screen.findByRole("button", { name: /dismiss/i }));
    expect(screen.queryByRole("region")).not.toBeInTheDocument();

    listQuarantined.mockResolvedValue([tool(), tool({ tool: "linear__delete_issue" })]);
    // Longer than the 2s poll interval, since the reopen depends on the next poll landing.
    expect(await screen.findByRole("region", {}, { timeout: 4000 })).toBeInTheDocument();
    expect(screen.getByText("linear__delete_issue")).toBeInTheDocument();
  });

  it("reopens when the SAME tool is quarantined again after being released", async () => {
    // Regression for a CodeRabbit finding. Keyed on name alone, a tool that was
    // dismissed, later re-approved, then drifted AGAIN produced an identical signature
    // and stayed hidden behind the stale dismissal - silently suppressing a brand new
    // quarantine, the exact failure this surface exists to prevent. The entry's ts is
    // what makes the second quarantine distinguishable from the first.
    const first = tool({ ts: 1_000 });
    listQuarantined.mockResolvedValue([first]);
    render(<QuarantineAlert />);

    await userEvent.click(await screen.findByRole("button", { name: /dismiss/i }));
    expect(screen.queryByRole("region")).not.toBeInTheDocument();

    // Released elsewhere, then quarantined again: same tool, same profile, new event.
    listQuarantined.mockResolvedValue([tool({ ts: 2_000 })]);
    expect(await screen.findByRole("region", {}, { timeout: 4000 })).toBeInTheDocument();
  });

  it("keeps the current list when a poll fails instead of flashing all-clear", async () => {
    listQuarantined.mockResolvedValueOnce([tool()]);
    render(<QuarantineAlert />);
    expect(await screen.findByRole("region")).toBeInTheDocument();

    listQuarantined.mockRejectedValue(new Error("backend down"));
    // Wait for a poll to actually land, rather than sleeping past the interval and hoping one
    // did. The old fixed 2100ms sleep gave a 2000ms timer only 100ms of headroom, and a loaded
    // runner routinely slips further than that - in which case the failing poll never fired and
    // the assertion below passed without exercising the failure path at all. Waiting on the call
    // count makes it impossible to pass for that reason, and returns as soon as the poll lands.
    const pollsBefore = listQuarantined.mock.calls.length;
    await waitFor(
      () => expect(listQuarantined.mock.calls.length).toBeGreaterThan(pollsBefore),
      { timeout: 8000 },
    );
    expect(screen.getByRole("region")).toBeInTheDocument();
  });
});
