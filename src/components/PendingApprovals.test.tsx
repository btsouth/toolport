import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { PendingApprovals } from "./PendingApprovals";
import type { PendingApproval } from "@/lib/types";

const listPendingApprovals = vi.fn();
const decideApproval = vi.fn();

vi.mock("@/lib/api", () => ({
  listPendingApprovals: () => listPendingApprovals(),
  decideApproval: (...a: unknown[]) => decideApproval(...a),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/lib/openUrl", () => ({ openExternal: vi.fn() }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

function approval(over: Partial<PendingApproval> = {}): PendingApproval {
  return {
    id: "req-1",
    client: null,
    server: "mailer",
    tool: "mailer__send",
    reason: "destructive",
    arguments: { to: "⟦EMAIL_1⟧" },
    deadlineMs: Date.now() + 120_000,
    ...over,
  };
}

const release = approval({
  reason: "pii_cross_server",
  piiRelease: {
    server: "mailer",
    values: [
      {
        token: "⟦EMAIL_1⟧",
        value: "ada@example.com",
        origins: ["crm"],
      },
    ],
  },
});

beforeEach(() => {
  listPendingApprovals.mockResolvedValue([]);
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("PendingApprovals PII release", () => {
  it("names the value, its origin, and the destination it has never reached", async () => {
    // SBS-696: a person cannot judge this release without seeing what is being released
    // and where it came from. This window is the only place the real value appears.
    listPendingApprovals.mockResolvedValue([release]);
    render(<PendingApprovals />);
    await act(async () => {});

    expect(screen.getByText("ada@example.com")).toBeInTheDocument();
    expect(screen.getByText(/from crm/)).toBeInTheDocument();
    expect(screen.getByText(/Would send to mailer/)).toBeInTheDocument();
    expect(screen.getByText("Releases private data")).toBeInTheDocument();
  });

  it("never offers to skip the prompt for a release", async () => {
    // The allow key binds a TOOL DEFINITION, and the broker refuses to auto-approve a
    // release on one. Offering the shortcut here would promise a bypass that never fires
    // — and if it ever did fire, it would be the blanket grant this feature exists to
    // avoid.
    listPendingApprovals.mockResolvedValue([release]);
    render(<PendingApprovals />);
    await act(async () => {});

    expect(screen.queryByText("Skip next time?")).not.toBeInTheDocument();
    expect(screen.queryByText("Allow for this session")).not.toBeInTheDocument();
  });

  it("still offers the skip shortcut for an ordinary tool approval", async () => {
    // Guard the premise of the test above: the shortcut is hidden because of the release,
    // not because it never renders.
    listPendingApprovals.mockResolvedValue([approval()]);
    render(<PendingApprovals />);
    await act(async () => {});

    expect(screen.getByText("Skip next time?")).toBeInTheDocument();
  });
});

const routineSave = (argumentsOver: Record<string, unknown> = {}) =>
  approval({
    id: "routine-save-1",
    client: "cursor",
    server: "toolport",
    tool: "save_routine",
    reason: "persistent_code_write",
    arguments: {
      runId: "run_abc",
      name: "daily-report",
      description: "Create a daily report",
      source: "return input.value;",
      inputSchema: { type: "object" },
      limits: { maxCalls: 64 },
      riskClass: "medium",
      evidence: {
        calls: 2,
        observedDependencies: [{ name: "github__issues" }],
      },
      contentHash: "sha256:abc",
      ...argumentsOver,
    },
  });

describe("PendingApprovals persistent routine writes", () => {
  it("shows the exact definition and permits only one-shot approval", async () => {
    listPendingApprovals.mockResolvedValue([routineSave()]);
    const user = userEvent.setup();
    render(<PendingApprovals />);

    expect(
      await screen.findByRole("alertdialog", {
        name: /tool calls awaiting your approval/i,
      }),
    ).toBeInTheDocument();
    expect(screen.getByText("Persistent code")).toBeInTheDocument();
    expect(screen.getByText("Routine definition")).toBeInTheDocument();
    expect(screen.getByText("daily-report")).toBeInTheDocument();
    expect(screen.getByText("Risk: medium")).toBeInTheDocument();
    expect(screen.getByText("Calls: 2")).toBeInTheDocument();
    expect(screen.getByText(/return input\.value/)).toBeInTheDocument();
    expect(screen.getByText(/sha256:abc/)).toBeInTheDocument();
    expect(screen.queryByText("Allow for this session")).not.toBeInTheDocument();
    expect(screen.queryByText("Always allow this tool")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Approve" }));
    expect(decideApproval).toHaveBeenCalledWith("routine-save-1", true, "once");
  });

  it("hides the synthesized-provenance banner for real immutable runs", async () => {
    listPendingApprovals.mockResolvedValue([routineSave()]);
    render(<PendingApprovals />);
    await screen.findByText("daily-report");
    expect(screen.queryByText(/Synthesized by Toolport/)).not.toBeInTheDocument();
  });

  it("discloses synthesized provenance so the user knows the glue never executed", async () => {
    listPendingApprovals.mockResolvedValue([
      routineSave({ provenance: "synthesized_from_observed_calls" }),
    ]);
    render(<PendingApprovals />);
    await screen.findByText("daily-report");
    expect(
      screen.getByText(/Synthesized by Toolport from observed direct calls/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/statically validated, not yet executed/),
    ).toBeInTheDocument();
  });
});
