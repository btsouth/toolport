# Supported clients

Toolport auto-detects these **35 AI clients**, installs the gateway into each with one
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
| Kimi Code               | `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code/mcp.json`)                                           | JSON (`mcpServers`)      |
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

### Codex setup walkthrough

Use this when Codex has already created its home directory (`$CODEX_HOME`, or `~/.codex/` when that env is unset).

1. In Toolport, add or enable the MCP servers you want Codex to use.
2. Open **Clients**, select **Codex**, optionally choose a profile, and click **Connect to Toolport**.
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
checkout (the folder that contains `plugin.json`). From the first release tagged
after this lands, the same folder also ships as `toolport-agent-plugin.zip` on
the [releases page](https://github.com/btsouth/toolport/releases). The plugin
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
