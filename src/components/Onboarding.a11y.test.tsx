import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Onboarding } from "./Onboarding";
import { listStacks } from "@/lib/api";
import type { DetectedClient, Registry } from "@/lib/types";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    listStacks: vi.fn().mockResolvedValue([]),
  };
});
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
// ClientLogo loads vendored SVGs via import.meta.glob; stub it so the test stays focused.
vi.mock("@/components/ClientLogo", () => ({ ClientLogo: () => null }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

const registry = {
  version: 1,
  servers: [{ id: "server-1", name: "Server", transport: "stdio" }],
  profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
  activeProfileId: "default",
} as unknown as Registry;

const client = {
  id: "cursor",
  name: "Cursor",
  appPresent: true,
  gatewayInstalled: true,
  servers: [],
  pluginServers: [],
} as unknown as DetectedClient;

const props = {
  clients: [client],
  registry,
  onRegistryChange: vi.fn(),
  onClientsRefresh: vi.fn(),
  onBrowseCatalog: vi.fn(),
  onOpenPlayground: vi.fn(),
  onFinish: vi.fn(),
};

describe("Onboarding dialog accessibility", () => {
  it("names the dialog after the current step and updates as steps advance", async () => {
    const probe = deferred<[]>();
    const user = userEvent.setup();
    render(<Onboarding {...props} onProbe={() => probe.promise} />);

    // Welcome
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Welcome to Toolport");

    await user.click(screen.getByRole("button", { name: /Get started/ }));
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Add your first servers");

    await user.click(screen.getByRole("button", { name: /I'll add servers later/ }));
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Connect a client");

    // Done: the name tracks the live verification state, then settles on success.
    await user.click(screen.getByRole("button", { name: /Skip for now/ }));
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Checking your setup");

    probe.resolve([]);
    await waitFor(() =>
      expect(screen.getByRole("dialog")).toHaveAccessibleName("You're set up"),
    );
  });

  it("names the dialog for the Join Team step", async () => {
    const user = userEvent.setup();
    render(<Onboarding {...props} onProbe={vi.fn().mockResolvedValue([])} />);

    await user.click(
      screen.getByRole("button", { name: /Joining a team\? Enter your invite code/ }),
    );
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Join your team");
  });

  it("exposes the selected state of the role/stack choice buttons", async () => {
    vi.mocked(listStacks).mockResolvedValue([
      { id: "dev", name: "Developer", description: "A dev stack", servers: [] },
      { id: "ops", name: "Operations", description: "An ops stack", servers: [] },
    ]);
    const user = userEvent.setup();
    render(
      <Onboarding {...props} initialStep={1} onProbe={vi.fn().mockResolvedValue([])} />,
    );

    const dev = await screen.findByRole("button", { name: "Developer" });
    const ops = screen.getByRole("button", { name: "Operations" });
    // Neither is selected initially.
    expect(dev).toHaveAttribute("aria-pressed", "false");
    expect(ops).toHaveAttribute("aria-pressed", "false");

    // Selecting a role presses only that button.
    await user.click(dev);
    expect(dev).toHaveAttribute("aria-pressed", "true");
    expect(ops).toHaveAttribute("aria-pressed", "false");

    // Clicking again toggles it back off (deselect).
    await user.click(dev);
    expect(dev).toHaveAttribute("aria-pressed", "false");
  });
});
