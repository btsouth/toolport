import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { importServers } from "@/lib/api";
import type { DetectedClient, ProbeResult, Registry } from "@/lib/types";
import { Onboarding } from "./Onboarding";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
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
      url: null,
      source: null,
    },
    {
      id: "github",
      name: "github",
      transport: "stdio",
      command: "npx",
      args: [],
      env: [],
      url: null,
      source: null,
    },
  ],
  profiles: [{ id: "default", name: "Default", enabledServerIds: enabled }],
});

const props = {
  clients: [client],
  onRegistryChange: vi.fn(),
  onClientsRefresh: vi.fn(),
  onBrowseCatalog: vi.fn(),
  onOpenTools: vi.fn(),
  onFinish: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
});

describe("Onboarding import copy", () => {
  it("routes native definitions to client connection without a separate import", async () => {
    const user = userEvent.setup();
    render(
      <Onboarding
        {...props}
        clients={[{ ...client, gatewayInstalled: false }]}
        initialStep={1}
        registry={empty}
        onProbe={vi.fn().mockResolvedValue([])}
      />,
    );
    await user.click(
      await screen.findByRole("button", { name: "Review and connect your clients" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Connect a client" }),
    ).toBeInTheDocument();
    expect(importServers).not.toHaveBeenCalled();
  });

  it("keeps unresolved credentials from claiming setup is ready", async () => {
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
      await screen.findByRole("heading", { name: "Some servers need attention" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "You're set up" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/now manages/)).not.toBeInTheDocument();
  });
});
