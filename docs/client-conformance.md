# Client conformance

Run the fixture suite on a Linux build host or CI:

```sh
npm run test:client-conformance
```

It builds with `test-support`, replays captured startup/health-command initialize params
through the real stdio adapter and daemon, and exercises Auto discovery for every
adapter in the generated client inventory. Cold Lazy search waits for the first
visible catalog within the client budget, including when no server filter is
provided. Inventory drift fails the test. There
are no real client executables, model calls, credentials or platform keychains in
the offline suite. Node is required, as it is for the existing gateway smoke.

The recorded profiles are in
[`test/client-conformance/profiles.json`](../test/client-conformance/profiles.json).
Their date and clientInfo versions are part of the evidence. An uncaptured client
gets a clearly labeled synthetic protocol baseline, not a fabricated handshake.
Gemini and Qwen's health commands use `mcp-test-client`; their advertised
capabilities do not establish the model-session client's capabilities.

Profiles with a captured `initialize` are replayed: Claude Code, Codex, Cursor,
OpenCode, Gemini CLI, Qwen Code, Kilo Code, Hermes, Factory Droid, Grok Build,
Continue, Kimi Code, Oh My Pi, Pi, Junie and the 2026-10-18 captures. Clients
captured only through sessionless `server/discover` (Copilot CLI, Crush, Goose,
Devin CLI, and Claude Code since 2.1.309) keep their record in `discover` and get
the synthetic baseline. Health commands may only initialize or ping, so their
actual follow-up method sequence is replayed separately from synthetic calls.
Cursor's health command opened two sessions; the profile represents one session.
Codex has separate unauthenticated CLI startup and app-server status captures. A two-client check also
uses the same request ID in both sessions and verifies isolated responses while
sharing one daemon and downstream connection.

For each captured profile the suite checks typed results/output schemas,
notifications, resource reads, prompt retrieval, concurrent work during a long
call, cancellation, large result budgets, oversized downstream frames, downstream
crash/reconnect and daemon restart without replay. Notification delivery does not
prove the real client re-lists. Only profiles with source or documentation evidence
re-list in that part of the replay. Synthetic legacy-version variants and modern
sessionless discovery cover protocol compatibility separately.

`client_conformance.rs` also tests mid-session HTTP OAuth expiry, one refresh and
revoked refresh credentials using a disposable encrypted vault and loopback
provider. That is downstream transport evidence, not real client OAuth UX.
`stale_daemon_reaper.rs`, included in the same command, checks version handover
with real processes and a scripted old-version daemon. It is not acceptance of
an actual pair of released gateway artifacts. Both tests also run in the normal
headless CI integration-test command.

## Structured tool results

Gateway discovery omits downstream `outputSchema` declarations because result
shaping can replace `structuredContent` with a preview and a fetch cursor. The
input schema is preserved, small structured results still pass through, and
retained structured results remain available through `toolport_fetch_result`
projections. This applies to Full, Lazy and Grouped discovery, including exact
searches and pinned tools. Output schemas stay omitted when shaping is disabled,
so changing a server's result budget does not invalidate a client's cached tool
contract. The original downstream catalog is unchanged.

The fixture replay covers small structured results, oversized structured results
and cursor retrieval through the real adapter and daemon. It does not claim
acceptance by an authenticated Claude Code or other client application.

## Tool name budgets

Client profiles in `clients.rs` declare combined name budgets. Cursor has a
60-character combined budget based on [Cursor's support report](https://forum.cursor.com/t/google-gws-cli-tool-names-too-long/153918/4).
Subtract the registered server key (`toolport`, eight characters) and the profile's
separator overhead (zero for Cursor) for a 52-character tool alias. Other profiles keep the 64-character default, and
names already within their budget keep their spelling. A bounded prefix and
identity digest keep longer names deterministic, including collisions.

Grok Build 1.0.67 qualifies tools as `<server>__<tool>` and drops a tool whose
own name contains `__` or starts with `_` (checked with `grok mcp doctor`; names
of 70 characters and hyphens pass). Its profile lists every `__` as `-`, so
`mock__add` becomes `mock-add`. Sanitized names never contain `-`, so the
spelling stays reversible, and search, browse tools and `toolport_call_tool`
use the same names.

Aliases route back to the same server and original tool. Access, overrides,
integrity pins, approvals and quarantine use that canonical policy identity across
client budgets. Fixture tests cover Cursor, Grok and Claude views; they do not establish
acceptance by a running Cursor application.

## Opt-in real client tracing

Build the gateway and mock with `test-support`, then supply an absolute executable
and a new evidence directory:

```sh
npm run test:client-conformance:live -- opencode /usr/local/bin/opencode /tmp/toolport-opencode-evidence
npm run test:client-conformance:live -- codex /usr/bin/codex /tmp/toolport-codex-evidence
```

The driver's `clients` table lists every supported client with its disposable
config and command. Add `--auto` to keep the client's Auto discovery mode (the
default is Full, so proxied tools are listed) and `--probe-list-changed` to send
one `notifications/tools/list_changed` 500 ms after the first `tools/list` reply.
Each run creates a fresh HOME, XDG dirs, working directory and Toolport
data/registry. It passes only OS/executable locators, writes only its disposable
client config, and never copies auth from a real home.

Some clients connect to MCP only inside a session. The `acp` driver sends ACP
`initialize` and `session/new` and never a prompt, unless the client lists tools
lazily (Goose, Kimi Code); then it sends one prompt. Clients that need a provider
before they start a session (Crush, Goose, Continue, Kimi Code, Copilot CLI in
offline BYOK mode, Junie) are pointed at a loopback OpenAI-compatible mock that
answers "ok". No request leaves the machine and no model service is called.
`model.jsonl` records only request paths and the tool names offered to the model.
Pi 1.4.0 has a built-in MCP extension; `TOOLPORT_LIVE_PI_PACKAGE` can instead load
an existing local `pi-mcp-adapter` package path.

On macOS, Claude Code reads the login Keychain regardless of HOME, so a disposable
HOME alone does not isolate its authentication. Use an isolated OS account/session with no saved Claude credentials for that probe.
Gemini's disposable config disables folder trust so its
health command can connect. Other approval/auth requirements are reported as
uncaptured, not bypassed. Codex uses app-server MCP status, without a model turn.
Copilot's `mcp list` inspects config without opening a connection, so the driver
runs an offline BYOK prompt against the loopback mock instead.

Run CLIs that touch session services in an isolated desktop session. Use an
absolute executable that does not depend on a user GUI launcher. Builds and client
installs belong on a Linux build host, in a task-local prefix. No global installation or real
client config mutation is needed. The live driver never installs a client.

`traffic.jsonl` contains the offered handshake (`initialize` params, or the
`io.modelcontextprotocol/*` `_meta` of `server/discover` and
`subscriptions/listen`), message method/ID metadata and error codes, not
environment values, auth headers or tool argument/result bodies.
`gateway-stderr.log` holds the gateway's own stderr for that run.
`summary.json` reports whether a handshake actually occurred. `status.txt` is
bounded CLI health output from the disposable environment; the macOS Keychain
caveat above applies.
Cleanup targets only spawned children and daemons published in that fresh data
folder. All requests and live commands have deadlines.

## Profile fields

`id`, `evidence`, `initialize`, `initialMethods`, `listChanged`, `source`,
`observedSessionKind` and `variants` keep their original meaning. `initialize` and
`initialMethods` are the latest captured legacy handshake and one session's
startup methods; the offline suite replays them. Claude Code's `initialize` is
from 2.1.286, its last version that sent one.

| Field           | Meaning                                                                                                                                                                                                                                         |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `clientVersion` | Client version the newest evidence was recorded against, or `null` (branch-pinned source review, or no version known).                                                                                                                          |
| `recordedAt`    | Date of the newest evidence for this profile. The file-level `recordedAt` is the original 2026-10-09 round.                                                                                                                                     |
| `evidenceLevel` | `live capture`, `partial capture` (handshake seen, client stopped before listing tools), `source review` or `documentation only`.                                                                                                               |
| `handshake`     | What the newest capture opened with: `initialize` or sessionless `server/discover` (protocol 2026-07-28).                                                                                                                                       |
| `discover`      | The captured `server/discover` request metadata for sessionless clients.                                                                                                                                                                        |
| `captures`      | Each live run of this round: kind, discovery mode, protocol version, clientInfo, capabilities, client methods, server-to-client requests, `subscriptions` filters, an example `modelToolName` and any `listChangedObserved`.                    |
| `captureNote`   | Why a partial capture stopped.                                                                                                                                                                                                                  |
| `uncaptured`    | A capture attempt that produced no MCP traffic, with the version and reason (usually a required login).                                                                                                                                         |
| `sourceReview`  | Repository, version and commit of the pinned source review.                                                                                                                                                                                     |
| `listChanged`   | Adds `source` (pinned permalink) and `observed`: `relisted` and `triggers` (`gateway` for Toolport's own notification, `injected` for the probe). Only a long-lived session's "no" is recorded; a health command exits before it could re-list. |
| `features`      | `roots`, `sampling`, `elicitation`, `resources` and `prompts`, each with a `status` and optional `source`, `methods`, `observed` and `detail`.                                                                                                  |

Feature `status` values: `implemented` (pinned source has a handler), `declared` (offered
in a captured handshake, handler not reviewed), `not declared` (absent from the
captured handshake of that version), `used` (source or captured traffic calls the
methods), `not observed` (captured traffic did not call them; clients may list
lazily), `not found in the inspected source` and `unverified`. "Not found" and
"not observed" are not "unsupported". A handshake capability also records
`handshake: declared | not declared` beside a source status.

## Verified notification evidence

Checked 2026-10-09 unless a row gives its own date. Installed versions and current
source may differ; these facts establish a supported behavior, not a latency guarantee for every release.

| Client           | Evidence                                                                                                                                                  | Refresh                                                                           |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Claude Code      | [Vendor MCP docs](https://code.claude.com/docs/en/mcp)                                                                                                    | Tools-list notifications supported                                                |
| OpenCode         | [Client handler](https://github.com/anomalyco/opencode/blob/388406238bd5ca15564a762840a2362c3a45bd9c/packages/opencode/src/mcp/index.ts)                  | Direct tools re-list handler; onclose drops the catalog                           |
| Gemini CLI       | [Client handlers](https://github.com/google-gemini/gemini-cli/blob/2ce1a6963e9e53a04afaf76111e4527cfa7c5dd7/packages/core/src/tools/mcp-client.ts)        | Tools, resources and prompts refresh                                              |
| Cline            | [Pinned client](https://github.com/cline/cline/blob/75111f347f275022abea2b2d784901920d8618be/apps/vscode/src/services/mcp/McpHub.ts)                      | 300 ms debounce, 2 s max deferral, bounded refresh retry                          |
| Zed              | [Vendor MCP docs](https://zed.dev/docs/ai/mcp)                                                                                                            | Automatically reloads tools                                                       |
| Codex            | [Pinned catalog](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs) | Generic notification refresh not established; cached tools                        |
| Cursor           | Isolated CLI health traffic                                                                                                                               | Tools/resources/prompts listed; notification refresh unverified                   |
| DeepSeek Harness | dsh 0.2.0-rc.2 Web startup against toolport-gateway 2.2.0, checked 2026-10-18                                                                             | `server/discover`, `subscriptions/listen`, re-lists tools after each notification |
| Reasonix         | Reasonix 2.44.0 Web startup against toolport-gateway 2.2.0, checked 2026-10-18                                                                            | `server/discover`, no `subscriptions/listen`; tools stay as first listed          |
| Codewhale        | Codewhale 0.10.1 `mcp tools` against toolport-gateway 2.2.0, checked 2026-10-18                                                                           | No tools/list_changed handler in source; `/mcp reload` re-reads                   |
| OpenClaw         | OpenClaw 2026.10.4 `mcp add` probe against toolport-gateway 2.2.0, checked 2026-10-18                                                                     | Refreshes its catalog on tools/list_changed (source)                              |
| OpenHands        | OpenHands CLI 1.16.0 `--headless` startup against toolport-gateway 2.2.0, checked 2026-10-18                                                              | `server/discover`, no `subscriptions/listen`; tools stay as first listed          |
| nanobot          | nanobot-ai 0.3.5 agent startup against toolport-gateway 2.2.0, checked 2026-10-18                                                                         | `initialize` 2025-11-25, tools, resources and prompts listed; no refresh handler  |
| Copilot CLI      | Copilot CLI 1.0.103 offline BYOK run against a loopback mock model, checked 2026-10-22                                                                    | `server/discover`, `subscriptions/listen`; re-listed on gateway notification      |
| Pi               | Pi 1.4.0 RPC startup and built-in MCP source, checked 2026-10-22                                                                                          | Re-lists tools on each notification (source and capture)                          |
| Oh My Pi         | omp 18.12.6 RPC and ACP startup and pinned source, checked 2026-10-22                                                                                     | Re-lists tools on each notification (source and capture)                          |
| Kilo Code        | Kilo 7.8.15 health command and pinned source, checked 2026-10-22                                                                                          | Re-lists immediately, same handler as OpenCode (source and capture)               |
| Hermes           | Hermes 0.22.4 `mcp test` and pinned source, checked 2026-10-22                                                                                            | Background tools re-list on each notification (source)                            |
| Goose            | Goose 1.55.0 ACP session against a loopback mock model, checked 2026-10-22                                                                                | Handler refreshes on next request, but no `subscriptions/listen`, so no channel   |
| Crush            | Crush 0.98.1 run against a loopback mock model, checked 2026-10-22                                                                                        | `subscriptions/listen`; source refreshes tools, prompts and resources             |
| Kimi Code        | Kimi Code 2.3.0 ACP session against a loopback mock model, checked 2026-10-22                                                                             | Lists tools once; no re-list in a live session (source and capture)               |
| Continue CLI     | `cn` 1.5.47 prompt against a loopback mock model, checked 2026-10-22                                                                                      | Lists tools and prompts once; no notification handler (source)                    |

Refresh support does not establish native tool search. OpenCode, Gemini, Cline,
Zed and DeepSeek Harness retain Auto Lazy and their conservative five-second cold Full budget.
Authenticated tool selection, API schema conversion, GUI behavior, Windows/macOS
client runtime behavior, reconnect timing and maximum tool counts still require
client/version-specific acceptance. A replay pass is not a claim of reliability
across those unmeasured surfaces.
