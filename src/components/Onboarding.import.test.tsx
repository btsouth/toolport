import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { importServers, listStacks, previewImportServers } from "@/lib/api";
import type { DetectedClient, ProbeResult, Registry } from "@/lib/types";
import { Onboarding } from "./Onboarding";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    listStacks: vi.fn(),
    previewImportServers: vi.fn(),
    importServers: vi.fn(),
  };
});
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
vi.mock("@/components/ClientLogo", () => ({ ClientLogo: () => null }));

const stdio = (name: string) => ({
  name,
  transport: "stdio",
  command: "npx",
  args: [name],
  envKeys: [],
  url: null,
});

const client = {
  id: "claude-code",
  name: "Claude Code",
  appPresent: true,
  configExists: true,
  gatewayInstalled: true,
  servers: [stdio("memory"), stdio("github")],
  pluginServers: [],
} as unknown as DetectedClient;

const empty: Registry = {
  version: 1,
  servers: [],
  profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
  activeProfileId: "default",
};

const withServers = (enabled: string[]): Registry => ({
  ...empty,
  servers: [
    {
      id: "memory",
      name: "memory",
      transport: "stdio",
      command: "npx",
      args: [],
      env: [],
    },
    {
      id: "github",
      name: "github",
      transport: "stdio",
      command: "npx",
      args: [],
      env: [],
    },
  ],
  profiles: [{ id: "default", name: "Default", enabledServerIds: enabled }],
});

const props = {
  clients: [client],
  onRegistryChange: vi.fn(),
  onClientsRefresh: vi.fn(),
  onBrowseCatalog: vi.fn(),
  onOpenPlayground: vi.fn(),
  onOpenRules: vi.fn(),
  onFinish: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listStacks).mockResolvedValue([]);
});

describe("Onboarding import copy", () => {
  async function importBoth(result: Registry) {
    vi.mocked(previewImportServers).mockResolvedValue(
      ["memory", "github"].map((name, i) => ({
        key: String(i),
        name,
        transport: "stdio",
        command: "npx",
        args: [name],
        isNew: true,
      })),
    );
    vi.mocked(importServers).mockResolvedValue(result);
    const user = userEvent.setup();
    render(
      <Onboarding
        {...props}
        initialStep={1}
        registry={empty}
        onProbe={vi.fn().mockResolvedValue([])}
      />,
    );
    await user.click(
      await screen.findByRole("button", { name: /Import 2 from your clients/ }),
    );
    await user.click(await screen.findByRole("button", { name: "Import 2 servers" }));
  }

  it("says the imported servers were turned on", async () => {
    await importBoth(withServers(["memory", "github"]));
    expect(
      await screen.findByText("Imported 2 servers and turned them on."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/now manages/)).not.toBeInTheDocument();
  });

  it("does not claim servers it could not turn on", async () => {
    await importBoth(withServers(["memory"]));
    expect(
      await screen.findByText(
        "Imported 2 servers, 1 turned on. Turn on the rest from Servers.",
      ),
    ).toBeInTheDocument();
  });

  it("counts serving servers on the done step and names the ones needing sign-in", async () => {
    const probe: ProbeResult[] = [
      { serverId: "memory", ok: true, toolCount: 9, error: null, authRequired: false },
      { serverId: "github", ok: false, toolCount: 0, error: "401", authRequired: true },
    ];
    render(
      <Onboarding
        {...props}
        initialStep={3}
        registry={withServers(["memory", "github"])}
        onProbe={vi.fn().mockResolvedValue(probe)}
      />,
    );
    expect(
      await screen.findByText(
        /Toolport is serving 1 server to 1 connected tool, and 1 needs sign-in\./,
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/now manages/)).not.toBeInTheDocument();
  });
});
