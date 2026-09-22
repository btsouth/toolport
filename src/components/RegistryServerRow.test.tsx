import { act, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ProbeResult, ServerEntry } from "@/lib/types";
import { TooltipProvider } from "@/components/ui/tooltip";
import { RegistryServerRow } from "./RegistryServerRow";

const server: ServerEntry = {
  id: "server-1",
  name: "Example",
  transport: "stdio",
  command: "example",
  args: [],
  env: [],
  url: null,
  source: "manual",
};

function health(overrides: Partial<ProbeResult>): ProbeResult {
  return {
    serverId: server.id,
    ok: false,
    toolCount: 0,
    error: null,
    authRequired: false,
    ...overrides,
  };
}

function renderRow(enabled: boolean, result?: ProbeResult, rowServer = server) {
  return render(
    <TooltipProvider>
      <RegistryServerRow
        server={rowServer}
        registry={null}
        enabled={enabled}
        health={result}
        onToggle={vi.fn()}
        onRemove={vi.fn()}
        onRegistryChange={vi.fn()}
      />
    </TooltipProvider>,
  );
}

describe("RegistryServerRow status accessibility", () => {
  it.each([
    ["Server disabled", false, undefined],
    ["Checking connection", true, undefined],
    ["Ready, 2 tools", true, health({ ok: true, toolCount: 2 })],
    ["Authentication required", true, health({ authRequired: true })],
    ["Connection error", true, health({ error: "connection refused" })],
  ] as const)("announces %s", (label, enabled, result) => {
    const view = renderRow(enabled, result);

    expect(screen.getByRole("status", { name: label })).toBeInTheDocument();
    view.unmount();
  });

  it("announces when a launcher package is being installed", () => {
    vi.useFakeTimers();
    const view = renderRow(true, undefined, {
      ...server,
      command: "npx",
      args: ["example-package"],
    });

    act(() => vi.advanceTimersByTime(4000));

    expect(
      screen.getByRole("status", { name: "Installing the server package" }),
    ).toBeInTheDocument();
    view.unmount();
    vi.useRealTimers();
  });

  it("announces when a non-launcher server is still initializing", () => {
    vi.useFakeTimers();
    const view = renderRow(true);

    act(() => vi.advanceTimersByTime(4000));

    expect(
      screen.getByRole("status", { name: "Server initializing" }),
    ).toBeInTheDocument();
    view.unmount();
    vi.useRealTimers();
  });
});
