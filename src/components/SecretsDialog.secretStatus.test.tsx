import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SecretsDialog } from "./SecretsDialog";
import type { ServerEntry } from "@/lib/types";

const secretStatus = vi.fn();
const setSecret = vi.fn();

vi.mock("@/lib/api", () => ({
  secretStatus: (...a: unknown[]) => secretStatus(...a),
  setSecret: (...a: unknown[]) => setSecret(...a),
  hasAuthToken: vi.fn().mockResolvedValue(false),
  hasClientSecret: vi.fn().mockResolvedValue(false),
  probeAuth: vi.fn().mockResolvedValue({ kind: "oauth", guidance: null }),
  setClientCredentials: vi.fn(),
  clearClientCredentials: vi.fn(),
  deleteSecret: vi.fn(),
  setAuthToken: vi.fn(),
  clearAuthToken: vi.fn(),
  authenticateOauth: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

vi.mock("@/lib/openUrl", () => ({ openExternal: vi.fn() }));

/** A stdio server (it has a command) with one declared secret env key. */
function server(over: Partial<ServerEntry> = {}): ServerEntry {
  return {
    id: "srv-1",
    name: "Resend",
    transport: "stdio",
    command: "npx",
    args: [],
    env: [{ key: "RESEND_API_KEY", value: null, secret: true }],
    url: null,
    source: "manual",
    ...over,
  };
}

async function openDialog(entry: ServerEntry) {
  const user = userEvent.setup();
  render(<SecretsDialog server={entry} onSaved={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: /Manage secrets/i }));
  return user;
}

/// SBS-841. The backend errs when a vault read fails instead of reporting the
/// key as unvaulted. That only helps if the dialog says so: `vaulted` starts
/// empty, so a swallowed rejection leaves the screen identical to "no key is
/// saved" - no badge, an inviting first-time paste prompt, and no Remove button
/// for a key that does exist.
describe("SecretsDialog vault probe (SBS-841)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    secretStatus.mockResolvedValue([["RESEND_API_KEY", true]]);
  });

  it("says the check failed instead of showing the key as unsaved", async () => {
    secretStatus.mockRejectedValue(new Error("keyring is locked"));
    await openDialog(server());

    await waitFor(() =>
      expect(secretStatus).toHaveBeenCalledWith("srv-1", ["RESEND_API_KEY"]),
    );
    await waitFor(() =>
      expect(screen.getByText(/Couldn't check the keychain/i)).toBeInTheDocument(),
    );
    // And it must not claim the opposite either way.
    expect(screen.queryByText("saved")).not.toBeInTheDocument();
  });

  it("stays quiet when the probe succeeds", async () => {
    await openDialog(server());

    await waitFor(() => expect(screen.getByText("saved")).toBeInTheDocument());
    expect(screen.queryByText(/Couldn't check the keychain/i)).not.toBeInTheDocument();
  });

  /// The keychain is usually unlockable on the spot, so the fix has to offer a
  /// way forward, not just an explanation.
  it("recovers the badges on retry, without reopening the dialog", async () => {
    secretStatus
      .mockRejectedValueOnce(new Error("keyring is locked"))
      .mockResolvedValueOnce([["RESEND_API_KEY", true]]);
    const user = await openDialog(server());

    await waitFor(() =>
      expect(screen.getByText(/Couldn't check the keychain/i)).toBeInTheDocument(),
    );
    await user.click(screen.getByRole("button", { name: "Retry the keychain check" }));

    await waitFor(() => expect(secretStatus).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.getByText("saved")).toBeInTheDocument());
    expect(screen.queryByText(/Couldn't check the keychain/i)).not.toBeInTheDocument();
    // The key exists, so removing it must be offered again.
    expect(
      screen.getByRole("button", { name: "Remove RESEND_API_KEY" }),
    ).toBeInTheDocument();
  });
});
