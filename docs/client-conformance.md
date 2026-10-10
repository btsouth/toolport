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

The captured profiles cover Claude Code, Codex, Cursor, OpenCode, Gemini CLI,
Qwen Code, Kilo Code and Hermes. Health commands may only initialize or ping, so their
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

## Tool name budgets

Client profiles in `clients.rs` declare combined name budgets. Cursor has a
60-character combined budget based on [Cursor's support report](https://forum.cursor.com/t/google-gws-cli-tool-names-too-long/153918/4).
Subtract the registered server key (`toolport`, eight characters) and the profile's
separator overhead (zero for Cursor) for a 52-character tool alias. Other profiles keep the 64-character default, and
names already within their budget keep their spelling. A bounded prefix and
identity digest keep longer names deterministic, including collisions.

Aliases route back to the same server and original tool. Access, overrides,
integrity pins, approvals and quarantine use that canonical policy identity across
client budgets. Fixture tests cover Cursor and Claude views; they do not establish
acceptance by a running Cursor application.

## Opt-in real client tracing

Build the gateway and mock with `test-support`, then supply an absolute executable
and a new evidence directory:

```sh
npm run test:client-conformance:live -- opencode /usr/local/bin/opencode /tmp/toolport-opencode-evidence
npm run test:client-conformance:live -- codex /usr/bin/codex /tmp/toolport-codex-evidence
```

The driver supports the eight captured clients and Copilot CLI. Each run creates a
fresh HOME, XDG dirs, working directory and Toolport data/registry. It passes only
OS/executable locators, writes only its disposable client config, and never copies
auth from a real home. On macOS, Claude Code reads the login Keychain regardless
of HOME, so a disposable HOME alone does not isolate its authentication. Use an
isolated OS account/session with no saved Claude credentials for that probe.
Gemini's disposable config disables folder trust so its
health command can connect. Other approval/auth requirements are reported as
uncaptured, not bypassed. Codex uses app-server MCP status, without a model turn.
Copilot's `mcp list` may inspect config without opening a connection; that produces
no handshake and a failing live result.

Run CLIs that touch session services in an isolated desktop session. Use an
absolute executable that does not depend on a user GUI launcher. Builds and client
installs belong on a Linux build host, in a task-local prefix. No global installation or real
client config mutation is needed. The live driver never installs a client.

`traffic.jsonl` contains the offered handshake and message method/ID metadata,
not environment values, auth headers or tool argument/result bodies.
`summary.json` reports whether a handshake actually occurred. `status.txt` is
bounded CLI health output from the disposable environment; the macOS Keychain
caveat above applies.
Cleanup targets only spawned children and daemons published in that fresh data
folder. All requests and live commands have deadlines.

## Verified notification evidence

Checked 2026-10-09. Installed versions and current source may differ; these facts
establish a supported behavior, not a latency guarantee for every release.

| Client      | Evidence                                                                                                                                                  | Refresh                                                         |
| ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| Claude Code | [Vendor MCP docs](https://code.claude.com/docs/en/mcp)                                                                                                    | Tools-list notifications supported                              |
| OpenCode    | [Client handler](https://github.com/anomalyco/opencode/blob/388406238bd5ca15564a762840a2362c3a45bd9c/packages/opencode/src/mcp/index.ts)                  | Direct tools re-list handler; onclose drops the catalog         |
| Gemini CLI  | [Client handlers](https://github.com/google-gemini/gemini-cli/blob/2ce1a6963e9e53a04afaf76111e4527cfa7c5dd7/packages/core/src/tools/mcp-client.ts)        | Tools, resources and prompts refresh                            |
| Cline       | [Pinned client](https://github.com/cline/cline/blob/75111f347f275022abea2b2d784901920d8618be/apps/vscode/src/services/mcp/McpHub.ts)                      | 300 ms debounce, 2 s max deferral, bounded refresh retry        |
| Zed         | [Vendor MCP docs](https://zed.dev/docs/ai/mcp)                                                                                                            | Automatically reloads tools                                     |
| Codex       | [Pinned catalog](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs) | Generic notification refresh not established; cached tools      |
| Cursor      | Isolated CLI health traffic                                                                                                                               | Tools/resources/prompts listed; notification refresh unverified |

Refresh support does not establish native tool search. OpenCode, Gemini, Cline
and Zed retain Auto Lazy and their conservative five-second cold Full budget.
Authenticated tool selection, API schema conversion, GUI behavior, Windows/macOS
client runtime behavior, reconnect timing and maximum tool counts still require
client/version-specific acceptance. A replay pass is not a claim of reliability
across those unmeasured surfaces.
