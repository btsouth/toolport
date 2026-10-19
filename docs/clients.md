# Supported clients

Toolport auto-detects **30+ AI clients**, installs the gateway into each with one
click, and can import a client's existing servers. It writes the config file shown
below for you, so you never have to edit these by hand.

| Client                  | Config file                                                                                            | Format                    |
| ----------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------- |
| Claude Desktop          | `<config>/Claude/claude_desktop_config.json`                                                           | JSON (`mcpServers`)       |
| Claude Code             | `~/.claude.json`                                                                                       | JSON (`mcpServers`)       |
| Cursor                  | `~/.cursor/mcp.json`                                                                                   | JSON (`mcpServers`)       |
| Factory Droid           | `~/.factory/mcp.json`                                                                                  | JSON (`mcpServers`)       |
| Crush                   | `$CRUSH_GLOBAL_CONFIG/crush.json`, or `$XDG_CONFIG_HOME/crush/crush.json` (`~/.config/...` by default) | JSON (`mcp`)              |
| VS Code                 | `<config>/Code/User/mcp.json`                                                                          | JSON (`servers`)          |
| Devin Desktop (Cascade) | `~/.codeium/windsurf/mcp_config.json`                                                                  | JSON (`mcpServers`)       |
| Devin Local / CLI       | `%APPDATA%/devin/mcp_config.json` (Windows), `~/.config/devin/mcp_config.json` (macOS; Linux default)  | JSON (`mcpServers`)       |
| OpenCode                | `~/.config/opencode/opencode.json`                                                                     | JSON (`mcp`)              |
| Kilo Code               | `~/.config/kilo/kilo.jsonc`                                                                            | JSONC (`mcp`)             |
| Codex                   | `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`)                                             | TOML (`mcp_servers`)      |
| Copilot CLI             | `~/.copilot/mcp-config.json`                                                                           | JSON (`mcpServers`)       |
| Grok Build              | `$GROK_HOME/config.toml` (default `~/.grok/config.toml`)                                               | TOML (`mcp_servers`)      |
| Continue                | `~/.continue/config.yaml`                                                                              | YAML (`mcpServers`)       |
| Antigravity             | `~/.gemini/config/mcp_config.json`                                                                     | JSON (`mcpServers`)       |
| Gemini CLI              | `$GEMINI_CLI_HOME/.gemini/settings.json` (default `~/.gemini/settings.json`)                           | JSON (`mcpServers`)       |
| Qwen Code               | `$QWEN_HOME/settings.json` (default `~/.qwen/settings.json`)                                           | JSON (`mcpServers`)       |
| JetBrains Junie         | `~/.junie/mcp/mcp.json`                                                                                | JSON (`mcpServers`)       |
| Cline                   | `<config>/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json`             | JSON (`mcpServers`)       |
| Roo Code                | `<config>/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json`               | JSON (`mcpServers`)       |
| Warp                    | `~/.warp/.mcp.json`                                                                                    | JSON (`mcpServers`)       |
| Amazon Q                | `~/.aws/amazonq/mcp.json`                                                                              | JSON (`mcpServers`)       |
| Kiro                    | `~/.kiro/settings/mcp.json`                                                                            | JSON (`mcpServers`)       |
| Auggie CLI              | `~/.augment/settings.json`                                                                             | JSON (`mcpServers`)       |
| Kimi Code               | `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code/mcp.json`)                                           | JSON (`mcpServers`)       |
| DeepSeek Harness        | `$DSH_HOME/cordis.patch.yml` (default `~/.dsh/cordis.patch.yml`)                                       | YAML (patch rows)         |
| Reasonix                | `$REASONIX_HOME/config.toml` (default `~/.reasonix/config.toml`)                                       | TOML (`[[plugins]]`)      |
| Codewhale               | `~/.codewhale/mcp.json` (or `CODEWHALE_MCP_CONFIG`, `mcp_config_path`)                                 | JSON (`servers`)          |
| OpenClaw                | `~/.openclaw/openclaw.json` (or `OPENCLAW_CONFIG_PATH`, `OPENCLAW_STATE_DIR`)                          | JSON5 (`mcp.servers`)     |
| OpenHands               | `~/.openhands/mcp.json` (or `OPENHANDS_PERSISTENCE_DIR`)                                               | JSON (`mcpServers`)       |
| nanobot (HKUDS)         | `~/.nanobot/config.json`                                                                               | JSON (`tools.mcpServers`) |
| ZCode                   | `~/.zcode/cli/config.json`                                                                             | JSON (`mcp.servers`)      |
| Command Code            | `~/.commandcode/mcp.json`                                                                              | JSON (`mcpServers`)       |
| Tabnine Agent           | `~/.tabnine/mcp_servers.json`                                                                          | JSON (`mcpServers`)       |
| Zed                     | `~/.config/zed/settings.json`                                                                          | JSON (`context_servers`)  |
| LM Studio               | `~/.lmstudio/mcp.json`                                                                                 | JSON (`mcpServers`)       |
| Jan                     | `<data>/Jan/data/mcp_config.json`                                                                      | JSON (`mcpServers`)       |
| BoltAI                  | `~/.boltai/mcp.json`                                                                                   | JSON (`mcpServers`)       |
| Pi                      | `~/.pi/agent/mcp.json`                                                                                 | JSON (`mcpServers`)       |
| Oh My Pi                | `~/.omp/agent/mcp.json`                                                                                | JSON (`mcpServers`)       |
| Goose                   | `~/.config/goose/config.yaml`                                                                          | YAML (`extensions`)       |
| Hermes                  | `~/.hermes/config.yaml`                                                                                | YAML (`mcp_servers`)      |
| AnythingLLM             | `<config>/anythingllm-desktop/storage/plugins/anythingllm_mcp_servers.json`                            | JSON (`mcpServers`)       |
| Witsy                   | `<config>/Witsy/settings.json`                                                                         | JSON (`mcpServers`)       |
| Amp                     | `~/.config/amp/settings.json`                                                                          | JSON (`amp.mcpServers`)   |

`<config>` is your OS application-config dir (`%APPDATA%` on Windows, `~/Library/Application Support` on macOS, `~/.config` on Linux); `<data>` is the data dir (`~/.local/share` on Linux, the same as `<config>` elsewhere). Zed and Goose paths vary slightly by OS; Toolport resolves the right one automatically.

### Several Claude Code accounts

Toolport also finds Claude Code config folders beside `~/.claude.json`, such as
`~/.claude2`, and you can add others with **Add client location**. When there is
more than one, the Clients page lists them under one Claude Code entry. Each
profile shows the command that starts it, the claude.ai account signed in to it
and when it was last used.

The command comes from small launcher scripts in `~/.local/bin`, `~/bin` or your
own PATH folders, and from bash or zsh aliases, that set `CLAUDE_CONFIG_DIR` and
run `claude`. A folder with no such launcher is named by its path.

`~/.claude/.claude.json` with the same account as `~/.claude.json` is usually
written by background tools that set `CLAUDE_CONFIG_DIR=~/.claude`. Claude Code
itself does not read it, so it is listed behind a note instead of as its own
profile. Show lists it if you want to connect it anyway.

### Auggie CLI setup

Toolport manages Auggie's user settings at `~/.augment/settings.json`
(`C:\Users\<username>\.augment\settings.json` on Windows). That file also holds
Auggie's shell, theme, tool permissions, and other settings, so Toolport
rewrites only the `mcpServers` key and leaves comments and the rest of the file
as they are. Project (`<workspace>/.augment/settings.json`) and managed
settings files are left alone.

### DeepSeek Harness setup

DeepSeek Harness (`dsh`) reads MCP servers from Cordis patch files. Toolport
manages the home layer, `$DSH_HOME/cordis.patch.yml` (default
`~/.dsh/cordis.patch.yml`), which dsh applies over every profile, so the gateway
is available in the Web UI and every other profile. Connect adds one `insert`
row with a `@deepseek-ai/dsh-mcp-client` entry named `toolport`; Disconnect
removes just that entry. Comments, `!!js` expressions, other plugins and your
own servers stay as they are, and a server you turned off with a
`disabled: true` row is left alone. Per-profile layers under
`$DSH_HOME/profiles/<name>/` are not changed. Start a new dsh session after
connecting; the running one keeps the tools it started with.

dsh connects to stdio and Streamable HTTP servers, so an SSE server can't be
moved into its config, and server names are limited to letters, digits, `-` and
`_` (32 at most). If a patch file is written in a layout Toolport can't edit
exactly, such as a single-line flow list, Toolport leaves it untouched and says
so. Groups created in the home patch are resolved in file order, including
later group disables. Inserts targeting an uninspected profile layer stay
untouched and are not offered as active servers. You can still connect the
gateway. Locally targeted server inserts can be edited in place. Grouped
servers stay in dsh when importing: moving them would lose their group's
activation context.

### Reasonix setup

Reasonix keeps MCP servers as `[[plugins]]` tables in its global config,
`config.toml` in the Reasonix home: `~/.reasonix` on macOS and Linux,
`%APPDATA%\reasonix` on Windows, or `REASONIX_HOME`. The CLI and Reasonix
Studio share it. Connect adds one `[[plugins]]` table named `toolport`;
Disconnect removes just that table. Comments, other settings and your own
servers stay as they are, and a changed server is edited in place. Project
`reasonix.toml` and `.mcp.json` files are not changed. Start a new Reasonix
session after connecting. Reasonix also loads the user servers in Claude Code's
`~/.claude.json`; once Reasonix is connected, its own `toolport` table takes
the place of Claude Code's, so its calls show up as Reasonix in Activity.

Reasonix keeps an MCP server's tools behind its own capability search, so
Toolport's Auto mode lists the full catalog. It does not pick up catalog
changes during a session, so a server you add in Toolport appears in the next
session. Plugin lists written inline (`plugins = [{ ... }]`) are left untouched.
Duplicate names use the last command with merged disabled-tool policies. You
can connect the gateway while leaving those tables intact; resolve a duplicated
name before moving or replacing that server.

Global rules follow `REASONIX_STATE_HOME`, then `[storage] state` in the global
config, then the Reasonix home. Absolute paths and leading `~` are supported;
configured `${VAR}` and `${VAR:-default}` references are expanded. Relative
paths depend on Reasonix's launch directory and fall back to the home here.
The Skills page includes nested directory and single-file Markdown skills,
with `[skills] max_depth` (default 3, capped at 5). Nested skills need a
description; flat files under `.claude/skills` need skill frontmatter. Skill
toggles use `[skills] disabled_skills` only when Toolport can verify the exact
identifier: an ASCII filename and printable ASCII frontmatter with LF or CRLF
line endings, no tabs or other controls, and at most one literal `name:` field.
That name must be a bare, unquoted value containing only letters, digits, dots,
underscores or hyphens. Numeric spelling is preserved. Quoted names, repeated
names and empty name fields are read-only.
Unicode names, complex YAML and other uncertain identifiers stay visible with
a read-only reason; use Reasonix to switch those skills. Other skills and
clients remain independently switchable.

### Codewhale setup

Codewhale reads MCP servers from `~/.codewhale/mcp.json`. Toolport finds the
same file Codewhale does: `CODEWHALE_MCP_CONFIG` (or the older
`DEEPSEEK_MCP_CONFIG`), then `mcp_config_path` in its `config.toml`, then
`$CODEWHALE_HOME/mcp.json`, and the legacy `~/.deepseek/mcp.json` when that is
the only one. Connect adds a `toolport` server under the key the file already
uses, `servers` or `mcpServers`, since Codewhale refuses a file with both;
Disconnect removes it. Servers you turned off with `disabled` or
`enabled: false` are left alone. Run `/mcp reload` in a running session, or
start a new one. Import and migration refuse native tool filters, working
directories, timeouts and other settings Toolport cannot retain; keep those
servers in Codewhale or configure equivalent behavior in Toolport first.

Codewhale keeps MCP tools in its deferred catalog behind `tool_search`, so
Toolport's Auto mode lists the full catalog. It doesn't pick up catalog changes
during a session; `/mcp reload` picks them up. Codewhale reads one global
instructions file, the first of `~/.codewhale/AGENTS.md`, `~/.agents/AGENTS.md`,
`~/.deepseek/AGENTS.md` and the matching `instructions.md`. It prepends these
instructions to project context, with the project's own instructions taking
precedence. Team rules go into the one it already reads, or
`~/.codewhale/AGENTS.md` when there is none.

### OpenClaw setup

OpenClaw reads MCP servers from `mcp.servers` in its gateway config,
`~/.openclaw/openclaw.json`. Toolport finds the same file OpenClaw does:
`OPENCLAW_CONFIG_PATH` wins; otherwise existing `openclaw.json` and legacy
`clawdbot.json` files are searched in `OPENCLAW_STATE_DIR`, `~/.openclaw` and
`~/.clawdbot`, in that order. A new file is created only when no candidate exists.
`OPENCLAW_HOME` stands in for your home when resolving these config paths.
Connect adds a `toolport` server and Disconnect removes it. The file is JSON5, and Toolport edits only the `mcp`
section, so comments, unquoted keys and your other settings stay. Servers you
turned off with `enabled: false` are left alone. A config that brings in `mcp`
through `$include` is refused; add the server with `openclaw mcp add` instead.
The Gateway hot-reloads the change, and `openclaw mcp doctor toolport --probe`
checks it.

OpenClaw's Tool Search defers MCP schemas by default, so Toolport's Auto mode
lists the full catalog, and OpenClaw refreshes its catalog when the list
changes. The Skills page shows the skills in the default agent's workspace
(`skills` and `.agents/skills`), `~/.agents/skills`, `~/.openclaw/skills` and
`skills.load.extraDirs`, and turns them off with `enabled: false` under
`skills.entries`, using manifest skill keys or installed source-origin slugs
before the display name. Personal skills use the OS home even with
`OPENCLAW_HOME`; later `extraDirs` win when names repeat. Team rules go to the
workspace's `AGENTS.md`
(`~/.openclaw/workspace` unless `OPENCLAW_WORKSPACE_DIR` or
`agents.defaults.workspace` moves it). An explicit default-agent workspace wins
over `agents.defaults.workspace`, which wins over `OPENCLAW_WORKSPACE_DIR`.
Config paths expand `~`; the workspace environment override accepts an absolute
path only, matching OpenClaw. Empty or ambiguous agent rosters, included agent
settings and cwd-dependent workspace paths have no skills or rules target in
Toolport. A known MCP config can still be managed separately.

Workspace and extra skill roots follow OpenClaw's symlink boundaries. Escaped
skill folders are skipped unless their real targets are covered by
`skills.load.allowSymlinkTargets`. Managed and personal roots accept folder
links; a linked `SKILL.md` must still stay inside its own skill folder.

### OpenHands setup

Toolport manages the OpenHands CLI's `mcp.json`, in `~/.openhands` unless
`OPENHANDS_PERSISTENCE_DIR` moves it. OpenHands uses that variable as written,
so a relative value leaves the client unresolved. Connect adds a `toolport`
server under `mcpServers` and Disconnect removes it; the rest of the file keeps
its text. Like OpenHands, Toolport reads servers listed at the top level of a
file with no `mcpServers` key, and Connect moves them under `mcpServers` so they
keep running. Disconnect restores the original bytes if the file is unchanged
since Connect. After native edits, it removes only Toolport and keeps the current
server layout, including servers added under `mcpServers`. Settings set to `null`,
as `openhands mcp add` writes them, count as unset. Servers turned off with `enabled: false` (`openhands mcp disable`) or
another false-like value such as `0` or `null` are left alone, and a Toolport
entry you turn off stays off when Toolport repairs it. Remote servers always get a `transport`. Start a new OpenHands session to
load the change; `openhands mcp list` shows it. The OpenHands web app keeps its
MCP settings in its own settings page, which Toolport doesn't change.

OpenHands puts every MCP tool in the agent's tool list and doesn't re-list
during a session, so Toolport's Auto mode stays Lazy. The Skills page shows the
skills in `~/.agents/skills`, `~/.openhands/skills` and the legacy
`~/.openhands/microagents`, after the same folders in the selected project
directory and its nearest Git root. Project rules also include case-insensitive
`AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `GEMINI.md` and `.cursorrules`; the selected
directory wins name collisions with the Git root. OpenHands
can only turn off skills it installed itself. Toolport shows those installed
skills and their enabled state after user skills, but keeps the OpenHands column
read-only. Ordinary Markdown files with triggers appear as skills; files without
triggers appear as rules. AgentSkills are read from immediate child folders, not
arbitrary nested folders. Like OpenHands, Toolport doesn't look for Markdown
inside linked folders, except for a `SKILL.md` in a linked top-level skill
folder. Toolport bounds recursive Markdown scans to six levels
and 20,000 entries per root. OpenHands has no global `AGENTS.md`; team rules go to
`~/.openhands/microagents/toolport-team-rules.md`, which OpenHands loads as
always-on context.

### nanobot setup

Toolport supports [HKUDS/nanobot](https://github.com/HKUDS/nanobot), installed
as `nanobot-ai`, through `~/.nanobot/config.json` on every platform. Connect
adds the gateway under `tools.mcpServers`; Disconnect removes it and keeps the
rest of the config. This is strict JSON. MCP edits leave configs using the accepted
snake_case `tools.mcp_servers` spelling untouched; rename that key to nanobot's
normal `mcpServers` spelling before connecting or moving servers. Workspace rules
and skill toggles work with either spelling and preserve the MCP map's text. Instances launched with `--home` or
`--config` are not managed. The unrelated Obot nanobot is not this client.

nanobot has no server off switch: `enabled: false` does not stop a server.
Import accepts nanobot's saved defaults (`cwd: ""`, `auth: null`,
`toolTimeout: 30` and `enabledTools: ["*"]`), and refuses non-default settings
Toolport cannot preserve. Incomplete server entries are skipped in the client
inventory and cannot be imported. Remote writes specify
`type: "streamableHttp"` or `"sse"` so a URL ending in `/sse` keeps the intended
transport. Auto uses Lazy because nanobot registers every MCP tool and has no
`tools/list_changed` handler. Restart the session to load changes.

The Skills page reads one level of folders, including hidden folders, under `<workspace>/skills` and
switches them through `agents.defaults.disabledSkills`. nanobot identifies
skills by their folder names. A config using `disabled_skills` is read but
must use the normal `disabledSkills` spelling before switching. Enabled Agent
Plugin package skills appear after workspace skills, with the first package
folder winning name collisions. They use the same native toggle, and package
content is protected from editing and removal. Activation must match the complete
package fingerprint and canonical root; discovery never changes activation markers.
Verification skips packages above 20,000 entries or 128 MiB of file content.
Package metadata follows PyYAML's implicit scalar and merge rules. Toolport
conservatively skips metadata containing explicit standard tags other than
`!!str`, `!!map`, `!!seq` and `!!merge`, including tags on unrelated fields;
nanobot may accept those documents.
Bundled skills are not inventoried. Team rules go to `<workspace>/AGENTS.md`;
the Rules page also reads `SOUL.md` and `USER.md`. The workspace defaults to
`~/.nanobot/workspace`, or follows an absolute or `~/` path in
`agents.defaults.workspace` (also `~\` paths on Windows). Relative workspace
settings remain unresolved. Toolport reads workspace and disabled skill settings
from the file; `NANOBOT_AGENTS__DEFAULTS__WORKSPACE` and
`NANOBOT_AGENTS__DEFAULTS__DISABLED_SKILLS` environment overrides are not managed.
When those fields are omitted from the file, nanobot can use environment values
and load a different workspace or skill state than Toolport shows.

### ZCode setup

Toolport manages ZCode's user config at `~/.zcode/cli/config.json` on all
platforms. Connect from the **Clients** view, then restart ZCode to load the
gateway entry under `mcp.servers`. Existing servers and other settings are
preserved. Workspace configs are not managed yet.

ZCode's desktop can load servers from `~/.agents/mcp.json` when the native
server list is empty. Toolport refuses to add a gateway while those fallback
servers are active, because doing so would hide them. Move the servers into
ZCode's native user config first; Toolport leaves the shared fallback file
untouched.

Disconnect also refuses to report success if ZCode would still load a Toolport
gateway from the shared fallback file. Remove that gateway from the shared
config manually before disconnecting.

Imports support basic stdio, HTTP, and SSE entries. Imported servers start
disabled for review. Migration and pasted imports refuse disabled ZCode
entries. Settings that the import model cannot preserve, such as a working
directory, custom timeout, protocol version, or OAuth configuration, require
manual setup. Toolport checks these before importing or migrating a ZCode config.

### Tabnine Agent setup

Toolport manages Tabnine's user config at `~/.tabnine/mcp_servers.json` on all
platforms. Project files at `.tabnine/mcp_servers.json` are left alone. Tabnine
only loads MCP servers while a workspace folder is open, so open a folder after
connecting. Imports read remote headers from `requestInit.headers`.

Tabnine's setup page calls the top-level key `mcp_server`, but every example uses
`mcpServers`. Toolport follows the examples.

### Codex setup walkthrough

Use this when Codex has already created its home directory (`$CODEX_HOME`, or `~/.codex/` when that env is unset).

1. In Toolport, add or enable the MCP servers you want Codex to use.
2. Open **Clients**, select **Codex**, choose its Access, and click **Connect to Toolport**.
3. Toolport updates `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`) with a single `[mcp_servers.toolport]` entry. That entry runs the resolved `toolport-gateway` binary; existing Codex TOML keys and other MCP servers are preserved, and an existing config is backed up before the write. (Older installs that still have `[mcp_servers.conduit]` are renamed to `toolport` on the next Toolport launch.)
4. Start a new Codex session so it re-reads the config. In Toolport, the Codex row changes to **connected to Toolport**; in Codex, Toolport-managed tools are served through the one `toolport` MCP server. With lazy discovery enabled, Codex gets Toolport's compact search tools instead of every downstream tool up front.

Gotcha: when running Toolport from source, build the gateway first with `npm run build:gateway`. The desktop dev server does not build the separate binary that Codex spawns, so Codex will report the gateway as missing until that binary exists.

### Open WebUI and other HTTP/OpenAPI consumers

The gateway speaks HTTP/OpenAPI natively, so Open WebUI (and any OpenAPI tool
client) connects straight to Toolport, no bridge or proxy. Flip on **Settings ->
Connections -> Open WebUI / HTTP endpoint** in the app (or run
`toolport-gateway --http 8765` after setting `TOOLPORT_HTTP_TOKEN`), then add
`http://localhost:8765` as an OpenAPI tool server. See
[docs/openwebui.md](openwebui.md). The same endpoint serves
any HTTP/OpenAPI MCP consumer (n8n, LibreChat, custom agents).

### Agent plugin (Agent Plugins 1.0 and Claude Code)

Clients that install [Agent Plugins 1.0](https://agent-plugins.org) packages
(VS Code, GitHub Copilot CLI, the Copilot app, and other conformant agents) can
connect to Toolport by installing one plugin instead of editing MCP config.
Point your client's plugin install flow at
[packaging/agent-plugin/toolport/](../packaging/agent-plugin/toolport) from a
checkout (the folder that contains `plugin.json`). The same folder also ships
as `toolport-agent-plugin.zip` on the
[releases page](https://github.com/btsouth/toolport/releases). The plugin
bundles the gateway's MCP server entry plus a skill that teaches the agent
Toolport's search → call workflow, and the same folder also carries the Claude
Code plugin layout. It launches the gateway already installed by the desktop
app, so every plugin install shares your existing servers, credentials, and
profiles.

If you already connected that client in the app's Clients view, disconnect it
there first. VS Code, Claude Code, and GitHub Copilot CLI are all managed there,
and leaving both in place connects the gateway twice and shows every meta-tool
in duplicate. Details
in [packaging/agent-plugin/toolport/README.md](../packaging/agent-plugin/toolport/README.md).

## Headless gateway

For Docker and MCP over HTTP, see [Headless gateway](headless.md).

## Client access in 2.0

Servers have one on/off switch. An off server is hidden from every client. On the
Clients page, Access can select **All enabled servers** or a named **access set**.
Access sets narrow server and tool access; manage them and project folder routing
under **Settings > Access**. Folder routing takes precedence over client access,
then `TOOLPORT_PROFILE`, then the default access. Unknown access sets fail closed.

The v3 registry migration backs up the original document and turns on the union
of servers included in existing profiles. If the old active profile was narrower,
it pins that profile as the default. Existing client bindings, folder mappings,
tool scopes and integrity stores keep their stable profile IDs. When the server
sets match, the old default tool policy is retained until you explicitly choose
All enabled servers in Settings. The default integrity store remains the same.

Shared HTTP tokens keep their existing full connected set when no access set is
selected. Named access sets narrow their servers; tool restrictions apply to
stdio clients. This preserves HTTP access during the upgrade.

### Disconnect and original configs

Before its first edit, Toolport saves exact original bytes or a file-absent record
under its data directory's `backups/<client>/original-<path hash>.json`. The record
includes the path, content hash, capture time and Toolport version. It is separate
from the rotating backups and uses owner-only permissions (Unix modes or a
private Windows ACL). Original provenance is retained after disconnect, so
repeated removal and a later uninstaller preserve the restored config. Reconnecting
captures a fresh original and replaces the completed snapshot payloads. Client
config writes and restoration preserve the existing file mode.

Disconnect restores exact original bytes when the config still matches Toolport's
last write and no intervening native edits were observed. Otherwise it preserves
your edits and reverses Toolport's changes, restoring moved entries without
replacing entries you re-added. A snapshot first captured after an older Toolport
install uses this merge path and the existing move record. The original pre-install
bytes of that older installation may be unavailable.

Settings offers **Remove Toolport from all clients**, with confirmation and
per-client results. The same operation is available through the gateway CLI; see
[headless usage](headless.md#remove-client-connections). Package uninstall hooks
are separate from this command.
