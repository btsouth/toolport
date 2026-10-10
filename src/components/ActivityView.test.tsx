import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ActivityView } from "./ActivityView";
import type { AuditEntry, SearchTrace } from "@/lib/types";
import type { SecurityEvent } from "@/lib/api";

let windowVisible = true;
vi.mock("@/lib/windowVisible", () => ({ useWindowVisible: () => windowVisible }));

const getAuditLog = vi.fn();
const getSearchTraces = vi.fn();
const getSecurityEvents = vi.fn();
const getToolIdentities = vi.fn();
const getInspectLog = vi.fn();
const getSavingsSummary = vi.fn();
const getAuditStats = vi.fn();

const clearActivityLogs = vi.fn();

vi.mock("@/lib/api", () => ({
  clearActivityLogs: (...a: unknown[]) => clearActivityLogs(...a),
  exportAuditToPath: vi.fn(),
  getAuditLog: (...a: unknown[]) => getAuditLog(...a),
  getAuditStats: (...a: unknown[]) => getAuditStats(...a),
  getInspectLog: (...a: unknown[]) => getInspectLog(...a),
  getSavingsSummary: (...a: unknown[]) => getSavingsSummary(...a),
  getSearchTraces: (...a: unknown[]) => getSearchTraces(...a),
  getSecurityEvents: (...a: unknown[]) => getSecurityEvents(...a),
  getToolIdentities: (...a: unknown[]) => getToolIdentities(...a),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn(), info: vi.fn() },
}));

vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));

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
  getAuditStats.mockResolvedValue(null);
  windowVisible = true;
  vi.useFakeTimers({ shouldAdvanceTime: true });
  getAuditLog.mockResolvedValue(initialLog);
  getSearchTraces.mockResolvedValue([]);
  getSecurityEvents.mockResolvedValue([]);
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
  await user.click(await screen.findByRole("button", { name: /recent calls/i }));
  expect(screen.getByText("calls recorded").parentElement).toHaveTextContent(
    /2\s*calls recorded/,
  );
  expect(screen.getByText("errors (50%)").parentElement).toHaveTextContent(/1\s*errors/);
  for (const [, label] of outcomes) expect(screen.getByText(label)).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: /errors only/i }));
  expect(screen.getByText("merge_pr")).toBeInTheDocument();
  for (const [, label] of outcomes)
    expect(screen.queryByText(label)).not.toBeInTheDocument();
});

it("p10c shows an approval-only history even when no tools ran", async () => {
  getAuditLog.mockResolvedValue([
    { ...entry(), kind: "approval", decision: "withdrawn" },
  ]);
  getAuditStats.mockResolvedValue({ total: 0, errors: 0, errorRate: 0, servers: [] });
  render(<ActivityView refreshKey={0} registry={null} />);
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  await user.click(await screen.findByRole("button", { name: /recent calls/i }));
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
  await act(() => vi.advanceTimersByTimeAsync(60_000));
  expect(getAuditLog).toHaveBeenCalledTimes(loaded + 1);
  windowVisible = true;
  view.rerender(<ActivityView refreshKey={0} registry={null} />);
  await act(() => vi.advanceTimersByTimeAsync(3000));
  expect(getAuditLog).toHaveBeenCalledTimes(loaded + 2);
});

describe("ActivityView trust-state loading", () => {
  it("shows initial loading without claiming protection is clear", () => {
    getAuditLog.mockReturnValue(new Promise(() => {}));
    getSecurityEvents.mockReturnValue(new Promise(() => {}));

    render(<ActivityView refreshKey={0} registry={null} />);

    expect(screen.getByText("Loading activity…")).toBeInTheDocument();
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
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText(/latest 2/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    fireEvent.click(screen.getByRole("button", { name: "Clear activity" }));
    await act(async () => {});

    expect(toast.success).toHaveBeenCalledWith("Cleared retained activity");
    expect(screen.queryByText(/latest 2/)).not.toBeInTheDocument();
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
      .mockReturnValueOnce(
        new Promise<AuditEntry[]>((res) => {
          resolveStale = res;
        }),
      )
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText(/latest 2/)).toBeInTheDocument();

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
    expect(screen.queryByText(/latest 2/)).not.toBeInTheDocument();
    expect(
      screen.getByText(/can't verify that the log is still empty/i),
    ).toBeInTheDocument();
  });

  it("does not turn a last-known empty audit log into a current all-clear", async () => {
    getAuditLog.mockResolvedValueOnce([]).mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    expect(screen.getByText("No activity yet")).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(3000);
    });

    expect(screen.getByText("Activity may be out of date.")).toBeInTheDocument();
    expect(screen.getByText("No current activity status")).toBeInTheDocument();
    expect(screen.queryByText("No activity yet")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Retry activity log" }),
    ).toBeInTheDocument();
  });

  it("preserves last-known calls and offers retry when a live refresh fails", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getAuditLog
      .mockResolvedValueOnce(initialLog)
      .mockRejectedValueOnce(new Error("locked"));

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});
    await user.click(screen.getByRole("button", { name: /recent calls/i }));

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
    getAuditLog.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce([]);

    render(<ActivityView refreshKey={0} registry={null} />);
    await act(async () => {});

    expect(screen.getByText("Couldn't load activity")).toBeInTheDocument();
    expect(screen.queryByText("No activity yet")).not.toBeInTheDocument();

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
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    await user.click(await screen.findByRole("button", { name: /recent calls/i }));
    expect(screen.getByText("Searched tools")).toBeInTheDocument();
    expect(screen.queryByText(/Nothing searched yet/)).not.toBeInTheDocument();
    expect(screen.queryByText(/warm catalog|dispatch 3/)).not.toBeInTheDocument();
    expect(screen.getByText(/Unrecorded client ·/)).toHaveAttribute(
      "title",
      expect.stringContaining("dispatch 3 ms"),
    );
    expect(screen.getByText("1 value masked")).toHaveAttribute(
      "title",
      expect.stringContaining("before reaching the model"),
    );
    expect(screen.getByText(/5,225 calls recorded/)).toHaveTextContent(
      "Showing the latest 1.",
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
    await user.click(screen.getByRole("button", { name: /recent calls/i }));
    expect(screen.queryByText(`Run: ${runId}`)).not.toBeInTheDocument();
    await user.click(screen.getByText("run_script"));
    expect(screen.getByText("script exception", { exact: true })).toBeInTheDocument();
    expect(screen.getByText(`Run: ${runId}`)).toBeInTheDocument();
  });

  it("keeps an expanded error row open across a live-poll refetch", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    render(<ActivityView refreshKey={0} registry={null} />);

    await act(async () => {});
    await user.click(screen.getByRole("button", { name: /recent calls/i }));

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
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
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
    await user.click(screen.getByRole("button", { name: /recent calls/i }));

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
    getSearchTraces.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce([]);

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
  expect(screen.getByText(/923/)).toHaveTextContent("catalog tokens avoided");
  expect(screen.getByText(/923/)).toHaveAttribute(
    "title",
    expect.stringContaining("counted locally"),
  );
  expect(screen.getByText("How this is counted").parentElement).not.toHaveAttribute(
    "open",
  );
  expect(screen.getByText(/8\.0 KB of tool descriptions available/)).toBeInTheDocument();
  expect(screen.getByText(/Latest load: 1725 tools available/)).toBeInTheDocument();
  expect(screen.getByText(/Older estimated records/)).toBeInTheDocument();
  expect(screen.getByText(/searches returned 2\.5 KB/)).toBeInTheDocument();
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
  expect(screen.queryByText("Catalog text avoided")).not.toBeInTheDocument();
  expect(screen.queryByText(/52.8M/)).not.toBeInTheDocument();
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
    tokenizedLoads: 1,
    listLoads: 2751,
    peakCatalog: 1725,
    sinceTs: 1700000000000,
  });
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  await user.click(screen.getByRole("button", { name: "Share" }));
  expect(writeText).toHaveBeenCalledWith(
    expect.stringContaining("-123 catalog tokens avoided"),
  );
  expect(writeText.mock.calls[0][0]).toContain("not a billing figure");
  expect(writeText.mock.calls[0][0]).not.toMatch(/cl100k|bytes\/4|exposures/);
  expect(writeText.mock.calls[0][0]).not.toMatch(/billed tokens|money saved/i);
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
  expect(screen.getByText(/3 searches returned 12\.3 KB/)).toBeInTheDocument();
  expect(screen.queryByText(/0 tool-list loads/)).not.toBeInTheDocument();
  expect(screen.queryByText(/tokens saved/)).not.toBeInTheDocument();
});

it("reports unavailable catalog telemetry without showing an empty measurement", async () => {
  getSavingsSummary.mockRejectedValue(new Error("corrupt savings store"));
  render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  expect(screen.getByRole("alert")).toHaveTextContent("Catalog telemetry unavailable");
  expect(screen.queryByText("Catalog text avoided")).not.toBeInTheDocument();
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
    .mockRejectedValueOnce(new Error("unreadable"));
  const view = render(<ActivityView refreshKey={0} registry={null} />);
  await act(async () => {});
  view.rerender(<ActivityView refreshKey={1} registry={null} />);
  await act(async () => {});
  expect(screen.getByText("Catalog text avoided")).toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("last loaded measurements");
});

describe("ActivityView tool identities", () => {
  it("shows an error with retry instead of hiding the panel when identities fail to load (#728)", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getToolIdentities
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
});

describe("ActivityView live inspector", () => {
  it("shows an error with retry instead of a false empty state when the inspect log fails to load (#728)", async () => {
    const user = userEvent.setup({
      advanceTimers: (ms) => vi.advanceTimersByTime(ms),
    });
    getInspectLog.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce([]);

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
  await user.click(await screen.findByRole("button", { name: /recent calls/i }));
  expect(
    screen.getByText('Claude Code (reports "Claude Code 2.1") · 2m ago · waited 1.5 s'),
  ).toHaveAttribute("title", expect.stringContaining("adapter:claude-code"));
  expect(screen.queryByText("adapter:claude-code")).not.toBeInTheDocument();
  await user.click(screen.getByRole("combobox"));
  expect(screen.getAllByRole("option", { name: /^team_slack$/ })).toHaveLength(1);
  expect(screen.queryByRole("option", { name: /^team-slack$/ })).not.toBeInTheDocument();
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
  await user.click(await screen.findByRole("button", { name: /recent calls/i }));
  await user.click(screen.getByRole("combobox"));
  expect(screen.getAllByRole("option", { name: /^team_slack$/ })).toHaveLength(1);
  expect(screen.queryByRole("option", { name: /^team-slack$/ })).not.toBeInTheDocument();
});
