import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  dismissSkillIssue,
  fixSkillIssue,
  openSkillPath,
  openSkillPathWith,
  removeSkill,
  setSkillState,
  skillOpenApps,
  skillsInventory,
} from "@/lib/api";
import { applySkillState, skillsFixture } from "@/test/skills-fixture";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { toastError } from "@/lib/toast";
import { SKILLS_REFRESH_EVENT, SKILLS_SEARCH_EVENT } from "@/lib/shortcuts";
import { columnNames, shortAgentName } from "@/lib/agentNames";
import { SkillsView } from "./SkillsView";

vi.mock("@/lib/api", () => ({
  skillsInventory: vi.fn(),
  setSkillState: vi.fn(),
  fixSkillIssue: vi.fn(),
  openSkillPath: vi.fn(),
  openSkillPathWith: vi.fn(),
  skillOpenApps: vi.fn(),
  removeSkill: vi.fn(),
  dismissSkillIssue: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

beforeEach(() => {
  localStorage.clear();
  vi.mocked(skillsInventory).mockResolvedValue(skillsFixture());
  vi.mocked(setSkillState).mockImplementation(async (agent, key, state) =>
    applySkillState(skillsFixture(), agent, key, state),
  );
  vi.mocked(fixSkillIssue).mockImplementation(async (issue) => {
    const inv = skillsFixture();
    return { ...inv, issues: inv.issues.filter((i) => i.id !== issue) };
  });
  vi.mocked(openSkillPath).mockResolvedValue(undefined);
  vi.mocked(openSkillPathWith).mockResolvedValue(undefined);
  vi.mocked(skillOpenApps).mockResolvedValue([
    { id: "obsidian.desktop", name: "Obsidian", default: true, chosen: false },
    { id: "dev.zed.Zed.desktop", name: "Zed", default: false, chosen: false },
  ]);
  vi.mocked(removeSkill).mockImplementation(async (key) => {
    const inv = skillsFixture();
    return {
      trashed: "~/.agents/skills/triage",
      links: 1,
      inventory: { ...inv, skills: inv.skills.filter((s) => s.key !== key) },
    };
  });
  vi.mocked(dismissSkillIssue).mockImplementation(async (fingerprint, dismissed) => {
    const inv = skillsFixture();
    return {
      ...inv,
      issues: inv.issues.map((i) =>
        i.fingerprint === fingerprint ? { ...i, dismissed } : i,
      ),
    };
  });
});

function renderView() {
  return render(
    <TooltipProvider>
      <SkillsView />
    </TooltipProvider>,
  );
}

describe("SkillsView", () => {
  it("shows all detected agents when none are connected", async () => {
    renderView();
    await screen.findByRole("region", { name: "Skills" });
    expect(screen.getByRole("button", { name: "Filter by agent" })).toHaveTextContent(
      "All detected agents",
    );
    await userEvent.click(screen.getByRole("button", { name: "Filter by agent" }));
    expect(
      screen.queryByRole("menuitemradio", { name: "Connected to Toolport" }),
    ).toBeNull();
    expect(
      screen.getByRole("menuitemradio", { name: "All detected agents" }),
    ).toHaveAttribute("aria-checked", "true");
  });

  it("defaults to exact connected profiles and offers every detected profile in the same selector", async () => {
    const inv = skillsFixture();
    const claude = inv.agents[0];
    const codex = inv.agents[1];
    inv.agents = [
      ...["claude", "claude2", "claude3", "claude-api"].map((name, i) => ({
        ...claude,
        id: i ? `claude-code@${name}` : "claude-code",
        name: `Claude Code (${name})`,
        connected: i < 2,
      })),
      ...["codex", "work", "second", "becca"].map((name, i) => ({
        ...codex,
        id: i ? `codex@${name}` : "codex",
        name: `Codex (${name})`,
        connected: i === 0,
      })),
    ];
    for (const skill of inv.skills) {
      for (const agent of inv.agents) {
        const base = agent.id.startsWith("codex") ? "codex" : "claude-code";
        if (skill.states[base]) skill.states[agent.id] = { ...skill.states[base] };
      }
    }
    const privateSkill = {
      ...inv.skills[0],
      key: "private",
      name: "work-only",
      states: { "codex@work": { state: "on" as const, via: "work" } },
    };
    inv.skills.push(privateSkill);
    vi.mocked(skillsInventory).mockResolvedValue(inv);
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    expect(screen.getByRole("button", { name: "Filter by agent" })).toHaveTextContent(
      "Connected to Toolport",
    );
    expect(
      within(list).getAllByRole("button", { name: /Show only skills .* finds/ }),
    ).toHaveLength(3);
    expect(within(list).queryByRole("button", { name: /work-only/ })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Filter by agent" }));
    await userEvent.click(
      screen.getByRole("menuitemradio", { name: "All detected agents" }),
    );
    expect(
      within(list).getAllByRole("button", { name: /Show only skills .* finds/ }),
    ).toHaveLength(8);
    expect(within(list).getByRole("button", { name: /work-only/ })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: /^Rules/ }));
    const rules = screen.getByRole("region", { name: "Rules" });
    expect(
      within(rules).getAllByRole("button", { name: /Show only what .* loads/ }),
    ).toHaveLength(8);
    await userEvent.click(screen.getByRole("button", { name: "Filter by agent" }));
    await userEvent.click(
      screen.getByRole("menuitemradio", { name: "Connected to Toolport" }),
    );
    expect(
      within(rules).getAllByRole("button", { name: /Show only what .* loads/ }),
    ).toHaveLength(3);
  });

  it("shows each skill's state per agent and the first skill's details", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    const row = within(list).getByRole("button", { name: /screenshot/ });
    expect(
      within(row).getByRole("img", { name: "Claude Code: Off" }),
    ).toBeInTheDocument();
    expect(within(row).getByRole("img", { name: "Cursor: On" })).toBeInTheDocument();
    // Only your own skills start open; other groups are one click away.
    expect(within(list).queryByRole("button", { name: /pdf/ })).not.toBeInTheDocument();
    await userEvent.click(
      within(list).getByRole("button", { name: /claude\.ai account/ }),
    );
    const pdf = within(list).getByRole("button", { name: /pdf/ });
    expect(
      within(pdf).getByRole("img", { name: "Grok: Another copy wins" }),
    ).toBeInTheDocument();
    expect(
      within(pdf).getByRole("img", { name: "Codex: Not loaded" }),
    ).toBeInTheDocument();
    const details = screen.getByRole("complementary", { name: "Skill details" });
    expect(
      within(details).getByRole("heading", { name: "release-notes" }),
    ).toBeInTheDocument();
    expect(screen.getByText("3 things need a look")).toBeInTheDocument();
  });

  it("filters to one agent from its column", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(
      within(list).getByRole("button", { name: "Show only skills Codex finds" }),
    );
    expect(within(list).queryByRole("button", { name: /pdf/ })).not.toBeInTheDocument();
    expect(within(list).getByRole("button", { name: /triage/ })).toBeInTheDocument();
  });

  it("writes a switch through the client's own setting and shows the result", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    await userEvent.click(
      within(details).getByRole("switch", { name: "Load screenshot in OpenCode" }),
    );
    expect(setSkillState).toHaveBeenCalledWith(
      "opencode",
      "/home/you/.agents/skills/screenshot/SKILL.md",
      "off",
      null,
    );
    expect(
      await within(details).findByRole("switch", { name: "Load screenshot in OpenCode" }),
    ).not.toBeChecked();
  });

  it("offers Claude Code's name-only state and leaves agents without a switch read-only", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /triage/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    // Name only is still loaded: the switch stays on and the chip says which way.
    const claude = within(details).getByRole("switch", {
      name: "Load triage in Claude Code",
    });
    expect(claude).toBeChecked();
    const claudeRow = claude.closest("li")!;
    expect(within(claudeRow).getByText("Name only")).toBeInTheDocument();
    await userEvent.click(
      within(claudeRow).getByRole("button", { name: "More options for Claude Code" }),
    );
    const item = await screen.findByRole("menuitemcheckbox", {
      name: "Load name and description only",
    });
    expect(item).toHaveAttribute("aria-checked", "true");
    await userEvent.click(item);
    expect(setSkillState).toHaveBeenCalledWith(
      "claude-code",
      "/home/you/.agents/skills/triage/SKILL.md",
      "on",
      null,
    );
    // Cursor has no switch Toolport can write: a lock that says why, and no switch.
    expect(
      within(details).queryByRole("switch", { name: "Load triage in Cursor" }),
    ).not.toBeInTheDocument();
    const lock = within(details).getByRole("img", {
      name: /Cursor has no per-skill switch/,
    });
    const cursorRow = lock.closest("li")!;
    expect(lock).toHaveAttribute("tabindex", "0");
    expect(within(cursorRow).getByText("On")).toBeInTheDocument();
    expect(
      within(cursorRow).getByText(/No switch Toolport can write/),
    ).toBeInTheDocument();
    // Only Claude Code has the menu.
    expect(
      within(details).queryByRole("button", { name: "More options for Codex" }),
    ).not.toBeInTheDocument();
  });

  it("turns Claude Code's name-only state on from its menu", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /release-notes/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    const claudeRow = within(details)
      .getByRole("switch", { name: "Load release-notes in Claude Code" })
      .closest("li")!;
    await userEvent.click(
      within(claudeRow).getByRole("button", { name: "More options for Claude Code" }),
    );
    await userEvent.click(
      await screen.findByRole("menuitemcheckbox", {
        name: "Load name and description only",
      }),
    );
    expect(setSkillState).toHaveBeenCalledWith(
      "claude-code",
      "/home/you/.agents/skills/release-notes/SKILL.md",
      "name-only",
      null,
    );
    expect(await within(claudeRow).findByText("Name only")).toBeInTheDocument();
  });

  it("leaves the control slot empty for an agent that doesn't load the skill", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(
      within(list).getByRole("button", { name: /claude\.ai account/ }),
    );
    await userEvent.click(within(list).getByRole("button", { name: /pdf/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    const codexRow = within(details).getByText("Codex").closest("li")!;
    expect(within(codexRow).getByText("Not loaded")).toBeInTheDocument();
    expect(within(codexRow).queryByRole("switch")).not.toBeInTheDocument();
    expect(within(codexRow).queryByRole("img")).not.toBeInTheDocument();
  });

  it("names the hovered column in the grid header and filters from the Agent menu", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    const row = within(list).getByRole("button", { name: /screenshot/ });
    await userEvent.hover(within(row).getByRole("img", { name: "Codex: Off" }));
    expect(
      within(list).getByText((_, el) => el?.textContent === "Codex: Off in config.toml"),
    ).toBeInTheDocument();
    const codexHeader = within(list).getByRole("button", {
      name: "Show only skills Codex finds",
    });
    await userEvent.unhover(within(row).getByRole("img", { name: "Codex: Off" }));
    await userEvent.click(codexHeader);
    // Filtering keeps the other columns at full strength.
    expect(
      within(list).getByRole("button", { name: "Show only skills Cursor finds" }),
    ).not.toHaveClass("opacity-40");
    await userEvent.click(screen.getByRole("button", { name: "Filter by agent" }));
    await userEvent.click(
      await screen.findByRole("menuitemradio", { name: "All detected agents" }),
    );
    expect(codexHeader).toHaveAttribute("aria-pressed", "false");
    await userEvent.click(screen.getByRole("button", { name: "Filter by agent" }));
    await userEvent.click(await screen.findByRole("menuitemradio", { name: "Codex" }));
    expect(
      within(list).getByRole("button", { name: "Show only skills Codex finds" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(within(list).queryByRole("button", { name: /pdf/ })).not.toBeInTheDocument();
  });

  it("tells keyboard and screen reader users an agent's folders weren't checked", async () => {
    const inv = skillsFixture();
    inv.agents = inv.agents.map((a) => (a.id === "grok" ? { ...a, verified: false } : a));
    vi.mocked(skillsInventory).mockResolvedValue(inv);
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    const header = within(list).getByRole("button", {
      name: "Show only skills Grok finds",
    });
    expect(header).toHaveAccessibleDescription(
      "folders from its docs, not checked on this machine",
    );
    act(() => header.focus());
    expect(
      within(list).getByText(
        (_, el) =>
          el?.textContent === "Grok: folders from its docs, not checked on this machine",
      ),
    ).toBeVisible();
  });

  it("keeps the keyboard column named when the pointer leaves", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    const header = within(list).getByRole("button", {
      name: "Show only skills Codex finds",
    });
    await userEvent.click(header);
    await userEvent.unhover(header);
    expect(header).toHaveFocus();
    expect(within(list).getByText("Codex", { exact: true })).toBeVisible();
    const dot = within(
      within(list).getByRole("button", { name: /screenshot/ }),
    ).getByRole("img", { name: "Cursor: On" });
    await userEvent.hover(dot);
    expect(
      within(list).getByText(
        (_, el) => el?.textContent === "Cursor: On via ~/.agents/skills",
      ),
    ).toBeVisible();
    await userEvent.unhover(dot);
    expect(within(list).getByText("Codex", { exact: true })).toBeVisible();
  });

  it("keeps a short agent name to 8 characters", () => {
    expect(shortAgentName("Claude Code")).toBe("Claude");
    expect(shortAgentName("Copilot CLI")).toBe("Copilot");
    expect(shortAgentName("OpenCode")).toBe("OpenCode");
    expect(shortAgentName("Antigravity")).toBe("Antigra…");
    expect(
      columnNames([
        "Claude Code (claude)",
        "Claude Code (claude2)",
        "Codex (codex)",
        "Codex (codex-becca)",
        "Cursor",
        "Command Code",
      ]),
    ).toEqual(["claude", "claude2", "codex", "becca", "Cursor", "Command"]);
    expect(
      columnNames(["Claude Code (claude)", "Codex (codex-work)", "Copilot CLI"]),
    ).toEqual(["Claude", "Codex", "Copilot"]);
    expect(columnNames(["Codex (codex-second-account)", "Codex (codex)"])).toEqual([
      "second-…",
      "codex",
    ]);
  });

  it("shows how each agent reaches a rules file, including skipped imports", async () => {
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Rules/ }));
    await userEvent.click(screen.getByRole("button", { name: /CLAUDE\.md/ }));
    const details = screen.getByRole("complementary", { name: "Rules file details" });
    expect(
      within(details).getByText("skips @import ~/.agents/working-agreements.md"),
    ).toBeInTheDocument();
    expect(
      within(details).getByText(
        "Cursor keeps user rules in its app settings, not a file.",
      ),
    ).toBeInTheDocument();
  });

  it("confirms before applying a fix", async () => {
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    const fixButton = screen.getByRole("button", { name: "Remove link" });
    // A fix repeats on every card, so it's a secondary action, after Dismiss, in the card's
    // own actions group that moves under the details on a narrow card.
    expect(fixButton).toHaveAttribute("data-variant", "outline");
    expect(fixButton.closest("[data-slot=issue-actions]")).not.toBeNull();
    await userEvent.click(fixButton);
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Remove link" }));
    expect(fixSkillIssue).toHaveBeenCalledWith(
      "broken-links",
      "broken-links:00000002",
      null,
    );
    await waitFor(() =>
      expect(screen.queryByText("1 link to a missing folder")).not.toBeInTheDocument(),
    );
  });

  it("lists the links a rules fix will make before making them", async () => {
    const inv = skillsFixture();
    vi.mocked(skillsInventory).mockResolvedValue({
      ...inv,
      issues: [
        {
          id: "rules-missing",
          level: "info",
          title: "OpenCode gets no global rules",
          detail: "Your global rules files reach other clients but not these.",
          agents: ["opencode"],
          skills: [],
          skillNames: [],
          fixLabel: "Link to working-agreements.md",
          fixNote:
            "Toolport makes each file below a link to ~/.agents/working-agreements.md.",
          fixPaths: ["~/.config/opencode/AGENTS.md"],
          fingerprint: "rules-missing:00000004",
          dismissed: false,
        },
      ],
    });
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    await userEvent.click(
      screen.getByRole("button", { name: "Link to working-agreements.md" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveAccessibleDescription(
      /link to ~\/\.agents\/working-agreements\.md/,
    );
    expect(within(dialog).getByText("~/.config/opencode/AGENTS.md")).toBeInTheDocument();
    expect(
      within(dialog).queryByText(/never deletes a skill folder/),
    ).not.toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Link to working-agreements.md" }),
    );
    expect(fixSkillIssue).toHaveBeenCalledWith(
      "rules-missing",
      "rules-missing:00000004",
      null,
    );
  });

  it("keeps the switch and rescans when a client refuses the write", async () => {
    vi.mocked(setSkillState).mockRejectedValueOnce(
      new Error("permissions isn't in a shape"),
    );
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    const loadsBefore = vi.mocked(skillsInventory).mock.calls.length;
    await userEvent.click(
      within(details).getByRole("switch", { name: "Load screenshot in OpenCode" }),
    );
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(
      within(details).getByRole("switch", { name: "Load screenshot in OpenCode" }),
    ).toBeChecked();
    await waitFor(() =>
      expect(vi.mocked(skillsInventory).mock.calls.length).toBeGreaterThan(loadsBefore),
    );
  });

  it("keeps the confirm open when a fix fails", async () => {
    vi.mocked(fixSkillIssue).mockRejectedValueOnce(new Error("disk full"));
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    const fixButton = screen.getByRole("button", { name: "Remove link" });
    // A fix repeats on every card, so it's a secondary action, after Dismiss, in the card's
    // own actions group that moves under the details on a narrow card.
    expect(fixButton).toHaveAttribute("data-variant", "outline");
    expect(fixButton.closest("[data-slot=issue-actions]")).not.toBeNull();
    await userEvent.click(fixButton);
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Remove link" }));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("moves between tabs with the arrow keys and clears an agent filter", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(
      within(list).getByRole("button", { name: "Show only skills Codex finds" }),
    );
    const skillsTab = screen.getByRole("tab", { name: /Skills/ });
    skillsTab.focus();
    await userEvent.keyboard("{ArrowRight}");
    const rulesTab = screen.getByRole("tab", { name: /Rules/ });
    expect(rulesTab).toHaveAttribute("aria-selected", "true");
    expect(rulesTab).toHaveFocus();
    expect(screen.getByRole("tabpanel")).toHaveAttribute("aria-labelledby", rulesTab.id);
    await userEvent.keyboard("{ArrowLeft}");
    const again = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(
      within(again).getByRole("button", { name: /claude\.ai account/ }),
    );
    // The Codex filter from before is gone, so skills Codex doesn't load are back.
    expect(within(again).getByRole("button", { name: /pdf/ })).toBeInTheDocument();
  });

  it("drops a write result that returns after the user picked another project", async () => {
    let resolveWrite: (value: ReturnType<typeof skillsFixture>) => void = () => {};
    vi.mocked(setSkillState).mockImplementationOnce(
      () => new Promise((resolve) => (resolveWrite = resolve)),
    );
    vi.mocked(openFolderDialog).mockResolvedValueOnce("/work/repo");
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    await userEvent.click(
      within(details).getByRole("switch", { name: "Load screenshot in OpenCode" }),
    );
    vi.mocked(skillsInventory).mockResolvedValue({
      ...skillsFixture(),
      project: "/work/repo",
    });
    await userEvent.click(screen.getByRole("button", { name: /Global only/ }));
    await userEvent.click(
      await screen.findByRole("menuitem", { name: /Add a project folder/ }),
    );
    await screen.findByRole("button", { name: /Global \+ repo/ });
    expect(localStorage.getItem("toolport-skills-project")).toBe("/work/repo");
    // The old global write lands late; the project view must not be replaced by it.
    // A real change to the skill on screen, so the test fails if the stale result is shown.
    const screenshot = skillsFixture().skills.find((s) => s.name === "screenshot")!;
    resolveWrite(applySkillState(skillsFixture(), "opencode", screenshot.key, "off"));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Global \+ repo/ })).toBeInTheDocument(),
    );
    expect(
      within(screen.getByRole("complementary", { name: "Skill details" })).getByRole(
        "switch",
        {
          name: "Load screenshot in OpenCode",
        },
      ),
    ).toBeChecked();
  });

  it("opens and reveals files by their full path, not the ~ form it shows", async () => {
    renderView();
    const details = await screen.findByRole("complementary", { name: "Skill details" });
    await userEvent.click(
      within(details).getByRole("button", { name: /Open SKILL\.md/ }),
    );
    expect(openSkillPath).toHaveBeenCalledWith(
      "/home/you/.agents/skills/release-notes/SKILL.md",
      false,
      null,
    );
    await userEvent.click(screen.getByRole("tab", { name: /Rules/ }));
    await userEvent.click(screen.getByRole("button", { name: /CLAUDE\.md/ }));
    const rule = screen.getByRole("complementary", { name: "Rules file details" });
    await userEvent.click(within(rule).getByRole("button", { name: /Reveal/ }));
    expect(openSkillPath).toHaveBeenLastCalledWith(
      "/home/you/.claude/CLAUDE.md",
      true,
      null,
    );
  });

  it("opens a file with the app picked from the Open menu", async () => {
    renderView();
    const details = await screen.findByRole("complementary", { name: "Skill details" });
    await userEvent.click(within(details).getByRole("button", { name: "Open with" }));
    expect(
      await screen.findByRole("menuitemradio", { name: "Obsidian (default)" }),
    ).toBeChecked();
    await userEvent.click(screen.getByRole("menuitemradio", { name: "Zed" }));
    expect(openSkillPathWith).toHaveBeenCalledWith(
      "/home/you/.agents/skills/release-notes/SKILL.md",
      "dev.zed.Zed.desktop",
      null,
    );
  });

  it("removes one of your skills after a confirm, and offers nothing for others", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /triage/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    await userEvent.click(within(details).getByRole("button", { name: "Remove triage" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Remove" }));
    expect(removeSkill).toHaveBeenCalledWith(
      "/home/you/.agents/skills/triage/SKILL.md",
      null,
    );
    await waitFor(() =>
      expect(
        within(list).queryByRole("button", { name: /triage/ }),
      ).not.toBeInTheDocument(),
    );
    await userEvent.click(
      within(list).getByRole("button", { name: /claude\.ai account/ }),
    );
    await userEvent.click(within(list).getByRole("button", { name: /pdf/ }));
    expect(
      within(screen.getByRole("complementary", { name: "Skill details" })).queryByRole(
        "button",
        { name: "Remove pdf" },
      ),
    ).not.toBeInTheDocument();
  });

  it("scans again when a dismiss fails, so an earlier queued change still shows", async () => {
    vi.mocked(dismissSkillIssue).mockRejectedValueOnce(new Error("disk full"));
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    const card = screen
      .getByText("Grok reads your rules file but not what it imports")
      .closest("article") as HTMLElement;
    const loadsBefore = vi.mocked(skillsInventory).mock.calls.length;
    await userEvent.click(within(card).getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    await waitFor(() =>
      expect(vi.mocked(skillsInventory).mock.calls.length).toBeGreaterThan(loadsBefore),
    );
  });

  it("dismisses an item until it changes and can bring it back", async () => {
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    const card = screen
      .getByText("Grok reads your rules file but not what it imports")
      .closest("article") as HTMLElement;
    await userEvent.click(within(card).getByRole("button", { name: "Dismiss" }));
    expect(dismissSkillIssue).toHaveBeenCalledWith("rules-imports:00000003", true, null);
    const tab = screen.getByRole("tab", { name: /Needs a look/ });
    await waitFor(() => expect(tab).toHaveTextContent("2"));
    await userEvent.click(screen.getByRole("button", { name: /Dismissed/ }));
    const back = screen
      .getByText("Grok reads your rules file but not what it imports")
      .closest("article") as HTMLElement;
    await userEvent.click(within(back).getByRole("button", { name: "Show again" }));
    expect(dismissSkillIssue).toHaveBeenLastCalledWith(
      "rules-imports:00000003",
      false,
      null,
    );
  });

  it("jumps from a skill chip on Needs a look to that skill", async () => {
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    await userEvent.click(screen.getByRole("button", { name: "screenshot" }));
    expect(screen.getByRole("tab", { name: /Skills/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      within(screen.getByRole("complementary", { name: "Skill details" })).getByRole(
        "heading",
        { name: "screenshot" },
      ),
    ).toBeInTheDocument();
  });

  it("queues a second change made while the first is saving, and keeps focus", async () => {
    const pending: ((inv: ReturnType<typeof skillsFixture>) => void)[] = [];
    vi.mocked(setSkillState).mockClear();
    vi.mocked(setSkillState).mockImplementation(
      () => new Promise((resolve) => pending.push(resolve)),
    );
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    const toggle = within(details).getByRole("switch", {
      name: "Load screenshot in OpenCode",
    });
    await userEvent.click(toggle);
    // Still usable while saving, showing the new state, and focus stays on it.
    expect(toggle).toBeEnabled();
    expect(toggle).not.toBeChecked();
    expect(toggle).toHaveFocus();
    await userEvent.click(toggle);
    expect(toggle).toBeChecked();
    // The second write waits for the first.
    expect(setSkillState).toHaveBeenCalledTimes(1);
    const key = "/home/you/.agents/skills/screenshot/SKILL.md";
    pending[0](applySkillState(skillsFixture(), "opencode", key, "off"));
    await waitFor(() => expect(setSkillState).toHaveBeenCalledTimes(2));
    expect(vi.mocked(setSkillState).mock.calls.map((c) => c[2])).toEqual(["off", "on"]);
    pending[1](skillsFixture());
    await waitFor(() =>
      expect(
        within(details).getByRole("switch", { name: "Load screenshot in OpenCode" }),
      ).toBeChecked(),
    );
  });

  it("lists only the other ways into a skill under Reached by", async () => {
    const inv = skillsFixture();
    const notes = inv.skills.find((s) => s.name === "release-notes")!;
    notes.links = ["~/.agents/skills/release-notes", "~/.claude/skills/release-notes"];
    vi.mocked(skillsInventory).mockResolvedValue(inv);
    renderView();
    const details = await screen.findByRole("complementary", { name: "Skill details" });
    expect(within(details).getByText("Reached by")).toBeInTheDocument();
    expect(
      within(details).getByText("~/.claude/skills/release-notes"),
    ).toBeInTheDocument();
    expect(within(details).queryByText("~/.agents/skills/release-notes")).toBeNull();
  });

  it("says when removing a skill only removes the links to it", async () => {
    const inv = skillsFixture();
    const notes = inv.skills.find((s) => s.name === "release-notes")!;
    notes.trashOnRemove = false;
    vi.mocked(skillsInventory).mockResolvedValue(inv);
    renderView();
    const details = await screen.findByRole("complementary", { name: "Skill details" });
    await userEvent.click(
      within(details).getByRole("button", { name: "Remove release-notes" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByText(
        "Only the links to it are removed, so no client loads it. Its folder at ~/.agents/skills/release-notes stays where it is.",
      ),
    ).toBeInTheDocument();
  });

  it("offers Reveal but not Open for a link to a missing folder", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /computer-use/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    expect(within(details).queryByRole("button", { name: /Open SKILL\.md/ })).toBeNull();
    expect(within(details).getByRole("button", { name: /Reveal/ })).toBeInTheDocument();
  });

  it("says so when a rescan fails with the page already on screen", async () => {
    renderView();
    await screen.findByRole("region", { name: "Skills" });
    vi.mocked(skillsInventory).mockRejectedValueOnce(new Error("disk gone"));
    window.dispatchEvent(new Event(SKILLS_REFRESH_EVENT));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(screen.getByRole("region", { name: "Skills" })).toBeInTheDocument();
  });

  it("focuses its own search when the app's Find shortcut fires", async () => {
    renderView();
    await userEvent.click(await screen.findByRole("tab", { name: /Needs a look/ }));
    window.dispatchEvent(new Event(SKILLS_SEARCH_EVENT));
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Search skills" })).toHaveFocus(),
    );
  });

  it("rescans when the app's Refresh shortcut fires", async () => {
    renderView();
    await screen.findByRole("region", { name: "Skills" });
    const before = vi.mocked(skillsInventory).mock.calls.length;
    window.dispatchEvent(new Event(SKILLS_REFRESH_EVENT));
    await waitFor(() =>
      expect(vi.mocked(skillsInventory).mock.calls.length).toBe(before + 1),
    );
  });
});

// A read-only decision belongs to one skill/client, not to the whole client.
it("shows the reason for an uncertain identifier without offering its toggle", async () => {
  vi.mocked(setSkillState).mockClear();
  const inv = skillsFixture();
  const entry = inv.skills.find((s) => s.name === "screenshot")!;
  entry.states.opencode.readOnlyReason = "Identifier cannot be verified; use the client.";
  vi.mocked(skillsInventory).mockResolvedValue(inv);
  renderView();
  const list = await screen.findByRole("region", { name: "Skills" });
  await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
  const details = screen.getByRole("complementary", { name: "Skill details" });
  expect(
    within(details).queryByRole("switch", { name: "Load screenshot in OpenCode" }),
  ).not.toBeInTheDocument();
  const label = within(details).getByRole("img", {
    name: "Identifier cannot be verified; use the client.",
  });
  await userEvent.hover(label);
  expect(await screen.findByRole("tooltip")).toHaveTextContent(
    "Identifier cannot be verified; use the client.",
  );
  expect(setSkillState).not.toHaveBeenCalled();
});

it.each(["broken", "shadowed", "off"] as const)(
  "keeps the %s explanation beside the read-only reason",
  async (state) => {
    const inv = skillsFixture();
    const entry = inv.skills.find((s) => s.name === "screenshot")!;
    entry.states.opencode = {
      state,
      via: "Specific state explanation",
      readOnlyReason: "Identifier cannot be verified; use the client.",
    };
    vi.mocked(skillsInventory).mockResolvedValue(inv);
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(within(list).getByRole("button", { name: /screenshot/ }));
    const details = screen.getByRole("complementary", { name: "Skill details" });
    expect(within(details).getByText("Specific state explanation")).toBeVisible();
    expect(
      within(details).getByText("Identifier cannot be verified; use the client."),
    ).toBeVisible();
    expect(
      within(details).queryByRole("switch", { name: "Load screenshot in OpenCode" }),
    ).not.toBeInTheDocument();
  },
);
