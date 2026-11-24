# The toolport CLI

`toolport` is a standalone command line for everything that sits around your coding
agents: their MCP servers, skills, rules files, credentials and token use. It reads the
same files the Toolport app reads, with the same code, and runs without the app.

## Install

```sh
npx @toolport/cli            # run once, nothing to install
npm install -g @toolport/cli # keep `toolport` on your PATH
```

The npm package carries a prebuilt binary for macOS (Apple silicon and Intel), Linux
(x64 and arm64, glibc) and Windows (x64). It has no install script and downloads
nothing at run time. On another platform, build it from source:

```sh
cargo install --git https://github.com/btsouth/toolport conduit --bin toolport --no-default-features --features cli
```

Set `TOOLPORT_CLI_BIN` to point the npm launcher at a binary of your own.

The Toolport desktop packages for Linux (`.deb`, `.rpm` and the Arch package) have the
CLI built in: `/usr/bin/toolport scan`, `toolport usage` and every other command below
run it without opening a window. Run with no command, `toolport` opens the app instead,
so the desktop launcher (`toolport-gtk`) and start at login (`toolport --hidden`) work
as before. `toolport --help` lists both. If you also install `@toolport/cli`, either one
can come first on your PATH; for a check of every agent with no command, run `toolport
scan` or `npx @toolport/cli`. On Linux AppImage installs (`~/.local/bin/toolport`) and
legacy 1.x Tauri `.deb` packages, `toolport` still opens the app, so use
`npx @toolport/cli` there.

## Commands

### `toolport` / `toolport scan`

Checks every agent on this machine and prints one screen:

- **Agents**: every client and profile found (Claude Code profiles, Codex homes,
  Cursor, VS Code, OpenCode, Zed, T3 Code instances and the rest), how many servers
  each has, whether it is connected to the Toolport gateway, and configs that can't be
  read.
- **Plaintext secrets on disk**: the agent, file, server and setting name of every
  credential stored in plain text: env vars, the `Authorization` header, secrets in a
  URL, and secret arguments. Values are never printed. References such as
  `${GITHUB_TOKEN}` or `{env:X}` are not counted.
- **MCP drift**: servers configured separately in several agents (duplicates), and
  servers with the same name but a different command, arguments or URL (conflicts).
- **Skills and rules**: broken skill links, Codex entries that turn off a missing skill,
  skills shadowed by another copy, skills off in one agent and on in another, agents
  that miss the shared rules file, and `@import` lines an agent doesn't follow. It also
  says whether `~/.agents/skills` (where `npx skills add` installs) exists and which
  agents read it.
- **Turn-0 context**: the tokens of rules files (with their imports) and skill listings
  each agent loads before your first prompt.

| Option      | Meaning                                                               |
| ----------- | --------------------------------------------------------------------- |
| `--check`   | Exit 2 when anything needs attention, for CI                          |
| `--all`     | List every agent and finding, including informational ones            |
| `--project` | Include this project's skills and rules (default: the current folder) |

### `toolport usage`

A token and cache profiler over your local agent transcripts. It reads Claude Code
(`<profile>/projects/**/*.jsonl` for every profile, including `CLAUDE_CONFIG_DIR` and
`~/.claude2`-style profiles), Codex (`<codex home>/sessions`, including `CODEX_HOME` and
homes T3 Code uses), OpenCode's JSON message storage, and Toolport's own activity log.
It makes no network requests.

| Option          | Meaning                                       |
| --------------- | --------------------------------------------- |
| `--days N`      | Count turns from the last N days (default 30) |
| `--client NAME` | `claude`, `codex`, `opencode` or `all`        |
| `--no-cache`    | Read every transcript again                   |

What it reports:

- **Summary**: sessions, model turns, mean context per turn, the share of input served
  from cache, and a list-price equivalent cost for Claude models. It is what the same
  tokens would cost at Anthropic's published API prices (cache reads, 5 minute and 1 hour
  cache writes, output), not what a subscription bills. Models without a price table
  show tokens only.
- **What stays in context**: each source's share of every turn's input. A tool result of
  T tokens that stays in context for N more turns counts T x N, which is what the
  context window and the bill see. A compaction ends every residency. Sources are the
  startup floor (the first turn's context: system prompt, tool definitions, rules
  files), shell output, file reads, screenshots and images, search, edits, web,
  subagents, each MCP server (calls through the Toolport gateway count for the server
  they reach), and conversation.
- **Turn-0 startup tax**: the same per-agent rules and skills budget `scan` shows.
- **Cache health**: turns that rewrote the cache inside its lifetime (a model switch, a
  changing tool list or an edited rules file does that), sessions resumed after the
  cache expired, and compactions. Also screenshots that stayed in context for 20 or more
  turns and repeat reads of an unchanged file.
- **MCP servers**: calls, sessions, and bursts (sessions with 9 or more calls to one
  server, a Code Mode candidate). Servers configured in Toolport, Claude Code, Codex or
  OpenCode with no calls in the window are listed as unused.
- **Recommendations** with the next step for each finding.

Each transcript is reduced to a small summary of counts and names, cached in
`~/.cache/toolport` (`TOOLPORT_CACHE_DIR` to move it), so later runs only read
transcripts that changed. The first run over a large history reads everything once.

Estimates: text tokens are about four characters each; images are sized from their
pixel dimensions the way Claude bills vision input. OpenCode versions that keep history
only in `opencode.db` (SQLite) are not read yet, and `usage` says so.

### `toolport fix`

Shows every change first, and applies only what you confirm.

| Option        | Meaning                                                     |
| ------------- | ----------------------------------------------------------- |
| `--dry-run`   | Show every file, link and keychain change without making it |
| `--yes`       | Apply without asking                                        |
| `--only KIND` | `skills`, `rules`, `secrets` or `all` (default)             |
| `--connect`   | With secrets: also connect those agents to the gateway      |

Skills and rules fixes are the Skills page's checked repairs: each one is planned again
from disk and refused if the problem changed since you saw it.

`--only secrets` moves each server with a plaintext credential into Toolport and its
credentials into your OS keychain. The agent's own config is left as it was, so its
copy stays until the agent uses the gateway. With `--connect` and a gateway installed
(the Toolport app or `toolport-gateway`), Toolport also connects the agent and removes
the plaintext copies, keeping a backup in Toolport's private backups.

### `toolport init` and `toolport up`

`init` reads the project's rules file (`AGENTS.md`, `CLAUDE.md`), skills
(`.agents/skills`, `.claude/skills`, `.cursor/skills`, `.opencode/skills`) and MCP
configs (`.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json`, `.gemini/settings.json`)
and writes `toolport.toml`. Credentials become env names, never values. A server whose
command, arguments or URL holds a credential, or that sends custom headers, is left out
with a comment that says why.

`up` reads `toolport.toml` (or `--file`) and sets up each agent found here, or the ones
in `--agents claude-code,codex`:

- **Rules**: Claude Code, Gemini CLI and Qwen Code get a pointer file (`CLAUDE.md` with
  `@AGENTS.md`). When the source isn't `AGENTS.md`, agents that read `AGENTS.md` get a
  link to it.
- **Skills**: a `path` skill is linked into each agent's project skills folder when that
  agent doesn't already load it. A `source` skill that isn't installed shows the exact
  `npx skills add` command.
- **Servers**: added to Toolport with the warnings an import shows (runs a local
  command, connects to a private address). Each name in `env` comes from
  `secret_refs`, then your environment, then a hidden prompt, and is stored in the OS
  keychain. A server stays off until its credentials are set. A stdio server that runs
  files from the project (`node ./server.js`, `dist/index.js`) starts in the project
  folder. Running `up` again after the file changes updates the values, references, env
  names and working folder of servers an earlier `up` added, and shows the update first.
  A changed command, argument or URL is a conflict, never a silent change.
- **Agents**: when the manifest has servers, agents that aren't connected to the
  Toolport gateway are connected, if a gateway is installed.

`up --dry-run` prints the plan and changes nothing. Without a terminal, `up` needs
`--yes`.

#### `toolport.toml`

```toml
version = 1                      # required
name = "acme-api"                # optional
description = "Agent setup"      # optional

[rules]
source = "AGENTS.md"             # a file in the repo every agent should read

[[skills]]
path = ".agents/skills/release"  # a skill folder in this repo

[[skills]]
source = "owner/repo@skill"      # a skill from skills.sh

[[servers]]
name = "github"
transport = "stdio"              # stdio (default with command), http or sse
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = ["GITHUB_TOKEN"]           # required variables, names only
[servers.values]
LOG_LEVEL = "info"               # plain settings that are not secret
[servers.secret_refs]
GITHUB_TOKEN = "op://Engineering/GitHub/token"

[[servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
```

`secret_refs` accepts the password manager references the app supports: `op://`,
`doppler://`, `infisical://`, `vault://`, `bws://`, `bw://`, `keeper://`, `dl://`,
`lpass://` and `env:`.

The file must never hold a credential. `up` refuses it, with every problem listed,
when a value, argument, URL, user name or password looks like one. Arguments may refer
to a variable as `${NAME}`.

### `toolport export` and `toolport import`

`export` writes your Toolport servers as a setup file (stdout by default, or `--out`).
Credentials, secret arguments and URL secrets are stripped, so the file is safe to
commit or share as a gist:

```sh
toolport export --out team-setup.json
toolport export | gh gist create -
```

`import <file|->` shows each server, what it runs or connects to, and its warnings, and
adds the new ones switched off. `--only a,b` picks servers; `--dry-run` previews.

### `toolport hook`

```sh
toolport hook install     # Claude Code (every profile) and Codex, when present
toolport hook check       # what the hook runs
toolport hook uninstall
```

Install and uninstall show the settings files they change and ask first; pass `--yes`
in scripts, or `--dry-run` to preview. The hook names the CLI by its absolute path, so
an agent that starts with a different `PATH` never runs the desktop app's `toolport`.

The hook runs `toolport hook check` when a session starts. It prints nothing when the
machine is healthy, and one line when something is wrong, for example:

```
toolport: 2 plaintext secrets on disk, 1 broken skill link (run 'toolport scan')
```

It always exits 0, so it never blocks a session. Install and uninstall edit the settings
file in place, keeping comments and your own hooks, and copy the previous file to
Toolport's backups. Uninstall removes only Toolport's entry. For Codex, install also
turns on `[features] hooks` in `config.toml`, and Codex asks you to trust the hook the
next time it starts.

### `toolport report`

Writes `toolport-report.html` (or `--out`): one static file with the scan and usage
highlights, light and dark themes, no scripts and no external requests. It holds counts,
names and paths, never secret values.

## Output, exit codes and environment

- `--json` prints one JSON document on stdout for any command.
- Color is on for terminals only. `--no-color`, `NO_COLOR` or `TERM=dumb` turn it off;
  `FORCE_COLOR` turns it on for CI logs.
- Exit codes: `0` ok, `1` error, `2` findings (only with `scan --check`).

| Variable                          | Meaning                                                  |
| --------------------------------- | -------------------------------------------------------- |
| `TOOLPORT_DATA_DIR`               | Toolport's data folder (registry, activity log, backups) |
| `TOOLPORT_CACHE_DIR`              | Where `usage` keeps its transcript summaries             |
| `TOOLPORT_CLI_BIN`                | The binary the npm launcher runs                         |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | Agent folders, as the agents themselves read them        |

## Privacy

Every command reads local files only. `import` fetches a setup only when you give it a
Toolport share link. Nothing the CLI prints or writes contains a secret value.
