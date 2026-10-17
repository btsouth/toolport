import type {
  SkillAgent,
  SkillEntry,
  SkillLoadState,
  SkillsInventory,
  SkillWanted,
} from "@/lib/types";

// Synthetic inventory for the browser fixture and component tests. Shapes match
// src-tauri/src/skills.rs; paths are made up.
const agent = (
  id: string,
  name: string,
  canToggle: boolean,
  switchNote: string,
  roots: string[],
  extra: Partial<SkillAgent> = {},
): SkillAgent => ({
  id,
  name,
  supportsSkills: true,
  verified: true,
  canToggle,
  switchNote,
  roots: roots.map((path) => ({ path, label: "Skills folder", exists: true })),
  rulesNote: null,
  ruleLocations: [],
  ...extra,
});

const agents: SkillAgent[] = [
  agent("claude-code", "Claude Code", true, "skillOverrides in ~/.claude/settings.json", [
    "~/.claude/skills",
  ]),
  agent("codex", "Codex", true, "[[skills.config]] in ~/.codex/config.toml", [
    "~/.codex/skills",
    "~/.agents/skills",
  ]),
  agent(
    "cursor",
    "Cursor",
    false,
    "Cursor has no per-skill switch. One setting turns off every Claude, Codex and Grok folder.",
    ["~/.cursor/skills", "~/.claude/skills", "~/.agents/skills"],
    { rulesNote: "Cursor keeps user rules in its app settings, not a file." },
  ),
  agent("opencode", "OpenCode", true, "A skill deny rule in opencode.json", [
    "~/.config/opencode/skills",
    "~/.agents/skills",
  ]),
  agent("grok", "Grok", false, "Turn skills off with /skills inside Grok.", [
    "~/.grok/skills",
    "~/.agents/skills",
  ]),
];

const every = (state: SkillLoadState, via: string) =>
  Object.fromEntries(agents.map((a) => [a.id, { state, via }]));

const skill = (
  name: string,
  description: string,
  extra: Partial<SkillEntry> = {},
): SkillEntry => ({
  key: `/home/you/.agents/skills/${name}/SKILL.md`,
  name,
  description,
  descriptionChars: description.length,
  path: `~/.agents/skills/${name}/SKILL.md`,
  folder: `/home/you/.agents/skills/${name}`,
  source: "yours",
  sourceNote: "Yours",
  editable: true,
  sizeBytes: 4200,
  lines: 96,
  modifiedSecs: 1_760_000_000,
  gitRepo: null,
  links: [`~/.agents/skills/${name}/SKILL.md`],
  states: every("on", "~/.agents/skills"),
  preview: `---\nname: ${name}\ndescription: ${description}\n---\n\n# ${name}\n`,
  ...extra,
});

export function skillsFixture(): SkillsInventory {
  return {
    agents,
    project: null,
    skills: [
      skill(
        "release-notes",
        "Write release notes from merged pull requests since the last tag.",
      ),
      skill(
        "screenshot",
        "Take and annotate screenshots of the running app for bug reports and pull requests.",
        {
          gitRepo: "dotfiles",
          states: {
            ...every("on", "~/.agents/skills"),
            "claude-code": { state: "off", via: "Off in skillOverrides" },
            codex: { state: "off", via: "Off in config.toml" },
          },
        },
      ),
      skill("triage", "Sort new issues by area and severity.", {
        states: {
          ...every("on", "~/.agents/skills"),
          "claude-code": { state: "name-only", via: "Name only in skillOverrides" },
        },
      }),
      skill("pdf", "Read, fill and split PDF files.", {
        key: "/home/you/.claude/skills/pdf/SKILL.md",
        path: "~/.claude/skills/pdf/SKILL.md",
        folder: "/home/you/.claude/skills/pdf",
        source: "account",
        sourceNote: "Synced from your claude.ai account",
        states: {
          "claude-code": { state: "on", via: "~/.claude/skills" },
          cursor: { state: "on", via: "~/.claude/skills" },
          grok: { state: "shadowed", via: 'Another "pdf" wins in Grok' },
        },
      }),
      skill("desktop-config", "Edit window manager and desktop config files.", {
        key: "/usr/share/desktop/skills/desktop-config/SKILL.md",
        path: "/usr/share/desktop/skills/desktop-config/SKILL.md",
        folder: "/usr/share/desktop/skills/desktop-config",
        source: "package",
        sourceNote: "Installed by a system package; updates replace edits",
        editable: false,
      }),
      skill("computer-use", "", {
        key: "/home/you/.grok/skills/computer-use",
        path: "~/.grok/skills/computer-use",
        folder: "/home/you/.grok/skills",
        source: "broken",
        sourceNote: "Link to a missing folder",
        editable: false,
        sizeBytes: 0,
        lines: 0,
        modifiedSecs: null,
        states: { grok: { state: "broken", via: "Points at nothing" } },
        preview: "",
      }),
    ],
    rules: [
      {
        key: "/home/you/.agents/working-agreements.md",
        name: "working-agreements.md",
        path: "~/.agents/working-agreements.md",
        scope: "global",
        sizeBytes: 5600,
        preview: "# Working agreements\n\nKeep changes small.\n",
        agents: {
          "claude-code": [
            { kind: "file", path: "~/.claude/CLAUDE.md" },
            { kind: "import", path: "~/.agents/working-agreements.md" },
          ],
          codex: [
            { kind: "file", path: "~/.codex/AGENTS.md" },
            { kind: "link", path: "~/.agents/working-agreements.md" },
          ],
        },
      },
      {
        key: "/home/you/.claude/CLAUDE.md",
        name: "CLAUDE.md",
        path: "~/.claude/CLAUDE.md",
        scope: "global",
        sizeBytes: 60,
        preview: "@~/.agents/working-agreements.md\n",
        agents: {
          "claude-code": [{ kind: "file", path: "~/.claude/CLAUDE.md" }],
          grok: [
            { kind: "file", path: "~/.claude/CLAUDE.md" },
            { kind: "skipped-import", path: "~/.agents/working-agreements.md" },
          ],
        },
      },
    ],
    issues: [
      {
        id: "off-elsewhere",
        level: "warn",
        title: "Skills you turned off still load in 3 other clients",
        detail:
          "screenshot is off in Claude Code and Codex. Cursor, OpenCode and Grok still load it.",
        agents: ["cursor", "opencode", "grok"],
        skills: ["/home/you/.agents/skills/screenshot/SKILL.md"],
        skillNames: ["screenshot"],
        fixLabel: "Turn off where possible",
      },
      {
        id: "broken-links",
        level: "error",
        title: "1 link to a missing folder",
        detail:
          "~/.grok/skills/computer-use points at nothing. Clients skip it without saying so.",
        agents: ["grok"],
        skills: ["/home/you/.grok/skills/computer-use"],
        skillNames: ["computer-use"],
        fixLabel: "Remove link",
      },
      {
        id: "rules-imports",
        level: "info",
        title: "Grok reads your rules file but not what it imports",
        detail:
          "These clients load the file itself but skip its @import lines, so the imported rules never reach them.",
        agents: ["grok"],
        skills: [],
        skillNames: [],
        fixLabel: null,
      },
    ],
  };
}

/** Apply a switch the way the backend would, for the fixture's round trip. */
export function applySkillState(
  inv: SkillsInventory,
  agentId: string,
  key: string,
  wanted: SkillWanted,
): SkillsInventory {
  return {
    ...inv,
    skills: inv.skills.map((s) =>
      s.key === key && s.states[agentId]
        ? {
            ...s,
            states: {
              ...s.states,
              [agentId]: {
                state: wanted,
                via: wanted === "on" ? s.states[agentId].via : `${wanted} in settings`,
              },
            },
          }
        : s,
    ),
  };
}
