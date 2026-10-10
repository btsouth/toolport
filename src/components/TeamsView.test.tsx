import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { InstructionsStatusView, Registry } from "@/lib/types";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const api = vi.hoisted(() => ({
  teamConnect: vi.fn(),
  teamUseManaged: vi.fn(),
  teamAccountLink: vi.fn(),
  teamJoinPoll: vi.fn(),
  teamSync: vi.fn(),
  teamSyncStatus: vi
    .fn()
    .mockResolvedValue({ state: "not_checked", lastSuccessMs: null }),
  teamDisconnect: vi.fn(),
  teamPushPreview: vi.fn(),
  teamPush: vi.fn(),
  getRegistry: vi.fn(),
  teamInstructionsStatus: vi.fn().mockResolvedValue(null),
  setServerEnabled: vi.fn(),
}));

vi.mock("@/lib/api", () => api);
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(vi.fn()),
}));

const { openExternal } = vi.hoisted(() => ({ openExternal: vi.fn() }));
vi.mock("@/lib/openUrl", () => ({ openExternal }));

import { listen } from "@tauri-apps/api/event";
import { TeamsView } from "./TeamsView";
import {
  TEAMS_ANNUAL_PRICE,
  TEAMS_BASE_PRICE,
  TEAMS_FREE_LINE,
  TEAMS_PAID_LINE,
  TEAMS_SEAT_PRICE,
  TEAMS_ANNUAL_SEAT_PRICE,
  TEAMS_TEAM_SEATS,
} from "@/lib/teamsPlan";

/** Everything on this tab that only a person without a team should ever see. Named once
 * so the "connected", "loading" and "waiting for approval" tests all assert against the
 * same list, and adding a new piece of pitch copy in one place fails all three. */
const PITCH_CTAS = [/Create a free team/, /Pricing/, /Self-host it/];
const PAIN_TILES = ["New teammate, day one", "No more config drift", "No shared secrets"];

function expectNoPitch() {
  expect(screen.queryByRole("heading", { name: "No team yet?" })).toBeNull();
  expect(screen.queryByText(TEAMS_FREE_LINE)).toBeNull();
  expect(screen.queryByText(TEAMS_PAID_LINE)).toBeNull();
  for (const name of PITCH_CTAS) {
    expect(screen.queryByRole("button", { name })).toBeNull();
  }
  for (const title of PAIN_TILES) {
    expect(screen.queryByText(title)).toBeNull();
  }
}

const registry: Registry = {
  version: 1,
  servers: [
    {
      id: "github",
      name: "Personal GitHub",
      transport: "stdio",
      command: "python3",
      args: [],
      env: [],
      url: null,
      source: null,
    },
  ],
  profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
  activeProfileId: "default",
  team: {
    serverUrl: "https://teams.toolport.app",
    teamId: "team-1",
    role: "admin",
    lastVersion: 6,
  },
};

/** The same registry with no team on it, which is what every free user sees. */
const noTeam: Registry = { ...registry, team: null };

describe("TeamsView shared-server update", () => {
  it("explains personal Pro environment references in the blocked notice", async () => {
    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    await waitFor(() => expect(listen).toHaveBeenCalled());
    const callback = vi
      .mocked(listen)
      .mock.calls.find(([event]) => event === "team-servers-review")?.[1];
    expect(callback).toBeDefined();
    act(() =>
      callback?.({
        event: "team-servers-review",
        id: 1,
        payload: { review: 0, blocked: 1 },
      }),
    );
    expect(
      screen.getByText(/env: references are local only, including personal Pro sync/),
    ).toHaveTextContent("Use a password manager reference instead.");
  });
  beforeEach(() => {
    vi.clearAllMocks();
    api.getRegistry.mockResolvedValue(registry);
  });

  it("explains that disconnecting the app does not remove Team membership", async () => {
    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Disconnect app" }));
    expect(
      screen.getByText(/Your Team membership and shared setup remain/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Reconnect from the Teams website/)).toBeInTheDocument();
    expect(api.teamDisconnect).not.toHaveBeenCalled();
  });

  it.each([null, "Unable to refresh local setup"])(
    "publishes only after confirmation and distinguishes local outcome: %s",
    async (localSetupError) => {
      const summary = localSetupError
        ? `Shared with your team (version 8).\nLocal setup needs attention: ${localSetupError}`
        : "Shared with your team (version 8).\nPersonal GitHub: Now uses the Team copy in this profile.";
      const preview = {
        baseVersion: 7,
        localFingerprint: "preview-fingerprint",
        added: ["Alpha", "beta"],
        changed: ["GitHub"],
        removed: ["Legacy"],
        definitions: [
          { id: "alpha", name: "Alpha", change: "Added", transport: "stdio", fields: [] },
          { id: "beta", name: "beta", change: "Added", transport: "http", fields: [] },
          {
            id: "github",
            name: "GitHub",
            change: "Changed",
            transport: "stdio",
            fields: [],
          },
        ],
        selections: [],
      };
      api.teamPushPreview.mockResolvedValue(preview);
      api.teamPush.mockResolvedValue({
        version: 8,
        published: true,
        localSetupError,
        handoffs: [],
        summary,
      });

      render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
      if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
        await userEvent.click(screen.getByRole("checkbox"));
      await userEvent.click(
        screen.getByRole("button", { name: "Share selected servers" }),
      );

      expect(await screen.findByText("Added (2)")).toBeInTheDocument();
      expect(screen.getByText("Changed (1)")).toBeInTheDocument();
      expect(screen.getByText("Removed (1)")).toBeInTheDocument();
      for (const name of ["Alpha", "beta", "GitHub", "Legacy"]) {
        expect(
          within(screen.getByRole("dialog")).getByText(name, { exact: false }),
        ).toBeInTheDocument();
      }
      expect(api.teamPush).not.toHaveBeenCalled();

      await userEvent.click(screen.getByRole("button", { name: "Share selected" }));
      await waitFor(() => expect(api.teamPush).toHaveBeenCalledWith(preview, ["github"]));
      const notice = await screen.findByText(/version 8/i);
      expect(notice).toHaveTextContent(summary.replace(/\n/g, " "));
      expect(api.teamSync).not.toHaveBeenCalled();
      expect(api.getRegistry).toHaveBeenCalled();
      if (localSetupError)
        expect(screen.getByText(/Local setup needs attention/)).toBeInTheDocument();
    },
  );

  it("switches to Team copies without uploading when every selection is already shared", async () => {
    const handoff = {
      id: "github",
      name: "Personal GitHub",
      outcome: "switched" as const,
      message: "Now uses the Team copy in this profile.",
    };
    const preview = {
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: [],
      changed: [],
      removed: [],
      definitions: [],
      selections: [
        {
          id: "github",
          name: "Personal GitHub",
          teamChange: "Already shared",
          teamDetail:
            "The Team already has this exact definition, so nothing changes for the team.",
          notes: [],
          local: { ...handoff, message: "This profile switches to the Team copy." },
        },
      ],
    };
    api.teamPushPreview.mockResolvedValue(preview);
    api.teamPush.mockResolvedValue({
      version: 7,
      published: false,
      localSetupError: null,
      handoffs: [handoff],
      summary:
        "Already shared with your team (version 7). Nothing new was uploaded.\nPersonal GitHub: Now uses the Team copy in this profile.",
    });

    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
      await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
    expect(
      await screen.findByText("Personal GitHub · Already shared"),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Use Team copies" }));
    await waitFor(() => expect(api.teamPush).toHaveBeenCalledWith(preview, ["github"]));
    expect(await screen.findByText(/Nothing new was uploaded/)).toBeInTheDocument();
  });

  it("shows a selection that needs separate setup as a warning with its route", async () => {
    const preview = {
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: [],
      changed: ["Personal GitHub"],
      removed: [],
      definitions: [],
      selections: [],
    };
    const summary =
      "Shared with your team (version 8).\nPersonal GitHub: This team copy already has its own local credentials. Your personal server stays on in this profile.";
    api.teamPushPreview.mockResolvedValue(preview);
    api.teamPush.mockResolvedValue({
      version: 8,
      published: true,
      localSetupError: null,
      handoffs: [
        {
          id: "github",
          name: "Personal GitHub",
          outcome: "attention",
          message:
            "This team copy already has its own local credentials. Your personal server stays on in this profile.",
        },
      ],
      summary,
    });

    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
      await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
    await userEvent.click(await screen.findByRole("button", { name: "Share selected" }));
    const warning = await screen.findByText(/stays on in this profile/);
    expect(warning.className).toMatch(/text-warning/);
  });

  it("disables the confirm when nothing would be uploaded or switched", async () => {
    api.teamPushPreview.mockResolvedValue({
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: [],
      changed: [],
      removed: [],
      definitions: [],
      selections: [
        {
          id: "github",
          name: "Personal GitHub",
          teamChange: "Already shared",
          teamDetail:
            "The Team already has this exact definition, so nothing changes for the team.",
          notes: [],
          local: {
            id: "github",
            name: "Personal GitHub",
            outcome: "kept",
            message: "This profile keeps using the Team copy.",
          },
        },
      ],
    });
    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
      await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
    expect(await screen.findByRole("button", { name: "Share selected" })).toBeDisabled();
    expect(api.teamPush).not.toHaveBeenCalled();
  });

  it("hints how each personal server relates to the team from the last sync", () => {
    const withCopies: Registry = {
      ...registry,
      servers: [
        ...registry.servers,
        {
          id: "linear",
          name: "Linear",
          transport: "http",
          command: null,
          args: [],
          env: [],
          url: "https://mcp.linear.app/mcp",
          source: null,
        },
        {
          id: "team_github-1",
          name: "Personal GitHub",
          transport: "stdio",
          command: "python3",
          args: [],
          env: [],
          url: null,
          source: "team:team-1",
        },
        {
          id: "team_linear-portal-1",
          name: " linear ",
          transport: "http",
          command: null,
          args: [],
          env: [],
          url: "https://mcp.linear.app/mcp",
          source: "team:team-1",
        },
      ] as Registry["servers"],
      profiles: [{ id: "default", name: "Default", enabledServerIds: ["team_github-1"] }],
      team: {
        ...registry.team!,
        managedServerIds: {
          "team_github-1": "github",
          "team_linear-portal-1": "linear-portal",
        },
      },
    };
    render(<TeamsView registry={withCopies} onRegistryChange={vi.fn()} />);
    expect(
      screen.getByText("Shared. The Team copy is in use in this profile."),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "The team has a different server with this name. Sharing adds a separate definition.",
      ),
    ).toBeInTheDocument();
  });

  it.each([
    {
      transport: "stdio",
      command: "npx",
      args: ["-y", "some-tool"],
      url: null,
      prompt: /recognize this command/,
    },
    {
      transport: "http",
      command: null,
      args: [],
      url: "https://changed.example/mcp",
      prompt: /saved authentication/,
    },
  ])(
    "requires reference and destination confirmation before enabling a $transport team server",
    async ({ transport, command, args, url, prompt }) => {
      const withReviewServer: Registry = {
        ...registry,
        servers: [
          {
            id: "team-tool",
            name: "Team tool",
            transport,
            command,
            args,
            env: [
              {
                key: "TOKEN",
                secret: true,
                value: null,
                source: { ref: "op://Private/GitHub Token/credential" },
              },
            ],
            url,
            source: "team:team-1",
          },
        ] as Registry["servers"],
      };
      api.setServerEnabled.mockResolvedValue(withReviewServer);

      render(<TeamsView registry={withReviewServer} onRegistryChange={vi.fn()} />);
      await userEvent.click(screen.getByRole("button", { name: "Enable" }));
      // The ConfirmDialog's confirm button carries the same label as the trigger;
      // the dialog copy shows the exact command being consented to (the row also
      // renders the command, so anchor on dialog-only copy).
      expect(await screen.findByText(prompt)).toBeInTheDocument();
      expect(screen.getByRole("dialog")).toHaveTextContent(
        '1Password entry "op://Private/GitHub Token/credential" will be sent to',
      );
      expect(api.setServerEnabled).not.toHaveBeenCalled();
      const confirm = screen
        .getAllByRole("button", { name: "Enable" })
        .at(-1) as HTMLElement;
      await userEvent.click(confirm);

      // The fourth arg is the backend's consent assertion: without it the gate
      // in set_server_enabled refuses and Teams enable silently breaks.
      await waitFor(() =>
        expect(api.setServerEnabled).toHaveBeenCalledWith(
          "default",
          "team-tool",
          true,
          true,
          withReviewServer.servers.find((s) => s.id === "team-tool"),
        ),
      );
    },
  );

  it("discards a stale confirmation and requires a fresh preview", async () => {
    const preview = {
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: [],
      changed: ["GitHub"],
      removed: [],
      definitions: [],
      selections: [],
    };
    api.teamPushPreview.mockResolvedValue(preview);
    api.teamPush.mockRejectedValue(
      new Error("The team config changed; nothing was overwritten."),
    );

    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
      await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
    await userEvent.click(await screen.findByRole("button", { name: "Share selected" }));

    expect(
      await screen.findByText(/team config changed; nothing was overwritten/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Share selected" }),
    ).not.toBeInTheDocument();

    if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
      await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
    await waitFor(() => expect(api.teamPushPreview).toHaveBeenCalledTimes(2));
  });
});

describe("TeamsView instructions status", () => {
  const instructions: InstructionsStatusView = {
    content: "Use the approved tools.",
    version: 6,
    clients: [{ id: "claude", name: "Claude", state: "applied" }],
  };

  beforeEach(() => {
    vi.clearAllMocks();
    api.getRegistry.mockResolvedValue(registry);
  });

  it("keeps the last status after a failed refresh and retries", async () => {
    const refreshed: InstructionsStatusView = {
      content: "Use the newly approved tools.",
      version: 7,
      clients: [{ id: "claude", name: "Claude", state: "stale" }],
    };
    api.teamInstructionsStatus
      .mockResolvedValueOnce(instructions)
      .mockRejectedValueOnce(new Error("temporary read failure"))
      .mockResolvedValueOnce(refreshed);
    const onRegistryChange = vi.fn();
    const { rerender } = render(
      <TeamsView registry={registry} onRegistryChange={onRegistryChange} />,
    );

    expect(await screen.findByText(instructions.content)).toBeInTheDocument();
    expect(screen.getByText("Applied")).toBeInTheDocument();

    rerender(
      <TeamsView
        registry={{
          ...registry,
          team: { ...registry.team!, lastVersion: 7 },
        }}
        onRegistryChange={onRegistryChange}
      />,
    );

    expect(
      await screen.findByText("Couldn't refresh this status. Showing the last result."),
    ).toBeInTheDocument();
    expect(screen.getByText(instructions.content)).toBeInTheDocument();
    expect(screen.getByText("Applied")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText(refreshed.content)).toBeInTheDocument();
    expect(screen.getByText("Not applied yet")).toBeInTheDocument();
    expect(
      screen.queryByText("Couldn't refresh this status. Showing the last result."),
    ).not.toBeInTheDocument();
    expect(api.teamInstructionsStatus).toHaveBeenCalledTimes(3);
  });

  it("does not show another team's cached instructions after a failed refresh", async () => {
    api.teamInstructionsStatus
      .mockResolvedValueOnce(instructions)
      .mockRejectedValueOnce(new Error("temporary read failure"));
    const onRegistryChange = vi.fn();
    const { rerender } = render(
      <TeamsView registry={registry} onRegistryChange={onRegistryChange} />,
    );

    expect(await screen.findByText(instructions.content)).toBeInTheDocument();

    rerender(
      <TeamsView
        registry={{
          ...registry,
          team: { ...registry.team!, teamId: "team-2", lastVersion: 1 },
        }}
        onRegistryChange={onRegistryChange}
      />,
    );

    expect(
      await screen.findByText("Toolport couldn't load the instructions status."),
    ).toBeInTheDocument();
    expect(screen.queryByText(instructions.content)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("clears the card when a successful refresh reports no active instructions", async () => {
    api.teamInstructionsStatus
      .mockResolvedValueOnce(instructions)
      .mockResolvedValueOnce(null);
    const onRegistryChange = vi.fn();
    const { rerender } = render(
      <TeamsView registry={registry} onRegistryChange={onRegistryChange} />,
    );

    expect(await screen.findByText(instructions.content)).toBeInTheDocument();

    rerender(
      <TeamsView
        registry={{
          ...registry,
          team: { ...registry.team!, lastVersion: 7 },
        }}
        onRegistryChange={onRegistryChange}
      />,
    );

    await waitFor(() =>
      expect(screen.queryByText(instructions.content)).not.toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("heading", { name: "Team instructions" }),
    ).not.toBeInTheDocument();
  });
});

describe("Sync setup", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.getRegistry.mockResolvedValue(registry);
    openExternal.mockResolvedValue(undefined);
  });
  it("offers sign in before pairing with a clear manual fallback", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign in to sync" }));
    expect(openExternal).toHaveBeenCalledWith(
      "https://teams.toolport.app/?intent=pro&from=app-sync",
    );
    expect(screen.getByText("Use a manual code")).toBeInTheDocument();
    expect(screen.queryByText("Ask an admin")).not.toBeInTheDocument();
  });
  it("connects with a manual code", async () => {
    const changed = vi.fn();
    api.teamConnect.mockResolvedValue({ status: "connected", registry });
    render(<TeamsView registry={noTeam} onRegistryChange={changed} />);
    await userEvent.click(screen.getByText("Use a manual code"));
    await userEvent.type(screen.getByLabelText("Manual code"), "fixture-code");
    await userEvent.click(screen.getByRole("button", { name: "Sign in with code" }));
    await waitFor(() =>
      expect(api.teamConnect).toHaveBeenCalledWith(
        "https://teams.toolport.app",
        "fixture-code",
      ),
    );
    expect(changed).toHaveBeenCalledWith(registry);
  });
  it("refuses a plaintext public service before transmitting the code", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByText("Use a manual code"));
    await userEvent.clear(screen.getByLabelText("Sync service URL"));
    await userEvent.type(screen.getByLabelText("Sync service URL"), "http://example.com");
    await userEvent.type(screen.getByLabelText("Manual code"), "fixture-code");
    await userEvent.click(screen.getByRole("button", { name: "Sign in with code" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/https/);
    expect(api.teamConnect).not.toHaveBeenCalled();
  });
  it("explains trial, grace and blocked status in personal mode", () => {
    const solo = {
      ...registry,
      team: {
        ...registry.team!,
        accountStatus: {
          personalSync: true,
          plan: "pro",
          trialActive: true,
          trialEndsAt: Date.now() + 2 * 86400000,
          freeSyncGraceEndsAt: Date.now() + 86400000,
          deviceId: "fixture",
          canReceiveConfig: false,
          reason: "Choose your active device",
        },
        personalSyncState: { lastSyncedAt: Date.now(), error: "Network offline" },
      },
    };
    render(<TeamsView registry={solo} onRegistryChange={vi.fn()} />);
    expect(screen.getByText("Pro · unlimited devices")).toBeInTheDocument();
    expect(screen.getByText("2 trial days left")).toBeInTheDocument();
    expect(screen.getByText(/Every device keeps syncing until/)).toBeInTheDocument();
    expect(screen.getByText("Choose your active device")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Network offline");
    expect(screen.getByText(/Last synced/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Share selected/ }),
    ).not.toBeInTheDocument();
  });
  it("shows per-server publish errors and asks which local servers to sync", async () => {
    const personal = structuredClone(registry);
    personal.team!.accountStatus = {
      personalSync: true,
      plan: "pro",
      trialActive: false,
      trialEndsAt: null,
      freeSyncGraceEndsAt: null,
      deviceId: "device",
      canReceiveConfig: true,
      reason: null,
    };
    personal.servers = [
      {
        id: "local",
        name: "Private local",
        transport: "http",
        command: null,
        args: [],
        env: [],
        url: "https://example.com/mcp",
        source: "manual",
        syncLocalOnly: true,
      },
    ];
    personal.team!.personalSyncState = {
      chooseLocalServers: true,
      pending: { local: { localId: "local", after: {} } },
      publishErrors: { local: "env: references cannot sync" },
    };
    invoke.mockResolvedValue(personal);
    render(<TeamsView registry={personal} onRegistryChange={vi.fn()} />);
    expect(screen.getByLabelText("Choose local servers to sync")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Private local" })).not.toBeChecked();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Private local: env: references cannot sync",
    );
    await userEvent.click(screen.getByRole("checkbox", { name: "Private local" }));
    expect(invoke).toHaveBeenCalledWith("personal_sync_local_only", {
      serverId: "local",
      localOnly: false,
    });
  });
  it("sends the opaque conflict version instead of the displayed JSON", async () => {
    const personal = structuredClone(registry);
    personal.team!.accountStatus = {
      personalSync: true,
      plan: "pro",
      trialActive: false,
      trialEndsAt: null,
      freeSyncGraceEndsAt: null,
      deviceId: "device",
      canReceiveConfig: true,
      reason: null,
    };
    personal.team!.personalSyncState = {
      conflicts: { local: { requestTimeoutMs: 1 } },
      conflictVersions: { local: "opaque-version" },
    };
    invoke.mockResolvedValue(personal);
    api.getRegistry.mockResolvedValue(personal);
    render(<TeamsView registry={personal} onRegistryChange={vi.fn()} />);
    await userEvent.click(
      screen.getByRole("button", { name: "Keep this machine's version" }),
    );
    expect(invoke).toHaveBeenCalledWith("personal_sync_resolve_conflict", {
      id: "local",
      expected: "opaque-version",
      keepMine: true,
    });
  });
  it("surfaces account status failures while keeping the governed view", () => {
    const governed = structuredClone(registry);
    governed.team!.accountStatusError = "Account status returned 403";
    render(<TeamsView registry={governed} onRegistryChange={vi.fn()} />);
    expect(screen.getByText("Account status returned 403")).toBeInTheDocument();
    expect(screen.getByText("Linked to team")).toBeInTheDocument();
  });
  it("keeps multi-person governance and the unloaded view", () => {
    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    expectNoPitch();
    expect(screen.getByText("Linked to team")).toBeInTheDocument();
  });

  it("shows no pitch before the registry has loaded", () => {
    render(<TeamsView registry={null} onRegistryChange={vi.fn()} />);

    // `registry` is null until the first read lands, and stays null all session if that
    // read fails. Treating that as "no team" pitches Teams at people who are already on
    // one — the single audience this page must never sell to. Not knowing is its own
    // state, and it renders as neither answer.
    expectNoPitch();
    expect(screen.getByLabelText("Loading Toolport Teams")).toBeInTheDocument();
  });
});

/** The app quotes a price in exactly one place. This is the guard that the copy and the
 * numbers behind it cannot drift apart inside the app; toolport.app/teams#pricing stays
 * the authority for whether the numbers themselves are still right. */
describe("Teams plan copy", () => {
  it("builds its copy from the shared numbers", () => {
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_BASE_PRICE}/month`);
    // "/month" on the seat price too: "$4 per person" reads as a one-time charge to add
    // someone, which undersells nothing and oversells the bill.
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_SEAT_PRICE}/month per additional person`);
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_ANNUAL_PRICE}/year`);
    expect(TEAMS_PAID_LINE).toContain(
      `$${TEAMS_ANNUAL_SEAT_PRICE}/year on annual billing`,
    );
    expect(TEAMS_ANNUAL_SEAT_PRICE).toBe(TEAMS_SEAT_PRICE * 10);
    expect(TEAMS_PAID_LINE).toContain(
      `for your whole team, up to ${TEAMS_TEAM_SEATS} people`,
    );
    expect(TEAMS_PAID_LINE).toMatch(/same price hosted or self-hosted/i);
  });

  it("uses no em dashes or en dashes", () => {
    for (const line of [TEAMS_FREE_LINE, TEAMS_PAID_LINE]) {
      expect(line).not.toMatch(/[—–]/);
    }
  });
});

describe("Teams member review", () => {
  const change = {
    key: "server:remote",
    title: "Server: Public tool",
    hash: "reviewed-content-hash",
    fields: [
      {
        field: "URL",
        before: "https://old.example/mcp",
        after: "https://new.example/mcp",
      },
    ],
    labels: [
      {
        author: { name: "Alice" },
        at: 1791417600000,
        via: "dashboard",
        approvedBy: { name: "Bob" },
      },
    ],
  };
  const reviewedRegistry = (labels = change.labels): Registry => ({
    ...registry,
    team: {
      ...registry.team!,
      memberReview: { pending: { [change.key]: { ...change, labels } } },
    } as NonNullable<Registry["team"]>,
  });

  beforeEach(() => {
    vi.clearAllMocks();
    invoke.mockResolvedValue(registry);
  });

  it.each([true, false])(
    "shows the full diff and submits the displayed hash, accept=%s",
    async (accept) => {
      const onRegistryChange = vi.fn();
      render(
        <TeamsView registry={reviewedRegistry()} onRegistryChange={onRegistryChange} />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Review team changes" }));
      const dialog = screen.getByRole("dialog");
      expect(
        within(dialog)
          .getByText(/Before:/)
          .closest("dd"),
      ).toHaveTextContent("https://old.example/mcp");
      expect(
        within(dialog)
          .getByText(/After:/)
          .closest("dd"),
      ).toHaveTextContent("https://new.example/mcp");
      expect(within(dialog).getByText(/Alice/)).toHaveTextContent(
        "via dashboard · approved by Bob",
      );
      await userEvent.click(
        within(dialog).getByRole("button", {
          name: `${accept ? "Accept" : "Reject"} ${change.title}`,
        }),
      );
      expect(invoke).toHaveBeenCalledWith("team_review", {
        key: change.key,
        hash: change.hash,
        accept,
      });
      await waitFor(() => expect(onRegistryChange).toHaveBeenCalledWith(registry));
    },
  );

  it("shows an unlabelled full diff for unavailable history", async () => {
    render(<TeamsView registry={reviewedRegistry([])} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Review team changes" }));
    expect(screen.getByText(/Full diff from your accepted configuration/)).toBeVisible();
    expect(screen.queryByText(/Alice/)).toBeNull();
  });

  it("leaves the review open with an explicit stale-content error", async () => {
    invoke.mockRejectedValueOnce(
      "The team change was updated. Review its new content before accepting.",
    );
    const onRegistryChange = vi.fn();
    render(
      <TeamsView registry={reviewedRegistry()} onRegistryChange={onRegistryChange} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Review team changes" }));
    await userEvent.click(
      within(screen.getByRole("dialog")).getByRole("button", {
        name: `Accept ${change.title}`,
      }),
    );
    await waitFor(() =>
      expect(
        within(screen.getByRole("dialog")).getByText(/The team change was updated/),
      ).toBeInTheDocument(),
    );
    expect(screen.getByRole("dialog")).toBeVisible();
    expect(onRegistryChange).not.toHaveBeenCalled();
  });
});

it("shows a 202 proposal with an Open action instead of a webview link", async () => {
  vi.clearAllMocks();
  const preview = {
    baseVersion: 7,
    localFingerprint: "hash",
    added: ["Personal GitHub"],
    changed: [],
    removed: [],
    definitions: [],
    selections: [],
  };
  api.teamPushPreview.mockResolvedValue(preview);
  api.teamPush.mockResolvedValue({
    version: 7,
    published: false,
    localSetupError: null,
    handoffs: [],
    summary: "Sent for confirmation",
    proposal: {
      id: "p",
      baseVersion: 7,
      confirmUrl: "https://teams.toolport.app/#changes=t/p",
    },
  });
  api.getRegistry.mockResolvedValue(registry);
  invoke.mockResolvedValue(undefined);
  render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
  if (!(screen.getByRole("checkbox") as HTMLInputElement).checked)
    await userEvent.click(screen.getByRole("checkbox"));
  await userEvent.click(screen.getByRole("button", { name: "Share selected servers" }));
  await screen.findByRole("dialog");
  await userEvent.click(screen.getByRole("button", { name: "Share selected" }));
  expect(await screen.findByText("Sent for confirmation")).toBeVisible();
  const open = screen.getByRole("button", { name: "Open confirmation" });
  expect(screen.queryByRole("link", { name: "Open confirmation" })).toBeNull();
  await userEvent.click(open);
  expect(invoke).toHaveBeenCalledWith("team_open_confirmation", {
    url: "https://teams.toolport.app/#changes=t/p",
  });
});

it("shows an offline result with the last successful sync, and sign-in for a team 401", async () => {
  api.teamSyncStatus.mockResolvedValueOnce({
    state: "offline",
    lastSuccessMs: 1791504000000,
  });
  const server = {
    ...registry.servers[0],
    id: "team-linear",
    name: "Team Linear",
    source: "team:team-1",
    transport: "http" as const,
    command: null,
    url: "https://linear.example/mcp",
    enabled: true,
  };
  render(
    <TeamsView
      registry={{ ...registry, version: 3, servers: [server] }}
      onRegistryChange={vi.fn()}
      health={{
        [server.id]: {
          serverId: server.id,
          ok: false,
          toolCount: 0,
          authRequired: true,
          authTarget: "endpoint",
          error: "HTTP 401",
          failure: { kind: "auth", target: "endpoint" },
        },
      }}
    />,
  );
  await screen.findByText("Offline: cannot reach the team server");
  expect(screen.getByText(/Last successful sync:/)).toHaveTextContent(
    new Date(1791504000000).toLocaleString(),
  );
  expect(screen.getByText("Needs sign-in")).toBeVisible();
  expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
  expect(screen.queryByText(/None declared/)).not.toBeInTheDocument();
});

it("does not show another team's successful sync time after switching teams", async () => {
  api.teamSyncStatus.mockResolvedValueOnce({
    state: "synced",
    lastSuccessMs: 1791504000000,
  });
  const view = render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
  await screen.findByText("Last sync succeeded");
  api.teamSyncStatus.mockRejectedValueOnce(new Error("status unavailable"));
  view.rerender(
    <TeamsView
      registry={{ ...registry, team: { ...registry.team!, teamId: "other-team" } }}
      onRegistryChange={vi.fn()}
    />,
  );
  expect(screen.queryByText("Last sync succeeded")).not.toBeInTheDocument();
  await screen.findByText("Last successful sync: not recorded yet");
});

it("shows both conflict versions as fields with the server name and highlighted differences", () => {
  const personal = structuredClone(registry);
  personal.team!.accountStatus = {
    personalSync: true,
    plan: "pro",
    trialActive: false,
    trialEndsAt: null,
    freeSyncGraceEndsAt: null,
    deviceId: "device",
    canReceiveConfig: true,
    reason: null,
  };
  personal.team!.personalSyncState = {
    conflicts: {
      "docs-http": {
        name: "Toolport docs",
        url: "https://gitmcp.io/btsouth/Toolport2026",
        args: ["machine B v2"],
      },
    },
    pending: {
      "docs-http": {
        localId: "docs-http",
        after: {
          name: "Toolport docs",
          url: "https://example.com/this",
          args: ["machine A v2"],
        },
      },
    },
  };
  personal.servers = [
    { id: "docs-http", name: "Toolport docs", transport: "http", args: [], env: [] },
  ];
  render(<TeamsView registry={personal} onRegistryChange={vi.fn()} />);
  expect(screen.getByText("Toolport docs changed on both machines")).toBeInTheDocument();
  expect(screen.getByText("This machine")).toBeInTheDocument();
  expect(screen.getByText("Other machine")).toBeInTheDocument();
  expect(screen.getByText("machine A v2")).toBeInTheDocument();
  expect(screen.getByText("machine B v2")).toBeInTheDocument();
  expect(screen.getAllByText("CHANGED: URL")).toHaveLength(2);
});
it("uses the chosen sync service for browser sign-in", async () => {
  render(<TeamsView registry={{ ...registry, team: null }} onRegistryChange={vi.fn()} />);
  await userEvent.click(screen.getByText("Use a manual code"));
  const url = screen.getByLabelText("Sync service URL");
  await userEvent.clear(url);
  await userEvent.type(url, "https://sync.example.com");
  await userEvent.click(screen.getByRole("button", { name: "Sign in to sync" }));
  expect(openExternal).toHaveBeenCalledWith(
    "https://sync.example.com/?intent=pro&from=app-sync",
  );
});
