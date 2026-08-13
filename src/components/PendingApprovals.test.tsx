import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const mocks = vi.hoisted(() => ({
  decideApproval: vi.fn(),
  listPendingApprovals: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  decideApproval: mocks.decideApproval,
  listPendingApprovals: mocks.listPendingApprovals,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@/lib/openUrl", () => ({ openExternal: vi.fn() }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

import { PendingApprovals } from "./PendingApprovals";

describe("PendingApprovals persistent routine writes", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.listen.mockResolvedValue(() => {});
    mocks.decideApproval.mockResolvedValue(undefined);
    mocks.listPendingApprovals.mockResolvedValue([
      {
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
        },
        deadlineMs: Date.now() + 120_000,
      },
    ]);
  });

  it("shows the exact definition and permits only one-shot approval", async () => {
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
    expect(mocks.decideApproval).toHaveBeenCalledWith("routine-save-1", true, "once");
  });

  it("hides the synthesized-provenance banner for real immutable runs", async () => {
    render(<PendingApprovals />);
    await screen.findByText("daily-report");
    expect(screen.queryByText(/Synthesized by Toolport/)).not.toBeInTheDocument();
  });

  it("discloses synthesized provenance so the user knows the glue never executed", async () => {
    const [approval] = await mocks.listPendingApprovals();
    mocks.listPendingApprovals.mockResolvedValue([
      {
        ...approval,
        arguments: {
          ...approval.arguments,
          provenance: "synthesized_from_observed_calls",
        },
      },
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
