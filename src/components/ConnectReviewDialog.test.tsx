import { beforeEach, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
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
    registry: { servers: [] },
    outcome: { path: "/fixture/config", backup: null },
    tools: [{ name: "toolport_search_tools" }],
    servers: [{ name: "one", toolCount: 7, credentialState: "none" }],
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
  expect(screen.getByRole("button", { name: "Done" })).toBeVisible();
});
