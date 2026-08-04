import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";

import { TooltipProvider } from "@/components/ui/tooltip";
import type { DetectedClient } from "@/lib/types";
import { AppSidebar } from "./AppSidebar";

const getSavingsSummary = vi.fn();
const listQuarantined = vi.fn();

vi.mock("@/lib/api", () => ({
  gatherDiagnostics: vi.fn(),
  getSavingsSummary: (...args: unknown[]) => getSavingsSummary(...args),
  listQuarantined: (...args: unknown[]) => listQuarantined(...args),
  openDataDir: vi.fn(),
}));

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("1.0.0"),
}));

vi.mock("@/lib/updater", () => ({
  checkForUpdate: vi.fn().mockResolvedValue({ kind: "current" }),
  installUpdate: vi.fn(),
}));

vi.mock("@/components/ShareDialog", () => ({
  ShareDialog: ({ trigger }: { trigger: ReactNode }) => trigger,
}));

function client(overrides: Partial<DetectedClient> = {}): DetectedClient {
  return {
    id: "cursor",
    name: "Cursor",
    usesConnectors: false,
    configPath: "/tmp/cursor.json",
    configExists: true,
    appPresent: true,
    servers: [],
    pluginServers: [],
    gatewayInstalled: false,
    entryState: "absent",
    error: null,
    ...overrides,
  };
}

beforeEach(() => {
  getSavingsSummary.mockReset();
  listQuarantined.mockReset();
  getSavingsSummary.mockResolvedValue({
    tokensSaved: 0,
    listLoads: 0,
    peakCatalog: 0,
    sinceTs: 0,
  });
  listQuarantined.mockResolvedValue([]);
});

describe("AppSidebar accessibility", () => {
  it("names both navigation landmarks and exposes client selection state", () => {
    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client()]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId="cursor"
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    expect(screen.getByRole("navigation", { name: "Views" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Clients" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /cursor/i })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("exposes whether the not-detected client list is expanded", async () => {
    render(
      <TooltipProvider>
        <AppSidebar
          clients={[client(), client({ id: "zed", name: "Zed", appPresent: false })]}
          registry={null}
          onRegistryChange={vi.fn()}
          selectedClientId={null}
          onSelectClient={vi.fn()}
          view="servers"
          onSelectView={vi.fn()}
          onReplayOnboarding={vi.fn()}
        />
      </TooltipProvider>,
    );

    const toggle = screen.getByRole("button", { name: /not detected/i });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("button", { name: /zed/i })).toBeInTheDocument();
  });
});
