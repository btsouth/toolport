# Client discovery defaults

Auto is the default in Clients. It advertises the full tool list when the client
has documented native search or deferred loading **and** refreshes on
`notifications/tools/list_changed`. Otherwise it exposes search and call tools.
The table is in `src-tauri/src/clients/discovery.rs` and travels with each detected
client to both shells. Evidence was checked on 2026-10-07; unknown means unverified,
not an assertion that a vendor lacks the capability.

A cold first list has a two-second catalog wait. Later servers arrive by
`list_changed`. Keeping clients with unknown refresh behavior lazy avoids losing
those tools: search and call read the live catalog. We retain the short startup
bound instead of assuming a vendor startup timeout. This deliberately keeps
Codex, Cursor and API-style hosts lazy until notification refresh is verified.
The API identifiers describe custom HTTP hosts, not new file-based adapters.

| Client ID            | Native search / deferral | Tool-list refresh | Auto | Evidence                                                                                                                                                                                                                                                   |
| -------------------- | ------------------------ | ----------------- | ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `claude-desktop`     | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `cursor`             | Yes                      | Unknown           | Lazy | [Cursor dynamic discovery](https://cursor.com/blog/dynamic-context-discovery); late-list refresh unverified                                                                                                                                                |
| `droid`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `crush`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `anythingllm`        | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `vscode`             | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `amp`                | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `windsurf`           | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `devin-cli`          | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `opencode`           | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `kilo-code`          | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `grok`               | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `codex`              | Yes                      | Unknown           | Lazy | [Codex config reference](https://developers.openai.com/codex/config-reference); [MCP catalog implementation](https://github.com/openai/codex/blob/main/codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs), general notification refresh unverified |
| `github-copilot-cli` | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `antigravity`        | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `claude-code`        | Yes                      | Yes               | Full | [Claude Code: tool search and dynamic updates](https://code.claude.com/docs/en/mcp)                                                                                                                                                                        |
| `gemini-cli`         | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `qwen-code`          | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `junie`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `cline`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `roo-code`           | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `warp`               | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `amazon-q`           | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `kiro`               | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `kimi-code`          | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `zcode`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `zed`                | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `lm-studio`          | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `jan`                | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `boltai`             | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `pi`                 | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `omp`                | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `goose`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `hermes`             | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `continue`           | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `witsy`              | Unknown                  | Unknown           | Lazy | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                            |
| `anthropic-api`      | Yes                      | Unknown           | Lazy | [Anthropic tool search](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool); host notification behavior unverified                                                                                                             |
| `openai-api`         | Yes                      | Unknown           | Lazy | [OpenAI tool search](https://developers.openai.com/api/docs/guides/tools-tool-search); host notification behavior unverified                                                                                                                               |

Choose Full, Lazy or Grouped in the existing per-client control to override Auto.
Existing `clientDiscovery` entries are preserved, including Grouped. Clearing an
entry restores Auto; this needs no schema migration (registry v3 stays v3).
`client:<adapter-id>` HTTP identities use the same table and adapter override
unless an override exists for the bearer ID itself. Arbitrary bearer IDs
stay conservative; Toolport does not guess capabilities from a display label.
Identified connections use Auto instead of the global discovery default; the
global mode and legacy boolean still apply to anonymous connections, including
stdio adapters with generated PID identities and no stable client ID. An explicit
`TOOLPORT_DISCOVERY` on a standalone stdio gateway still wins; shared daemon
sessions resolve their per-client choice or Auto.

Full exposes Toolport's downstream names and schemas for the client's own search.
Lazy and Grouped dispatch through `toolport_call_tool`, so client-side rules
cannot target individual downstream tools. Client per-tool permission rules need
Full mode. Gateway prefixes and renamed tools can also require updating rule
names; vendor-specific allow/deny matching has not been verified end to end.
Toolport access sets, tool restrictions, Safety and quarantine apply in every
mode, even to direct calls for tools that were not advertised.

The regression `discovery_surface_token_measurement` reports `cl100k_base` counts
for the exact serialized tool arrays with Code Mode off and a fixed 14-tool
fixture. It also checks that the lazy floor is independent of catalog size.
These are MCP catalog costs, not vendor prompt usage or billed savings after
native deferral and caching. Small catalogs can cost less than the lazy floor.
