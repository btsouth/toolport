# Client discovery defaults

Auto is the default in Clients. It advertises the full tool list when the client
has documented native search or deferred loading. Otherwise it exposes search
and call tools. The capability table in `src-tauri/src/clients/discovery.rs`
travels with each detected client to both shells. Evidence was checked on
2026-10-07; unknown means unverified, not an assertion that a vendor lacks support.

A cold Full `tools/list` waits for the first catalogs within the client's budget
below, including startup and rooted servers in one deadline. It then answers
with whatever has loaded and still sends `notifications/tools/list_changed`
for later arrivals. Clients with verified refresh retain the two-second bound
from #1052. Warm lists with cached tools for the client's view answer immediately.
The longer budget applies only to Full mode; Lazy, Grouped, prompts and resources
retain the two-second catalog bound. This adds no setting or registry version.

Codex uses eight seconds, leaving two seconds below its documented default
`startup_timeout_sec` of ten seconds for protocol overhead. Cursor, API hosts and
unknown clients use five seconds: half that known startup window, allowing more
first catalogs while limiting latency when the host timeout is unknown. This is
a conservative fallback, not a verified timeout guarantee for those hosts or for
customized shorter client timeouts. API identifiers describe custom HTTP hosts,
not new file-based adapters.

| Client ID            | Native search / deferral | Tool-list refresh   | Auto | Cold Full budget | Evidence                                                                                                                                                                                                                                                                                 |
| -------------------- | ------------------------ | ------------------- | ---- | ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `claude-desktop`     | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `cursor`             | Yes                      | Unknown             | Full | 5 s              | [Cursor dynamic discovery](https://cursor.com/blog/dynamic-context-discovery); [MCP docs](https://cursor.com/docs/mcp) and [current changelog](https://cursor.com/changelog) do not document list-changed refresh                                                                        |
| `droid`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `crush`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `anythingllm`        | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `vscode`             | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `amp`                | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `windsurf`           | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `devin-cli`          | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `opencode`           | Unknown                  | Yes                 | Lazy | 5 s              | [Client conformance source evidence](client-conformance.md#verified-notification-evidence), checked 2026-10-09                                                                                                                                                                           |
| `kilo-code`          | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `grok`               | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `codex`              | Yes                      | No (source checked) | Full | 8 s              | [Codex config reference](https://developers.openai.com/codex/config-reference); [notification handler at 2351d9e1](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/rmcp-client/src/logging_client_handler.rs#L78-L80) only logs tool-list changes |
| `github-copilot-cli` | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `antigravity`        | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `claude-code`        | Yes                      | Yes                 | Full | 2 s              | [Claude Code: tool search and dynamic updates](https://code.claude.com/docs/en/mcp)                                                                                                                                                                                                      |
| `gemini-cli`         | Unknown                  | Yes                 | Lazy | 5 s              | [Client conformance source evidence](client-conformance.md#verified-notification-evidence), checked 2026-10-09                                                                                                                                                                           |
| `qwen-code`          | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `junie`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `cline`              | Unknown                  | Yes                 | Lazy | 5 s              | [Client conformance source evidence](client-conformance.md#verified-notification-evidence), checked 2026-10-09                                                                                                                                                                           |
| `roo-code`           | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `warp`               | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `amazon-q`           | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `kiro`               | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `kimi-code`          | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `zcode`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `zed`                | Unknown                  | Yes                 | Lazy | 5 s              | [Client conformance source evidence](client-conformance.md#verified-notification-evidence), checked 2026-10-09                                                                                                                                                                           |
| `lm-studio`          | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `jan`                | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `boltai`             | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `pi`                 | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `omp`                | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `goose`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `hermes`             | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `continue`           | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `witsy`              | Unknown                  | Unknown             | Lazy | 5 s              | [Client adapter notes](clients.md); search and notification behavior unverified                                                                                                                                                                                                          |
| `anthropic-api`      | Yes                      | Unknown             | Full | 5 s              | [Anthropic tool search](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool); host notification behavior unverified                                                                                                                                           |
| `openai-api`         | Yes                      | Unknown             | Full | 5 s              | [OpenAI tool search](https://developers.openai.com/api/docs/guides/tools-tool-search); host notification behavior unverified                                                                                                                                                             |

Verification: a bounded GitHub code search for `list_changed repo:openai/codex`
and a shallow source checkout found `on_tool_list_changed` in the RMCP handler.
At revision `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`, that callback logs only;
the elicitation service delegates notifications to it. This verifies receipt,
but not automatic catalog refresh. Cursor's MCP docs and current changelog were
searched for list-changed support without finding a documented refresh contract;
its refresh capability remains unknown. These are source/documentation checks,
not live vendor-client acceptance tests.

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
