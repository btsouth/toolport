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
  it("explains why a removed shared server's personal original stays off", () => {
    const view = renderRow(false, undefined, { ...server, teamRouteRemoved: true });
    expect(
      screen.getByText(
        "Removed or disabled by the team. Your personal server stays off.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Toggle Example" })).not.toBeChecked();
    view.unmount();
    renderRow(true, health({ ok: true }), { ...server, teamRouteRemoved: true });
    expect(screen.queryByText(/Your personal server stays off/)).not.toBeInTheDocument();
  });
  it("names the provider for a launch credential on the server card", () => {
    renderRow(false, undefined, {
      ...server,
      launch: {
        inputs: [
          {
            key: "KEY",
            label: "Key",
            secret: true,
            required: true,
            source: { ref: "op://v/i/key" },
          },
        ],
        bindings: [],
      },
    });
    expect(screen.getByText("Keys from 1Password")).toBeInTheDocument();
  });

  it.each([
    ["endpoint", "Needs sign-in"],
    ["service_credential", "Service key required"],
  ] as const)("shows %s auth ownership", (authTarget, text) => {
    renderRow(true, health({ authRequired: true, authTarget }));
    expect(screen.getByText(text)).toBeInTheDocument();
  });

  it.each([
    ["endpoint", "Sign in"],
    ["service_credential", "Edit service key"],
  ] as const)("offers a concrete %s action", (authTarget, label) => {
    renderRow(true, health({ authRequired: true, authTarget }));
    expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
  });

  it.each([
    ["Server disabled", false, undefined],
    ["Checking connection", true, undefined],
    ["Ready, 2 tools", true, health({ ok: true, toolCount: 2 })],
    ["Needs sign-in", true, health({ authRequired: true })],
    ["connection refused", true, health({ error: "connection refused" })],
  ] as const)("announces %s", (label, enabled, result) => {
    const view = renderRow(enabled, result);

    expect(screen.getByRole("status", { name: label })).toBeInTheDocument();
    view.unmount();
  });

  it.each(["timeout", "unavailable", "server_error"] as const)(
    "shows a %s reason and recovery without expansion",
    (kind) => {
      const onReprobe = vi.fn();
      render(
        <TooltipProvider>
          <RegistryServerRow
            server={server}
            registry={null}
            enabled
            health={health({ failure: { kind }, error: "full server output" })}
            onToggle={vi.fn()}
            onRemove={vi.fn()}
            onRegistryChange={vi.fn()}
            onReprobe={onReprobe}
          />
        </TooltipProvider>,
      );
      expect(
        screen.getByText(
          {
            timeout: "Timed out",
            unavailable: "Unreachable",
            server_error: "Server failed",
          }[kind],
        ),
      ).toBeVisible();
      screen.getByRole("button", { name: "Retry" }).click();
      expect(onReprobe).toHaveBeenCalledOnce();
      act(() => screen.getByRole("button", { name: "View log" }).click());
      expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute(
        "aria-selected",
        "true",
      );
      expect(screen.getAllByText("full server output", { exact: true })).toHaveLength(2);
    },
  );

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
