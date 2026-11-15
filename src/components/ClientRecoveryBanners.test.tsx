import { beforeEach, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ClientRecoveryBanners } from "./ClientRecoveryBanners";
import type { ClientRecovery } from "@/lib/api";

const api = vi.hoisted(() => ({
  listClientRecoveries: vi.fn(),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
  discardClientRecovery: vi.fn(),
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
    message: `Cursor: restored ${pending ? 0 : 1} saved values; 0 were already resolved or removed; ${pending ? 1 : 0} still need attention.`,
    pending,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  api.retryClientRecovery.mockResolvedValue(result());
  api.dismissClientRecovery.mockResolvedValue(undefined);
  api.discardClientRecovery.mockResolvedValue(undefined);
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
    "Cursor: restored 1 saved values; 0 were already resolved or removed; 0 still need attention.",
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
    "Cursor: restored 0 saved values; 0 were already resolved or removed; 1 still need attention.",
  );
  expect(screen.getByRole("region")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
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

it("requires explicit loss confirmation before discarding and preserves the native config", async () => {
  render(<ClientRecoveryBanners refreshKey="discard" />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Discard these saved values" }),
  );
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("API_KEY for keyring-echo");
  expect(dialog).toHaveTextContent("/fixture/.cursor/mcp.json");
  expect(dialog).toHaveTextContent("current client config");
  expect(api.discardClientRecovery).not.toHaveBeenCalled();
  await userEvent.click(
    within(dialog).getByRole("button", { name: "Discard these saved values" }),
  );
  expect(api.discardClientRecovery).toHaveBeenCalledWith("receipt-cursor");
  expect(await screen.findByRole("status")).toHaveTextContent(
    "current client config was not changed",
  );
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
});
it("shows a failed Discard once on its own banner and retains Retry", async () => {
  api.discardClientRecovery.mockRejectedValue(
    "Unlock your credential store, then try Discard again.",
  );
  render(<ClientRecoveryBanners refreshKey="discard-error" />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Discard these saved values" }),
  );
  await userEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", {
      name: "Discard these saved values",
    }),
  );
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Unlock your credential store",
  );
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(
    screen.getAllByText("Unlock your credential store, then try Discard again."),
  ).toHaveLength(1);
  expect(screen.getByRole("button", { name: "Retry" })).toHaveFocus();
});
it("keeps good recoveries beside a per-record error and can refresh that row", async () => {
  const bad = recovery({
    id: "bad",
    clientName: "Saved client locations",
    clientId: "saved-locations",
    message: "Saved locations need attention.",
    description: "Repair the saved locations file.",
    error: "Repair the saved locations file.",
    canDiscard: false,
    valueLines: [],
    missing: [],
  });
  api.listClientRecoveries.mockResolvedValue([bad, recovery()]);
  render(<ClientRecoveryBanners refreshKey="bad-row" />);
  const notices = await screen.findAllByRole("region");
  expect(notices).toHaveLength(2);
  expect(
    within(notices[0]).queryByRole("button", { name: "Discard these saved values" }),
  ).not.toBeInTheDocument();
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  await userEvent.click(within(notices[0]).getByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("region")).toHaveAccessibleName(
    "Client config recovery: Cursor",
  );
  expect(api.retryClientRecovery).not.toHaveBeenCalled();
});
it("reports partial success without removing the remaining recovery", async () => {
  api.retryClientRecovery.mockResolvedValue({
    ...result(recovery()),
    restored: 1,
    message:
      "Cursor: restored 1 saved values; 0 were already resolved or removed; 1 still need attention.",
  });
  render(<ClientRecoveryBanners refreshKey="partial" />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent("restored 1 saved values");
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
it("keeps one latest success per client in both-client recovery", async () => {
  const claude = recovery({
    id: "claude",
    clientId: "claude-code",
    clientName: "Claude Code",
  });
  api.listClientRecoveries.mockResolvedValue([recovery(), claude]);
  api.retryClientRecovery.mockImplementation(async (id: string) =>
    id === "claude"
      ? {
          ...result(),
          id,
          clientId: "claude-code",
          clientName: "Claude Code",
          message: "Claude Code restored 1 saved value.",
        }
      : result(),
  );
  render(<ClientRecoveryBanners refreshKey="two-successes" />);
  const notices = await screen.findAllByRole("region");
  await userEvent.click(within(notices[0]).getByRole("button", { name: "Retry" }));
  await userEvent.click(
    within(screen.getByRole("region")).getByRole("button", { name: "Retry" }),
  );
  expect(await screen.findAllByRole("status")).toHaveLength(2);
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
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
