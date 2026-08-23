# Agent rules

Write your agent instructions once in Toolport and have them applied to every AI
client on your machine, instead of hand-editing `CLAUDE.md`, `AGENTS.md`,
`GEMINI.md` and the rest and keeping them in sync yourself.

Open the **Agent rules** tab in the sidebar. No MCP server or gateway needed.

## How it works

You write one or more named **rule sets**. Exactly one is active at a time, so you
can keep, say, a "Work" set and a "Personal" set and switch between them.

Toolport writes the active set into each client's own **global** rules file, using
one of two strategies depending on how the client stores them:

- **Toolport owns a whole file.** For clients that read a rules _directory_,
  Toolport creates its own file in it (`toolport-rules.md`). Nothing of yours is
  in that file, so it can be replaced and deleted freely.
- **Toolport owns a marked block.** For clients that read a single shared file you
  also edit, Toolport appends a block between two HTML-comment markers and only
  ever rewrites what is between them. Every other byte in the file is left exactly
  as it is.

Either way, your own instructions are never overwritten. Turning a client off, or
deleting the active set, removes what Toolport wrote and leaves the rest of the
file alone.

Deleting the set that is currently applied clears the selection rather than
promoting another one, so nothing is pushed to your clients that you did not pick.
Deleting any other set changes nothing on disk.

One thing Toolport will not store: rules containing its own marker comments
(`toolport:rules:start` and friends). It uses those to find the block it owns, so
it refuses the save and tells you. This only comes up if you copy out of the
preview pane, which shows the finished file including the markers. Copy just your
own text.

## Before anything is written

- **Every client starts switched off.** Nothing is written until you tick a client
  in the **Clients** section of the **Agent rules** tab. (Not the Clients entry in
  the sidebar: that one connects a client to the MCP gateway and has nothing to do
  with rules.)
- **Preview shows the exact bytes.** Each client has a Preview button that renders
  the file Toolport would write, without writing it. It reflects whatever is in the
  editor, saved or not, and previewing never saves: a save applies to every client
  you have switched on, so it would defeat the point.

## Supported clients

| Client                  | Rules file                                                                       | Strategy     |
| ----------------------- | -------------------------------------------------------------------------------- | ------------ |
| Claude Code             | `~/.claude/rules/toolport-rules.md`                                              | Owned file   |
| VS Code                 | `~/.claude/rules/toolport-rules.md` (shared with Claude Code)                    | Owned file   |
| Kiro                    | `~/.kiro/steering/toolport-rules.md`                                             | Owned file   |
| Roo Code                | `~/.roo/rules/toolport-rules.md`                                                 | Owned file   |
| Cline                   | `~/Documents/Cline/Rules/toolport-rules.md`                                      | Owned file   |
| Codex                   | `$CODEX_HOME/AGENTS.md` (default `~/.codex/AGENTS.md`)                           | Marked block |
| Gemini CLI              | `$GEMINI_CLI_HOME/.gemini/GEMINI.md` (default `~/.gemini/GEMINI.md`)             | Marked block |
| Antigravity             | `~/.gemini/GEMINI.md` (shared with Gemini CLI)                                   | Marked block |
| Devin Desktop (Cascade) | `~/.codeium/windsurf/memories/global_rules.md`                                   | Marked block |
| Devin Local / CLI       | `%APPDATA%\devin\AGENTS.md` (Windows), `~/.config/devin/AGENTS.md` (macOS/Linux) | Marked block |
| Goose                   | `.goosehints` beside `config.yaml` (honours `GOOSE_PATH_ROOT`)                   | Marked block |
| Zed                     | `AGENTS.md` in Zed's config directory                                            | Marked block |
| Pi                      | `~/.pi/agent/AGENTS.md`                                                          | Marked block |
| Oh My Pi                | `~/.omp/agent/AGENTS.md`                                                         | Marked block |

On Linux, Devin Local / CLI, Goose, and Zed follow `XDG_CONFIG_HOME`. On Windows,
they use the roaming config directory.

Where two clients share a file, Toolport writes it once. Both are covered even if
only one is installed.

The VS Code row resolves to Claude Code's rules directory because VS Code reads it:
its [custom instructions documentation](https://code.visualstudio.com/docs/copilot/customization/custom-instructions)
lists `~/.claude/rules` (alongside `~/.copilot/instructions`) as a user-profile
instructions location, and `~/.claude/CLAUDE.md` as personal instructions across all
projects. So the file Toolport writes there reaches GitHub Copilot Chat as well as the
Claude Code extension, and one write covers both when both are installed.

### Clients with no rules file Toolport can write

Some clients keep their global rules somewhere Toolport cannot write. They have no
checkbox; the Clients section lists whichever of them it detects underneath ("No rules
file Toolport can write for ..."), so you know to paste your rules in yourself. Toolport
does not silently skip them. Which names appear depends on what is installed, so the list
below is the full set rather than what you will see.

- **Cursor** and **Warp** keep global rules in their own UI or account: Cursor's User
  rules in _Customize -> Rules_, Warp's in Warp Drive. Both also read per-project files
  (`.cursor/rules/`, `AGENTS.md`, `WARP.md`), which Agent rules does not cover yet either
  (see [Project-level rules](#project-level-rules)).
- **LM Studio**, **Jan** and **Hermes** keep the system prompt per chat or per model in
  their own store, not in a global file.
- **Claude Desktop** here means the chat app, which has no rules file. Claude Code running
  _inside_ the desktop app is a separate thing and shares `~/.claude` with the CLI, so it
  is already covered by the Claude Code row above.
- **Continue** has no global rules file of the shape Toolport writes. Its `.continue/rules/`
  directory is per-project, and its user-level rules are a `rules:` array inside
  `~/.continue/config.yaml` listing hub references or `file://` paths - a YAML list in the
  same file Toolport already writes MCP config into, not a markdown file it could own or
  bracket with markers. With `continuedev/continue` archived read-only in June 2026, no
  adapter is planned. Continue is still detected as an MCP client.

## Per-client states

| State                       | Meaning                                                                                                                                                                                                                    |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Applied                     | This client's rules file is up to date.                                                                                                                                                                                    |
| Not applied yet             | The current rules are not on disk for this client yet. Use Re-apply.                                                                                                                                                       |
| Blocked by a local override | The client has an override file making it ignore the file Toolport writes. Codex's `AGENTS.override.md` is the case this covers: while it exists, Codex ignores `AGENTS.md` entirely, so writing there would be invisible. |
| Too long for this client    | The client caps its global rules file and these rules would exceed it. Devin Desktop's Cascade agent caps its file at 6,000 characters, counted across the whole file, including anything you have in it.                  |
| Copy manually               | No rules file Toolport can write. Shown in the Teams tab; the Agent rules tab lists these clients separately instead. See above.                                                                                           |
| Write error                 | The file could not be read or written. It was left untouched.                                                                                                                                                              |

## Team instructions

If you are in a Toolport Teams org, your admin can push team-wide instructions as
well (see the Teams tab). Team and personal rules are independent and coexist in
the same files: they use different markers and different file names, so applying or
removing one never disturbs the other.

Where a client caps its rules file, both blocks count toward that cap.

## Project-level rules

Not supported yet. Agent rules currently covers **global** (user-level) rules only.
Per-project `CLAUDE.md` / `AGENTS.md` files are yours to manage.
