import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Onboarding } from "./Onboarding";
import type { DetectedClient, Registry } from "@/lib/types";

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
  onOpenTools: vi.fn(),
  onFinish: vi.fn(),
};

describe("Onboarding dialog accessibility", () => {
  it("names the dialog after the current step and updates as steps advance", async () => {
    const probe = deferred<[]>();
    const user = userEvent.setup();
    render(<Onboarding {...props} onProbe={() => probe.promise} />);

    // Welcome
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Welcome to Toolport");

    await user.click(screen.getByRole("button", { name: /Set up MCP servers/ }));
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

  it("offers reviewed client import and the catalog from the add step", async () => {
    const user = userEvent.setup();
    render(
      <Onboarding {...props} initialStep={1} onProbe={vi.fn().mockResolvedValue([])} />,
    );
    expect(screen.getByText(/import your existing servers/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Browse the full catalog" }));
    expect(props.onBrowseCatalog).toHaveBeenCalledOnce();
    await user.click(
      screen.getByRole("button", { name: "Review and connect your clients" }),
    );
    expect(screen.getByRole("dialog")).toHaveAccessibleName("Connect a client");
  });
});
