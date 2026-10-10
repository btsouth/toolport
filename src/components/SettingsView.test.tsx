import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "@/lib/theme";
import { toastError } from "@/lib/toast";
import { SettingsView } from "./SettingsView";
import {
  clientsNeedingRestart,
  disconnectAllClients,
  getRegistry,
  isAutostartEnabled,
  listServerTools,
  setCodeMode,
  setDefaultAccess,
  setPiiRedaction,
  setSafetyLevel,
  stopStaleGateways,
} from "@/lib/api";
import type { Registry } from "@/lib/types";

vi.mock("@/lib/toast", () => ({
  toastError: vi.fn(),
}));

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();

  return {
    ...actual,
    disconnectAllClients: vi.fn(),
    getRegistry: vi.fn(),
    listServerTools: vi.fn(),
    isAutostartEnabled: vi.fn(),
    enableAutostart: vi.fn().mockResolvedValue(undefined),
    disableAutostart: vi.fn().mockResolvedValue(undefined),
    setCodeMode: vi.fn(),
    setDefaultAccess: vi.fn(),
    setPiiRedaction: vi.fn(),
    setSafetyLevel: vi.fn(),
    clientsNeedingRestart: vi.fn().mockResolvedValue([]),
    stopStaleGateways: vi.fn(),
  };
});

const mockedListServerTools = vi.mocked(listServerTools);
const mockedIsAutostartEnabled = vi.mocked(isAutostartEnabled);
const mockedSetCodeMode = vi.mocked(setCodeMode);
const mockedSetPiiRedaction = vi.mocked(setPiiRedaction);
const mockedClientsNeedingRestart = vi.mocked(clientsNeedingRestart);
const mockedStopStaleGateways = vi.mocked(stopStaleGateways);
const mockedToastError = vi.mocked(toastError);

const registry: Registry = {
  version: 1,
  servers: [
    {
      id: "github",
      name: "GitHub",
      transport: "stdio",
      command: null,
      args: [],
      env: [],
      url: null,
      source: null,
    },
    {
      id: "slack",
      name: "Slack",
      transport: "stdio",
      command: null,
      args: [],
      env: [],
      url: null,
      source: null,
    },
  ],
  profiles: [
    {
      id: "default",
      name: "Default",
      enabledServerIds: ["github", "slack"],
    },
  ],
  activeProfileId: "default",
  codeMode: true,
};

function renderSettings() {
  render(
    <ThemeProvider>
      <SettingsView registry={registry} onRegistryChange={vi.fn()} />
    </ThemeProvider>,
  );
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;

  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });

  return {
    promise,
    resolve,
    reject,
  };
}

describe("SettingsView tool loading", () => {
  it("keeps loading state scoped to each server", async () => {
    const user = userEvent.setup();

    const githubRequest = deferred<{ name: string }[]>();
    const slackRequest = deferred<{ name: string }[]>();

    mockedListServerTools
      .mockReturnValueOnce(githubRequest.promise)
      .mockReturnValueOnce(slackRequest.promise);

    renderSettings();

    await user.click(screen.getByText("Advanced"));
    // Open the access set.
    await user.click(
      screen.getByRole("button", {
        name: /default 2 servers/i,
      }),
    );

    // Expand GitHub (request A starts).
    const githubToggle = screen.getByRole("button", {
      name: /github/i,
    });
    expect(githubToggle).toHaveAttribute("type", "button");
    expect(githubToggle).toHaveAttribute("aria-expanded", "false");
    await user.click(githubToggle);
    expect(githubToggle).toHaveAttribute("aria-expanded", "true");

    expect(screen.getByText("Loading tools…")).toBeInTheDocument();

    // Expand Slack while GitHub is still pending (request B starts).
    await user.click(
      screen.getByRole("button", {
        name: /slack/i,
      }),
    );

    // Slack is now the visible expanded server.
    expect(screen.getByText("Loading tools…")).toBeInTheDocument();

    // Resolve GitHub first.
    githubRequest.resolve([{ name: "repo-search" }]);

    // Slack should still be loading because loading is tracked per server.
    await waitFor(() => {
      expect(screen.getByText("Loading tools…")).toBeInTheDocument();
    });

    // Resolve Slack afterwards.
    slackRequest.resolve([{ name: "send-message" }]);

    expect(await screen.findByText("send-message")).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.queryByText("Loading tools…")).not.toBeInTheDocument();
    });
  });
});

describe("SettingsView launch at login", () => {
  it("keeps the switch disabled until the OS autostart state is known", async () => {
    const request = deferred<boolean>();
    mockedIsAutostartEnabled.mockReturnValueOnce(request.promise);

    renderSettings();

    const control = screen.getByRole("switch", { name: /launch at login/i });
    expect(control).toBeDisabled();
    expect(screen.getByText(/checking the current os setting/i)).toBeInTheDocument();

    request.resolve(true);
    await waitFor(() => expect(control).toBeEnabled());
    expect(control).toHaveAttribute("aria-checked", "true");
    expect(
      screen.queryByText(/checking the current os setting/i),
    ).not.toBeInTheDocument();
  });

  it("renders a verified disabled state once the OS read succeeds with false", async () => {
    mockedIsAutostartEnabled.mockResolvedValueOnce(false);

    renderSettings();

    const control = screen.getByRole("switch", { name: /launch at login/i });
    await waitFor(() => expect(control).toBeEnabled());
    expect(control).toHaveAttribute("aria-checked", "false");
  });

  it("shows unavailable/retry feedback on a failed read and restores the real state on retry", async () => {
    const user = userEvent.setup();
    mockedIsAutostartEnabled.mockRejectedValueOnce(new Error("boom"));

    renderSettings();

    const control = screen.getByRole("switch", { name: /launch at login/i });
    await waitFor(() =>
      expect(screen.getByText(/couldn't read the os setting/i)).toBeInTheDocument(),
    );
    // A failed read must never be presented as a verified Off state.
    expect(control).toBeDisabled();
    const row = screen.getByText("Launch at login").closest("label");
    expect(row).not.toBeNull();
    expect(within(row!).getByRole("button", { name: /retry/i })).toBeInTheDocument();

    // A successful retry restores the real enabled/disabled state.
    mockedIsAutostartEnabled.mockResolvedValueOnce(true);
    await user.click(within(row!).getByRole("button", { name: /retry/i }));

    await waitFor(() => expect(control).toBeEnabled());
    expect(control).toHaveAttribute("aria-checked", "true");
    expect(screen.queryByText(/couldn't read the os setting/i)).not.toBeInTheDocument();
  });
});

describe("SettingsView restart check", () => {
  it("shows an error toast when the old-gateway check fails", async () => {
    mockedClientsNeedingRestart.mockRejectedValue(new Error("backend down"));
    renderSettings();

    await waitFor(() => {
      expect(mockedToastError).toHaveBeenCalledWith(
        "Couldn't check for apps using an old gateway",
      );
    });
  });

  it("retries the check from the error panel", async () => {
    mockedClientsNeedingRestart
      .mockRejectedValueOnce(new Error("backend down"))
      .mockResolvedValueOnce([{ client: "Codex", gateway: "old", clientPid: 1234 }]);
    const user = userEvent.setup();
    renderSettings();

    const retry = await screen.findByRole("button", {
      name: "Retry checking for old gateway",
    });
    await user.click(retry);

    await waitFor(() => {
      expect(
        screen.getByText(/1 app is still launching an old gateway/i),
      ).toBeInTheDocument();
    });
    expect(
      screen.queryByRole("button", { name: "Retry checking for old gateway" }),
    ).not.toBeInTheDocument();
  });

  it("clears the error panel when Stop old gateways answers the same question", async () => {
    // A successful run IS a fresh answer to "which apps need a restart", so
    // leaving the failure panel up would say the check failed directly above
    // the check's own result.
    mockedClientsNeedingRestart.mockRejectedValue(new Error("backend down"));
    mockedStopStaleGateways.mockResolvedValue({
      killed: [],
      failed: [],
      needsRestart: [{ client: "Codex", gateway: "old", clientPid: 1234 }],
    });
    const user = userEvent.setup();
    renderSettings();

    await screen.findByRole("button", { name: "Retry checking for old gateway" });
    await user.click(screen.getByRole("button", { name: "Run" }));

    await waitFor(() => {
      expect(
        screen.queryByRole("button", { name: "Retry checking for old gateway" }),
      ).not.toBeInTheDocument();
    });
    expect(
      screen.getByText(/1 app is still launching an old gateway/i),
    ).toBeInTheDocument();
  });
});

it("selects and persists one safety level", async () => {
  const user = userEvent.setup();
  const onRegistryChange = vi.fn();
  vi.mocked(setSafetyLevel).mockResolvedValueOnce({ ...registry, safetyLevel: "strict" });
  render(
    <ThemeProvider>
      <SettingsView
        registry={{ ...registry, safetyLevel: "ask" }}
        onRegistryChange={onRegistryChange}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("ask");
  await user.selectOptions(screen.getByRole("combobox", { name: "Safety" }), "strict");
  expect(setSafetyLevel).toHaveBeenCalledWith("strict");
  await waitFor(() =>
    expect(onRegistryChange).toHaveBeenCalledWith({ ...registry, safetyLevel: "strict" }),
  );
  expect(
    screen.queryByRole("switch", { name: /block destructive tools/i }),
  ).not.toBeInTheDocument();
});

it.each([
  ["ask", "off"],
  ["strict", "off"],
  ["strict", "ask"],
] as const)("shows the %s team floor and rejects selecting %s", async (floor, below) => {
  const user = userEvent.setup();
  vi.mocked(setSafetyLevel).mockReset();
  render(
    <ThemeProvider>
      <SettingsView
        registry={{ ...registry, safetyLevel: "off", teamMinSafetyLevel: floor }}
        onRegistryChange={vi.fn()}
      />
    </ThemeProvider>,
  );
  const control = screen.getByRole("combobox", { name: "Safety" });
  expect(control).toHaveValue(floor);
  expect(
    screen.getByRole("option", { name: below === "off" ? "Off" : "Ask" }),
  ).toBeDisabled();
  await user.selectOptions(control, below);
  expect(control).toHaveValue(floor);
  expect(setSafetyLevel).not.toHaveBeenCalled();
  expect(
    screen.getByText(
      `Team minimum safety level: ${floor === "ask" ? "Ask" : "Strict"}. Choices below this floor are unavailable.`,
    ),
  ).toBeInTheDocument();
});

it("uses legacy approval as an Ask floor without hiding the Strict choice", () => {
  render(
    <ThemeProvider>
      <SettingsView
        registry={{
          ...registry,
          safetyLevel: "off",
          teamMinSafetyLevel: "off",
          teamForcedHumanApproval: true,
        }}
        onRegistryChange={vi.fn()}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("ask");
  expect(screen.getByRole("option", { name: "Off" })).toBeDisabled();
  expect(screen.getByRole("option", { name: "Strict" })).not.toBeDisabled();
});

it("independent team flags leave Off selected and all levels available", () => {
  render(
    <ThemeProvider>
      <SettingsView
        registry={{
          ...registry,
          safetyLevel: "off",
          teamForcedDenyDestructive: true,
          teamForcedQuarantineOnDrift: true,
          teamForcedBlockOnInjection: true,
        }}
        onRegistryChange={vi.fn()}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("off");
  expect(screen.getByRole("option", { name: "Off" })).not.toBeDisabled();
  expect(
    screen.getByText(/Team also enforces: quarantine on drift, block on injection/),
  ).toBeInTheDocument();
  expect(screen.queryByText(/Team minimum safety level/)).not.toBeInTheDocument();
});

it("keeps a stronger member choice when the team lowers its floor", () => {
  const change = vi.fn();
  const { rerender } = render(
    <ThemeProvider>
      <SettingsView
        registry={{ ...registry, safetyLevel: "strict", teamMinSafetyLevel: "ask" }}
        onRegistryChange={change}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("strict");
  rerender(
    <ThemeProvider>
      <SettingsView
        registry={{ ...registry, safetyLevel: "strict", teamMinSafetyLevel: "off" }}
        onRegistryChange={change}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("strict");
  expect(screen.getByRole("option", { name: "Off" })).not.toBeDisabled();
});

it("keeps Code Mode off by default under Advanced and persists opt-in", async () => {
  const user = userEvent.setup();
  const onRegistryChange = vi.fn();
  const absent = { ...registry, codeMode: undefined };
  vi.mocked(setCodeMode).mockReset();
  vi.mocked(setCodeMode).mockResolvedValueOnce({ ...absent, codeMode: true });
  render(
    <ThemeProvider>
      <SettingsView registry={absent} onRegistryChange={onRegistryChange} />
    </ThemeProvider>,
  );
  const advanced = screen.getByText("Advanced").closest("details");
  expect(advanced).not.toHaveAttribute("open");
  await user.click(screen.getByText("Advanced"));
  expect(advanced).toHaveAttribute("open");
  const control = within(advanced!).getByRole("switch", { name: /code mode/i });
  expect(control).not.toBeChecked();
  await user.click(control);
  expect(setCodeMode).toHaveBeenCalledWith(true);
  await waitFor(() =>
    expect(onRegistryChange).toHaveBeenCalledWith({ ...absent, codeMode: true }),
  );
});

describe("SettingsView setting merges", () => {
  it("keeps concurrent successful setting responses from reverting each other", async () => {
    const user = userEvent.setup();
    const onRegistryChange = vi.fn();
    const piiRequest = deferred<Registry>();
    const codeModeRequest = deferred<Registry>();
    mockedSetPiiRedaction.mockReset();
    mockedSetCodeMode.mockReset();
    mockedSetPiiRedaction.mockReturnValueOnce(piiRequest.promise);
    mockedSetCodeMode.mockReturnValueOnce(codeModeRequest.promise);

    render(
      <ThemeProvider>
        <SettingsView registry={registry} onRegistryChange={onRegistryChange} />
      </ThemeProvider>,
    );
    await user.click(screen.getByText("Advanced"));

    const piiControl = screen.getByRole("switch", {
      name: /hide personal data from the model/i,
    });
    const codeModeControl = screen.getByRole("switch", { name: /code mode/i });
    const safetyControl = screen.getByRole("combobox", { name: "Safety" });

    await user.click(piiControl);
    expect(mockedSetPiiRedaction).toHaveBeenCalledWith(true);
    expect(piiControl).toBeDisabled();
    expect(codeModeControl).toBeEnabled();
    expect(safetyControl).toBeEnabled();

    await user.click(codeModeControl);
    expect(mockedSetCodeMode).toHaveBeenCalledWith(false);
    expect(codeModeControl).toBeDisabled();
    expect(piiControl).toBeDisabled();
    expect(safetyControl).toBeEnabled();

    // Resolve the later request first, then return a stale Hide-personal-data snapshot
    // that still has Code Mode enabled. Each response must update only the setting it
    // owns, so the stale snapshot must not turn Code Mode back on.
    const codeModeOff = { ...registry, codeMode: false };
    codeModeRequest.resolve(codeModeOff);
    await waitFor(() => expect(codeModeControl).toBeEnabled());
    expect(onRegistryChange).toHaveBeenCalledWith(codeModeOff);

    const piiOn = { ...registry, codeMode: true, piiRedaction: true };
    piiRequest.resolve(piiOn);
    await waitFor(() => expect(piiControl).toBeEnabled());
    expect(onRegistryChange).toHaveBeenLastCalledWith({
      ...registry,
      codeMode: false,
      piiRedaction: true,
    });
    expect(safetyControl).toBeEnabled();
  });
});

it("keeps access sets and folder routing under Advanced and changes the default explicitly", async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  const pinned = { ...registry, version: 3, defaultAccessProfileId: "default" };
  vi.mocked(setDefaultAccess).mockResolvedValue({
    ...pinned,
    defaultAccessProfileId: null,
  });
  render(
    <ThemeProvider>
      <SettingsView registry={pinned} onRegistryChange={onChange} />
    </ThemeProvider>,
  );
  const advanced = screen.getByText("Advanced").closest("details")!;
  expect(advanced).not.toHaveAttribute("open");
  expect(within(advanced).getByText("Access sets")).toBeInTheDocument();
  expect(within(advanced).getByText(/folder routing/i)).toBeInTheDocument();
  await user.click(screen.getByText("Advanced"));
  await user.click(screen.getByRole("combobox", { name: "Default access" }));
  await user.click(await screen.findByRole("option", { name: "All enabled servers" }));
  await waitFor(() => expect(setDefaultAccess).toHaveBeenCalledWith(null));
  expect(onChange).toHaveBeenCalledWith(
    expect.objectContaining({ defaultAccessProfileId: null }),
  );
});

it("shows the expand affordance for an empty access set", async () => {
  const empty = {
    ...registry,
    version: 3,
    profiles: [{ id: "empty", name: "Empty set", enabledServerIds: [] }],
  };
  render(
    <ThemeProvider>
      <SettingsView registry={empty} onRegistryChange={vi.fn()} />
    </ThemeProvider>,
  );
  await userEvent.click(screen.getByText("Advanced"));
  const toggle = screen.getByRole("button", { name: /Empty set/ });
  expect(toggle.querySelector("svg")).not.toHaveClass("invisible");
  await userEvent.click(toggle);
  expect(toggle).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByRole("checkbox", { name: /GitHub/ })).not.toBeChecked();
});

describe("Remove Toolport from all clients", () => {
  beforeEach(() => {
    vi.mocked(disconnectAllClients).mockReset();
    vi.mocked(getRegistry).mockReset();
    vi.mocked(toastError).mockClear();
  });

  it("keeps removal results when refreshing the registry fails", async () => {
    const user = userEvent.setup();
    vi.mocked(disconnectAllClients).mockResolvedValue([
      { clientId: "codex", path: "/fixture/config.toml", dryRun: false, error: null },
    ]);
    vi.mocked(getRegistry).mockRejectedValue(new Error("refresh failed"));
    renderSettings();
    await user.click(
      screen.getByRole("button", { name: "Remove Toolport from all clients" }),
    );
    await user.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Remove from all clients",
      }),
    );
    await screen.findByText("codex: Client configuration restored");
    await waitFor(() =>
      expect(toastError).toHaveBeenCalledWith(
        "Client removal finished, but could not refresh settings: Error: refresh failed",
      ),
    );
  });

  it("requires confirmation and reports a partial failure per client", async () => {
    const user = userEvent.setup();
    vi.mocked(disconnectAllClients).mockResolvedValue([
      { clientId: "codex", path: "/fixture/config.toml", dryRun: false, error: null },
      {
        clientId: "cursor",
        path: "/fixture/mcp.json",
        dryRun: false,
        error: "Client config conflict",
      },
    ]);
    vi.mocked(getRegistry).mockResolvedValue(registry);
    renderSettings();
    await user.click(
      screen.getByRole("button", { name: "Remove Toolport from all clients" }),
    );
    const dialog = screen.getByRole("dialog");
    expect(disconnectAllClients).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(disconnectAllClients).not.toHaveBeenCalled();
    await user.click(
      screen.getByRole("button", { name: "Remove Toolport from all clients" }),
    );
    await user.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: "Remove from all clients",
      }),
    );
    await screen.findByText("codex: Client configuration restored");
    expect(screen.getByText("cursor: Client config conflict")).toBeInTheDocument();
    expect(disconnectAllClients).toHaveBeenCalledTimes(1);
  });
});

it.each([
  [3, undefined, "ask"],
  [3, "off", "off"],
  [1, undefined, "off"],
] as const)(
  "matches backend defaults for registry v%s and safety %s",
  (version, safetyLevel, expected) => {
    render(
      <ThemeProvider>
        <SettingsView
          registry={{ ...registry, version, safetyLevel }}
          onRegistryChange={vi.fn()}
        />
      </ThemeProvider>,
    );
    expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue(expected);
    expect(
      screen.getByText(
        new RegExp(`Safety is set to ${expected === "ask" ? "Ask" : "Off"}`),
      ),
    ).toBeInTheDocument();
  },
);

it("shows a team's Ask minimum even when the member chose Off", () => {
  render(
    <ThemeProvider>
      <SettingsView
        registry={{
          ...registry,
          version: 3,
          safetyLevel: "off",
          teamMinSafetyLevel: "ask",
        }}
        onRegistryChange={vi.fn()}
      />
    </ThemeProvider>,
  );
  expect(screen.getByRole("combobox", { name: "Safety" })).toHaveValue("ask");
  expect(
    screen.getByText(/Destructive calls need your approval before they run/),
  ).toBeInTheDocument();
});
