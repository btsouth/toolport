import { beforeEach, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ClientRecoveryBanners } from "./ClientRecoveryBanners";
import type { ClientRecovery } from "@/lib/api";

const api = vi.hoisted(() => ({
  listClientRecoveries: vi.fn(),
  retryClientRecovery: vi.fn(),
  dismissClientRecovery: vi.fn(),
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
    instructions: "Unlock your keyring, then choose Retry to put the values back.",
    retention:
      "The recovery backup is kept at /fixture/backups/cursor/original.json until recovery succeeds or you remove Toolport data. It does not expire.",
    details: "API_KEY for keyring-echo: TOOLPORT_MISSING_CREDENTIAL_0",
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  api.listClientRecoveries.mockResolvedValue([recovery()]);
  api.retryClientRecovery.mockResolvedValue(null);
  api.dismissClientRecovery.mockResolvedValue(undefined);
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
    "Cursor config is fully restored. The missing values are back.",
  );
  expect(screen.getByRole("status")).toHaveFocus();
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
  expect(onRecovered).toHaveBeenCalledOnce();
});

it("keeps the recovery visible when retry is still locked", async () => {
  api.retryClientRecovery.mockResolvedValue(recovery());
  render(<ClientRecoveryBanners refreshKey="initial" />);
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Toolport still could not read the missing values. Unlock your keyring, then choose Retry.",
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

it("shows multiple clients once and all 20 bullet values in natural page flow", async () => {
  const missing = Array.from({ length: 20 }, (_, index) => ({
    server: `server-${index}`,
    key: `API_KEY_${index}`,
  }));
  const body = missing.map((item) => `• ${item.key} for ${item.server}`).join("\n");
  const message = `Cursor is disconnected.\n\n${body}`;
  api.listClientRecoveries.mockResolvedValue([
    recovery({ missing, message }),
    recovery({
      id: "receipt-claude",
      clientId: "claude-code",
      clientName: "Claude Code",
    }),
  ]);
  const { rerender } = render(<ClientRecoveryBanners refreshKey="initial" />);
  expect(await screen.findAllByRole("region")).toHaveLength(2);
  const complete = screen.getByText(
    (_, element) => element?.tagName === "P" && element.textContent === body,
  );
  expect(complete).not.toHaveAttribute("tabindex");
  expect(complete).not.toHaveClass("max-h-52", "overflow-y-auto", "truncate");
  expect(complete).toHaveClass("whitespace-pre-wrap", "break-words");
  for (const value of missing)
    expect(complete).toHaveTextContent(`• ${value.key} for ${value.server}`);
  expect(complete.textContent?.split("\n")).toHaveLength(20);
  const section = complete.closest("section");
  expect(section?.querySelector('[class*="overflow-y"], [class*="max-h-"]')).toBeNull();
  rerender(<ClientRecoveryBanners clientId="cursor" refreshKey="initial" />);
  expect(screen.getAllByRole("region")).toHaveLength(1);
});

it("uses keyboard order Retry, Dismiss, Details for a short notice", async () => {
  const user = userEvent.setup();
  render(<ClientRecoveryBanners refreshKey="initial" />);
  const retry = await screen.findByRole("button", { name: "Retry" });
  await user.tab();
  expect(retry).toHaveFocus();
  await user.tab();
  expect(screen.getByRole("button", { name: "Dismiss" })).toHaveFocus();
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
  api.retryClientRecovery.mockResolvedValue(recovery());
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
