import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ActivityView } from "./ActivityView";
import type { AuditEntry, SearchTrace } from "@/lib/types";

const getAuditLog = vi.fn();
const getSearchTraces = vi.fn();

vi.mock("@/lib/api", () => ({
  clearActivityLogs: vi.fn(),
  exportAuditToPath: vi.fn(),
  getAuditLog: (...a: unknown[]) => getAuditLog(...a),
  getAuditStats: vi.fn(() => Promise.resolve(null)),
  getInspectLog: vi.fn(() => Promise.resolve([])),
  getSavingsSummary: vi.fn(() => Promise.resolve(null)),
  getSearchTraces: (...a: unknown[]) => getSearchTraces(...a),
  getSecurityEvents: vi.fn(() => Promise.resolve([])),
  getToolIdentities: vi.fn(() => Promise.resolve([])),
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
  vi.useFakeTimers({ shouldAdvanceTime: true });
  getAuditLog.mockResolvedValue(initialLog);
  getSearchTraces.mockResolvedValue([]);
});

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

describe("ActivityView recent calls", () => {
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

    expect(screen.getByText("3 pseudonymized")).toBeInTheDocument();

    // The fail-open case has to read as a warning, not as a tidy count: values reached
    // the model in the clear even though redaction was on.
    const incomplete = screen.getByText("2 pseudonymized, incomplete");
    expect(incomplete).toBeInTheDocument();
    expect(incomplete).toHaveAttribute(
      "title",
      expect.stringContaining("did not fully apply"),
    );

    // A pass that matched nothing, and a call made with redaction off, both stay silent —
    // a badge on every row would bury the two cases above.
    expect(screen.queryByText(/0 pseudonymized/)).not.toBeInTheDocument();
    expect(screen.getAllByText(/pseudonymized/)).toHaveLength(2);

    // The values are the point of the feature and must never reach this view.
    expect(document.body.textContent).not.toMatch(/@example\.com/);
  });
});

describe("ActivityView discovery", () => {
  it("shows a tiny nonzero saving without rounding it down to zero", async () => {
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

    expect(row.parentElement).toHaveTextContent(/<0\.1% less this turn\)\./);
    expect(row.parentElement).not.toHaveTextContent(/\(0% less this turn\)\./);
  });
});
