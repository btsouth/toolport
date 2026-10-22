# Skills and rules

AI coding agents load more than MCP tools. They also read **skills** (folders with a
`SKILL.md` file) and **rules files** (such as `CLAUDE.md` and `AGENTS.md`). The
Skills page shows what each installed agent actually loads, where every file
lives, and where an agent's setup disagrees with what you expect.

Open it from **Skills** in the sidebar. Skills and Rules start with **Connected to
Toolport**, showing only the exact client profiles connected to your gateway. Use
that same agent selector to choose **All detected agents** or one specific profile.
Skills and rules also work without an MCP connection, so unconnected agents remain
available there. With no connected clients, the page starts with all detected
agents. The counts, columns and details follow your selection; **Needs a look**
continues to check the whole detected setup.

It has three tabs.

- **Skills** lists every skill Toolport found, grouped by where it came from: your
  own folders, your claude.ai account, a system package, a client's built-in set, a
  client's own library, or parked. Only your own skills start expanded. Each row
  shows its state in every agent: on, name only, off, another copy wins, broken or
  not loaded. Click an agent's icon to show only the skills that agent finds. Pick a
  skill to see its description, its file, the paths that reach it and its state in
  each agent.
- **Rules** lists the rules files each agent reads, and how it gets there: the file
  it opens, any symlinks, and the `@import` lines it follows or skips.
- **Needs a look** lists problems Toolport found, with a fix where one is safe.

By default the page shows your global setup. Use **Add a project folder** to also
include the skills and rules files a project adds.

In a narrow or tiled window, the details move under the list. **Ctrl+F** searches
the page and **Ctrl+R** scans again.

## Several Claude Code or Codex profiles

With more than one Claude Code profile, such as `~/.claude` and a `~/.claude2` you
start through `CLAUDE_CONFIG_DIR`, each profile gets its own column: **Claude Code
(claude)**, **Claude Code (claude2)** and so on. Each one shows that profile's
skills, plugins and `CLAUDE.md`, and turning a skill on or off writes that profile's
own `settings.json`. Profiles match those detected on Clients; backups and other
tools' folders do not become extra columns. Two profile folders with the same name in different places are named by
their full path. With one profile, the page shows a single **Claude Code** column.

Codex homes listed on Clients also get separate columns, including extra
`CODEX_HOME` folders found through your launchers or added as client locations.
Each reads its own skills, rules and `config.toml`; changing a skill in one profile
leaves the others alone. Profile identities and connection status match Clients.

## Opening and removing files

**Open SKILL.md** and **Open file** use your system's default app for the file.
The arrow beside them lists the other apps that open text files. Pick one and
Toolport opens the file there and uses that app from then on.

The trash button removes a skill you own, from your folders, a client's library or
the parked folder. Its folder goes to the trash, where you can restore it, and the
links that reach it are deleted, so no agent loads it. A skill that only reaches
your skills folders through a link, such as one kept in a repo, loses the link and
its folder stays where it is. The confirmation says which of the two will happen.

Toolport doesn't offer to remove a skill it found inside a linked folder, such as
`~/.claude/skills/work` pointing at `~/work`, because the rest of that folder isn't
skills. Skills from a package, your claude.ai account or a client's built-in set
can't be removed here either. A link to a missing folder offers **Reveal** to show
where the link sits.

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
| Grok        | the `disabled` list under `[skills]` in `~/.grok/config.toml`       |
| Crush       | `options.disabled_skills` in `~/.config/crush/crush.json`           |
| Qwen Code   | `skills.disabled` in `~/.qwen/settings.json`                        |
| Oh My Pi    | `skills.ignoredSkills` in `~/.omp/agent/config.yml`                 |

Claude Code alone supports **name only**: the skill is listed by name and its full
text loads when the agent asks for it.

Cursor has no per-skill switch. Its "Include third-party plugins, skills, and other
configs" setting turns off the Claude Code, Codex and Grok folders all at once, so
Toolport shows Cursor's state but can't change one skill there. Kimi Code, Goose,
Continue and Antigravity have no per-skill switch either. Pi and Kilo Code have
one, a `-path` entry in Pi's `settings.json` and a permission rule in Kilo, that
Toolport doesn't write yet.

## Where agents look

Toolport reads the same folders the agents do, including environment overrides such
as `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME`, `GEMINI_CLI_HOME`, `GROK_HOME`,
`HERMES_HOME`, `OPENCODE_CONFIG_DIR`, `KILO_CONFIG_DIR`, `QWEN_HOME`, `KIMI_CODE_HOME` and
`PI_CODING_AGENT_DIR`.

| Agent       | Skill folders                                                                                                        |
| ----------- | -------------------------------------------------------------------------------------------------------------------- |
| Claude Code | `~/.claude/skills` one level deep, its claude.ai `synced` folders, and skills from enabled plugins                   |
| Codex       | `~/.codex/skills`, `~/.agents/skills` (six levels deep), its bundled skills and enabled plugins                      |
| Cursor      | `~/.cursor/skills`, `~/.claude/skills`, `~/.codex/skills`, `~/.grok/skills`, `~/.agents/skills` and its built-in set |
| Copilot CLI | `~/.copilot/skills`, `~/.agents/skills` (in subfolders too) and its built-in set                                     |
| OpenCode    | `~/.config/opencode/skills`, `~/.agents/skills`, `~/.claude/skills`                                                  |
| Grok        | `~/.grok/skills`, `~/.grok/commands`, `~/.agents/skills`, its bundled skills, `~/.claude/skills`, `~/.cursor/skills` |
| Hermes      | `~/.hermes/skills`, plus the folders listed under `skills.external_dirs` in its `config.yaml`                        |
| Gemini CLI  | `~/.gemini/skills`, `~/.agents/skills`                                                                               |
| Antigravity | `~/.gemini/config/skills`                                                                                            |
| Cline       | `~/.cline/skills`, `~/.agents/skills`                                                                                |
| Continue    | `~/.continue/skills`                                                                                                 |
| Crush       | `~/.config/crush/skills`, `~/.config/agents/skills`, `~/.agents/skills`, `~/.claude/skills`                          |
| Goose       | `~/.config/goose/skills`, `~/.agents/skills`, `~/.claude/skills`                                                     |
| Kilo Code   | `~/.config/kilo/skills`, `~/.kilocode/skills`, `~/.kilo/skills`, `~/.claude/skills`, `~/.agents/skills`              |
| Kimi Code   | `~/.kimi-code/skills`, `~/.agents/skills`                                                                            |
| Pi          | `~/.pi/agent/skills`, `~/.agents/skills`                                                                             |
| Oh My Pi    | `~/.omp/agent/skills`, `~/.agents/skills`                                                                            |
| Qwen Code   | `~/.qwen/skills`, `~/.agents/skills`                                                                                 |

Cursor, OpenCode, Grok and the others that share `~/.claude/skills` read the default
`~/.claude` folder even when Claude Code uses a different `CLAUDE_CONFIG_DIR`.

Some agents treat links differently. Codex skips a `SKILL.md` that is itself a link,
so share a skill with Codex by linking its folder. Continue skips a skill folder that
is a link. Toolport shows these the way each agent sees them, in a project's skill
folders too. A project skill beats a personal one with the same name in Gemini CLI,
Hermes, Continue, Zed, Kiro and Factory Droid; in the others the personal one wins.

These agents were checked against real installs. For other agents that support
skills, among them VS Code, Amp, Devin, Factory Droid, Junie, Kiro, Roo Code,
Warp, Zed, Auggie, Command Code and Tabnine (Roo Code too, though it is archived),
Toolport uses the folders from each
agent's documentation and source and marks them **from docs**.

For rules, Toolport follows each agent's own list. OpenCode reads only its own
`AGENTS.md`, not `~/.claude/CLAUDE.md`. Gemini CLI follows `context.fileName` in its
settings, in a project too. Antigravity loads a file from its rules folder only with
`trigger: always_on`. Goose skips a rules file in its folder that links outside
that folder. Grok adds up its own rules files and the ones in `~/.claude`
and `~/.cursor`. Hermes's rules file is `SOUL.md`, its persona, so keep shared rules
out of it.

When two different files share a skill name, each agent keeps one. Toolport shows
which copy wins in each agent and marks the other as **another copy wins**.

## What "Needs a look" checks

- Skills you turned off in one agent that still load in others. **Turn off where
  possible** applies the same choice in every agent with its own switch. Only a
  setting for that one skill counts: a rule that turns off every skill in an agent,
  such as OpenCode's `"skill": "deny"`, isn't spread to the others.
- Links in a skills folder that point at a missing folder. Agents skip these
  without saying so. **Remove link** deletes the dangling link, not a skill. A link
  into a drive or share that isn't mounted right now is left alone.
- Codex config entries that turn off a skill that is no longer there.
  **Remove entry** cleans them out of `config.toml`.
- Skill names with more than one copy.
- Descriptions longer than the 1,536 characters Claude Code keeps.
- Skills left in an old claude.ai sync folder.
- Skills from a system package, where edits are replaced on update.
- Agents that read your rules file but skip its `@import` lines.
- Agents that get no global rules file. **Link to** gives them the rules file most
  of your agents already read, by making a link at each agent's own rules path, such
  as `~/.copilot/copilot-instructions.md` or `~/.gemini/GEMINI.md`. The confirmation
  lists every link. Toolport never replaces a file that's already there, skips a path
  an agent with rules also reads, and only links to a file without `@import` lines,
  since most agents skip those. Each Claude Code profile gets its own link, and
  Hermes's `SOUL.md` is never shared, because it is Hermes's persona. To undo, delete
  the link. On Windows, making links needs Developer Mode.

Every fix asks for confirmation first, and does nothing if the item changed after
you looked, so check it again. **Dismiss** hides an item you're fine with.
It comes back if the skills, agents or folders it covers change (not when you switch
languages), and **Show again** in the
Dismissed section brings it back sooner. Click a skill's name in an item to open it
on the Skills tab.
