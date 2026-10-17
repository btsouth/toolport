# Skills and rules

AI coding agents load more than MCP tools. They also read **skills** (folders with a
`SKILL.md` file) and **rules files** (such as `CLAUDE.md` and `AGENTS.md`). The
Skills page shows what each installed agent actually loads, where every file
lives, and where an agent's setup disagrees with what you expect.

Open it from **Skills** in the sidebar. It has three tabs.

- **Skills** lists every skill Toolport found, grouped by where it came from: your
  own folders, your claude.ai account, a system package, a client's built-in set, a
  client's own library, or parked. Each row shows its state in every agent: on,
  name only, off, another copy wins, broken or not loaded. Click an agent's icon to
  show only what that agent loads. Pick a skill to see its description, its file,
  the paths that reach it and its state in each agent.
- **Rules** lists the rules files each agent reads, and how it gets there: the file
  it opens, any symlinks, and the `@import` lines it follows or skips.
- **Needs a look** lists problems Toolport found, with a fix where one is safe.

By default the page shows your global setup. Use **Add a project folder** to also
include the skills and rules files a project adds.

## Turning a skill on or off

Toolport changes a skill only through the agent's own setting. It never moves,
renames or deletes your skill folders to turn one off, and it backs up every config
file before it writes.

| Agent       | Where Toolport writes                                               |
| ----------- | ------------------------------------------------------------------- |
| Claude Code | `skillOverrides` in `settings.json` (on, name only or off)          |
| Codex       | `[[skills.config]]` entries with `enabled = false` in `config.toml` |
| Copilot CLI | `disabledSkills` in `~/.copilot/settings.json`                      |
| OpenCode    | a `skill` deny rule in the `permissions` of `opencode.json`         |
| Gemini CLI  | `skills.disabled` in `settings.json`                                |
| Hermes      | `skills.disabled` in `~/.hermes/config.yaml`                        |

Claude Code alone supports **name only**: the skill is listed by name and its full
text loads when the agent asks for it.

Cursor has no per-skill switch. Its "Include third-party plugins, skills, and other
configs" setting turns off the Claude Code, Codex and Grok folders all at once, so
Toolport shows Cursor's state but can't change one skill there. Grok turns skills
off with `/skills` inside Grok; Toolport doesn't write that setting yet.

## Where agents look

Toolport reads the same folders the agents do, including environment overrides such
as `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME`, `GEMINI_CLI_HOME`, `GROK_HOME`
and `HERMES_HOME`.

| Agent       | Skill folders                                                                                                        |
| ----------- | -------------------------------------------------------------------------------------------------------------------- |
| Claude Code | `~/.claude/skills`, plus skills from enabled plugins                                                                 |
| Codex       | `~/.codex/skills`, `~/.agents/skills`, its bundled skills and enabled plugins                                        |
| Cursor      | `~/.cursor/skills`, `~/.claude/skills`, `~/.codex/skills`, `~/.grok/skills`, `~/.agents/skills` and its built-in set |
| Copilot CLI | `~/.copilot/skills`, `~/.agents/skills` and its built-in set                                                         |
| OpenCode    | `~/.config/opencode/skills`, `~/.claude/skills`, `~/.agents/skills`                                                  |
| Grok        | `~/.grok/skills`, `~/.agents/skills`, its bundled skills, then `~/.claude/skills`                                    |
| Hermes      | `~/.hermes/skills`                                                                                                   |
| Gemini CLI  | `~/.gemini/skills`, `~/.agents/skills`                                                                               |

Cursor, OpenCode and Grok read the default `~/.claude` folder even when Claude Code
uses a different `CLAUDE_CONFIG_DIR`.

These agents were checked against real installs. For other agents that support
skills, Toolport uses the folders from each agent's documentation and marks them
**from docs**.

When two different files share a skill name, each agent keeps one. Toolport shows
which copy wins in each agent and marks the other as **another copy wins**.

## What "Needs a look" checks

- Skills you turned off in one agent that still load in others. **Turn off where
  possible** applies the same choice in every agent with its own switch.
- Links in a skills folder that point at a missing folder. Agents skip these
  without saying so. **Remove link** deletes the dangling link, not a skill.
- Codex config entries that turn off a skill that is no longer there.
  **Remove entry** cleans them out of `config.toml`.
- Skill names with more than one copy.
- Descriptions longer than the 1,536 characters Claude Code keeps.
- Skills left in an old claude.ai sync folder.
- Skills from a system package, where edits are replaced on update.
- Agents that read your rules file but skip its `@import` lines.
- Agents that get no global rules file.

Every fix asks for confirmation first.
