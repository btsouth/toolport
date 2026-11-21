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
- An instance's home comes from its home folder setting, else `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME`, `XDG_CONFIG_HOME` (OpenCode), `GROK_HOME` or `PI_CODING_AGENT_DIR`
  from its environment variables, else the default.
- A Claude Code, Codex, OpenCode, Grok, Pi or Antigravity home only T3 Code knows
  about, such as one under `~/.t3`, gets its own row once its folder exists, so it
  can be connected like any other. Each row writes the file that provider reads
  under T3 Code:
  - OpenCode: `opencode/opencode.json` in its `XDG_CONFIG_HOME`, or
    `opencode.jsonc` when only that exists (comments are kept).
  - Grok: `config.toml` in its `GROK_HOME`.
  - Pi: `mcp.json` in its `PI_CODING_AGENT_DIR`. Settings Pi saves in Toolport's
    entry, such as `enabled` and `exposure`, are kept when Toolport rewrites it,
    also when you connect again through the review. Pi's settings never make the
    entry count as edited by you, and Disconnect takes them out with the entry. If
    nothing else in the file changed, the file goes back exactly as it was.
  - Antigravity: `config/mcp_config.json` in the private profile T3 Code gives each
    instance (`~/.t3/userdata/providers/antigravity/<id hash>/`), once T3 Code has run
    it. Toolport touches only that file, never the profile's `settings.json` or its
    `config/skills` link. T3 Code keeps this folder private, and a later T3 Code
    release may change or clear it; if Antigravity loses Toolport after a T3 Code
    update, open Clients and connect its row again. The `disabled` switch `agy` saves
    in Toolport's entry is kept, and Disconnect takes it out with the entry.
- On Linux, the Toolport entry in a T3 Code home names Toolport's data folder
  (`TOOLPORT_DATA_DIR`), because OpenCode passes its instance's `XDG_CONFIG_HOME`
  on to MCP servers. If an OpenCode instance sets `mcp.toolport` in its inline
  `OPENCODE_CONFIG_CONTENT`, that wins over the file; Toolport doesn't read that
  value, since it sits beside API keys.
- Folders named like `~/.claude_personal` and `~/.codex_personal`, which T3 Code's
  guides suggest, are found the same way as `~/.claude2`.
- Toolport uses only homes inside your home folder, written plainly. A home outside
  it, on a network share or device, or written with `.`, `..` or `//` is ignored
  without being opened. A home with a link anywhere below your home folder (the home
  itself, a folder above it, or its config file) is not followed: it gets no row and
  labels nothing. To use such a folder anyway, add it on the Clients page with **Add
  client location**.
- A folder you added by hand and the same folder named in T3 Code are one row.
- An instance whose home folder is marked sensitive, isn't plain text or is a
  relative path labels no row: Toolport can't tell which config it reads.
- Instances Toolport leaves out are listed on the T3 Code card, below.
- When you add, remove or switch on an instance in T3 Code, the open Clients page
  updates within a few seconds while Toolport's window is in front, or as soon as
  you switch back to it or return from a client's details. Without T3 Code, Toolport checks for it only when the page
  opens and when you switch back to Toolport.
- A connected T3 Code home whose folder is deleted moves to **Not installed**, and
  Toolport never recreates the folder. Once its folder is gone or T3 Code no longer
  lists it, it can be forgotten there.
- While T3 Code still lists a home whose folder exists, even one with no config yet,
  **Remove location** is shown disabled with the reason: remove the instance from T3
  Code's provider settings first.

## The data folder in a row's entry

On Linux, a T3 Code home's entry names Toolport's data folder
(`TOOLPORT_DATA_DIR`), because some providers, such as OpenCode, pass their own
`XDG_CONFIG_HOME` to MCP servers and the gateway would look for its data there.
Toolport adds it when you connect the row, or on a later rewrite of an older
entry. Toolport replaces that value only when it wrote it itself, for example
after its data folder was renamed. A folder you set by hand, or one another
Toolport on its own data folder wrote, stays as it is. Some limits:

- On macOS the entry gets no data folder. A client run there with its own `HOME`
  starts the gateway without your servers. Windows is not affected.
- A client's main row gets no data folder either. If T3 Code runs a provider with
  no home of its own but sets `XDG_CONFIG_HOME` in the instance's environment
  variables, the gateway won't find your servers. Give the instance its own home.
- A data folder whose path is not plain UTF-8 text, or is relative, is not written.
- A config shared by two machines or two users names the folder of whoever
  connected it last. Connect it again on the other one.
- If Toolport's data folder is renamed after a row was connected, the entry names
  the old folder until Toolport next starts or opens Clients. That repair waits
  while the keyring is locked.

## The T3 Code card

With T3 Code installed, the Clients page shows a T3 Code card. It counts the
instances that are on and not connected, and names each one Toolport can't connect
as it is, with why and what to do:

- A row it uses that isn't installed, has a config Toolport can't read, or has a
  Toolport entry changed by hand. Open that client below to fix it.
- **fx**, **Muse Code**, **ACP Registry** agents and any provider T3 Code adds later:
  Toolport has no client for them yet.
- A home folder that is marked sensitive, isn't plain text, or is a relative path
  (T3 Code passes `CLAUDE_CONFIG_DIR` and `CODEX_HOME` as they are, so `~` there is
  relative too). Set a full, plain path in T3 Code's provider settings.
- A home folder outside your home folder, written with `.`, `..` or `//`, or on a
  network or device path.
  Toolport judges it by its text alone and never opens it. Add it by hand as a client
  location if you trust it. Once a saved location names the same config, by its text,
  the card stops listing it.
- A home folder with a link anywhere below your home folder
  (the home itself, a folder above it, or its config file), or something that isn't a
  plain folder or file. Point the instance at the real folder in T3 Code's provider
  settings.
- A home folder that doesn't exist yet (for OpenCode, the `opencode` folder in its
  `XDG_CONFIG_HOME`; for Antigravity, the `config` folder of its private profile).
  Start the instance once in T3 Code, then refresh Clients.
- An entry in `settings.json` Toolport can't read, or a settings file it can't read
  at all, named by its path with `~` for your home folder.

The card names instances by their name and provider only, never by the paths in the
settings file.

### Connect all

While a row an instance uses can be connected as it is, the card offers **Connect
all**. It opens the usual connect review for each of those rows in turn, with a line
such as "Step 1 of 3: Claude Work" naming the instances on that row. Instances that are
off are skipped; you can still connect one yourself.

- **Next** moves on after a row connects, **Done** ends the last one.
- When a row fails, **Retry** tries again and **Skip** moves on without it.
- **Escape**, **Cancel** or closing a review stops the batch. Rows already connected
  stay connected.

Each row is connected on its own, as its own **Connect** would: Toolport backs up that
client's config first, then writes the new one in a single step. A row that fails part
way leaves its config exactly as it was, with nothing of it added to Toolport, and the
rows connected before it stay connected, each with its own backup.

If your keyring is locked, Connect all asks you to unlock it at most once: when the
first row that reads it opens its review, or when you connect that row. If you don't
unlock it, that row fails, and nothing after it asks again: a row that needs the
keyring says to unlock it and choose **Retry**, and **Retry** may ask once more.

A row that was connected to Toolport before and later disconnected keeps saved
recovery values in the keyring. Unlock your keyring before you connect such a row:
while it is locked, connecting it fails without asking.

When the batch ends, a summary says how many instances it connected. If any were
skipped, failed or not reached, or a row needs attention first, it names them and
stays until you dismiss it. Focus returns to **Connect all**, or to the card once
everything it can connect is connected.

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
- **OpenCode** and **Antigravity**: "via T3 Code", the same way as Codex: T3 Code
  runs one `opencode serve` per OpenCode instance for all its threads, and starts
  Antigravity's `agy acp` itself. Antigravity also shows the thread when T3 Code
  passes it on.
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
