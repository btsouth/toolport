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

If you change Toolport's block by hand in a client's file, Toolport notices and does not
quietly put its text back: the client shows **Edited on disk** with a diff, and you decide
whether the file's version goes into the set or the set goes back into the file. Only
**Overwrite this file** / **Re-apply** rewrites an edited block. See the state table below.

One thing Toolport will not store: rules containing its own marker comments
(`toolport:rules:start` and friends). It uses those to find the block it owns, so
it refuses the save and tells you. This only comes up if you copy out of the
preview pane, which shows the finished file including the markers. Copy just your
own text.

## Starting from an existing file

Most people already have rules somewhere before they open this tab: a `~/.claude/CLAUDE.md`,
a `~/.codex/AGENTS.md`, a `GEMINI.md`, a `.goosehints`. **Start from a file** (next to
**New set**) lists the rules files the detected clients already read, with their sizes, and
offers a file picker for anything else. Picking one creates a new set named after the client
("Imported from Codex") from the file's text. The new set is **not** selected for you -
selecting a set applies it to every client you have switched on, and that is your call to
make with the new set in front of you - unless no set was selected at all, in which case it
becomes the applied set, as any first set does.

Two rules, both of them about not surprising you:

- **The file is read, never written.** Importing changes nothing on disk. If you later switch
  that client on, Toolport appends its block beside your original text as usual; remove the
  original by hand if you want only one copy.
- **Only your text comes in.** Anything Toolport itself wrote into that file (its own marked
  block, or a whole file it owns) is left out, and the import says so. A file whose remaining
  text still looks like Toolport's marker comments is refused, the same way a save is, so you
  are told up front rather than at the first write.

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
| Command Code            | `~/.commandcode/AGENTS.md`                                                       | Marked block |

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
checkbox; the Clients section lists whichever of them it detects underneath, so nothing
is silently skipped, and it says which of three cases each one is: clients that read
project rules (Cursor, GitHub Copilot CLI) are pointed at the Projects section, Claude
Desktop is noted as the chat app with Claude Code inside it already covered, and the
rest say to paste your rules in by hand. Which names appear depends on what is
installed, so the list below is the full set rather than what you will see.

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

| State                       | Meaning                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Applied                     | This client's rules file is up to date.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Not applied yet             | The current rules are not on disk for this client yet. Use Re-apply.                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Edited on disk              | Toolport wrote this block and it has been changed in the file since. Toolport leaves it alone on every automatic apply (startup, saving, switching sets, toggling clients) until you choose: **View diff** shows the file against the set, **Pull into set** makes the file's version your unsaved draft, **Overwrite this file** puts the set back over that one file (**Re-apply** does it for every switched-on client). A change to the set itself is a newer revision and is written, so pull first if you want to keep the edit. |
| Blocked by a local override | The client has an override file making it ignore the file Toolport writes. Codex's `AGENTS.override.md` is the case this covers: while it exists, Codex ignores `AGENTS.md` entirely, so writing there would be invisible.                                                                                                                                                                                                                                                                                                             |
| Too long for this client    | The client caps its global rules file and these rules would exceed it. Devin Desktop's Cascade agent caps its file at 6,000 characters, counted across the whole file, including anything you have in it.                                                                                                                                                                                                                                                                                                                              |
| Copy manually               | No rules file Toolport can write. Shown in the Teams tab; the Agent rules tab lists these clients separately instead. See above.                                                                                                                                                                                                                                                                                                                                                                                                       |
| Write error                 | The file could not be read or written. It was left untouched.                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |

## Team instructions

If you are in a Toolport Teams org, your admin can push team-wide instructions as
well (see the Teams tab). Team and personal rules are independent and coexist in
the same files: they use different markers and different file names, so applying or
removing one never disturbs the other.

Where a client caps its rules file, both blocks count toward that cap.

## Project-level rules

The **Projects** section of the Agent rules tab applies a rule set inside a project
folder, for the clients that read rules from the repository itself. Writing into your
repositories is a bigger thing than writing into your home directory, so the rules are
deliberately narrower than for global rules:

- **Only folders you register.** Toolport never scans for repositories. Add a folder, and
  it appears with every file switched off and no set chosen.
- **The unit of consent is a file, not a client.** At project level nearly every client
  reads the root `AGENTS.md`, Gemini reads `GEMINI.md`, and Claude Code and VS Code read
  `.claude/rules/`, so each project offers those files (only the ones at least one
  detected client reads), each naming the clients it reaches. Switch a file on per project.
- **Written only by that project's Apply, never at startup.** Switching a file on changes
  nothing on disk; pressing **Apply to <project>** writes the switched-on files from the
  project's set. Switching a file off, or removing the project, removes what Toolport wrote.
- **Preview first**, per file, exactly as for global rules.
- **Only Toolport's own block or own file.** The same marked-block / owned-file writer and
  the same markers; your `AGENTS.md` text is left byte-for-byte. The files live in the
  repo and will show up in `git status`; whether to commit them is yours to decide.
- A project's set is independent of the global active set. Deleting a set a project used
  removes that project's files and leaves the project registered with no set.
- A file you edit by hand after Toolport wrote it shows **Edited on disk** here too; a
  project's **Apply** rewrites it (it is the explicit button), so read the badge first.

### Project files and who reads them

| File                              | Clients                                                                                                                                 | Citation                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AGENTS.md` (marked block)        | Codex, Cursor, GitHub Copilot CLI, Kiro, Roo Code, Cline, Devin Desktop (Cascade), Devin Local / CLI, Goose, Pi, Oh My Pi, Command Code | Codex: "Starting from the Git root down to your current working directory, checking each directory for `AGENTS.override.md`, `AGENTS.md`" ([docs](https://learn.chatgpt.com/docs/agent-configuration/agents-md)). Cursor: "Place it in your project root as an alternative to `.cursor/rules`" ([docs](https://cursor.com/docs/context/rules)). Copilot: "one or more `AGENTS.md` files, stored anywhere within the repository" ([docs](https://docs.github.com/en/copilot/how-tos/configure-custom-instructions/add-repository-instructions)). Kiro: AGENTS.md in "your workspace root" is "always included" ([docs](https://kiro.dev/docs/steering/)). Roo: `AGENTS.md` "automatically loaded by default" ([docs](https://roocodeinc.github.io/Roo-Code/features/custom-instructions)). Cline recognizes `AGENTS.md` ([docs](https://docs.cline.bot/features/cline-rules)). Devin Desktop: "Root-level `AGENTS.md` files ... activate automatically" ([docs](https://docs.devin.ai/desktop/cascade/memories)). Goose "looks for `AGENTS.md` then `.goosehints`" ([docs](https://github.com/block/goose/blob/main/documentation/docs/guides/context-engineering/using-goosehints.md)). Command Code reads `<project>/AGENTS.md` for team-shared project instructions ([docs](https://commandcode.ai/docs/memory)). |
| `GEMINI.md` (marked block)        | Gemini CLI, Antigravity                                                                                                                 | "The CLI searches for `GEMINI.md` files in your configured workspace directories and their parent directories" ([docs](https://geminicli.com/docs/cli/gemini-md/)).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `.claude/rules/toolport-rules.md` | Claude Code, VS Code                                                                                                                    | Claude Code: "Place markdown files in your project's `.claude/rules/` directory ... Rules without `paths` frontmatter are loaded at launch" ([docs](https://code.claude.com/docs/en/memory)). VS Code: "Workspace (Claude format): `.claude/rules` folder" ([docs](https://code.visualstudio.com/docs/copilot/customization/custom-instructions)).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

Two clients are deliberately not offered at project level. **Zed** reads only the first of
`.rules`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md`,
`AGENT.md`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md` ([docs](https://github.com/zed-industries/zed/blob/main/docs/src/ai/rules.md)),
so whether it would read Toolport's `AGENTS.md` block depends on files Toolport cannot see
into, and an "Applied" could be false; paste the rules into whichever of those your repo
uses. **Warp** has no documented project rules file Toolport can cite. Codex ignores
`AGENTS.md` in a folder that also has `AGENTS.override.md`; Toolport writes the former and
does not check for the latter at project level.
