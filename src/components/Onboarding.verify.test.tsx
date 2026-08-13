import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { DetectedClient } from "@/lib/types";

// Poll target: watch the local audit log for the first new call.
const getAuditLog = vi.fn();
vi.mock("@/lib/api", () => ({ getAuditLog: (n: number) => getAuditLog(n) }));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
// ClientLogo loads vendored SVGs via import.meta.glob; stub it so the test stays focused.
vi.mock("@/components/ClientLogo", () => ({ ClientLogo: () => null }));

import { VerifyCall } from "./Onboarding";

const client = {
  id: "cursor",
  name: "Cursor",
  appPresent: true,
  gatewayInstalled: true,
  entryState: "managed" as const,
  servers: [],
  pluginServers: [],
  configPath: "",
} as unknown as DetectedClient;

describe("VerifyCall", () => {
  beforeEach(() => getAuditLog.mockReset());

  it("celebrates the first new call after the snapshot", async () => {
    // First read = snapshot (one old call at ts 100); subsequent polls surface a newer one.
    getAuditLog
      .mockResolvedValueOnce([{ ts: 100, server: "GitHub", tool: "old", ok: true }])
      .mockResolvedValue([
        { ts: 200, server: "GitHub", tool: "get_me", ok: true },
        { ts: 100, server: "GitHub", tool: "old", ok: true },
      ]);

    render(<VerifyCall client={client} onOpenPlayground={vi.fn()} pollMs={5} />);

    await waitFor(() => expect(screen.getByText(/It works/)).toBeInTheDocument());
    // Names the tool + server that succeeded, without claiming Cursor sent it.
    expect(screen.getByText("get_me")).toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
    expect(screen.getByText(/after this check started/)).toBeInTheDocument();
    expect(screen.getByText(/not proof it came from Cursor/)).toBeInTheDocument();
  });

  it("does not celebrate historical traffic when the baseline read fails", async () => {
    getAuditLog
      .mockRejectedValueOnce(new Error("audit unavailable"))
      .mockResolvedValue([{ ts: 50, server: "GitHub", tool: "old", ok: true }]);

    render(<VerifyCall client={client} onOpenPlayground={vi.fn()} pollMs={5} />);

    await waitFor(() =>
      expect(screen.getByText(/Couldn't read the audit log/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/It works/)).not.toBeInTheDocument();
    expect(screen.queryByText("old")).not.toBeInTheDocument();
    expect(
      screen.queryByText("List the tools you can use through Toolport."),
    ).not.toBeInTheDocument();
  });

  /// The prompt is hidden until a baseline exists, so a user cannot paste it
  /// early and have Retry's snapshot absorb that very call as the baseline with
  /// nothing newer ever arriving. After Retry, only calls newer than the fresh
  /// baseline count.
  it("retries the snapshot and celebrates only calls newer than the retry baseline", async () => {
    getAuditLog
      .mockRejectedValueOnce(new Error("audit unavailable"))
      // Retry's snapshot: the newest row is an interim call (e.g. the prompt
      // pasted while the check was down). It becomes the baseline, not proof.
      .mockResolvedValueOnce([{ ts: 150, server: "GitHub", tool: "interim", ok: true }])
      .mockResolvedValueOnce([{ ts: 150, server: "GitHub", tool: "interim", ok: true }])
      .mockResolvedValue([
        { ts: 200, server: "GitHub", tool: "fresh", ok: true },
        { ts: 150, server: "GitHub", tool: "interim", ok: true },
      ]);
    const user = userEvent.setup();

    render(<VerifyCall client={client} onOpenPlayground={vi.fn()} pollMs={5} />);

    await waitFor(() =>
      expect(screen.getByText(/Couldn't read the audit log/)).toBeInTheDocument(),
    );
    await user.click(screen.getByRole("button", { name: /Retry the check/ }));

    // A working baseline brings the copy-paste prompt back.
    await waitFor(() =>
      expect(
        screen.getByText("List the tools you can use through Toolport."),
      ).toBeInTheDocument(),
    );
    await waitFor(() => expect(screen.getByText(/It works/)).toBeInTheDocument());
    // The celebrated call is the one after the retry baseline, not the interim row.
    expect(screen.getByText("fresh")).toBeInTheDocument();
    expect(screen.queryByText("interim")).not.toBeInTheDocument();
  });

  it("ignores unrelated traffic that is not newer than the baseline", async () => {
    getAuditLog
      .mockResolvedValueOnce([{ ts: 100, server: "GitHub", tool: "baseline", ok: true }])
      .mockResolvedValue([
        { ts: 100, server: "GitHub", tool: "baseline", ok: true },
        { ts: 40, server: "Other", tool: "older", ok: true },
      ]);

    render(
      <VerifyCall client={client} onOpenPlayground={vi.fn()} pollMs={5} timeoutMs={40} />,
    );

    await waitFor(() => expect(screen.getByText(/No call yet/)).toBeInTheDocument());
    expect(screen.queryByText(/It works/)).not.toBeInTheDocument();
    expect(screen.queryByText("older")).not.toBeInTheDocument();
  });

  it("shows recovery guidance when no call arrives before the deadline", async () => {
    // The log never advances past the snapshot, so nothing is ever "fresh".
    getAuditLog.mockResolvedValue([{ ts: 100, server: "GitHub", tool: "old", ok: true }]);

    render(
      <VerifyCall client={client} onOpenPlayground={vi.fn()} pollMs={5} timeoutMs={20} />,
    );

    await waitFor(() => expect(screen.getByText(/No call yet/)).toBeInTheDocument());
    expect(screen.getByText(/Restart Cursor/)).toBeInTheDocument();
    // Never falsely celebrates.
    expect(screen.queryByText(/It works/)).not.toBeInTheDocument();
  });

  it("offers the Playground fallback while waiting", () => {
    getAuditLog.mockResolvedValue([]);
    const onOpenPlayground = vi.fn();
    render(<VerifyCall client={client} onOpenPlayground={onOpenPlayground} pollMs={5} />);
    const btn = screen.getByRole("button", { name: /Playground/ });
    btn.click();
    expect(onOpenPlayground).toHaveBeenCalledOnce();
  });
});
