import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { fixSkillIssue, openSkillPath, setSkillState, skillsInventory } from "@/lib/api";
import { applySkillState, skillsFixture } from "@/test/skills-fixture";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { toastError } from "@/lib/toast";
import { SKILLS_REFRESH_EVENT } from "@/lib/shortcuts";
import { SkillsView } from "./SkillsView";

vi.mock("@/lib/api", () => ({
  skillsInventory: vi.fn(),
  setSkillState: vi.fn(),
  fixSkillIssue: vi.fn(),
  openSkillPath: vi.fn(),
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
});

function renderView() {
  return render(
    <TooltipProvider>
      <SkillsView />
    </TooltipProvider>,
  );
}

describe("SkillsView", () => {
  it("shows each skill's state per agent and the first skill's details", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    const row = within(list).getByRole("button", { name: /screenshot/ });
    expect(
      within(row).getByRole("img", { name: "Claude Code: Off" }),
    ).toBeInTheDocument();
    expect(within(row).getByRole("img", { name: "Cursor: On" })).toBeInTheDocument();
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
      within(list).getByRole("button", { name: "Show only what Codex loads" }),
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
    const claude = within(details).getByRole("radiogroup", {
      name: "triage in Claude Code",
    });
    expect(within(claude).getByRole("radio", { name: "Name only" })).toBeChecked();
    expect(
      within(details).queryByRole("switch", { name: "Load triage in Cursor" }),
    ).not.toBeInTheDocument();
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
    await userEvent.click(screen.getByRole("button", { name: "Remove link" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Remove link" }));
    expect(fixSkillIssue).toHaveBeenCalledWith("broken-links", null);
    await waitFor(() =>
      expect(screen.queryByText("1 link to a missing folder")).not.toBeInTheDocument(),
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
    await userEvent.click(screen.getByRole("button", { name: "Remove link" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Remove link" }));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("moves between tabs with the arrow keys and clears an agent filter", async () => {
    renderView();
    const list = await screen.findByRole("region", { name: "Skills" });
    await userEvent.click(
      within(list).getByRole("button", { name: "Show only what Codex loads" }),
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
    resolveWrite(applySkillState(skillsFixture(), "opencode", "x", "off"));
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
