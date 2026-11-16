import { beforeEach, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { ConnectReviewDialog } from "./ConnectReviewDialog";
const api = vi.hoisted(() => ({ previewClientSetup: vi.fn(), migrateClient: vi.fn() }));
vi.mock("@/lib/api", () => api);
beforeEach(() => {
  vi.clearAllMocks();
  api.previewClientSetup.mockResolvedValue({
    configPath: "/fixture/config",
    backupDir: "/fixture/backups",
    revision: "reviewed",
    items: ["one", "two"].map((name) => ({
      key: name,
      name,
      transport: "stdio",
      command: name,
      args: [],
      url: null,
      envKeys: name === "two" ? ["PAT"] : [],
      isNew: true,
    })),
  });
});
it("passes only reviewed definitions and keeps failed launch or credential errors open", async () => {
  const connected = vi.fn();
  api.migrateClient.mockRejectedValue(
    new Error("two needs credentials. Client config unchanged."),
  );
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={connected}
    />,
  );
  expect(await screen.findByText(/Credentials: PAT/)).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: /two/ }));
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  expect(api.migrateClient).toHaveBeenCalledWith(
    "fixture",
    undefined,
    undefined,
    ["one"],
    "reviewed",
  );
  expect(await screen.findByRole("alert")).toHaveTextContent("Client config unchanged");
  expect(connected).not.toHaveBeenCalled();
});

it("keeps the failed selection for retry and collapses raw paths", async () => {
  api.migrateClient.mockRejectedValue(
    new Error("one could not start. Client config unchanged."),
  );
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  await screen.findByRole("button", { name: /one/ });
  await userEvent.click(screen.getByRole("button", { name: /two/ }));
  expect(screen.queryByText(/Backup saved/)).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  await screen.findByRole("alert");
  expect(screen.getByRole("button", { name: /two/ })).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  await userEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(api.migrateClient).toHaveBeenLastCalledWith(
    "fixture",
    undefined,
    undefined,
    ["one"],
    "reviewed",
  );
});
it("shows downstream tool counts and only Done on success", async () => {
  api.migrateClient.mockResolvedValue({
    registry: {
      servers: [{ id: "one", name: "one", env: [{ key: "PAT", secret: true }] }],
    },
    outcome: { path: "/fixture/config", backup: null },
    tools: [{ name: "toolport_search_tools" }],
    servers: [{ name: "one", toolCount: 7, credentialState: "stored" }],
  });
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  await screen.findByRole("button", { name: /one/ });
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  expect(await screen.findByText(/7 tools/)).toBeVisible();
  expect(
    screen.queryByRole("button", { name: "Connect to Toolport" }),
  ).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Open Credentials" }),
  ).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Done" })).toBeVisible();
});

it("puts Done in the right-aligned footer and dates the backup without seconds", async () => {
  const backupDate = new Date(2026, 10, 16, 1, 4, 37);
  api.migrateClient.mockResolvedValue({
    registry: { servers: [] },
    outcome: { path: "/fixture/config", backup: "/fixture/backups/config.bak" },
    backupDate: Math.floor(backupDate.getTime() / 1000),
    tools: [],
    servers: [],
  });
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  await screen.findByRole("button", { name: /one/ });
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  const done = await screen.findByRole("button", { name: "Done" });
  const footer = done.closest('[data-slot="dialog-footer"]');
  expect(footer).not.toBeNull();
  expect(footer).toHaveClass("justify-end");
  expect(done).not.toHaveClass("w-full");
  expect(done).toHaveAttribute("data-variant", "default");
  const expected = backupDate.toLocaleString("en", {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
  expect(screen.getByText(`Backup saved ${expected}`)).toBeInTheDocument();
});

it("puts Retry in the footer after a failed check", async () => {
  api.previewClientSetup.mockRejectedValueOnce(new Error("config unreadable"));
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  expect(await screen.findByRole("alert")).toHaveTextContent("config unreadable");
  const retry = screen.getByRole("button", { name: "Retry" });
  expect(retry.closest('[data-slot="dialog-footer"]')).not.toBeNull();
  await userEvent.click(retry);
  expect(await screen.findByRole("button", { name: /one/ })).toBeVisible();
});

it("says what the agent sees when nothing moved, as GTK does", async () => {
  api.migrateClient.mockResolvedValue({
    registry: { servers: [] },
    outcome: { path: "/fixture/config", backup: null, warnings: [] },
    tools: [],
    servers: [],
  });
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  expect(
    await screen.findByText("Selected entries move into Toolport after verification."),
  ).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  expect(await screen.findByText("Restart Fixture to load Toolport.")).toBeVisible();
  // No empty server list is drawn.
  expect(screen.queryByRole("list")).not.toBeInTheDocument();
  await userEvent.click(screen.getByText("What your agent sees"));
  expect(
    screen.getByText("No servers moved, so your agent has no new tools."),
  ).toBeVisible();
  await userEvent.click(screen.getByText("Details"));
  expect(screen.getByText("Config: /fixture/config")).toBeVisible();
});

it("shows the Codex settings Toolport will add before connecting", async () => {
  api.previewClientSetup.mockResolvedValue({
    configPath: "/fixture/config.toml",
    backupDir: "/fixture/backups",
    revision: "reviewed",
    items: [],
    gatewaySettings: ["supports_parallel_tool_calls = true", "tool_timeout_sec = 600"],
  });
  render(
    <ConnectReviewDialog
      clientId="codex"
      clientName="Codex"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  const notice = await screen.findByText(/each only if it is not set/);
  expect(notice).toBeVisible();
  expect(notice).toHaveTextContent(
    "A setting you delete stays deleted until you connect again.",
  );
  expect(screen.getByText("supports_parallel_tool_calls = true")).toBeVisible();
  expect(screen.getByText("tool_timeout_sec = 600")).toBeVisible();
  // A setting line has few spaces; it must break anywhere rather than run out of
  // the notice at narrow widths.
  expect(screen.getByText("tool_timeout_sec = 600").closest("ul")).toHaveClass(
    "wrap-anywhere",
  );
});

it("lists no settings for a client that gets none", async () => {
  render(
    <ConnectReviewDialog
      clientId="fixture"
      clientName="Fixture"
      onClose={vi.fn()}
      onConnected={vi.fn()}
    />,
  );
  await screen.findByRole("button", { name: /one/ });
  expect(screen.queryByText(/each only if it is not set/)).not.toBeInTheDocument();
});

function Opener({ onConnected = vi.fn() }: { onConnected?: () => void }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Connect Fixture
      </button>
      {open && (
        <ConnectReviewDialog
          clientId="fixture"
          clientName="Fixture"
          onClose={() => setOpen(false)}
          onConnected={onConnected}
        />
      )}
    </>
  );
}

it("opens on Cancel and returns focus to the opener after Escape or Cancel", async () => {
  render(<Opener />);
  const opener = screen.getByRole("button", { name: "Connect Fixture" });
  await userEvent.click(opener);
  await screen.findByRole("button", { name: /one/ });
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
  );
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  await waitFor(() => expect(opener).toHaveFocus());

  await userEvent.click(opener);
  await screen.findByRole("button", { name: /one/ });
  await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  await waitFor(() => expect(opener).toHaveFocus());
});

it("returns focus to the opener after Done", async () => {
  api.migrateClient.mockResolvedValue({
    registry: { servers: [] },
    outcome: { path: "/fixture/config", backup: null },
    tools: [],
    servers: [],
  });
  render(<Opener />);
  const opener = screen.getByRole("button", { name: "Connect Fixture" });
  await userEvent.click(opener);
  await screen.findByRole("button", { name: /one/ });
  await userEvent.click(screen.getByRole("button", { name: "Connect to Toolport" }));
  await userEvent.click(await screen.findByRole("button", { name: "Done" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  await waitFor(() => expect(opener).toHaveFocus());
});

it("keeps focus in the dialog and turns Close off while the check runs", async () => {
  api.migrateClient.mockReturnValue(new Promise(() => {}));
  render(<Opener />);
  await userEvent.click(screen.getByRole("button", { name: "Connect Fixture" }));
  await screen.findByRole("button", { name: /one/ });
  const connect = screen.getByRole("button", { name: "Connect to Toolport" });
  await userEvent.click(connect);
  expect(
    await screen.findByRole("button", { name: "Checking gateway..." }),
  ).toBeDisabled();
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveFocus();
  expect(screen.getByRole("button", { name: "Close" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
  await userEvent.keyboard("{Escape}");
  expect(screen.getByRole("dialog")).toBeInTheDocument();
});
