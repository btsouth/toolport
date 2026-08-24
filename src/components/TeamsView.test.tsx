import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { InstructionsStatusView, Registry } from "@/lib/types";

const api = vi.hoisted(() => ({
  teamConnect: vi.fn(),
  teamJoinPoll: vi.fn(),
  teamSync: vi.fn(),
  teamDisconnect: vi.fn(),
  teamPushPreview: vi.fn(),
  teamPush: vi.fn(),
  teamInstructionsStatus: vi.fn().mockResolvedValue(null),
  setServerEnabled: vi.fn(),
}));

vi.mock("@/lib/api", () => api);
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(vi.fn()),
}));

const { openExternal } = vi.hoisted(() => ({ openExternal: vi.fn() }));
vi.mock("@/lib/openUrl", () => ({ openExternal }));

import { TeamsView } from "./TeamsView";
import { TEAMS_CREATE_URL, TEAMS_PRICING_URL, TEAMS_SELFHOST_URL } from "@/lib/teamUrl";
import {
  TEAMS_ANNUAL_PRICE,
  TEAMS_BASE_PRICE,
  TEAMS_FREE_LINE,
  TEAMS_FREE_SEATS,
  TEAMS_PAID_LINE,
  TEAMS_SEAT_PRICE,
  TEAMS_TRIAL_DAYS,
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
  servers: [],
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
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("shows a deterministic diff and does not push until the admin confirms", async () => {
    const preview = {
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: ["Alpha", "beta"],
      changed: ["GitHub"],
      removed: ["Legacy"],
    };
    api.teamPushPreview.mockResolvedValue(preview);
    api.teamPush.mockResolvedValue(8);

    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Update shared servers" }));

    expect(await screen.findByText("Added (2)")).toBeInTheDocument();
    expect(screen.getByText("Changed (1)")).toBeInTheDocument();
    expect(screen.getByText("Removed (1)")).toBeInTheDocument();
    for (const name of ["Alpha", "beta", "GitHub", "Legacy"]) {
      expect(screen.getByText(name)).toBeInTheDocument();
    }
    expect(api.teamPush).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Replace shared servers" }));
    await waitFor(() => expect(api.teamPush).toHaveBeenCalledWith(preview));
    expect(await screen.findByText(/now version 8/i)).toBeInTheDocument();
  });

  it("passes reviewed=true when the member confirms enabling a review server", async () => {
    const withReviewServer: Registry = {
      ...registry,
      servers: [
        {
          id: "team-tool",
          name: "Team tool",
          transport: "stdio",
          command: "npx",
          args: ["-y", "some-tool"],
          env: [],
          url: null,
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
    expect(await screen.findByText(/recognize this command/)).toBeInTheDocument();
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
      ),
    );
  });

  it("discards a stale confirmation and requires a fresh preview", async () => {
    const preview = {
      baseVersion: 7,
      localFingerprint: "preview-fingerprint",
      added: [],
      changed: ["GitHub"],
      removed: [],
    };
    api.teamPushPreview.mockResolvedValue(preview);
    api.teamPush.mockRejectedValue(
      new Error("The team config changed; nothing was overwritten."),
    );

    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Update shared servers" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Replace shared servers" }),
    );

    expect(
      await screen.findByText(/team config changed; nothing was overwritten/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Replace shared servers" }),
    ).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Update shared servers" }));
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

/** The disconnected Teams tab is the only sales page Toolport Teams gets in front of a
 * free user, and it is also the join form for someone who already has a code. These
 * tests hold both halves: the pitch has to be there, and it must not have pushed the
 * form down or broken it. */
describe("TeamsView disconnected pitch", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    openExternal.mockClear();
  });

  it("keeps the connect form ahead of the pitch in the DOM", () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    const form = screen.getByRole("heading", { name: "Have an invite or connect code?" });
    const pitch = screen.getByRole("heading", { name: "No team yet?" });

    // Someone who came here holding a code is the conversion this page already has. If
    // the pitch ever lands first in the DOM it also lands first on a narrow window,
    // where the lanes stack, and that person has to scroll past an ad to paste a code.
    expect(form.compareDocumentPosition(pitch)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  it("still connects with a pasted invite code", async () => {
    const onRegistryChange = vi.fn();
    api.teamConnect.mockResolvedValue({ status: "connected", registry: noTeam });

    render(<TeamsView registry={noTeam} onRegistryChange={onRegistryChange} />);
    await userEvent.type(
      screen.getByPlaceholderText("Paste your invite or connect code"),
      "invite-abc",
    );
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    await waitFor(() =>
      expect(api.teamConnect).toHaveBeenCalledWith(
        "https://teams.toolport.app",
        "invite-abc",
        undefined,
      ),
    );
    expect(onRegistryChange).toHaveBeenCalledWith(noTeam);
  });

  it("offers a way to start a team, which the desktop app cannot do itself", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    await userEvent.click(screen.getByRole("button", { name: /Create a free team/ }));

    // The hosted app reads both of these: `intent` restores team creation after the
    // sign-in round trip, `from` attributes the app tab separately from the marketing
    // funnel. Dropping either silently degrades to the generic manage view.
    expect(openExternal).toHaveBeenCalledWith(TEAMS_CREATE_URL);
    expect(TEAMS_CREATE_URL).toContain("intent=create-team");
    expect(TEAMS_CREATE_URL).toContain("from=app-teams-tab");
  });

  it("states the free tier and what the paid tier actually buys", () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    expect(screen.getByText(TEAMS_FREE_LINE)).toBeInTheDocument();
    expect(screen.getByText(TEAMS_PAID_LINE)).toBeInTheDocument();
    // Team costs the same at the free seat count as Free does; the difference is
    // governance. Quoting a per-person price on its own would read as a seat paywall.
    expect(TEAMS_PAID_LINE).toMatch(/access control/i);
    // Anchored to the phrase, not to the bare digit: "5" also appears inside "$39" and
    // "$390", so a `toContain("5")` would survive the seat count being dropped entirely.
    expect(TEAMS_FREE_LINE).toContain(`up to ${TEAMS_FREE_SEATS} people`);
  });

  it("says how long the free trial of Team features lasts", () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    // The number is the whole reason "Create a free team" is not a commitment. It is
    // interpolated, so it can silently vanish without the surrounding sentence changing.
    expect(
      screen.getByText(new RegExp(`free to try for ${TEAMS_TRIAL_DAYS} days`)),
    ).toBeInTheDocument();
  });

  it("shows why a team is worth having, not just what it costs", () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    // Price answers "how much", these answer "why at all". They are the only part of the
    // page that names a problem the reader already has.
    for (const title of PAIN_TILES) {
      expect(screen.getByText(title)).toBeInTheDocument();
    }
  });

  it("refuses a non-https team server URL before it reaches the backend", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    const url = screen.getByPlaceholderText("https://toolport.yourcompany.com");
    await userEvent.clear(url);
    await userEvent.type(url, "http://teams.evil.example.com");
    await userEvent.type(
      screen.getByPlaceholderText("Paste your invite or connect code"),
      "invite-abc",
    );
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    // The check has to be worth something to the person reading it: a rejected URL that
    // says nothing is indistinguishable from a broken button. And it must run before the
    // call, not after — an invite code posted over plaintext http is already spent.
    expect(await screen.findByText(/must use https:\/\//i)).toBeInTheDocument();
    expect(api.teamConnect).not.toHaveBeenCalled();
  });

  it("keeps self-hosting a first-class option, not a footnote", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    expect(screen.getByText(/self-hosted on your own network/i)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Self-host it/ }));
    expect(openExternal).toHaveBeenCalledWith(TEAMS_SELFHOST_URL);
  });

  it("links out for the authoritative price", async () => {
    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);

    await userEvent.click(screen.getByRole("button", { name: /Pricing/ }));
    expect(openExternal).toHaveBeenCalledWith(TEAMS_PRICING_URL);
  });

  it("shows none of the pitch once a team is connected", () => {
    render(<TeamsView registry={registry} onRegistryChange={vi.fn()} />);

    // The ask happens once, on a tab the person chose to open, and stops the moment it
    // has been answered. A member should never see marketing for the thing they joined,
    // and that covers every piece of it: headings, prices, buttons and pain tiles alike.
    expectNoPitch();
    expect(screen.getByText("Connected")).toBeInTheDocument();
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

  it("drops the pitch while a join waits for an admin", async () => {
    api.teamConnect.mockResolvedValue({ status: "pending", requestToken: "req-1" });
    api.teamJoinPoll.mockResolvedValue({ status: "pending" });

    render(<TeamsView registry={noTeam} onRegistryChange={vi.fn()} />);
    await userEvent.type(
      screen.getByPlaceholderText("Paste your invite or connect code"),
      "invite-abc",
    );
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));

    expect(
      await screen.findByText(/Leave this open, it finishes on its own/),
    ).toBeVisible();
    // This person has picked their team and is waiting on a human. Offering them a second
    // team to create, at a price, is the app arguing against the thing it just did.
    expectNoPitch();
  });
});

/** The app quotes a price in exactly one place. This is the guard that the copy and the
 * numbers behind it cannot drift apart inside the app; toolport.app/teams#pricing stays
 * the authority for whether the numbers themselves are still right. */
describe("Teams plan copy", () => {
  it("builds its copy from the shared numbers", () => {
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_BASE_PRICE}/month`);
    // "/month" on the seat price too: "$12 per person" reads as a one-time charge to add
    // someone, which undersells nothing and oversells the bill.
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_SEAT_PRICE}/month per person`);
    expect(TEAMS_PAID_LINE).toContain(`$${TEAMS_ANNUAL_PRICE}/year`);
    expect(TEAMS_PAID_LINE).toContain(`up to ${TEAMS_FREE_SEATS}`);
    expect(TEAMS_PAID_LINE).toMatch(/same price hosted or self-hosted/i);
  });

  it("uses no em dashes or en dashes", () => {
    for (const line of [TEAMS_FREE_LINE, TEAMS_PAID_LINE]) {
      expect(line).not.toMatch(/[—–]/);
    }
  });
});
