# Agent permissions

Write the rules Claude Code should enforce on its own native tool calls once in Toolport
and have them in every Claude Code profile on your machine, instead of hand-editing each
`settings.json` and keeping them in sync yourself.

Open the **Agent permissions** tab in the sidebar. No MCP server or gateway needed.

## What it does, and what it does not

Toolport's gateway governs MCP calls: approvals, destructive-call blocking, per-tool
policy. It cannot see, let alone stop, what Claude Code does natively - a shell command,
a file edit, a web fetch - because none of that is MCP.

Claude Code has a native switch for exactly that: the `permissions` lists in its
`settings.json`. Every native tool call is checked against `deny`, then `ask`, then
`allow`, on every call, and per Claude Code's own
[permissions documentation](https://code.claude.com/docs/en/permissions) a matching deny
or ask rule applies "regardless of what a PreToolUse hook returns". So a policy written in
Claude Code's rule syntax needs no hook and no Toolport process in the loop to be
enforced; it needs to be in the file, in every profile, and to come back out cleanly.
That is the whole of this feature.

Claude Code enforces the rules itself. **Cursor** has no settings-level rule list, so
Toolport enforces the same rules there through a guard hook - see
[Cursor](#cursor) below. Codex uses approval and sandbox settings of a different shape
and Gemini CLI is mixed; neither is covered yet, and the tab says so rather than
pretending. The gateway's own gates still cover every MCP call from every client.

## Rules

A rule is a pattern in Claude Code's own syntax plus what to do when a native call
matches it:

| Action           | Claude Code list | Meaning                                                          |
| ---------------- | ---------------- | ---------------------------------------------------------------- |
| **Never**        | `deny`           | The call is refused. A bare tool name removes the tool entirely. |
| **Ask first**    | `ask`            | Claude Code prompts you before the call runs.                    |
| **Always allow** | `allow`          | The call runs without a prompt.                                  |

Patterns: a tool name, optionally followed by a specifier in parentheses.
`Bash(rm -rf *)`, `Bash(git push --force*)`, `Read(./.env)`, `Edit(src/**/*.ts)`,
`WebFetch(domain:example.com)`, `mcp__github__create_issue`, or a bare `WebFetch`. When
more than one rule matches, deny beats ask beats allow, exactly as Claude Code evaluates
them. Toolport checks the shape of a pattern (it must be a tool name with an optional
parenthesised specifier) and refuses anything else; Claude Code owns the meaning.

Presets - never delete recursively, never force-push, ask before any git push, never
read `.env` files, never read SSH keys - are one-click adds to the list. They are never
applied on their own.

## Before anything is written

- **Off by default, empty by default.** Nothing is written until you turn the switch on,
  and the rule list starts empty.
- **Preview shows the exact bytes** each profile's `settings.json` would hold, before
  the first write, including your own formatting and comments, which are preserved
  (only the `permissions` key is rewritten).
- **Every Claude Code profile is covered**, including ones selected with
  `CLAUDE_CONFIG_DIR`. A rule in only one profile would quietly not apply in the others.

## What Toolport owns

JSON arrays of strings cannot carry a marker, so ownership is by record: for each file,
Toolport remembers exactly which rule strings it added. A rule you already had in the
file is not added and not recorded, so turning Toolport off, or removing the rule in
Toolport, never takes away a rule you wrote yourself. Changing a rule's action moves it
between lists; removing it from the policy removes it from every file; a profile that
appears later picks the policy up at the next start. A hand edit that drops one of the
rules is put back at the next start - these are restrictions you asked for, so, unlike
[agent rules](agent-rules.md), a hand edit is not a reason to stand down.

A `settings.json` that cannot be parsed is reported, left untouched, and does not stop
the other profiles from being written or cleaned.

## Cursor

Cursor has hooks (`beforeShellExecution`, `beforeMCPExecution`, `beforeReadFile`) but no
rule list, so Toolport installs a small **guard hook** into `~/.cursor/hooks.json`: before
a shell command runs, an MCP tool is called, or a file is read, Cursor runs
`toolport-gateway --toolport-guard cursor` with the call on stdin and acts on the JSON it
prints - `allow`, `deny`, or `ask`. The guard evaluates the **same rules** as above, so
you write them once.

Because the rule language is Claude Code's, here is exactly what carries over:

| Rule                                             | Cursor event           | Notes                                                                                       |
| ------------------------------------------------ | ---------------------- | ------------------------------------------------------------------------------------------- |
| `Bash(...)`                                      | `beforeShellExecution` | Matched against the command as Cursor reports it, with Claude Code's semantics below.       |
| `Read(...)`                                      | `beforeReadFile`       | `~/` is your home, `//` is absolute, anything else is relative to the workspace root.       |
| `mcp__server__tool`                              | `beforeMCPExecution`   | Cursor names the tool, not the server, so the server part cannot be checked and is ignored. |
| `Edit`, `WebFetch`, `Agent`, `Tool(param:value)` | -                      | No Cursor event; these rules do nothing in Cursor (they still work in Claude Code).         |

Matching follows Claude Code where it documents it: `*` spans anything including
spaces; a trailing ` *` needs a word boundary (`Bash(ls *)` matches `ls -la` but not
`lsof`); `:*` is the same as ` *`; a compound command (`&&`, `||`, `;`, `|`) is denied
or asked if **any** part matches and allowed only if **every** part is; `timeout N`,
`time`, `nice` and `nohup` are stripped first. Deny beats ask beats allow when more than
one rule matches.

Three modes, chosen in the tab:

- **Off** - no hook installed (the default).
- **Observe** - the hook is installed, every call is evaluated and recorded in **Agent
  activity** with what the rules _would_ have decided, and the answer is always allow.
  Use this to read a policy's effect before letting it act.
- **Enforce** - Never and Ask first take effect. "Ask first" uses Cursor's own
  confirmation prompt. The hook is installed with Cursor's `failClosed` set, so if the
  guard itself crashes or times out Cursor blocks the call rather than letting it through.

When no rule matches, the guard answers Cursor's own canonical "proceed" response. When
the guard cannot judge a call - a payload it does not understand, or one larger than it
reads (64 MiB; a file read carries the file's content) - it depends on the mode: in
**Observe** it answers allow and records that it could not judge; in **Enforce** it
**refuses**, with a message saying why, because an agent must not be able to get a call
past a rule by making the payload unreadable, and Cursor's `failClosed` would not catch
a well-formed allow. Cursor's `failClosed` still covers the guard crashing or timing out.

Only entries carrying `--toolport-guard` are ever added to or removed from
`hooks.json`; your own hooks stay exactly where they are, and the file's formatting is
preserved. A Preview shows the exact bytes before the first write.

## Next

Routing "ask" through Toolport's own approval window (instead of Cursor's prompt) and a
`PreToolUse` guard for Claude Code for the same purpose are the next steps; they build
on this and on the authenticated approval broker shipped in 1.17.
