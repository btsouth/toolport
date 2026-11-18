import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ActivityView } from "./ActivityView";
import { setLanguage } from "@/lib/i18n";
import type { AuditEntry, SearchTrace } from "@/lib/types";
import type { SecurityEvent } from "@/lib/api";

let windowVisible = true;
vi.mock("@/lib/windowVisible", () => ({ useWindowVisible: () => windowVisible }));

const getActivityRevision = vi.fn();
const getActivityHealth = vi.fn();
const releaseActivityRevision = vi.fn();
const getAuditLog = vi.fn();
const getSearchTraces = vi.fn();
const getSecurityEvents = vi.fn();
const getActivitySafety = vi.fn();
const getToolIdentities = vi.fn();
const getInspectLog = vi.fn();
const getSavingsSummary = vi.fn();
const getAuditStats = vi.fn();

const clearActivityLogs = vi.fn();
const exportAuditToPath = vi.fn();
const releaseQuarantine = vi.fn();

vi.mock("@/lib/api", () => ({
  releaseQuarantine: (...a: unknown[]) => releaseQuarantine(...a),
  clearActivityLogs: (...a: unknown[]) => clearActivityLogs(...a),
  exportAuditToPath: (...a: unknown[]) => exportAuditToPath(...a),
  getActivityRevision: (...a: unknown[]) => getActivityRevision(...a),
  releaseActivityRevision: (...a: unknown[]) => releaseActivityRevision(...a),
  getActivityHealth: (...a: unknown[]) => getActivityHealth(...a),
  getAuditLog: (...a: unknown[]) => getAuditLog(...a),
  getAuditStats: (...a: unknown[]) => getAuditStats(...a),
  getInspectLog: (...a: unknown[]) => getInspectLog(...a),
  getSavingsSummary: (...a: unknown[]) => getSavingsSummary(...a),
  getSearchTraces: (...a: unknown[]) => getSearchTraces(...a),
  getSecurityEvents: (...a: unknown[]) => getSecurityEvents(...a),
  getActivitySafety: (...a: unknown[]) => getActivitySafety(...a),
  getToolIdentities: (...a: unknown[]) => getToolIdentities(...a),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn(), info: vi.fn() },
}));

vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

const save = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: (...a: unknown[]) => save(...a) }));

function entry(over: Partial<AuditEntry> = {}): AuditEntry {
  return {
    ts: 1700000000000,
    server: "github",
    tool: "create_issue",
    ok: true,
    durationMs: 120,
    ...over,
  };
}

const failed = entry({
  ts: 1700000001000,
  tool: "merge_pr",
  ok: false,
  error: "403: token lacks repo scope",
});
const initialLog = [failed, entry()];
// Same list with a fresh call prepended, as the 3s live tick would refetch it.
const refreshedLog = [entry({ ts: 1700000002000, tool: "list_issues" }), ...initialLog];

beforeEach(() => {
  releaseActivityRevision.mockResolvedValue(undefined);
  let revision = 0;
  getActivityRevision.mockImplementation(async () => String(revision++));
  getActivityHealth.mockResolvedValue({
    telemetry: {
      queueDropped: 0,
      writeFailedRecords: 0,
      writeFailures: 0,
      incompleteFlushes: 0,
    },
    gatewayNotes: [],
  });
  getAuditStats.mockResolvedValue(null);
  windowVisible = true;
  vi.useFakeTimers({ shouldAdvanceTime: true });
  getAuditLog.mockResolvedValue(initialLog);
  getSearchTraces.mockResolvedValue([]);
  getSecurityEvents.mockResolvedValue([]);
  getActivitySafety.mockResolvedValue({
    toolChangeReviews: true,
    blocksNothing: false,
    reviewsConfirmed: true,
  });
  getToolIdentities.mockResolvedValue([]);
  getInspectLog.mockResolvedValue([]);
  getSavingsSummary.mockResolvedValue(null);
});

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

it("p10c shows approval outcomes without treating them as call errors", async () => {
  const outcomes = [
    ["denied", "Denied"],
    ["no_response", "No answer"],
    ["withdrawn", "Withdrawn"],
    ["stale_state", "Changed after approval"],
    ["approved", "Approved"],
    ["unreachable", "No approver available"],
  ];
  getAuditLog.mockResolvedValue([
    ...outcomes.map(([decision], index) => ({
      ...entry({ ts: 1700000010000 + index, tool: `approval_${index}`, ok: false }),
      kind: "approval",
      decision,
      heldMs: 90000,
      durationMs: undefined,
    })),
    ...initialLog,
  ]);
  getAuditStats.mockResolvedValue({ total: 2, errors: 1, errorRate: 0.5, servers: [] });
  render(<ActivityView refreshKey={0} registry={null} />);
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(screen.getByText("calls recorded").parentElement).toHaveTextContent(
    /2\s*calls recorded/,
  );
  expect(screen.getByText("errors (50%)").parentElement).toHaveTextContent(/1\s*errors/);
  for (const [, label] of outcomes) expect(screen.getByText(label)).toBeInTheDocument();
  // Approvals are rows but not calls, so the shown rows are counted as entries,
  // and 2 calls are not set beside more entries.
  expect(
    screen.getByText(
      `Showing the latest ${outcomes.length + initialLog.length} entries.`,
    ),
  ).toBeInTheDocument();
  expect(screen.queryByText(/calls recorded on this computer/)).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: /errors only/i }));
  expect(screen.getByText("merge_pr")).toBeInTheDocument();
  for (const [, label] of outcomes)
    expect(screen.queryByText(label)).not.toBeInTheDocument();
  expect(
    screen.getByText(
      `Showing 1 matching entry from the latest ${outcomes.length + initialLog.length}.`,
    ),
  ).toBeInTheDocument();
});

it("counts Activity rows as entries in singular and plural", async () => {
  getAuditLog.mockResolvedValue([entry(), entry({ ts: 1700000003000 })]);
  getAuditStats.mockResolvedValue({ total: 2, errors: 0, errorRate: 0, servers: [] });
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  // The list starts open; closed, the line says how many entries it holds.
  const toggle = await screen.findByRole("button", {
    name: /recent calls/i,
    expanded: true,
  });
  expect(screen.getByText("Showing the latest 2 entries.")).toBeInTheDocument();
  await user.click(toggle);
  expect(screen.getByText("Latest 2 entries available.")).toBeInTheDocument();
  await user.click(toggle);
  await user.click(screen.getByRole("button", { name: /errors only/i }));
  expect(
    screen.getByText("Showing 0 matching entries from the latest 2."),
  ).toBeInTheDocument();
  expect(screen.getByText("No errors in the latest 2 entries.")).toBeInTheDocument();
  view.unmount();

  getAuditLog.mockResolvedValue([entry()]);
  getAuditStats.mockResolvedValue({ total: 1, errors: 0, errorRate: 0, servers: [] });
  render(<ActivityView refreshKey={0} registry={null} />);
  const single = await screen.findByRole("button", { name: /recent calls/i });
  expect(screen.getByText("Showing the latest entry.")).toBeInTheDocument();
  await user.click(single);
  expect(screen.getByText("Latest entry available.")).toBeInTheDocument();
  await user.click(single);
  await user.click(screen.getByRole("button", { name: /errors only/i }));
  expect(screen.getByText("No errors in the latest entry.")).toBeInTheDocument();
});

it("p10c shows an approval-only history even when no tools ran", async () => {
  getAuditLog.mockResolvedValue([
    { ...entry(), kind: "approval", decision: "withdrawn" },
  ]);
  getAuditStats.mockResolvedValue({ total: 0, errors: 0, errorRate: 0, servers: [] });
  render(<ActivityView refreshKey={0} registry={null} />);
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(screen.getByText("Withdrawn")).toBeInTheDocument();
  expect(screen.queryByText("No activity yet")).not.toBeInTheDocument();
  expect(screen.queryByText("calls recorded")).not.toBeInTheDocument();
});

it("pauses Activity polling while hidden and resumes when visible", async () => {
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  const loaded = getAuditLog.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(3000));
  expect(getAuditLog).toHaveBeenCalledTimes(loaded + 1);
  windowVisible = false;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  expect(releaseActivityRevision).toHaveBeenCalled();
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(loaded + 1);
  windowVisible = true;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(3000));
  expect(getAuditLog).toHaveBeenCalledTimes(loaded + 3);
});

it("keeps an idle Activity page unchanged and reads new rows when the revision changes", async () => {
  getActivityRevision.mockResolvedValue("same");
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  const calls = getAuditLog.mock.calls.length;
  const stats = getAuditStats.mock.calls.length;
  const security = getSecurityEvents.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(calls);
  expect(getAuditStats).toHaveBeenCalledTimes(stats);
  expect(getSecurityEvents).toHaveBeenCalledTimes(security);
  expect(getSearchTraces).toHaveBeenCalledTimes(2);
  getActivityRevision.mockResolvedValue("changed");
  getAuditLog.mockResolvedValue(refreshedLog);
  await act(() => vi.advanceTimersByTimeAsync(3000));
  expect(getAuditLog).toHaveBeenCalledTimes(calls + 1);
  view.unmount();
  const revisions = getActivityRevision.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(6000));
  expect(getActivityRevision).toHaveBeenCalledTimes(revisions);
});

it("refreshes live health without rereading unchanged stores and preserves retained drops", async () => {
  getActivityRevision.mockResolvedValue("same");
  getAuditStats.mockResolvedValue({
    total: 0,
    errors: 0,
    errorRate: 0,
    servers: [],
    telemetry: {
      queueDropped: 0,
      writeFailedRecords: 0,
      writeFailures: 0,
      incompleteFlushes: 0,
      retainedDropped: 7,
    },
  });
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  const calls = getAuditLog.mock.calls.length;
  const stats = getAuditStats.mock.calls.length;
  getActivityHealth.mockResolvedValue({
    telemetry: {
      queueDropped: 3,
      writeFailedRecords: 2,
      writeFailures: 1,
      incompleteFlushes: 0,
    },
    gatewayNotes: [],
  });
  await act(() => vi.advanceTimersByTimeAsync(15_000));
  expect(screen.getByText(/3 records dropped/)).toBeInTheDocument();
  expect(
    screen.getByText(/7 dropped telemetry records are recorded/),
  ).toBeInTheDocument();
  getActivityHealth.mockRejectedValue(new Error("unreachable"));
  await act(() => vi.advanceTimersByTimeAsync(15_000));
  expect(screen.getByText(/Gateway telemetry health is unavailable/)).toBeInTheDocument();
  expect(screen.getByText(/3 records dropped/)).toBeInTheDocument();
  getActivityHealth.mockResolvedValue({
    telemetry: {
      queueDropped: 0,
      writeFailedRecords: 0,
      writeFailures: 0,
      incompleteFlushes: 0,
    },
    gatewayNotes: [],
  });
  await act(() => vi.advanceTimersByTimeAsync(15_000));
  expect(
    screen.queryByText(/Gateway telemetry health is unavailable/),
  ).not.toBeInTheDocument();
  expect(screen.queryByText(/3 records dropped/)).not.toBeInTheDocument();
  expect(getAuditLog).toHaveBeenCalledTimes(calls);
  expect(getAuditStats).toHaveBeenCalledTimes(stats);
  view.unmount();
  const probes = getActivityHealth.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(30_000));
  expect(getActivityHealth).toHaveBeenCalledTimes(probes);
});

it("keeps health probes single-flight and rejects a completion after hiding", async () => {
  getActivityRevision.mockResolvedValue("same");
  let finish!: (value: unknown) => void;
  getActivityHealth.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(45_000));
  expect(getActivityHealth).toHaveBeenCalledTimes(1);
  windowVisible = false;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(30_000));
  expect(getActivityHealth).toHaveBeenCalledTimes(1);
  await act(async () => {
    finish({ telemetry: { unavailable: true }, gatewayNotes: [] });
  });
  expect(
    screen.queryByText(/Gateway telemetry health is unavailable/),
  ).not.toBeInTheDocument();
  getActivityHealth.mockResolvedValue({
    telemetry: { unavailable: true },
    gatewayNotes: [],
  });
  windowVisible = true;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(15_000));
  expect(getActivityHealth).toHaveBeenCalledTimes(2);
  expect(screen.getByText(/Gateway telemetry health is unavailable/)).toBeInTheDocument();
  view.unmount();
});

describe("ActivityView trust-state loading", () => {
  it("shows initial loading without claiming protection is clear", () => {
    getAuditLog.mockReturnValue(new Promise(() => {}));
    getSecurityEvents.mockReturnValue(new Promise(() => {}));

    render(<ActivityView refreshKey={0} registry={null} />);

    expect(screen.getByRole("status", { name: "Loading activity…" })).toBeInTheDocument();
    expect(screen.getByText("Checking protection status…")).toBeInTheDocument();
    expect(screen.queryByText("Protection active.")).not.toBeInTheDocument();
  });

  it("treats a successful empty read as verified empty", async () => {
    getAuditLog.mockResolvedValue([]);
    getSecurityEvents.mockResolvedValue([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("No activity yet")).toBeInTheDocument();
    expect(screen.getByText("Protection active.")).toBeInTheDocument();
  });

  it("shows an unknown security state with retry after the initial read fails", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getAuditLog.mockResolvedValue([]);
    getSecurityEvents
      .mockRejectedValueOnce(new Error("unreadable"))
      .mockRejectedValueOnce(new Error("unreadable"))
      .mockResolvedValueOnce([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't verify protection status.")).toBeInTheDocument();
    expect(screen.getByText(/this is not an all-clear/i)).toBeInTheDocument();
    expect(screen.queryByText("Protection active.")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Retry protection status" }));
    await act(async () => {});

    expect(screen.getByText("Protection active.")).toBeInTheDocument();
    expect(
      screen.queryByText("Couldn't verify protection status."),
    ).not.toBeInTheDocument();
  });

  it("preserves last-known security findings when a live refresh fails", async () => {
    const finding = {
      ts: 1700000000000,
      type: "tool_poison_flag",
      server: "github",
      tool: "github__create_issue",
      change: "poison",
      severity: "high" as const,
    };
    getSecurityEvents
      .mockResolvedValueOnce([finding])
      .mockResolvedValueOnce([finding])
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("github__create_issue")).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    expect(screen.getByText("Security status may be out of date.")).toBeInTheDocument();
    expect(screen.getByText("github__create_issue")).toBeInTheDocument();
    expect(screen.queryByText("Protection active.")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Retry protection status" }),
    ).toBeInTheDocument();
  });

  it("does not restore cleared calls when the post-clear refetch fails", async () => {
    const { toast } = await import("sonner");
    clearActivityLogs.mockResolvedValue(undefined);
    getAuditLog
      .mockResolvedValueOnce(initialLog)
      .mockResolvedValueOnce(initialLog)
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("latest 2")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    fireEvent.click(screen.getByRole("button", { name: "Clear activity" }));
    await act(async () => {});

    expect(toast.success).toHaveBeenCalledWith("Cleared retained activity");
    expect(screen.queryByText("latest 2")).not.toBeInTheDocument();
    expect(
      screen.getByText(/can't verify that the log is still empty/i),
    ).toBeInTheDocument();
  });

  it("ignores an audit read that started before the clear and resolved after it", async () => {
    const { toast } = await import("sonner");
    clearActivityLogs.mockResolvedValue(undefined);
    let resolveStale!: (rows: AuditEntry[]) => void;
    getAuditLog
      .mockResolvedValueOnce(initialLog)
      .mockResolvedValueOnce(initialLog)
      .mockReturnValueOnce(
        new Promise<AuditEntry[]>((res) => {
          resolveStale = res;
        }),
      )
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("latest 2")).toBeInTheDocument();

    // A live tick starts a refetch that is still in flight when the user clears.
    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    fireEvent.click(screen.getByRole("button", { name: "Clear activity" }));
    // The pre-clear read resolves with the deleted rows in the same flush as the
    // clear finishing, before its effect's cleanup has run.
    resolveStale(initialLog);
    await act(async () => {});

    expect(toast.success).toHaveBeenCalledWith("Cleared retained activity");
    expect(screen.queryByText("latest 2")).not.toBeInTheDocument();
    expect(
      screen.getByText(/can't verify that the log is still empty/i),
    ).toBeInTheDocument();
  });

  it("does not turn a last-known empty audit log into a current all-clear", async () => {
    getAuditLog
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([])
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("No activity yet")).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    expect(screen.queryByText("Activity may be out of date.")).not.toBeInTheDocument();
    expect(screen.getByText("No current activity status")).toBeInTheDocument();
    expect(screen.queryByText("No activity yet")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Retry activity log" }),
    ).toBeInTheDocument();
  });

  it("preserves last-known calls and offers retry when a live refresh fails", async () => {
    getAuditLog
      .mockResolvedValueOnce(initialLog)
      .mockResolvedValueOnce(initialLog)
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });

    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    expect(screen.getByText("Activity may be out of date.")).toBeInTheDocument();
    expect(screen.getByText("merge_pr")).toBeInTheDocument();
    expect(screen.queryByText("Couldn't load activity")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Retry activity log" }),
    ).toBeInTheDocument();
  });

  it("shows an initial audit error with retry instead of a false empty log", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    const cause = "Couldn't read the activity log: Is a directory (os error 21)";
    getAuditLog
      .mockRejectedValueOnce(cause)
      .mockRejectedValueOnce(cause)
      .mockResolvedValueOnce([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't load activity")).toBeInTheDocument();
    // The real cause, as GTK shows it, not a fixed sentence.
    expect(screen.getByText(cause)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "View log" })).toBeInTheDocument();
    expect(screen.queryByText("No activity yet")).not.toBeInTheDocument();
    // Protection still shows its real state while the log is unreadable.
    expect(screen.getByText("Protection active.")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Retry activity log" }));
    await act(async () => {});
    expect(screen.getByText("No activity yet")).toBeInTheDocument();
  });
});

describe("ActivityView recent calls", () => {
  it("keeps catalog timing in tooltips and distinguishes internal lookups", async () => {
    getAuditStats.mockResolvedValue({
      total: 5225,
      errors: 0,
      errorRate: 0,
      servers: [],
    });
    getAuditLog.mockResolvedValue([
      entry({
        kind: "internal",
        server: "toolport",
        tool: "search",
        cold: false,
        dispatchMs: 3,
        piiReplaced: 1,
      }),
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });
    expect(screen.getByText("Searched tools")).toBeInTheDocument();
    expect(screen.queryByText(/Nothing searched yet/)).not.toBeInTheDocument();
    expect(screen.queryByText(/warm catalog|dispatch 3/)).not.toBeInTheDocument();
    expect(screen.getByText("Unrecorded client").parentElement).toHaveAttribute(
      "title",
      expect.stringContaining("dispatch 3 ms"),
    );
    expect(screen.getByText("1 value masked")).toHaveAttribute(
      "title",
      expect.stringContaining("before reaching the model"),
    );
    expect(screen.getByText(/5,225 calls recorded/)).toHaveTextContent(
      "Showing the latest entry.",
    );
    expect(screen.getByRole("button", { name: "Export" })).toHaveAttribute("title");
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByText("Clear retained activity?")).toBeInTheDocument();
    expect(clearActivityLogs).not.toHaveBeenCalled();
  });
  it("expands a typed Code Mode failure without retaining error text", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    const runId = "0123456789abcdef0123456789abcdef";
    getAuditLog.mockResolvedValue([
      entry({
        server: "toolport",
        tool: "run_script",
        ok: false,
        failureKind: "script_exception",
        runId,
      }),
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });
    expect(screen.queryByText(`Run: ${runId}`)).not.toBeInTheDocument();
    await user.click(screen.getByText("run_script"));
    expect(screen.getByText("script exception", { exact: true })).toBeInTheDocument();
    expect(screen.getByText(`Run: ${runId}`)).toBeInTheDocument();
  });

  it("gives each call row one status color, on its chip", async () => {
    const { container } = render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });
    const list = container.querySelector(".toolport-list")!;
    const chips = [...list.querySelectorAll('[data-slot="badge"]')];
    expect(chips.map((chip) => chip.textContent)).toEqual(["Failed", "Succeeded"]);
    expect(chips.map((chip) => chip.getAttribute("data-variant"))).toEqual([
      "destructive",
      "success",
    ]);
    // The row icons say the call kind and carry no status color.
    expect(list.querySelector("svg.text-success, svg.text-destructive")).toBeNull();
  });

  it("keeps an expanded error row open across a live-poll refetch", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    render(<ActivityView refreshKey={0} registry={null} />);

    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });

    // Expand the failed call's error detail.
    await user.click(screen.getByText("merge_pr"));
    expect(screen.getByText("403: token lacks repo scope")).toBeInTheDocument();

    // Next poll returns the same entries with a new call prepended.
    getAuditLog.mockResolvedValue(refreshedLog);
    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    expect(screen.getByText("list_issues")).toBeInTheDocument();
    expect(screen.getByText("403: token lacks repo scope")).toBeInTheDocument();
  });

  it("shows the pseudonymization count, and flags a pass that did not fully apply", async () => {
    getAuditLog.mockResolvedValue([
      entry({ tool: "redacted_call", piiReplaced: 3 }),
      entry({
        ts: 1700000003000,
        tool: "leaky_call",
        piiReplaced: 2,
        piiIncomplete: true,
      }),
      entry({ ts: 1700000004000, tool: "matched_nothing", piiReplaced: 0 }),
      entry({ ts: 1700000005000, tool: "redaction_off" }),
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);

    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });

    expect(screen.getByText("3 values masked")).toBeInTheDocument();

    // The fail-open case has to read as a warning, not as a tidy count: values reached
    // the model in the clear even though redaction was on.
    const incomplete = screen.getByText("2 values masked, incomplete");
    expect(incomplete).toBeInTheDocument();
    expect(incomplete).toHaveAttribute(
      "title",
      expect.stringContaining("did not fully apply"),
    );

    // A pass that matched nothing, and a call made with redaction off, both stay silent —
    // a badge on every row would bury the two cases above.
    expect(screen.queryByText(/0 values masked/)).not.toBeInTheDocument();
    expect(screen.getAllByText(/values masked/)).toHaveLength(2);

    // The values are the point of the feature and must never reach this view.
    expect(document.body.textContent).not.toMatch(/@example\.com/);
  });
});

describe("ActivityView discovery", () => {
  it("shows an error with retry instead of a false empty state when discovery traces fail to load (#728)", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getSearchTraces
      .mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't load discovery.")).toBeInTheDocument();
    expect(screen.queryByText(/Nothing searched yet/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Retry loading discovery" }));
    await act(async () => {});
    expect(screen.getByText(/Nothing searched yet/)).toBeInTheDocument();
  });

  it("labels legacy discovery figures as schema-only estimates", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    const trace: SearchTrace = {
      ts: 1700000000000,
      query: "tiny savings",
      top: "github.search",
      names: ["github.search"],
      returned: 1,
      total: 20,
      returnedTokens: 1999,
      flatTokens: 2000,
      savedTokens: 1,
      escalated: false,
    };
    getSearchTraces.mockResolvedValue([trace]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    await user.click(screen.getByRole("button", { name: /Discovery/ }));
    const row = screen.getByRole("button", { name: /tiny savings/i });
    await user.click(row);

    expect(row.parentElement).toHaveTextContent(/Legacy schema-only estimates/);
    expect(row.parentElement).toHaveTextContent(/Search guidance text was not counted/);
  });

  it("shows measured search content bytes separately from schemas", async () => {
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    getSearchTraces.mockResolvedValue([
      {
        ts: 1700000000000,
        query: "charges",
        top: "stripe__list",
        names: ["stripe__list"],
        returned: 1,
        total: 1,
        returnedTokens: 50,
        flatTokens: 500,
        savedTokens: 450,
        escalated: false,
        responseContentBytes: 2450,
        matchedSchemaBytes: 200,
        catalogSchemaBytes: 5000,
        estimatedResponseTokens: 613,
        estimateMethod: "utf8_bytes_div_4",
      } satisfies SearchTrace,
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: /Discovery/ }));
    const row = screen.getByRole("button", { name: /charges/i });
    await user.click(row);
    expect(row.parentElement).toHaveTextContent(/Returned 2\.5 KB of discovery content/);
    expect(row.parentElement).toHaveTextContent(/containing 1 matching schema/);
    expect(row.parentElement).toHaveTextContent(/UTF-8 bytes ÷ 4/);
  });
});

it("distinguishes measured bytes from legacy estimates in catalog savings", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 923,
    fullTokens: 1_000,
    exposedTokens: 75,
    discoveryTokens: 2,
    listLoads: 2751,
    peakCatalog: 1725,
    sinceTs: 1700000000000,
    legacyEstimatedTokensAvoided: 3_692_944_000,
    measuredLoads: 1,
    tokenizedLoads: 1,
    estimatedTokensAvoided: 2000,
    latestCatalogTs: 1700000000001,
    latestFullToolCount: 1725,
    latestExposedToolCount: 7,
    latestFullSurfaceBytes: 8_000,
    latestExposedSurfaceBytes: 1_000,
    fullSurfaceBytes: 8_000,
    exposedSurfaceBytes: 1_000,
    avoidedSurfaceBytes: 7_000,
    discoveryCount: 2,
    discoveryResponseBytes: 2_450,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText(/923/, { selector: "span[title]" })).toHaveTextContent(
    "tokens saved",
  );
  expect(screen.getByText(/923/, { selector: "span[title]" })).toHaveAttribute(
    "title",
    expect.stringContaining("cl100k_base"),
  );
  expect(screen.getByText("How this is measured").parentElement).not.toHaveAttribute(
    "open",
  );
  expect(screen.getByText(/8\.0 KB baseline catalog text/)).toBeInTheDocument();
  expect(screen.queryByText(/Latest load/)).not.toBeInTheDocument();
  expect(screen.getByText(/Older estimated records/)).toBeInTheDocument();
  expect(screen.getByText(/2\.5 KB search text sent/)).toBeInTheDocument();
});

it("does not promote historical estimates into a catalog token headline", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    listLoads: 2751,
    peakCatalog: 1725,
    sinceTs: 1700000000000,
    legacyEstimatedTokensAvoided: 52_800_000,
    tokenizedLoads: 0,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.queryByText("Net tool description savings")).not.toBeInTheDocument();
  expect(screen.queryByText(/52.8M/)).not.toBeInTheDocument();
});

it("keeps contradictory pre-session history outside all measured totals", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: -10,
    fullTokens: 100,
    exposedTokens: 50,
    discoveryTokens: 60,
    tokenizedLoads: 1,
    measuredLoads: 1,
    listLoads: 2,
    discoveryCount: 1,
    peakCatalog: 4,
    sinceTs: 1,
    fullSurfaceBytes: 1200,
    exposedSurfaceBytes: 2400,
    historicalListLoads: 3019,
    historicalMeasuredLoads: 3019,
    previousReportedTokensSaved: 188_100_000,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText(/100 baseline tokens/)).toHaveTextContent(
    "100 baseline tokens − 50 catalog response tokens − 60 search response tokens = -10 net token change. A negative change means responses used more tokens than the baseline.",
  );
  const net = screen.getByText(/-10/, { selector: "span.text-3xl" });
  expect(net).toHaveTextContent("-10 net token change");
  // A net cost is not drawn as a saving: no green.
  expect(net).not.toHaveClass("text-success");
  expect(net.closest(".toolport-alert")).not.toHaveClass("border-success");
  expect(screen.queryByText("net tokens saved")).not.toBeInTheDocument();
  expect(screen.getByText(/1 client sessions/)).toHaveTextContent("2 catalog responses");
  expect(screen.getByText(/1.2 KB baseline catalog text/)).toHaveTextContent(
    "2.4 KB catalog text sent",
  );
  expect(screen.getByText(/Excluded history:/)).toHaveTextContent(
    "3,019 catalog records",
  );
  expect(screen.getByText(/Old estimate:/)).toHaveTextContent(
    "Old estimate: 188,100,000 tokens",
  );
  expect(screen.queryByText(/18.4 GB/)).not.toBeInTheDocument();
});

it("shows preserved history without a shareable savings headline", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    listLoads: 0,
    peakCatalog: 0,
    sinceTs: 0,
    historicalListLoads: 3019,
    historicalMeasuredLoads: 3019,
    previousReportedTokensSaved: 195_640_064,
    legacyEstimatedTokensAvoided: 3_861_119_161,
    estimatedTokensAvoided: 4_387_380_774,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText("Not measured yet")).toBeInTheDocument();
  expect(screen.getByText(/Old estimate:/)).toBeVisible();
  expect(screen.getByText(/Old estimate:/)).toHaveTextContent("195,640,064 tokens");
  expect(screen.queryByRole("button", { name: "Share" })).not.toBeInTheDocument();
});

it("labels zero measured savings as savings, not as a change", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    fullTokens: 50,
    exposedTokens: 40,
    discoveryTokens: 10,
    tokenizedLoads: 1,
    listLoads: 1,
    peakCatalog: 1,
    sinceTs: 1,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText(/^0/, { selector: "span.text-3xl" })).toHaveTextContent(
    "0 net tokens saved",
  );
  expect(screen.getByText(/50 baseline tokens/)).toHaveTextContent(
    "= 0 net tokens saved.",
  );
  expect(screen.queryByText(/net token change/)).not.toBeInTheDocument();
});

it("preserves a negative old estimate separately from new measured savings", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 42,
    tokenizedLoads: 1,
    listLoads: 1,
    peakCatalog: 1,
    sinceTs: 1,
    previousReportedTokensSaved: -291,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText(/Old estimate:/)).toBeVisible();
  expect(screen.getByText(/Old estimate:/)).toHaveTextContent("-291 tokens");
  expect(screen.getByText(/42/, { selector: "span.text-3xl" })).toHaveTextContent(
    "net tokens saved",
  );
  expect(
    screen.getByText("From previous versions; excluded from new savings."),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Share" })).toBeInTheDocument();
});

it("shares a token savings statement without a billing claim", async () => {
  const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  getSavingsSummary.mockResolvedValue({
    tokensSaved: -123,
    previousReportedTokensSaved: 195_640_064,
    tokenizedLoads: 1,
    listLoads: 2751,
    peakCatalog: 1725,
    sinceTs: 1700000000000,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  await user.click(screen.getByRole("button", { name: "Share" }));
  expect(writeText).toHaveBeenCalledWith(
    expect.stringContaining("a net token change of -123 across 1 client sessions"),
  );
  expect(writeText.mock.calls[0][0]).not.toContain("tokens saved");
  expect(writeText.mock.calls[0][0]).toContain("not a billing figure");
  expect(writeText.mock.calls[0][0]).toContain("cl100k_base");
  expect(writeText.mock.calls[0][0]).not.toMatch(
    /billed tokens|money saved|195|Old estimate/i,
  );
});

it("shows discovery bytes without a catalog load or a zero-token savings claim", async () => {
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    listLoads: 0,
    peakCatalog: 0,
    sinceTs: 1700000000000,
    measuredLoads: 0,
    discoveryCount: 3,
    discoveryResponseBytes: 12_340,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText("Discovery payload returned")).toBeInTheDocument();
  expect(screen.getByText(/3 search responses/)).toBeInTheDocument();
  expect(screen.queryByText(/0 tool-list loads/)).not.toBeInTheDocument();
  expect(screen.queryByText(/tokens saved/)).not.toBeInTheDocument();
});

it("reports unavailable catalog telemetry without showing an empty measurement", async () => {
  getSavingsSummary.mockRejectedValue(new Error("corrupt savings store"));
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByRole("alert")).toHaveTextContent("Catalog telemetry unavailable");
  expect(screen.queryByText("Net tool description savings")).not.toBeInTheDocument();
});

it("keeps last-loaded catalog telemetry visibly stale after a failed refresh", async () => {
  getSavingsSummary
    .mockResolvedValueOnce({
      tokensSaved: 100,
      listLoads: 1,
      tokenizedLoads: 1,
      peakCatalog: 3,
      sinceTs: 1700000000000,
    })
    .mockResolvedValueOnce({
      tokensSaved: 100,
      listLoads: 1,
      tokenizedLoads: 1,
      peakCatalog: 3,
      sinceTs: 1700000000000,
    })
    .mockRejectedValueOnce(new Error("unreadable"));
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  view.rerender(<ActivityView refreshKey={1} registry={null} />);
  await act(async () => {});
  expect(screen.getByText("Net tool description savings")).toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("last loaded measurements");
});

describe("ActivityView tool identities", () => {
  it("shows an error with retry instead of hiding the panel when identities fail to load (#728)", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getToolIdentities
      .mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't load tool identities.")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Retry loading tool identities" }),
    );
    await act(async () => {});
    expect(screen.queryByText("Couldn't load tool identities.")).not.toBeInTheDocument();
  });
});

describe("ActivityView security drift dismissals", () => {
  function warnEvent(ts: number): SecurityEvent {
    return {
      ts,
      type: "tool_drift",
      server: "srv",
      tool: "srv__read",
      change: "changed",
      severity: "info",
      blocked: false,
    };
  }

  it.each(["toolport.security.dismissed", "conduit.security.dismissed"])(
    "preserves v1 reviews from %s without hiding current or blocked definitions",
    async (storageKey) => {
      localStorage.clear();
      localStorage.setItem(
        storageKey,
        JSON.stringify(["tool_drift:srv:srv__read:changed:info"]),
      );
      getSecurityEvents.mockResolvedValue([warnEvent(1)]);
      render(<ActivityView refreshKey={0} registry={null} />);
      await act(async () => {});
      expect(screen.queryByText("Tool changes")).not.toBeInTheDocument();
      for (const change of [{ blocked: null }, { blocked: true }, { new_fp: "v3:new" }]) {
        getSecurityEvents.mockResolvedValue([{ ...warnEvent(2), ...change }]);
        await act(() => vi.advanceTimersByTimeAsync(3000));
        expect(
          screen.getByRole("button", { name: /srv: 1 tool changed/ }),
        ).toBeInTheDocument();
      }
      expect(JSON.parse(localStorage.getItem(storageKey)!)).toEqual([
        "tool_drift:srv:srv__read:changed:info",
      ]);
    },
  );

  it("shows server changes and re-surfaces a later rewrite after acceptance", async () => {
    localStorage.clear();
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    getSecurityEvents.mockResolvedValue([warnEvent(1_700_000_000_000)]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    // A description rewrite stays visible as a server update.
    expect(screen.getByText("Tool changes")).toBeInTheDocument();

    // Accept just this tool.
    await user.click(screen.getByRole("button", { name: /srv: 1 tool changed/ }));
    await user.click(screen.getByText("Read"));
    await user.click(screen.getByRole("button", { name: "Accept this tool" }));
    await act(async () => {});
    expect(screen.queryByText("Read")).not.toBeInTheDocument();

    // A later, different rewrite of the SAME tool must reappear: an accepted change is
    // per instance, not per tool identity.
    getSecurityEvents.mockResolvedValue([warnEvent(1_700_000_000_000 + 20 * 60 * 1000)]);
    await act(async () => {
      vi.advanceTimersByTime(3000);
    });
    expect(
      screen.getByRole("button", { name: /srv: 1 tool changed/ }),
    ).toBeInTheDocument();
  });
  it("accepts one profile while the same finding stays visible and blocked in another", async () => {
    localStorage.clear();
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    const blocked = new Set(["personal", "work"]);
    const events = ["personal", "work"].map((profile) => ({
      ...warnEvent(1_700_000_000_000),
      profile,
      new_fp: "v2:same-definition",
      blocked: true,
      blocked_profiles: [profile],
    }));
    getSecurityEvents.mockImplementation(async () =>
      events.map((event) => ({
        ...event,
        blocked: blocked.has(event.profile),
        blocked_profiles: blocked.has(event.profile) ? [event.profile] : [],
      })),
    );
    releaseQuarantine.mockImplementation(async (profile: string) =>
      blocked.delete(profile),
    );
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getAllByRole("button", { name: /srv: 1 tool changed/ })).toHaveLength(
      2,
    );
    await user.click(
      screen.getAllByRole("button", { name: "Accept all for this server" })[0],
    );
    await act(async () => {});
    expect(releaseQuarantine).toHaveBeenCalledExactlyOnceWith(
      "personal",
      "srv__read",
      "v2:same-definition",
    );
    expect(blocked).toEqual(new Set(["work"]));
    expect(screen.getAllByRole("button", { name: /srv: 1 tool changed/ })).toHaveLength(
      1,
    );
    expect(screen.getByText(/1 tool is blocked/)).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(
      screen.getByRole("button", { name: /srv: 1 tool changed/ }),
    ).toBeInTheDocument();
    expect(screen.getByText(/1 tool is blocked/)).toBeInTheDocument();
  });

  it("keeps older fingerprint-less records in one expandable history summary", async () => {
    localStorage.clear();
    getSecurityEvents.mockResolvedValue([
      {
        ...warnEvent(1_700_000_000_000),
        tool: "cloudflare__update",
        server: "cloudflare",
      },
      ...Array.from({ length: 1500 }, (_, i) => ({
        ...warnEvent(1_600_000_000_000),
        tool: `old__read${i}`,
        server: "old",
        historical: true,
      })),
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText(/1500 older tool-change records/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /cloudflare: 1 tool changed/ }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/1500 older tool-change records/).closest("details"),
    ).not.toHaveAttribute("open");
  });

  it.each([undefined, "work"])(
    "keeps an older unknown quarantine in the main view with profile %s",
    async (profile) => {
      localStorage.clear();
      getSecurityEvents.mockResolvedValue([
        ...Array.from({ length: 55 }, (_, i) => ({
          ...warnEvent(1_700_000_000_000),
          tool: `old__read${i}`,
          server: "old",
          historical: i >= 50,
        })),
        {
          ...warnEvent(1_600_000_000_000),
          tool: "legacy__update",
          server: "legacy",
          profile,
          blocked: null,
        },
      ]);
      render(<ActivityView refreshKey={0} registry={null} />);
      await act(async () => {});
      const row = screen.getByRole("button", { name: /legacy: 1 tool changed/ });
      expect(row.closest("details")).toBeNull();
      expect(screen.getByText(/5 older tool-change records/)).toBeInTheDocument();
      expect(screen.getByText(/Settings > Quarantined tools/)).toBeInTheDocument();
      const card = within(row.parentElement!);
      expect(
        card.queryByRole("button", { name: "Accept all for this server" }),
      ).not.toBeInTheDocument();
      fireEvent.click(row);
      const accept = card.getByRole("button", { name: "Accept this tool" });
      expect(accept).toBeDisabled();
      fireEvent.click(accept);
      expect(releaseQuarantine).not.toHaveBeenCalled();
    },
  );

  it("accepts a server update larger than the old dismissal limit", async () => {
    localStorage.clear();
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    const events = Array.from({ length: 600 }, (_, i) => ({
      ...warnEvent(1_700_000_000_000),
      tool: `srv__read${i}`,
    }));
    getSecurityEvents.mockResolvedValue(events);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: "Accept all for this server" }));
    await act(async () => {});
    expect(screen.queryByText("Tool changes")).not.toBeInTheDocument();
    await act(async () => {
      vi.advanceTimersByTime(3000);
    });
    expect(screen.queryByText("Tool changes")).not.toBeInTheDocument();
  });

  it("retains a v1 review marker after accepting a 357-tool shared profile update", async () => {
    localStorage.clear();
    const legacyMarker = "tool_drift:older:older__read:changed:warn";
    localStorage.setItem("toolport.security.dismissed", JSON.stringify([legacyMarker]));
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    getSecurityEvents.mockResolvedValue(
      Array.from({ length: 357 }, (_, i) => ({
        ...warnEvent(1_700_000_000_000),
        tool: `srv__read${i}`,
        profiles: ["", "a", "b", "c", "d", "e"],
        blocked: false,
      })),
    );
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: "Accept all for this server" }));
    await act(async () => {});
    const markers = JSON.parse(localStorage.getItem("toolport.security.dismissed")!);
    expect(markers).toContain(legacyMarker);
    expect(markers).toHaveLength(358);
  });

  it("keeps separate definitions within the old duplicate window", async () => {
    getSecurityEvents.mockResolvedValue([
      { ...warnEvent(1_700_000_000_000), new_fp: "v2:old" },
      { ...warnEvent(1_700_000_120_000), new_fp: "v2:new" },
    ]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getAllByRole("button", { name: /srv: 1 tool changed/ })).toHaveLength(
      2,
    );
  });

  it("keeps verified-unblocked tool changes as history when Safety is effectively Off", async () => {
    localStorage.clear();
    const change = (tool: string, blocked: boolean | null, historical = false) => ({
      ...warnEvent(1_700_000_000_000),
      tool: `srv__${tool}`,
      new_fp: `v2:${tool}`,
      blocked,
      ...(blocked ? { blocked_profiles: ["work"] } : {}),
      ...(historical ? { historical: true } : {}),
    });
    const unblocked = [change("read", false), change("list", false, true)];
    const OFF = {
      toolChangeReviews: false,
      blocksNothing: true,
      reviewsConfirmed: false,
    };
    // The registry prop no longer decides: Rust answers through the command.
    const shown = async (
      safety:
        | {
            toolChangeReviews: boolean;
            blocksNothing: boolean;
            reviewsConfirmed: boolean;
          }
        | Error,
      events: SecurityEvent[],
    ) => {
      if (safety instanceof Error) getActivitySafety.mockRejectedValue(safety);
      else getActivitySafety.mockResolvedValue(safety);
      getSecurityEvents.mockResolvedValue(events);
      const view = render(<ActivityView refreshKey={0} registry={null} />);
      await act(async () => {});
      const text = view.container.textContent ?? "";
      const accept = screen.queryAllByRole("button", {
        name: "Accept all for this server",
      }).length;
      view.unmount();
      return { text, accept };
    };
    const offCard = "Toolport records tool changes but does not block them.";

    const off = await shown(OFF, unblocked);
    expect(off.accept).toBe(0);
    expect(off.text).not.toContain("Review the changes or accept them");
    expect(off.text).not.toContain("Open history to review them");
    expect(off.text).toContain("2 tool-change records");
    expect(off.text).toContain("Safety is Off.");
    expect(off.text).toContain(offCard);
    expect(off.text).not.toContain("Protection active.");
    // The state comes first, then the records.
    expect(off.text.indexOf("Safety is Off.")).toBeLessThan(
      off.text.indexOf("2 tool-change records"),
    );

    // A blocked, unknown or held change on screen: never the Off card.
    const blocked = await shown(OFF, [...unblocked, change("delete", true)]);
    expect(blocked.accept).toBe(1);
    expect(blocked.text).toContain("1 tool is blocked");
    expect(blocked.text).toContain("2 tool-change records");
    expect(blocked.text).not.toContain(offCard);
    expect(blocked.text).not.toContain("Protection active.");
    const olderBlocked = await shown(OFF, [change("old", true, true)]);
    expect(olderBlocked.text).toContain("1 older tool-change records");
    expect(olderBlocked.text).not.toContain(offCard);
    const unknown = await shown(OFF, [change("read", null)]);
    expect(unknown.text).toContain("Blocking status unavailable");
    expect(unknown.text).not.toContain("tool-change records");
    expect(unknown.text).not.toContain(offCard);
    const review = await shown(OFF, [
      { ...change("read", false), review_required: true },
    ]);
    expect(review.text).toContain("Review the captured definition in Settings");
    expect(review.text).not.toContain("tool-change records");
    expect(review.text).not.toContain(offCard);
    // A tool held in a store but not on screen: Rust says Off does not block nothing.
    const held = await shown(
      { toolChangeReviews: false, blocksNothing: false, reviewsConfirmed: false },
      unblocked,
    );
    expect(held.text).toContain("2 tool-change records");
    expect(held.text).not.toContain(offCard);
    expect(held.text).not.toContain("Protection active.");

    // Tool security notices block nothing at Off: the card stays, first, then the
    // records, then the notices. A held tool still hides it.
    const notices = [
      {
        ts: 1_700_000_000_000,
        type: "tool_poison_flag",
        server: "srv",
        tool: "srv__poisoned",
        change: "poison",
        severity: "high" as const,
      },
      {
        ts: 1_700_000_000_000,
        type: "result_injection",
        server: "srv",
        tool: "srv__fetch",
        change: "result",
        severity: "high" as const,
      },
    ];
    const withNotices = await shown(OFF, [...unblocked, ...notices]);
    expect(withNotices.text).toContain("Tool security notices");
    expect(withNotices.text).toContain(offCard);
    expect(withNotices.text).not.toContain("Protection active.");
    expect(withNotices.text.indexOf("Safety is Off.")).toBeLessThan(
      withNotices.text.indexOf("2 tool-change records"),
    );
    expect(withNotices.text.indexOf("2 tool-change records")).toBeLessThan(
      withNotices.text.indexOf("Tool security notices"),
    );
    const noticesHeld = await shown(
      { toolChangeReviews: false, blocksNothing: false, reviewsConfirmed: false },
      [...unblocked, ...notices],
    );
    expect(noticesHeld.text).toContain("Tool security notices");
    expect(noticesHeld.text).not.toContain(offCard);
    const noticesBlocked = await shown(OFF, [
      ...unblocked,
      change("delete", true),
      ...notices,
    ]);
    expect(noticesBlocked.text).toContain("Tool security notices");
    expect(noticesBlocked.text).not.toContain(offCard);

    // A refresh never pairs fresh events with the previous Safety answer: a block
    // that arrives before Safety does must not appear under the Off card.
    getActivitySafety.mockResolvedValue(OFF);
    getSecurityEvents.mockResolvedValue(unblocked);
    const racing = render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(racing.container.textContent).toContain(offCard);
    let answerSafety: (value: typeof OFF) => void = () => {};
    getActivitySafety.mockReturnValue(
      new Promise((resolve) => {
        answerSafety = resolve;
      }),
    );
    getSecurityEvents.mockResolvedValue([
      ...unblocked,
      {
        ts: 1_700_000_000_000,
        type: "result_injection_blocked",
        server: "srv",
        tool: "srv__fetch",
        change: "result",
        severity: "high" as const,
      },
    ]);
    racing.rerender(<ActivityView refreshKey={1} registry={null} />);
    await act(async () => {});
    const between = racing.container.textContent ?? "";
    expect(between.includes(offCard) && between.includes("Tool security notices")).toBe(
      false,
    );
    await act(async () => {
      answerSafety({
        toolChangeReviews: false,
        blocksNothing: false,
        reviewsConfirmed: false,
      });
    });
    expect(racing.container.textContent).toContain("Tool security notices");
    expect(racing.container.textContent).not.toContain(offCard);
    racing.unmount();

    // Reviews on, or an unanswered check, keep the cards as before.
    for (const safety of [
      { toolChangeReviews: true, blocksNothing: false, reviewsConfirmed: true },
      new Error("registry unreadable"),
    ]) {
      const ask = await shown(safety, unblocked);
      expect(ask.text).toContain("Not blocked. Review the changes or accept them.");
      expect(ask.text).toContain("1 older tool-change records");
      expect(ask.text).not.toContain("2 tool-change records");
      expect(ask.text).not.toContain("Safety is Off.");
    }
    const resting = await shown(
      { toolChangeReviews: true, blocksNothing: false, reviewsConfirmed: true },
      [],
    );
    expect(resting.text).toContain("Protection active.");
    // Only a successful read that confirmed reviews may claim protection: not
    // after a failed read, an unconfirmed answer, or while the read is pending.
    for (const safety of [
      new Error("registry unreadable"),
      { toolChangeReviews: true, blocksNothing: false, reviewsConfirmed: false },
    ]) {
      const unknown = await shown(safety, []);
      expect(unknown.text).not.toContain("Protection active.");
      expect(unknown.text).not.toContain("Safety is Off.");
    }
    getActivitySafety.mockReturnValue(new Promise(() => {}));
    getSecurityEvents.mockResolvedValue([]);
    const pending = render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(pending.container.textContent).not.toContain("Protection active.");
    pending.unmount();
  });
});

describe("ActivityView live inspector", () => {
  it("shows an error with retry instead of a false empty state when the inspect log fails to load (#728)", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getInspectLog
      .mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([]);

    // LiveInspector only mounts while live inspection is on (ActivityView.tsx:1772).
    render(<ActivityView refreshKey={0} registry={{ liveInspect: true } as never} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't load live inspector.")).toBeInTheDocument();
    expect(
      screen.queryByText(/No calls captured yet\. Run a tool/),
    ).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Retry loading live inspector" }),
    );
    await act(async () => {});
    expect(screen.getByText(/No calls captured yet\. Run a tool/)).toBeInTheDocument();
  });
});

describe("telemetry health", () => {
  it("keeps persisted drop evidence visible after the gateway exits", async () => {
    getAuditLog.mockResolvedValue([]);
    getAuditStats.mockResolvedValue({
      total: 0,
      errors: 0,
      errorRate: 0,
      servers: [],
      telemetry: {
        queueDropped: 0,
        writeFailedRecords: 0,
        writeFailures: 0,
        incompleteFlushes: 0,
        retainedDropped: 7,
      },
    });
    render(<ActivityView refreshKey={0} registry={null} />);
    expect(
      await screen.findByText(
        /7 dropped telemetry records are recorded in retained history/,
      ),
    ).toBeInTheDocument();
  });
  it("shows dropped records and partial persistence even with no calls", async () => {
    getAuditLog.mockResolvedValue([]);
    getAuditStats.mockResolvedValue({
      total: 0,
      errors: 0,
      errorRate: 0,
      servers: [],
      telemetry: {
        queueDropped: 3,
        writeFailedRecords: 2,
        writeFailures: 1,
        incompleteFlushes: 1,
      },
      gatewayNotes: ["Client configs updated, but ownership state was not saved."],
    });
    render(<ActivityView refreshKey={0} registry={null} />);
    expect(await screen.findByText(/3 records dropped/)).toBeInTheDocument();
    expect(screen.getByText(/ownership state was not saved/)).toBeInTheDocument();
  });

  it("shows unavailable shared gateway health instead of healthy zero counters", async () => {
    getAuditStats.mockResolvedValue({
      total: 0,
      errors: 0,
      errorRate: 0,
      servers: [],
      telemetry: {
        queueDropped: 0,
        writeFailedRecords: 0,
        writeFailures: 0,
        incompleteFlushes: 0,
        unavailable: true,
      },
    });
    render(<ActivityView refreshKey={0} registry={null} />);
    expect(
      await screen.findByText(/Gateway telemetry health is unavailable/),
    ).toBeInTheDocument();
  });
});

it("uses one server filter and one identity, time and wait meta line", async () => {
  const ts = Date.now() - 120000;
  getAuditLog.mockResolvedValue([
    entry({
      ts,
      server: "team_slack",
      serverId: "team-slack",
      kind: "approval",
      decision: "denied",
      client: "adapter:claude-code",
      clientName: "Claude Code",
      clientLabel: "Claude Code 2.1",
      heldMs: 1500,
    }),
    entry({
      ts,
      server: "team_slack",
      serverId: "team-slack",
      clientName: "Claude Code",
      durationMs: 850,
    }),
  ]);
  render(<ActivityView refreshKey={0} registry={null} />);
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  const meta = screen
    .getAllByText("Claude Code")
    .map((name) => name.parentElement!)
    .find((line) => line.textContent === "Claude Code · 2m ago · waited 1.5 s");
  expect(meta).toHaveAttribute("title", expect.stringContaining("adapter:claude-code"));
  expect(screen.queryByText("adapter:claude-code")).not.toBeInTheDocument();
  await user.click(screen.getByRole("combobox"));
  expect(screen.getAllByRole("option", { name: /^team_slack$/ })).toHaveLength(1);
  expect(screen.queryByRole("option", { name: /^team-slack$/ })).not.toBeInTheDocument();
});

it("names a Tools tab Run as Toolport and shows its whole duration", async () => {
  const ts = Date.now() - 120000;
  // As the backend returns it: the Tools tab's own ID and trusted name, no label.
  getAuditLog.mockResolvedValue([
    entry({
      ts,
      tool: "echo",
      client: "adapter:toolport-tools-tab",
      clientName: "Toolport Tools tab",
      durationMs: 9200,
    }),
  ]);
  render(<ActivityView refreshKey={0} registry={null} />);
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(detailLine("Toolport Tools tab · 2m ago · 9.2 s")).toBeVisible();
  expect(
    screen.queryByText(/toolport-gtk|toolport-gateway|reports/),
  ).not.toBeInTheDocument();
  // A name Toolport gives, so it follows the language.
  await act(() => setLanguage("fr"));
  try {
    expect(screen.getByText("Toolport, onglet Outils")).toBeVisible();
  } finally {
    await act(() => setLanguage("en"));
  }
});

/** The row's detail line, whose client name and time are separate spans. */
function detailLine(text: string) {
  return screen.getByText(
    (_, element) => element?.tagName === "DIV" && element.textContent === text,
  );
}

it("keeps the time and duration visible when the client name is long", async () => {
  const ts = Date.now() - 120000;
  const longName = `Agent ${"with a very long reported name ".repeat(6)}`.trim();
  getAuditLog.mockResolvedValue([
    entry({ ts, clientName: longName, client: "adapter:long", durationMs: 850 }),
  ]);
  render(<ActivityView refreshKey={0} registry={null} />);
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  const name = screen.getByText(longName);
  // On a wide row only the name truncates; the time and duration sit beside it and
  // never shrink. A narrow row wraps the whole line instead.
  expect(name).toHaveClass("@xl:truncate", "@xl:min-w-0");
  expect(name).toHaveAttribute("dir", "auto");
  const when = name.nextElementSibling!;
  expect(when).toHaveTextContent("· 2m ago · 850 ms");
  expect(when).toHaveClass("@xl:shrink-0");
  expect(when).not.toHaveClass("@xl:truncate");
  expect(name.parentElement).toHaveAttribute(
    "title",
    expect.stringContaining(`${longName} · 2m ago · 850 ms`),
  );
});

it("uses one server option for hyphenated call and approval identities", async () => {
  getAuditLog.mockResolvedValue([
    {
      ...entry({
        server: "team_slack",
        serverId: "team-slack",
        clientName: "Claude Code",
      }),
      kind: "approval",
      decision: "denied",
    },
    entry({ server: "team_slack", serverId: "team-slack", clientName: "Claude Code" }),
  ]);
  render(<ActivityView refreshKey={0} registry={null} />);
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  await user.click(screen.getByRole("combobox"));
  expect(screen.getAllByRole("option", { name: /^team_slack$/ })).toHaveLength(1);
  expect(screen.queryByRole("option", { name: /^team-slack$/ })).not.toBeInTheDocument();
});

it("keeps revision failures visible after successful panel reads and clears them on recovery", async () => {
  getActivityRevision.mockRejectedValue("unreadable");
  render(<ActivityView refreshKey={0} registry={null} />);
  expect(
    await screen.findByText(/Activity change checks are unavailable/),
  ).toBeInTheDocument();
  expect(getAuditLog).toHaveBeenCalled();
  getActivityRevision.mockResolvedValue("recovered");
  await act(() => vi.advanceTimersByTimeAsync(3000));
  expect(
    screen.queryByText(/Activity change checks are unavailable/),
  ).not.toBeInTheDocument();
});

it("discards a late revision without evicting a newer visibility session", async () => {
  let complete!: (revision: string) => void;
  getActivityRevision.mockImplementation(
    () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      }),
  );
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  const releases = releaseActivityRevision.mock.calls.length;
  view.unmount();
  expect(releaseActivityRevision).toHaveBeenCalledTimes(releases + 1);
  await act(async () => complete("late"));
  expect(releaseActivityRevision).toHaveBeenCalledTimes(releases + 1);
});

it.each(["success", "cancellation"] as const)(
  "preserves a fresh mounted session when an old revision returns %s",
  async (completion) => {
    let finish!: (revision: string) => void;
    let cancel!: (error: string) => void;
    getActivityRevision.mockImplementationOnce(
      () =>
        new Promise<string>((resolve, reject) => {
          finish = resolve;
          cancel = reject;
        }),
    );
    getActivityRevision.mockResolvedValue("fresh");
    const old = render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    old.unmount();
    const releases = releaseActivityRevision.mock.calls.length;
    getAuditLog.mockResolvedValue(refreshedLog);
    const fresh = render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await screen.findByRole("button", { name: /recent calls/i, expanded: true });
    expect(screen.getByText("list_issues")).toBeInTheDocument();
    const reads = getAuditLog.mock.calls.length;
    await act(async () => {
      if (completion === "success") finish("obsolete");
      else cancel("Activity change check cancelled");
    });
    expect(releaseActivityRevision).toHaveBeenCalledTimes(releases);
    expect(screen.getByText("list_issues")).toBeInTheDocument();
    expect(
      screen.queryByText(/Activity change checks are unavailable/),
    ).not.toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    expect(getAuditLog).toHaveBeenCalledTimes(reads);
    fresh.unmount();
    expect(releaseActivityRevision).toHaveBeenCalledTimes(releases + 1);
  },
);

it("reconciles a write between initial panel snapshot and first revision baseline", async () => {
  let finish!: (revision: string) => void;
  getActivityRevision.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
  );
  getActivityRevision.mockResolvedValue("new-content");
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  const calls = getAuditLog.mock.calls.length;
  expect(calls).toBe(1);
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  // A real new append is now available to panel readers. The queued initial
  // digest observes it, after the first panel reader has returned older bytes.
  getAuditLog.mockResolvedValue(refreshedLog);
  await act(async () => {
    finish("new-content");
  });
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(calls + 1);
  expect(screen.queryByText("list_issues")).toBeInTheDocument();
  view.unmount();
});

it("reconciles after an initial revision error and rejects obsolete panel completions", async () => {
  let finishOld!: (rows: AuditEntry[]) => void;
  getActivityRevision.mockRejectedValueOnce("unreadable");
  getActivityRevision.mockResolvedValue("same");
  getAuditLog.mockImplementationOnce(
    () =>
      new Promise<AuditEntry[]>((resolve) => {
        finishOld = resolve;
      }),
  );
  getAuditLog.mockResolvedValue(refreshedLog);
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByText(/Activity change checks are unavailable/)).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(3000));
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(screen.getByText("list_issues")).toBeInTheDocument();
  await act(async () => finishOld(initialLog));
  expect(screen.getByText("list_issues")).toBeInTheDocument();
  const calls = getAuditLog.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(calls);
});

it("reconciles hide/show with a delayed first revision and no overlapping checks", async () => {
  let finish!: (revision: string) => void;
  getActivityRevision.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
  );
  getActivityRevision.mockResolvedValue("same");
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  windowVisible = false;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  getAuditLog.mockResolvedValue(refreshedLog);
  windowVisible = true;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(6000));
  expect(getActivityRevision).toHaveBeenCalledTimes(1);
  await act(async () => finish("obsolete"));
  await act(() => vi.advanceTimersByTimeAsync(3000));
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(screen.getByText("list_issues")).toBeInTheDocument();
  const calls = getAuditLog.mock.calls.length;
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(calls);
});

it("names calls made through T3 Code and shows their thread", async () => {
  const ts = Date.now() - 120000;
  const thread = "0f8e2c1a-9b7d-4e3f-a1b2-c3d4e5f60718";
  getAuditLog.mockResolvedValue([
    entry({
      ts,
      tool: "hosted",
      clientName: "Codex",
      host: "t3-code",
      hostThread: thread,
    }),
    entry({ ts, tool: "hosted_no_thread", clientName: "Cursor", host: "t3-code" }),
    entry({
      ts,
      tool: "hosted_reports",
      clientName: "Claude Code (claude_personal)",
      clientLabel: "claude-code-cli",
      host: "t3-code",
    }),
    entry({ ts, tool: "plain_reports", clientName: "Gemini", clientLabel: "my-agent" }),
    entry({ ts, tool: "other_host", clientName: "Zed", host: "other", hostThread: "x1" }),
    entry({ ts, tool: "plain", clientName: "Claude Code" }),
  ]);
  render(<ActivityView refreshKey={0} registry={null} />);
  await screen.findByRole("button", { name: /recent calls/i, expanded: true });
  expect(detailLine("Codex via T3 Code · 2m ago · 120 ms")).toBeVisible();
  const chip = screen.getByText("Thread 0f8e2c1a");
  expect(chip).toHaveAttribute("title", `T3 Code thread ${thread}`);
  // A control that copies the full id.
  expect(screen.getByRole("button", { name: `Copy thread id ${thread}` })).toBe(chip);
  expect(detailLine("Cursor via T3 Code · 2m ago · 120 ms")).toBeVisible();
  // The host stays beside the client it describes; what it reported comes next.
  expect(
    detailLine(
      'Claude Code (claude_personal) via T3 Code · reports "claude-code-cli" · 2m ago · 120 ms',
    ),
  ).toBeVisible();
  expect(detailLine('Gemini · reports "my-agent" · 2m ago · 120 ms')).toBeVisible();
  expect(screen.getAllByText(/^Thread /)).toHaveLength(1);
  expect(detailLine("Zed · 2m ago · 120 ms")).toBeVisible();
  expect(detailLine("Claude Code · 2m ago · 120 ms")).toBeVisible();
  expect(screen.queryByText(/Zed via/)).not.toBeInTheDocument();
});

describe("ActivityView header actions", () => {
  it("offers Export and Clear on an empty log, as on a busy one", async () => {
    getAuditLog.mockResolvedValue([]);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("No activity yet")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clear" })).toBeInTheDocument();
  });

  it("exports the format picked from the Export menu", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    save.mockResolvedValue("/tmp/toolport-activity.json");
    exportAuditToPath.mockResolvedValue(undefined);
    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: "Export" }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Export JSON",
      "Export CSV",
    ]);
    await user.click(screen.getByRole("menuitem", { name: "Export JSON" }));
    await act(async () => {});
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        defaultPath: "toolport-activity.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      }),
    );
    expect(exportAuditToPath).toHaveBeenCalledWith("/tmp/toolport-activity.json", "json");
  });
});
