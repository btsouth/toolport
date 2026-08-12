import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
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
