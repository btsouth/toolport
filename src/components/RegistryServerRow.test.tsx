import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ProbeResult, ServerEntry } from "@/lib/types";
import { TooltipProvider } from "@/components/ui/tooltip";
import { RegistryServerRow } from "./RegistryServerRow";
import { probeRegistered, testServer } from "@/lib/api";

vi.mock("@/lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api")>()),
  probeRegistered: vi.fn(),
  testServer: vi.fn(),
}));

vi.mock("@/components/ServerToolsPanel", () => ({
  ServerToolsPanel: ({ serverId }: { serverId: string }) => (
    <button type="button">{`Tools panel for ${serverId}`}</button>
  ),
}));

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
    expect(screen.getByRole("switch", { name: "Enable Example" })).not.toBeChecked();
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
    expect(
      screen.getByText("Personal · Local stdio · Keys from 1Password"),
    ).toBeInTheDocument();
  });

  it("draws a two-line row with the status chip and a trailing switch", () => {
    const { container } = renderRow(true, health({ ok: true, toolCount: 3 }));
    expect(screen.getByText("Personal · Local stdio")).toBeInTheDocument();
    const chip = container.querySelector('[data-slot="badge"]');
    expect(chip).toHaveTextContent("Ready · 3 tools");
    expect(chip).toHaveAttribute("data-variant", "success");
    expect(screen.queryByText("Enabled")).not.toBeInTheDocument();
    const toggle = screen.getByRole("switch", { name: "Disable Example" });
    const name = screen.getByText("Example");
    // The switch trails the name and the status chip.
    expect(
      name.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      chip!.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("shows a turned-off server as an Off chip", () => {
    const { container } = renderRow(false);
    const chip = container.querySelector('[data-slot="badge"]');
    expect(chip).toHaveTextContent("Off");
    expect(chip).toHaveAttribute("data-variant", "off");
  });

  it.each([
    ["endpoint", "Needs sign-in"],
    ["scope", "Permission required"],
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
    ["Connection failed", true, health({ error: "connection refused" })],
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

  it("offers View log alongside Sign in for a failed team auth probe", () => {
    renderRow(
      true,
      health({
        authRequired: true,
        authTarget: "endpoint",
        failure: { kind: "auth", target: "endpoint" },
        error: "HTTP 401: team Linear unauthorized",
      }),
      { ...server, source: "team:fixture" },
    );
    expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    act(() => screen.getByRole("button", { name: "View log" }).click());
    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      screen.getByText("HTTP 401: team Linear unauthorized", { exact: true }),
    ).toBeVisible();
  });

  it("shows a closed local connection instead of an unreachable endpoint", () => {
    renderRow(
      true,
      health({
        failure: { kind: "unavailable" },
        error: "Broken pipe (os error 32)",
      }),
    );
    expect(screen.getByText("Connection closed")).toBeVisible();
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

describe("RegistryServerRow detail tabs", () => {
  const second: ServerEntry = { ...server, id: "server-2", name: "Second" };

  async function renderExpandedRows() {
    const user = userEvent.setup();
    render(
      <TooltipProvider>
        {[server, second].map((rowServer) => (
          <RegistryServerRow
            key={rowServer.id}
            server={rowServer}
            registry={null}
            enabled={false}
            onToggle={vi.fn()}
            onRemove={vi.fn()}
            onRegistryChange={vi.fn()}
          />
        ))}
      </TooltipProvider>,
    );
    await user.click(screen.getByRole("button", { name: "Show Example details" }));
    await user.click(screen.getByRole("button", { name: "Show Second details" }));
    const tablist = (name: string) => screen.getByRole("tablist", { name });
    const tab = (list: string, name: string) =>
      within(tablist(list)).getByRole("tab", { name });
    return { user, tab };
  }

  it("links each tab to its own panel with unique IDs per row", async () => {
    const { tab } = await renderExpandedRows();
    const tabs = screen.getAllByRole("tab");
    expect(tabs).toHaveLength(4);
    const ids = tabs.flatMap((t) => [t.id, t.getAttribute("aria-controls")]);
    expect(ids.every(Boolean)).toBe(true);
    expect(new Set(ids).size).toBe(ids.length);
    for (const t of tabs) {
      const panel = document.getElementById(t.getAttribute("aria-controls")!);
      expect(panel).toHaveAttribute("role", "tabpanel");
      expect(panel).toHaveAttribute("aria-labelledby", t.id);
    }
    // Only the selected panel of each row is exposed.
    const panels = screen.getAllByRole("tabpanel");
    expect(panels).toHaveLength(2);
    expect(panels.map((p) => p.getAttribute("aria-labelledby"))).toEqual([
      tab("Example details", "Overview").id,
      tab("Second details", "Overview").id,
    ]);
    for (const list of ["Example details", "Second details"]) {
      expect(tab(list, "Overview")).toHaveAttribute("tabindex", "0");
      expect(tab(list, "Tools")).toHaveAttribute("tabindex", "-1");
    }
  });

  it("moves focus and selection with the arrow keys, Home and End", async () => {
    const { user, tab } = await renderExpandedRows();
    const overview = tab("Example details", "Overview");
    const tools = tab("Example details", "Tools");
    act(() => overview.focus());

    await user.keyboard("{ArrowRight}");
    expect(tools).toHaveFocus();
    expect(tools).toHaveAttribute("aria-selected", "true");
    expect(tools).toHaveAttribute("tabindex", "0");
    expect(overview).toHaveAttribute("aria-selected", "false");
    expect(overview).toHaveAttribute("tabindex", "-1");
    // Mounting the lazy Tools panel keeps focus on the tab.
    expect(
      await screen.findByRole("button", { name: "Tools panel for server-1" }),
    ).toBeInTheDocument();
    expect(tools).toHaveFocus();
    expect(screen.getByRole("tabpanel", { name: "Tools" })).toHaveAttribute(
      "id",
      tools.getAttribute("aria-controls"),
    );
    // The other row keeps its own selection.
    expect(tab("Second details", "Overview")).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByText("Tools panel for server-2")).not.toBeInTheDocument();

    await user.keyboard("{ArrowRight}");
    expect(overview).toHaveFocus();
    expect(overview).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowLeft}");
    expect(tools).toHaveFocus();
    expect(tools).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{Home}");
    expect(overview).toHaveFocus();
    expect(overview).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{End}");
    expect(tools).toHaveFocus();
    expect(tools).toHaveAttribute("aria-selected", "true");
  });

  it("cancels the default action only for the tab navigation keys", async () => {
    const { tab } = await renderExpandedRows();
    const overview = tab("Example details", "Overview");
    // fireEvent returns false when the handler called preventDefault, which keeps
    // the arrow keys and Home/End from also scrolling the page.
    for (const key of ["ArrowRight", "ArrowLeft", "Home", "End"]) {
      expect(fireEvent.keyDown(overview, { key })).toBe(false);
    }
    expect(fireEvent.keyDown(overview, { key: "a" })).toBe(true);
  });

  it("tabs from the selected tab into its panel and skips the other tab", async () => {
    const { user, tab } = await renderExpandedRows();
    const overview = tab("Example details", "Overview");
    act(() => overview.focus());

    await user.tab();
    expect(screen.getAllByRole("tabpanel", { name: "Overview" })[0]).toHaveFocus();
    await user.tab({ shift: true });
    expect(overview).toHaveFocus();

    await user.keyboard("{End}");
    const tools = tab("Example details", "Tools");
    await screen.findByRole("button", { name: "Tools panel for server-1" });
    expect(tools).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("tabpanel", { name: "Tools" })).toHaveFocus();
    await user.tab();
    expect(
      screen.getByRole("button", { name: "Tools panel for server-1" }),
    ).toHaveFocus();
    await user.tab({ shift: true });
    await user.tab({ shift: true });
    expect(tools).toHaveFocus();
    // Back out of the tabs to the row's last control, its actions menu.
    await user.tab({ shift: true });
    expect(screen.getByRole("button", { name: "Actions for Example" })).toHaveFocus();
  });
});

describe("RegistryServerRow actions menu", () => {
  it("offers the same actions as the GTK row menu", async () => {
    const user = userEvent.setup();
    renderRow(true, health({ ok: true, toolCount: 3 }), {
      ...server,
      transport: "http",
      url: "https://example.test/mcp",
    });
    await user.click(screen.getByRole("button", { name: "Actions for Example" }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Test connection",
      "Server details and tools",
      "Edit",
      "Sync settings",
      "Duplicate",
      "Authentication",
      "Credentials",
      "Remove",
    ]);
  });

  it("tests the saved server by its id, as the GTK row menu does", async () => {
    const user = userEvent.setup();
    vi.mocked(probeRegistered).mockResolvedValue(health({ ok: true, toolCount: 3 }));
    renderRow(true, health({ ok: true }));
    await user.click(screen.getByRole("button", { name: "Actions for Example" }));
    await user.click(screen.getByRole("menuitem", { name: "Test connection" }));
    expect(probeRegistered).toHaveBeenCalledWith("server-1");
    expect(testServer).not.toHaveBeenCalled();
  });

  it("starts nothing for a team server awaiting review", async () => {
    const user = userEvent.setup();
    renderRow(false, undefined, {
      ...server,
      source: "team:t",
      teamEnableReview: true,
    });
    await user.click(screen.getByRole("button", { name: "Actions for Example" }));
    expect(screen.getByRole("menuitem", { name: "Test connection" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("leaves out Authentication for a local server", async () => {
    const user = userEvent.setup();
    renderRow(true, health({ ok: true }));
    await user.click(screen.getByRole("button", { name: "Actions for Example" }));
    expect(
      screen.queryByRole("menuitem", { name: "Authentication" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Credentials" })).toBeInTheDocument();
  });

  it("opens the details from the server name", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    render(
      <TooltipProvider>
        <RegistryServerRow
          server={server}
          registry={null}
          enabled
          onToggle={vi.fn()}
          onRemove={vi.fn()}
          onRegistryChange={vi.fn()}
          onOpen={onOpen}
        />
      </TooltipProvider>,
    );
    await user.click(screen.getByRole("button", { name: "Show Example details" }));
    expect(onOpen).toHaveBeenCalledWith("overview");
  });

  it("offers Retry for a locked keyring, with no log to view", () => {
    const onReprobe = vi.fn();
    render(
      <TooltipProvider>
        <RegistryServerRow
          server={server}
          registry={null}
          enabled
          health={health({ keyringLocked: true })}
          onToggle={vi.fn()}
          onRemove={vi.fn()}
          onRegistryChange={vi.fn()}
          onReprobe={onReprobe}
        />
      </TooltipProvider>,
    );
    act(() => screen.getByRole("button", { name: "Retry" }).click());
    expect(onReprobe).toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "View log" })).not.toBeInTheDocument();
  });
});
