import { beforeEach, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  CLIENT_RECOVERY_SCAN_EVENT,
  ClientRecoveryBanners,
} from "./ClientRecoveryBanners";
import type { ClientRecovery, DiscardConfirmation } from "@/lib/api";

const api = vi.hoisted(() => ({
  listClientRecoveries: vi.fn(),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
  discardClientRecovery: vi.fn(),
  prepareClientRecoveryDiscard: vi.fn(),
  refreshClientRecoveryNotice: vi.fn(),
  refreshClientRecoveryStatus: vi.fn(),
}));
vi.mock("@/lib/api", () => api);

function recovery(overrides: Partial<ClientRecovery> = {}): ClientRecovery {
  return {
    id: "receipt-cursor",
    clientId: "cursor",
    clientName: "Cursor",
    path: "/fixture/.cursor/mcp.json",
    backupPath: "/fixture/backups/cursor/original.json",
    dismissed: false,
    missing: [{ server: "keyring-echo", key: "API_KEY" }],
    message:
      "Cursor is disconnected.\n\nIts original config is back, except for values Toolport could not read because your keyring is locked or unavailable:\n\n• API_KEY for keyring-echo\n\nThe config has placeholders for these values for now.",
    description: "Its original config is back. These values still have placeholders:",
    valueLines: ["API_KEY for keyring-echo: keyring locked"],
    discardValues: ["API_KEY for keyring-echo"],
    error: null,
    canDiscard: true,
    canRetry: true,
    discardStarted: false,
    discardAction: "Discard these saved values",
    cleanup: false,
    instructions: "Unlock your keyring, then choose Retry.",
    retention:
      "The recovery backup is kept at /fixture/backups/cursor/original.json until recovery succeeds or you remove Toolport data. It does not expire.",
    details: "API_KEY for keyring-echo: TOOLPORT_MISSING_CREDENTIAL_0",
    ...overrides,
  };
}

function result(pending: ClientRecovery | null = null) {
  return {
    id: "receipt-cursor",
    clientId: "cursor",
    clientName: "Cursor",
    restored: pending ? 0 : 1,
    accounted: 0,
    remaining: pending ? 1 : 0,
    message: `Cursor: saved values restored: ${pending ? 0 : 1}; already resolved or removed: 0; still missing: ${pending ? 1 : 0}.`,
    pending,
    refreshed: false,
  };
}

const DISCARD_SUCCESS =
  "Cursor's saved recovery copies were deleted. The current config at /fixture/.cursor/mcp.json was not changed. Any temporary text for missing values is still there; re-enter those values in the client if needed.";

function prepared(overrides: Partial<DiscardConfirmation> = {}): DiscardConfirmation {
  return {
    manifest: "manifest-1",
    heading: "Discard these saved values?",
    description:
      "Toolport will permanently delete recovery copies for Cursor. Saved values: 1. Backup files: 2.\n\nThe current client config at /fixture/.cursor/mcp.json will not change. Any temporary text for missing values will stay there; re-enter those values in the client if needed. Toolport's server credentials will not change.",
    lines: [
      "Value: API_KEY for keyring-echo",
      "Backup file: original-1111.json",
      "Backup file: 1794746461499-mcp.json",
    ],
    location: "Recovery files to delete:\n/fixture/backups/cursor",
    actionLabel: "Discard these saved values",
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  api.retryClientRecovery.mockResolvedValue(result());
  api.dismissClientRecovery.mockResolvedValue(undefined);
  api.prepareClientRecoveryDiscard.mockResolvedValue(prepared());
  api.discardClientRecovery.mockResolvedValue(DISCARD_SUCCESS);
  api.refreshClientRecoveryNotice.mockResolvedValue({
    rows: [],
    focusId: null,
    message: null,
  });
  api.refreshClientRecoveryStatus.mockResolvedValue(undefined);
});

it("shows one complete selectable inline message, keeping markers in expandable details", async () => {
  render(<ClientRecoveryBanners refreshKey="initial" />);
  const notice = await screen.findByRole("region", {
    name: "Client config recovery: Cursor",
  });
  expect(within(notice).getAllByText(/Cursor is disconnected/)).toHaveLength(1);
  expect(notice).toHaveClass("select-text");
  const message = within(notice).getByText(/Cursor is disconnected/);
  expect(message).toHaveClass("whitespace-pre-wrap", "break-words", "font-semibold");
  expect(within(notice).getByText(/Its original config is back/)).toHaveClass(
    "whitespace-pre-wrap",
    "break-words",
  );
  expect(within(notice).getByText(/The recovery backup is kept/)).toBeVisible();
  expect(message).not.toHaveClass("truncate");
  const technical = within(notice).getByText(/TOOLPORT_MISSING_CREDENTIAL_0/);
  expect(technical.closest("details")).not.toHaveAttribute("open");
  await userEvent.click(within(notice).getByText("Details"));
  expect(technical.closest("details")).toHaveAttribute("open");
});

it("retries only the selected receipt and replaces its warning with success", async () => {
  const onRecovered = vi.fn();
  render(<ClientRecoveryBanners refreshKey="initial" onRecovered={onRecovered} />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(api.retryClientRecovery).toHaveBeenCalledWith("receipt-cursor");
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Cursor: saved values restored: 1; already resolved or removed: 0; still missing: 0.",
  );
  expect(screen.getByRole("status")).toHaveFocus();
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
  expect(onRecovered).toHaveBeenCalledOnce();
});

it("keeps the recovery visible when retry is still locked", async () => {
  api.retryClientRecovery.mockResolvedValue(result(recovery()));
  render(<ClientRecoveryBanners refreshKey="initial" />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Cursor: saved values restored: 0; already resolved or removed: 0; still missing: 1.",
  );
  expect(screen.getByRole("region")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
});

it("reloads client rows when a still pending Retry refreshed waiting client configs", async () => {
  api.retryClientRecovery.mockResolvedValue({ ...result(recovery()), refreshed: true });
  const onRecovered = vi.fn();
  render(<ClientRecoveryBanners refreshKey="initial" onRecovered={onRecovered} />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent("still missing: 1.");
  expect(screen.getByRole("region")).toBeInTheDocument();
  expect(onRecovered).toHaveBeenCalledOnce();
});

it("requires an informed dismissal and keeps the receipt discoverable after remount", async () => {
  const first = render(<ClientRecoveryBanners refreshKey="initial" />);
  await userEvent.click(await screen.findByRole("button", { name: "Dismiss" }));
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("/fixture/backups/cursor/original.json");
  expect(dialog).toHaveTextContent("It does not expire.");
  expect(api.dismissClientRecovery).not.toHaveBeenCalled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Dismiss" }));
  expect(api.dismissClientRecovery).toHaveBeenCalledWith("receipt-cursor", true);
  expect(await screen.findByRole("button", { name: "Show recovery" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Show recovery" })).toHaveFocus();
  first.unmount();
  api.listClientRecoveries.mockResolvedValue([recovery({ dismissed: true })]);
  render(<ClientRecoveryBanners refreshKey="reopened" />);
  await userEvent.click(await screen.findByRole("button", { name: "Show recovery" }));
  expect(api.dismissClientRecovery).toHaveBeenCalledWith("receipt-cursor", false);
  expect(await screen.findByRole("button", { name: "Retry" })).toBeEnabled();
});

it("shows multiple clients once and a bounded keyboard-scrollable list of 20 values", async () => {
  const missing = Array.from({ length: 20 }, (_, index) => ({
    server: `server-${index}`,
    key: `API_KEY_${index}`,
  }));
  const body = missing.map((item) => `• ${item.key} for ${item.server}`).join("\n");
  const message = `Cursor is disconnected.\n\n${body}`;
  api.listClientRecoveries.mockResolvedValue([
    recovery({
      missing,
      message,
      valueLines: missing.map((item) => `${item.key} for ${item.server}`),
    }),
    recovery({
      id: "receipt-claude",
      clientId: "claude-code",
      clientName: "Claude Code",
    }),
  ]);
  const { rerender } = render(<ClientRecoveryBanners refreshKey="initial" />);
  expect(await screen.findAllByRole("region")).toHaveLength(2);
  const notice = screen.getAllByRole("region")[0];
  const complete = within(notice).getByRole("list", { name: "Missing saved values" });
  expect(complete).toHaveAttribute("tabindex", "0");
  expect(complete).toHaveClass("max-h-[240px]", "overflow-y-auto");
  expect(within(complete).getAllByRole("listitem")).toHaveLength(20);
  for (const value of missing)
    expect(complete).toHaveTextContent(`${value.key} for ${value.server}`);
  expect(complete.closest("section")).not.toHaveClass("overflow-y-auto");
  rerender(<ClientRecoveryBanners clientId="cursor" refreshKey="initial" />);
  expect(screen.getAllByRole("region")).toHaveLength(1);
});

it("uses keyboard order values, Retry, Dismiss, Discard, Details", async () => {
  const user = userEvent.setup();
  render(<ClientRecoveryBanners refreshKey="initial" />);
  const retry = await screen.findByRole("button", { name: "Retry" });
  await user.tab();
  expect(screen.getByRole("list", { name: "Missing saved values" })).toHaveFocus();
  await user.tab();
  expect(retry).toHaveFocus();
  await user.tab();
  expect(screen.getByRole("button", { name: "Dismiss" })).toHaveFocus();
  await user.tab();
  expect(
    screen.getByRole("button", { name: "Discard these saved values" }),
  ).toHaveFocus();
  await user.tab();
  expect(screen.getByText("Details")).toHaveFocus();
});

it("keeps unsplit messages readable under the recovery heading", async () => {
  api.listClientRecoveries.mockResolvedValue([
    recovery({ message: "Legacy complete recovery message." }),
  ]);
  render(<ClientRecoveryBanners refreshKey="initial" />);
  expect(await screen.findByText("Client config recovery")).toHaveClass("font-semibold");
  expect(screen.getByText("Legacy complete recovery message.")).toHaveClass(
    "whitespace-pre-wrap",
    "break-words",
  );
});

it("closes only the retried receipt's details while preserving the other receipt", async () => {
  api.listClientRecoveries.mockResolvedValue([
    recovery(),
    recovery({
      id: "receipt-claude",
      clientId: "claude-code",
      clientName: "Claude Code",
    }),
  ]);
  api.retryClientRecovery.mockResolvedValue(result(recovery()));
  render(<ClientRecoveryBanners refreshKey="initial" />);
  const notices = await screen.findAllByRole("region");
  for (const notice of notices)
    await userEvent.click(within(notice).getByText("Details"));
  expect(notices[0].querySelector("details")).toHaveAttribute("open");
  expect(notices[1].querySelector("details")).toHaveAttribute("open");
  await userEvent.click(within(notices[0]).getByRole("button", { name: "Retry" }));
  expect(notices[0].querySelector("details")).not.toHaveAttribute("open");
  expect(notices[1].querySelector("details")).toHaveAttribute("open");
});

it("confirms the exact prepared Discard inventory in one bounded list and sends its manifest", async () => {
  const onRecovered = vi.fn();
  render(<ClientRecoveryBanners refreshKey="discard" onRecovered={onRecovered} />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Discard these saved values" }),
  );
  expect(api.prepareClientRecoveryDiscard).toHaveBeenCalledWith("receipt-cursor");
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("Discard these saved values?");
  expect(dialog).toHaveTextContent("Saved values: 1. Backup files: 2.");
  expect(dialog).toHaveTextContent(
    "Any temporary text for missing values will stay there",
  );
  expect(dialog).toHaveTextContent("Recovery files to delete:");
  expect(dialog).toHaveTextContent("/fixture/backups/cursor");
  // Discard must never reuse the Dismiss retention promise.
  expect(dialog).not.toHaveTextContent("It does not expire");
  expect(dialog).not.toHaveTextContent("until recovery succeeds");
  const lists = within(dialog).getAllByRole("list");
  expect(lists).toHaveLength(1);
  expect(lists[0]).toHaveAccessibleName("Saved values and files to delete");
  expect(lists[0]).toHaveClass("max-h-[240px]", "overflow-y-auto");
  expect(
    within(lists[0])
      .getAllByRole("listitem")
      .map((item) => item.textContent),
  ).toEqual(prepared().lines);
  expect(api.discardClientRecovery).not.toHaveBeenCalled();
  api.listClientRecoveries.mockResolvedValue([]);
  await userEvent.click(
    within(dialog).getByRole("button", { name: "Discard these saved values" }),
  );
  expect(api.discardClientRecovery).toHaveBeenCalledWith("receipt-cursor", "manifest-1");
  const status = await screen.findByText(DISCARD_SUCCESS);
  expect(status).toHaveAttribute("role", "status");
  await waitFor(() => expect(status).toHaveFocus());
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
  expect(onRecovered).toHaveBeenCalledOnce();
});

it("cancelling a prepared Discard deletes nothing and returns focus to Discard", async () => {
  render(<ClientRecoveryBanners refreshKey="discard-cancel" />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Discard these saved values" }),
  );
  const dialog = await screen.findByRole("dialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(api.discardClientRecovery).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Discard these saved values" }),
    ).toHaveFocus(),
  );
});

it("shows a failed Discard once on its own banner and focuses the refreshed Finish action", async () => {
  const error =
    "Discard needs to finish. Some saved copies may already be gone. Choose Finish discarding.";
  api.discardClientRecovery.mockRejectedValue(error);
  render(<ClientRecoveryBanners refreshKey="discard-error" />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Discard these saved values" }),
  );
  api.listClientRecoveries.mockResolvedValue([
    recovery({
      canRetry: false,
      discardStarted: true,
      discardAction: "Finish discarding",
    }),
  ]);
  await userEvent.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Discard these saved values",
    }),
  );
  expect(await screen.findByText(error)).toHaveAttribute("role", "status");
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(screen.getAllByText(error)).toHaveLength(1);
  const finish = await screen.findByRole("button", { name: "Finish discarding" });
  await waitFor(() => expect(finish).toHaveFocus());
  // Same emphasis as GTK's destructive-action Finish button.
  expect(finish).toHaveAttribute("data-variant", "destructive");
  // A started Discard refuses restoration and cannot be merely dismissed.
  expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Dismiss" })).not.toBeInTheDocument();
});

it("Finish discarding prepares a fresh manifest of the remaining files", async () => {
  api.listClientRecoveries.mockResolvedValue([
    recovery({
      canRetry: false,
      discardStarted: true,
      discardAction: "Finish discarding",
    }),
  ]);
  api.prepareClientRecoveryDiscard.mockResolvedValue(
    prepared({
      manifest: "manifest-remaining",
      heading: "Finish discarding?",
      actionLabel: "Finish discarding",
      lines: ["Backup file: original-1111.json"],
    }),
  );
  render(<ClientRecoveryBanners refreshKey="finish" />);
  await userEvent.click(await screen.findByRole("button", { name: "Finish discarding" }));
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("Finish discarding?");
  expect(within(dialog).getAllByRole("listitem")).toHaveLength(1);
  await userEvent.click(
    within(dialog).getByRole("button", { name: "Finish discarding" }),
  );
  expect(api.discardClientRecovery).toHaveBeenCalledWith(
    "receipt-cursor",
    "manifest-remaining",
  );
});

it("a failed Discard preparation keeps the row and focuses its Discard action", async () => {
  api.prepareClientRecoveryDiscard.mockRejectedValue(
    "Toolport could not read the recovery folder. Check its permissions, then choose Discard these saved values.",
  );
  render(<ClientRecoveryBanners refreshKey="prepare-error" />);
  const discard = await screen.findByRole("button", {
    name: "Discard these saved values",
  });
  await userEvent.click(discard);
  expect(await screen.findByText(/could not read the recovery folder/)).toBeVisible();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(api.discardClientRecovery).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Discard these saved values" }),
    ).toHaveFocus(),
  );
});

it("replaces the saved diagnostic with the latest Retry error, matching GTK", async () => {
  api.listClientRecoveries.mockResolvedValue([
    recovery({ error: "Previous config diagnostic" }),
  ]);
  api.retryClientRecovery.mockRejectedValue("Current safe Retry diagnostic");
  render(<ClientRecoveryBanners refreshKey="retry-diagnostic" />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Previous config diagnostic",
  );
  await userEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Current safe Retry diagnostic",
  );
  expect(screen.queryByText("Previous config diagnostic")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

function errorRow(overrides: Partial<ClientRecovery> = {}): ClientRecovery {
  const description =
    overrides.description ??
    "Toolport could not read this recovery file. Check its permissions.";
  return recovery({
    id: "bad",
    clientName: "Devin Desktop (Cascade)",
    clientId: "windsurf",
    backupPath: "/fixture/backups/windsurf/original-2222.json",
    message: `Devin Desktop (Cascade) recovery needs attention.\n\n${description}`,
    description: "Toolport could not read this recovery file. Check its permissions.",
    error: "Toolport could not read this recovery file. Check its permissions.",
    canDiscard: false,
    valueLines: [],
    missing: [],
    ...overrides,
  });
}

it("keeps good recoveries beside a per-record error and refreshes that row through the backend", async () => {
  api.listClientRecoveries.mockResolvedValue([errorRow(), recovery()]);
  render(<ClientRecoveryBanners refreshKey="bad-row" />);
  const notices = await screen.findAllByRole("region");
  expect(notices).toHaveLength(2);
  // Untrusted rows cannot be dismissed or discarded; their provenance guard stays.
  expect(
    within(notices[0]).queryByRole("button", { name: "Discard these saved values" }),
  ).not.toBeInTheDocument();
  expect(
    within(notices[0]).queryByRole("button", { name: "Dismiss" }),
  ).not.toBeInTheDocument();
  api.refreshClientRecoveryNotice.mockResolvedValue({
    rows: [errorRow(), recovery()],
    focusId: "bad",
    message: null,
  });
  await userEvent.click(within(notices[0]).getByRole("button", { name: "Retry" }));
  expect(api.refreshClientRecoveryNotice).toHaveBeenCalledWith(
    "bad",
    "windsurf",
    "/fixture/backups/windsurf/original-2222.json",
  );
  expect(api.retryClientRecovery).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      within(screen.getAllByRole("region")[0]).getByRole("button", { name: "Retry" }),
    ).toHaveFocus(),
  );
  expect(screen.queryByText(/can be read again|refreshed/)).not.toBeInTheDocument();
});

it("an unreadable recovery file offers only Discard recovery file, never Dismiss", async () => {
  const unreadable = errorRow({
    description: "This recovery file is malformed. Its saved values have been kept.",
    error: "This recovery file is malformed. Its saved values have been kept.",
    canDiscardFile: true,
    discardAction: "Discard recovery file",
  });
  api.listClientRecoveries.mockResolvedValue([unreadable, recovery()]);
  api.prepareClientRecoveryDiscard.mockResolvedValue(
    prepared({
      manifest: "manifest-file",
      heading: "Discard the unreadable recovery file for Devin Desktop (Cascade)?",
      lines: ["Backup file: original-2222.json"],
      actionLabel: "Discard recovery file",
    }),
  );
  api.discardClientRecovery.mockResolvedValue(
    "Devin Desktop (Cascade)'s unreadable recovery file was deleted. The client config was not changed.",
  );
  render(<ClientRecoveryBanners refreshKey="unreadable" />);
  const notices = await screen.findAllByRole("region");
  expect(
    within(notices[0]).queryByRole("button", { name: "Dismiss" }),
  ).not.toBeInTheDocument();
  expect(within(notices[0]).getByRole("button", { name: "Retry" })).toBeInTheDocument();
  await userEvent.click(
    within(notices[0]).getByRole("button", { name: "Discard recovery file" }),
  );
  expect(api.prepareClientRecoveryDiscard).toHaveBeenCalledWith("bad");
  const dialog = await screen.findByRole("dialog");
  expect(
    within(dialog)
      .getAllByRole("listitem")
      .map((item) => item.textContent),
  ).toEqual(["Backup file: original-2222.json"]);
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  await userEvent.click(
    within(dialog).getByRole("button", { name: "Discard recovery file" }),
  );
  expect(api.discardClientRecovery).toHaveBeenCalledWith("bad", "manifest-file");
  const status = await screen.findByRole("status");
  expect(status).toHaveTextContent("unreadable recovery file was deleted");
  expect(screen.getAllByRole("region")).toHaveLength(1);
  // The result line takes focus without selecting its text, as in GTK.
  await waitFor(() => expect(status).toHaveFocus());
  expect(window.getSelection()?.toString() ?? "").toBe("");
});

it("a file error replaced by a folder error focuses the replacement and reports no success", async () => {
  api.listClientRecoveries.mockResolvedValue([errorRow()]);
  render(<ClientRecoveryBanners refreshKey="replacement" />);
  await screen.findByRole("region");
  const folder = errorRow({
    id: "folder-windsurf",
    backupPath: "/fixture/backups/windsurf",
    description: "Toolport could not read the recovery folder. Check its permissions.",
    error: "Toolport could not read the recovery folder. Check its permissions.",
  });
  api.refreshClientRecoveryNotice.mockResolvedValue({
    rows: [folder],
    focusId: "folder-windsurf",
    message: null,
  });
  await userEvent.click(screen.getByRole("button", { name: "Retry" }));
  const notice = await screen.findByRole("region");
  expect(notice).toHaveTextContent("could not read the recovery folder");
  await waitFor(() =>
    expect(within(notice).getByRole("button", { name: "Retry" })).toHaveFocus(),
  );
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(document.body.textContent).not.toMatch(/can be read again|refreshed/);
});

it("a genuinely removed error record gets the neutral backend message, not a read claim", async () => {
  api.listClientRecoveries.mockResolvedValue([errorRow(), recovery()]);
  render(<ClientRecoveryBanners refreshKey="removed" />);
  const notices = await screen.findAllByRole("region");
  api.refreshClientRecoveryNotice.mockResolvedValue({
    rows: [recovery()],
    focusId: null,
    message: "Devin Desktop (Cascade): recovery listing refreshed.",
  });
  await userEvent.click(within(notices[0]).getByRole("button", { name: "Retry" }));
  const status = await screen.findByText(
    "Devin Desktop (Cascade): recovery listing refreshed.",
  );
  await waitFor(() => expect(status).toHaveFocus());
  expect(screen.getAllByRole("region")).toHaveLength(1);
  expect(document.body.textContent).not.toContain("can be read again");
});

it("reports partial success without removing the remaining recovery", async () => {
  api.retryClientRecovery.mockResolvedValue({
    ...result(recovery()),
    restored: 1,
    message:
      "Cursor: saved values restored: 1; already resolved or removed: 0; still missing: 1.",
  });
  render(<ClientRecoveryBanners refreshKey="partial" />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent("saved values restored: 1");
  expect(screen.getByRole("region")).toBeInTheDocument();
});
it("does not display an unexpected worker exception or its credential", async () => {
  api.retryClientRecovery.mockRejectedValue(
    new Error("sk-credential-in-panic-must-stay-private"),
  );
  render(<ClientRecoveryBanners refreshKey="worker-error" />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Recovery did not finish reporting its result",
  );
  expect(document.body.textContent).not.toContain(
    "sk-credential-in-panic-must-stay-private",
  );
});
it("keeps one latest result in total; a new action clears it but not other rows' errors", async () => {
  const claude = recovery({
    id: "claude",
    clientId: "claude-code",
    clientName: "Claude Code",
  });
  const codex = recovery({ id: "codex", clientId: "codex", clientName: "Codex" });
  api.listClientRecoveries.mockResolvedValue([recovery(), claude, codex]);
  api.retryClientRecovery.mockImplementation(async (id: string) => {
    if (id === "codex") throw "Codex Retry diagnostic";
    return id === "claude"
      ? {
          ...result(),
          id,
          clientId: "claude-code",
          clientName: "Claude Code",
          message: "Claude Code: values put back: 1.",
        }
      : result();
  });
  render(<ClientRecoveryBanners refreshKey="two-successes" />);
  const codexNotice = (await screen.findAllByRole("region"))[2];
  await userEvent.click(within(codexNotice).getByRole("button", { name: "Retry" }));
  expect(await screen.findByText("Codex Retry diagnostic")).toBeVisible();
  await userEvent.click(
    within(screen.getAllByRole("region")[0]).getByRole("button", { name: "Retry" }),
  );
  expect(
    await screen.findByText(
      "Cursor: saved values restored: 1; already resolved or removed: 0; still missing: 0.",
    ),
  ).toBeVisible();
  await userEvent.click(
    within(screen.getAllByRole("region")[0]).getByRole("button", { name: "Retry" }),
  );
  expect(await screen.findByText("Claude Code: values put back: 1.")).toBeVisible();
  expect(screen.queryByText(/Cursor: saved values restored/)).not.toBeInTheDocument();
  expect(document.querySelectorAll('[data-recovery-focus="result"]')).toHaveLength(1);
  // The other pending row keeps its own error feedback.
  expect(screen.getByText("Codex Retry diagnostic")).toBeVisible();
  expect(screen.getAllByRole("region")).toHaveLength(1);
});

it("shows a working status with a spinner in the active row and disables every row's actions", async () => {
  let finish!: (value: ReturnType<typeof result>) => void;
  api.retryClientRecovery.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  api.listClientRecoveries.mockResolvedValue([
    recovery(),
    recovery({ id: "claude", clientId: "claude-code", clientName: "Claude Code" }),
  ]);
  render(<ClientRecoveryBanners refreshKey="busy" />);
  const [cursor, claude] = await screen.findAllByRole("region");
  await userEvent.click(within(cursor).getByRole("button", { name: "Retry" }));
  expect(await within(cursor).findByText("Working on this recovery…")).toBeVisible();
  expect(cursor).toHaveAttribute("aria-busy", "true");
  expect(within(claude).queryByText("Working on this recovery…")).not.toBeInTheDocument();
  for (const button of screen.getAllByRole("button").filter((b) => b.closest("section")))
    expect(button).toBeDisabled();
  await act(async () => finish(result(recovery())));
  expect(screen.queryByText("Working on this recovery…")).not.toBeInTheDocument();
  expect(within(cursor).getByRole("button", { name: "Retry" })).toBeEnabled();
});

it("an explicit Scan checks saved values, then relists without moving focus", async () => {
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  render(<ClientRecoveryBanners refreshKey="scan" />);
  const retry = await screen.findByRole("button", { name: "Retry" });
  retry.focus();
  api.listClientRecoveries.mockResolvedValue([
    recovery({
      valueLines: ["API_KEY for keyring-echo: ready to put back"],
      instructions: "Choose Retry to put these values back in the client config.",
    }),
  ]);
  await act(async () => {
    window.dispatchEvent(new Event(CLIENT_RECOVERY_SCAN_EVENT));
  });
  expect(api.refreshClientRecoveryStatus).toHaveBeenCalledOnce();
  expect(await screen.findByText(/ready to put back/)).toBeVisible();
  expect(screen.getByRole("button", { name: "Retry" })).toHaveFocus();
});

it("shows a failed Show recovery action on the dismissed card and restores focus", async () => {
  api.listClientRecoveries.mockResolvedValue([recovery({ dismissed: true })]);
  api.dismissClientRecovery.mockRejectedValue(new Error("sk-unsafe-worker-details"));
  render(<ClientRecoveryBanners refreshKey="show-failure" />);
  await userEvent.click(await screen.findByRole("button", { name: "Show recovery" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Recovery did not finish reporting its result",
  );
  expect(screen.getByRole("button", { name: "Show recovery" })).toHaveFocus();
  expect(document.body.textContent).not.toContain("sk-unsafe-worker-details");
});

it("keeps the row working while its Discard confirmation is open, as GTK does", async () => {
  const unreadable = errorRow({
    description: "This recovery file is malformed. Its saved values have been kept.",
    error: "This recovery file is malformed. Its saved values have been kept.",
    canDiscardFile: true,
    discardAction: "Discard recovery file",
  });
  api.listClientRecoveries.mockResolvedValue([unreadable]);
  api.prepareClientRecoveryDiscard.mockResolvedValue(
    prepared({
      heading: "Discard the unreadable recovery file for Devin Desktop (Cascade)?",
      lines: ["Backup file: original-2222.json"],
      actionLabel: "Discard recovery file",
    }),
  );
  render(<ClientRecoveryBanners refreshKey="busy-while-open" />);
  const notice = await screen.findByRole("region");
  await userEvent.click(
    within(notice).getByRole("button", { name: "Discard recovery file" }),
  );
  const dialog = await screen.findByRole("dialog");
  expect(document.querySelector("[data-recovery-busy]")).toHaveTextContent(
    "Working on this recovery…",
  );
  for (const button of notice.querySelectorAll("button[data-recovery-focus]")) {
    expect(button).toBeDisabled();
  }
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.querySelector("[data-recovery-busy]")).toBeNull());
  expect(api.discardClientRecovery).not.toHaveBeenCalled();
  await waitFor(() =>
    expect(
      within(screen.getByRole("region")).getByRole("button", {
        name: "Discard recovery file",
      }),
    ).toHaveFocus(),
  );
});

it("keeps an open Discard confirmation when a reload empties the list", async () => {
  const unreadable = errorRow({
    canDiscardFile: true,
    discardAction: "Discard recovery file",
  });
  api.listClientRecoveries.mockResolvedValue([unreadable]);
  api.prepareClientRecoveryDiscard.mockResolvedValue(
    prepared({
      lines: ["Backup file: original-2222.json"],
      actionLabel: "Discard recovery file",
    }),
  );
  const { rerender } = render(<ClientRecoveryBanners refreshKey="before" />);
  const notice = await screen.findByRole("region");
  await userEvent.click(
    within(notice).getByRole("button", { name: "Discard recovery file" }),
  );
  const dialog = await screen.findByRole("dialog");
  api.listClientRecoveries.mockResolvedValue([]);
  rerender(<ClientRecoveryBanners refreshKey="after" />);
  await waitFor(() => expect(api.listClientRecoveries).toHaveBeenCalledTimes(2));
  expect(screen.getByRole("dialog")).toBe(dialog);
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(document.querySelector("[data-recovery-busy]")).toBeNull();
});

it("says plainly when Retry leaves the keyring locked, with the backend's words", async () => {
  // The backend writes this line for both shells; GTK shows the same text.
  const locked =
    "Keyring locked. Nothing was restored. Unlock your keyring, then choose Retry.";
  api.retryClientRecovery.mockResolvedValue({ ...result(recovery()), message: locked });
  render(<ClientRecoveryBanners refreshKey="initial" />);
  const notice = await screen.findByRole("region", {
    name: "Client config recovery: Cursor",
  });
  // Every line comes verbatim from the shared recovery listing.
  const row = recovery();
  expect(within(notice).getByText(/Cursor is disconnected/)).toBeVisible();
  expect(within(notice).getByText(row.description)).toBeVisible();
  for (const line of row.valueLines) {
    expect(within(notice).getByText(line, { exact: false })).toBeVisible();
  }
  expect(within(notice).getByText(row.instructions)).toBeVisible();
  await userEvent.click(within(notice).getByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent(locked);
  expect(screen.getByRole("status")).not.toHaveTextContent(
    /values put back|still missing/,
  );
  expect(screen.getByRole("region")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
});
