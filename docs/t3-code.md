# Toolport with T3 Code

[T3 Code](https://github.com/pingdotgg/t3code) runs coding agents such as Claude
Code and Codex as providers. Each provider reads its own MCP config, the same file
it reads when you start it in a terminal. So Toolport connects to the providers,
not to T3 Code. There is no T3 Code entry on the Clients page, and you do not need
one: connect Claude Code, Codex or the other provider on the Clients page as usual.

## Rows T3 Code uses

Toolport reads T3 Code's list of provider instances from `~/.t3/userdata/settings.json` (or
`$T3CODE_HOME/userdata/settings.json`). It reads only each instance's provider,
name, on/off state and home folder. That file can also hold API keys; Toolport
skips them and never stores, shows or logs them. It reads the file only when the
file it opened is inside T3 Code's data folder, so a link swapped in to point
elsewhere is never read.

- Each row a T3 Code instance uses is labelled **Used by T3 Code** with the
  instance names. An instance that is off in T3 Code reads "(off)". This includes
  rows such as Cursor or Kiro that T3 Code runs with their usual config.
- An instance's home comes from its home folder setting, else `CLAUDE_CONFIG_DIR`
  or `CODEX_HOME` from its environment variables, else the default.
- A Claude Code or Codex home only T3 Code knows about, such as one under `~/.t3`,
  gets its own row once its folder exists, so it can be connected like any other.
  Homes of other providers, such as OpenCode, Grok or Pi, get no row of their own.
- Folders named like `~/.claude_personal` and `~/.codex_personal`, which T3 Code's
  guides suggest, are found the same way as `~/.claude2`.
- Toolport uses only homes inside your home folder. A home outside it, on a network
  share or device, or given with `..`, is ignored without being opened. A home
  whose folder is a link gets no row of its own. To use such a folder anyway, add it
  on the Clients page with **Add client location**.
- An instance whose home folder is marked sensitive, isn't plain text or is a
  relative path labels no row: Toolport can't tell which config it reads.
- When you add, remove or switch on an instance in T3 Code, the open Clients page
  updates within a few seconds while Toolport's window is in front, or as soon as
  you switch back to it. Without T3 Code, Toolport checks for it only when the page
  opens and when you switch back to Toolport.
- A connected T3 Code home whose folder is deleted moves to **Not installed**. Once
  T3 Code no longer lists it, it can be forgotten there.

## What Activity shows

Calls from a T3 Code session keep their usual client name, so filters and totals do
not change. Activity and approval prompts add **via T3 Code**:

- **Claude Code**: "Claude Code via T3 Code" and the T3 Code thread, shown as a
  short id. Hover it for the full id, or select it to copy the id. T3 Code passes
  the thread to Claude Code's MCP servers.
- **Codex**: "Codex via T3 Code", without a thread. Codex passes MCP servers only a
  few environment variables, and one Codex process serves many threads, so Toolport
  goes by T3 Code having started Codex. Toolport knows T3 Code's process names on
  Linux, macOS and Windows; this was tested on Linux.
- **Pi, Kiro, Grok, fx and ACP Registry agents**: T3 Code gives these its thread
  variable too. When the provider passes it on to its MCP servers, Activity shows "via
  T3 Code" and the thread, as for Claude Code. Toolport checks that T3 Code started
  the session; a thread id alone is not enough.
- **Other providers**: no T3 Code label. Toolport does not guess.

The approval notification names T3 Code and the start of the thread id, and the
Clients page's recent client activity says how many of a client's sessions came
through T3 Code.

To tell, Toolport checks when it starts that T3 Code's data folder exists, then
reads its own environment and the names of up to eight parent processes. It writes
nothing for this and starts no extra threads. "via T3 Code" is for information
only: it comes from the process tree and environment at start, and Toolport never
uses it for access or approval decisions.

## T3 Code's own tools

T3 Code gives each session its own tools, named like `mcp__t3-code__…`. They come
from T3 Code, not from Toolport. Don't add T3 Code's MCP endpoint as a server in
Toolport: T3 Code sessions already have these tools, so they would see them twice,
and the thread tools work only in the thread T3 Code gave them to. To use T3 Code
from another app, follow T3 Code's
[outside agents guide](https://github.com/pingdotgg/t3code/blob/main/docs/user/outside-agents.md).
