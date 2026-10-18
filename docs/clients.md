# Supported clients

Toolport auto-detects **30+ AI clients**, installs the gateway into each with one
click, and can import a client's existing servers. It writes the config file shown
below for you, so you never have to edit these by hand.

| Client                  | Config file                                                                                            | Format                   |
| ----------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------ |
| Claude Desktop          | `<config>/Claude/claude_desktop_config.json`                                                           | JSON (`mcpServers`)      |
| Claude Code             | `~/.claude.json`                                                                                       | JSON (`mcpServers`)      |
| Cursor                  | `~/.cursor/mcp.json`                                                                                   | JSON (`mcpServers`)      |
| Factory Droid           | `~/.factory/mcp.json`                                                                                  | JSON (`mcpServers`)      |
| Crush                   | `$CRUSH_GLOBAL_CONFIG/crush.json`, or `$XDG_CONFIG_HOME/crush/crush.json` (`~/.config/...` by default) | JSON (`mcp`)             |
| VS Code                 | `<config>/Code/User/mcp.json`                                                                          | JSON (`servers`)         |
| Devin Desktop (Cascade) | `~/.codeium/windsurf/mcp_config.json`                                                                  | JSON (`mcpServers`)      |
| Devin Local / CLI       | `%APPDATA%/devin/mcp_config.json` (Windows), `~/.config/devin/mcp_config.json` (macOS; Linux default)  | JSON (`mcpServers`)      |
| OpenCode                | `~/.config/opencode/opencode.json`                                                                     | JSON (`mcp`)             |
| Kilo Code               | `~/.config/kilo/kilo.jsonc`                                                                            | JSONC (`mcp`)            |
| Codex                   | `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`)                                             | TOML (`mcp_servers`)     |
| Copilot CLI             | `~/.copilot/mcp-config.json`                                                                           | JSON (`mcpServers`)      |
| Grok Build              | `$GROK_HOME/config.toml` (default `~/.grok/config.toml`)                                               | TOML (`mcp_servers`)     |
| Continue                | `~/.continue/config.yaml`                                                                              | YAML (`mcpServers`)      |
| Antigravity             | `~/.gemini/config/mcp_config.json`                                                                     | JSON (`mcpServers`)      |
| Gemini CLI              | `$GEMINI_CLI_HOME/.gemini/settings.json` (default `~/.gemini/settings.json`)                           | JSON (`mcpServers`)      |
| Qwen Code               | `$QWEN_HOME/settings.json` (default `~/.qwen/settings.json`)                                           | JSON (`mcpServers`)      |
| JetBrains Junie         | `~/.junie/mcp/mcp.json`                                                                                | JSON (`mcpServers`)      |
| Cline                   | `<config>/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json`             | JSON (`mcpServers`)      |
| Roo Code                | `<config>/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json`               | JSON (`mcpServers`)      |
| Warp                    | `~/.warp/.mcp.json`                                                                                    | JSON (`mcpServers`)      |
| Amazon Q                | `~/.aws/amazonq/mcp.json`                                                                              | JSON (`mcpServers`)      |
| Kiro                    | `~/.kiro/settings/mcp.json`                                                                            | JSON (`mcpServers`)      |
| Auggie CLI              | `~/.augment/settings.json`                                                                             | JSON (`mcpServers`)      |
| Kimi Code               | `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code/mcp.json`)                                           | JSON (`mcpServers`)      |
| DeepSeek Harness        | `$DSH_HOME/cordis.patch.yml` (default `~/.dsh/cordis.patch.yml`)                                       | YAML (patch rows)        |
| ZCode                   | `~/.zcode/cli/config.json`                                                                             | JSON (`mcp.servers`)     |
| Command Code            | `~/.commandcode/mcp.json`                                                                              | JSON (`mcpServers`)      |
| Tabnine Agent           | `~/.tabnine/mcp_servers.json`                                                                          | JSON (`mcpServers`)      |
| Zed                     | `~/.config/zed/settings.json`                                                                          | JSON (`context_servers`) |
| LM Studio               | `~/.lmstudio/mcp.json`                                                                                 | JSON (`mcpServers`)      |
| Jan                     | `<data>/Jan/data/mcp_config.json`                                                                      | JSON (`mcpServers`)      |
| BoltAI                  | `~/.boltai/mcp.json`                                                                                   | JSON (`mcpServers`)      |
| Pi                      | `~/.pi/agent/mcp.json`                                                                                 | JSON (`mcpServers`)      |
| Oh My Pi                | `~/.omp/agent/mcp.json`                                                                                | JSON (`mcpServers`)      |
| Goose                   | `~/.config/goose/config.yaml`                                                                          | YAML (`extensions`)      |
| Hermes                  | `~/.hermes/config.yaml`                                                                                | YAML (`mcp_servers`)     |
| AnythingLLM             | `<config>/anythingllm-desktop/storage/plugins/anythingllm_mcp_servers.json`                            | JSON (`mcpServers`)      |
| Witsy                   | `<config>/Witsy/settings.json`                                                                         | JSON (`mcpServers`)      |
| Amp                     | `~/.config/amp/settings.json`                                                                          | JSON (`amp.mcpServers`)  |

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
so.

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
Integrations -> Open WebUI / HTTP endpoint** in the app (or run
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
under **Settings > Advanced**. Folder routing takes precedence over client access,
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
