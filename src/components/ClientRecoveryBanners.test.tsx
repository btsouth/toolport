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
      "Cursor is disconnected. API_KEY for keyring-echo could not be read. The config has a placeholder for now.",
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
  expect(message).toHaveClass("whitespace-pre-wrap", "break-words");
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
  first.unmount();
  api.listClientRecoveries.mockResolvedValue([recovery({ dismissed: true })]);
  render(<ClientRecoveryBanners refreshKey="reopened" />);
  await userEvent.click(await screen.findByRole("button", { name: "Show recovery" }));
  expect(api.dismissClientRecovery).toHaveBeenCalledWith("receipt-cursor", false);
  expect(await screen.findByRole("button", { name: "Retry" })).toBeEnabled();
});

it("shows multiple clients once and constrains a 20-value message without truncating it", async () => {
  const missing = Array.from({ length: 20 }, (_, index) => ({
    server: `server-${index}`,
    key: `API_KEY_${index}`,
  }));
  const message = missing.map((item) => `${item.key} for ${item.server}`).join("; ");
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
  const complete = screen.getByText(message);
  expect(complete).toHaveAttribute("tabindex", "0");
  expect(complete).toHaveClass("max-h-52", "overflow-y-auto");
  expect(complete).toHaveTextContent("API_KEY_19 for server-19");
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
