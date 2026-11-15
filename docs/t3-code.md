# Toolport with T3 Code

[T3 Code](https://github.com/pingdotgg/t3code) runs coding agents such as Claude
Code and Codex as providers. Each provider reads its own MCP config, the same file
it reads when you start it in a terminal. So Toolport connects to the providers,
not to T3 Code. There is no T3 Code entry on the Clients page, and you do not need
one: connect Claude Code, Codex or the other provider on the Clients page as usual.

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

To tell, Toolport reads its own environment and, only when T3 Code is installed,
the names of up to eight parent processes when it starts. It writes nothing for
this and starts no extra threads.

## T3 Code's own tools

T3 Code gives each session its own tools, named like `mcp__t3-code__…`. They come
from T3 Code, not from Toolport. Don't add T3 Code's MCP endpoint as a server in
Toolport: T3 Code sessions already have these tools, so they would see them twice,
and the thread tools work only in the thread T3 Code gave them to. To use T3 Code
from another app, follow T3 Code's
[outside agents guide](https://github.com/pingdotgg/t3code/blob/main/docs/user/outside-agents.md).
