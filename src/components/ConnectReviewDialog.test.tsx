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
