import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "@/lib/theme";
import { SettingsView } from "./SettingsView";
import {
  approveRoutineSuggestion,
  dismissRoutineSuggestion,
  listRoutineSuggestions,
  listServerTools,
  setAllowRoutineWrites,
  setCodeMode,
} from "@/lib/api";
import type { Registry, RoutineSuggestion } from "@/lib/types";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();

  return {
    ...actual,
    listServerTools: vi.fn(),
    setAllowRoutineWrites: vi.fn(),
    setCodeMode: vi.fn(),
    listRoutineSuggestions: vi.fn().mockResolvedValue([]),
    approveRoutineSuggestion: vi.fn(),
    dismissRoutineSuggestion: vi.fn(),
  };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  isEnabled: vi.fn(),
  enable: vi.fn().mockResolvedValue(undefined),
  disable: vi.fn().mockResolvedValue(undefined),
}));

import { isEnabled as isAutostartEnabled } from "@tauri-apps/plugin-autostart";

const mockedListServerTools = vi.mocked(listServerTools);
const mockedIsAutostartEnabled = vi.mocked(isAutostartEnabled);
const mockedSetAllowRoutineWrites = vi.mocked(setAllowRoutineWrites);
const mockedSetCodeMode = vi.mocked(setCodeMode);
const mockedListRoutineSuggestions = vi.mocked(listRoutineSuggestions);
const mockedApproveRoutineSuggestion = vi.mocked(approveRoutineSuggestion);
const mockedDismissRoutineSuggestion = vi.mocked(dismissRoutineSuggestion);

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

    // Open the profile.
    await user.click(
      screen.getByRole("button", {
        name: /default active 2 servers/i,
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

describe("SettingsView routine writes", () => {
  it("keeps concurrent successful setting responses from reverting each other", async () => {
    const user = userEvent.setup();
    const onRegistryChange = vi.fn();
    const routineRequest = deferred<Registry>();
    const codeModeRequest = deferred<Registry>();
    mockedSetAllowRoutineWrites.mockReturnValueOnce(routineRequest.promise);
    mockedSetCodeMode.mockReturnValueOnce(codeModeRequest.promise);

    render(
      <ThemeProvider>
        <SettingsView registry={registry} onRegistryChange={onRegistryChange} />
      </ThemeProvider>,
    );

    const routineControl = screen.getByRole("switch", {
      name: /allow routine writes/i,
    });
    const codeModeControl = screen.getByRole("switch", { name: /code mode/i });
    const destructiveControl = screen.getByRole("switch", {
      name: /block destructive tools/i,
    });

    await user.click(routineControl);
    expect(routineControl).toBeDisabled();
    expect(codeModeControl).toBeEnabled();
    expect(destructiveControl).toBeEnabled();

    await user.click(codeModeControl);
    expect(mockedSetCodeMode).toHaveBeenCalledWith(false);
    expect(codeModeControl).toBeDisabled();
    expect(routineControl).toBeDisabled();
    expect(destructiveControl).toBeEnabled();

    // Resolve the later request first, then return a stale Routine snapshot that still
    // has Code Mode enabled. Each response must update only the setting it owns.
    const codeModeOff = { ...registry, codeMode: false };
    codeModeRequest.resolve(codeModeOff);
    await waitFor(() => expect(codeModeControl).toBeEnabled());
    expect(onRegistryChange).toHaveBeenCalledWith(codeModeOff);

    const routineWritesOn = { ...registry, allowRoutineWrites: true };
    routineRequest.resolve(routineWritesOn);
    await waitFor(() => expect(routineControl).toBeEnabled());
    expect(onRegistryChange).toHaveBeenLastCalledWith({
      ...registry,
      codeMode: false,
      allowRoutineWrites: true,
    });
    expect(destructiveControl).toBeEnabled();
  });

  it("defaults off and persists the explicit opt-in", async () => {
    const user = userEvent.setup();
    const onRegistryChange = vi.fn();
    const updated = { ...registry, allowRoutineWrites: true };
    mockedSetAllowRoutineWrites.mockResolvedValueOnce(updated);

    render(
      <ThemeProvider>
        <SettingsView registry={registry} onRegistryChange={onRegistryChange} />
      </ThemeProvider>,
    );

    const control = screen.getByRole("switch", { name: /allow routine writes/i });
    expect(control).not.toBeChecked();
    await user.click(control);

    expect(mockedSetAllowRoutineWrites).toHaveBeenCalledWith(true);
    await waitFor(() => expect(onRegistryChange).toHaveBeenCalledWith(updated));
  });

  it("stays off when persisting the opt-in fails", async () => {
    const user = userEvent.setup();
    const onRegistryChange = vi.fn();
    mockedSetAllowRoutineWrites.mockRejectedValueOnce(new Error("locked"));

    render(
      <ThemeProvider>
        <SettingsView registry={registry} onRegistryChange={onRegistryChange} />
      </ThemeProvider>,
    );

    const control = screen.getByRole("switch", { name: /allow routine writes/i });
    await user.click(control);
    await waitFor(() => expect(mockedSetAllowRoutineWrites).toHaveBeenCalledWith(true));
    await waitFor(() => expect(control).toBeEnabled());
    expect(control).not.toBeChecked();
    expect(onRegistryChange).not.toHaveBeenCalled();
  });

  it("hides the write opt-in while Code Mode is disabled", () => {
    render(
      <ThemeProvider>
        <SettingsView
          registry={{ ...registry, codeMode: false, allowRoutineWrites: true }}
          onRegistryChange={vi.fn()}
        />
      </ThemeProvider>,
    );

    expect(
      screen.queryByRole("switch", { name: /allow routine writes/i }),
    ).not.toBeInTheDocument();
  });
});

describe("SettingsView routine suggestions", () => {
  const suggestion: RoutineSuggestion = {
    suggestedName: "batch-deepwiki-ask-question",
    source:
      "// Synthesized by Toolport from observed deepwiki__ask_question calls.\nreturn input.items;",
    inputSchema: { type: "object" },
    limits: {},
    definitionFingerprint: "sha256:fp1",
    evidence: {
      sourceRunId: `run_${"a".repeat(32)}`,
      executedAtMs: 1,
      calls: 3,
      observedDependencies: [{ name: "deepwiki__ask_question" }],
      validationVersion: 1,
      riskClass: "medium",
      provenance: "synthesized_from_observed_calls",
    },
    intermediateBytes: 24_576,
  };

  it("saves a queued pattern with the edited name and no second prompt", async () => {
    const user = userEvent.setup();
    mockedListRoutineSuggestions
      .mockResolvedValueOnce([suggestion])
      .mockResolvedValueOnce([]);
    mockedApproveRoutineSuggestion.mockResolvedValueOnce({});

    renderSettings();

    expect(await screen.findByText("Suggested routines")).toBeInTheDocument();
    expect(
      screen.getByText(/Synthesized by Toolport from observed direct calls/),
    ).toBeInTheDocument();
    expect(screen.getByText("Calls: 3")).toBeInTheDocument();

    const name = screen.getByRole("textbox", { name: /routine name/i });
    await user.clear(name);
    await user.type(name, "ask-many-repos");
    await user.click(screen.getByRole("button", { name: /save routine/i }));

    expect(mockedApproveRoutineSuggestion).toHaveBeenCalledWith(
      "sha256:fp1",
      "ask-many-repos",
    );
    await waitFor(() =>
      expect(screen.queryByText("Suggested routines")).not.toBeInTheDocument(),
    );
  });

  it("dismisses a suggestion for the rest of the app run", async () => {
    const user = userEvent.setup();
    mockedListRoutineSuggestions
      .mockResolvedValueOnce([suggestion])
      .mockResolvedValueOnce([]);
    mockedDismissRoutineSuggestion.mockResolvedValueOnce(undefined);

    renderSettings();

    await screen.findByText("Suggested routines");
    await user.click(screen.getByRole("button", { name: /dismiss/i }));

    expect(mockedDismissRoutineSuggestion).toHaveBeenCalledWith("sha256:fp1");
    await waitFor(() =>
      expect(screen.queryByText("Suggested routines")).not.toBeInTheDocument(),
    );
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
