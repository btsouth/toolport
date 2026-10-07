# Configuration

Lazy discovery, the destructive-tool block, and agent control are global settings,
stored in the registry and toggled in the app's Settings view, so they apply to every
client (lazy discovery is on by default). Per-client behavior is set via env vars on the
gateway entry, written for you when you connect a client:

- `TOOLPORT_CLIENT_ID=<id>` - identifies this client for live profile resolution
  (written automatically when you Connect a client).
- `TOOLPORT_PROFILE=<name>` - initial profile scope for a scoped install. Unset =
  follow the active profile (resolved live via `TOOLPORT_CLIENT_ID`).
- `TOOLPORT_DISCOVERY=lazy|full|grouped` - optional per-client override of the global
  discovery setting. Rarely needed; the gateway reads the registry default otherwise.
- `TOOLPORT_REGISTRY=<path>` - override the registry file location. Defaults to a
  stable per-user path so packaged and unpackaged clients agree.
- `TOOLPORT_DATA_DIR=<path>` - override the full Toolport data directory. A desktop
  launch with this override keeps its startup migration within that instance:
  it does not rewrite client configs or agent hooks in the user's normal home.
- `TOOLPORT_RESULT_BUDGET=<bytes>` - cap oversized tool results at this many bytes
  (0 disables it). Optional; default budget applies when unset.
- `TOOLPORT_HTTP=<port>` (with optional `TOOLPORT_HTTP_HOST`, default `127.0.0.1`,
  and `TOOLPORT_HTTP_TOKEN` for the required bearer token) - run the gateway in
  HTTP/OpenAPI mode instead of stdio, for Open WebUI and other OpenAPI clients (see [Open WebUI](openwebui.md)). The in-app Settings -> Integrations toggle sets these for you, and the
  gateway refuses to bind without a token or registered HTTP client. For isolated
  local development only, `--insecure-loopback` explicitly permits an unauthenticated
  loopback listener; it never permits an open non-loopback bind.
- `TOOLPORT_METRICS=1` - opt-in Prometheus `GET /metrics` on the HTTP surface.
- `TOOLPORT_DEBUG=1` - per-request gateway trace logging.
- `TOOLPORT_GATEWAY_TOPOLOGY=daemon|legacy` - override the local stdio topology
  for one client launch. `legacy` is the immediate rollback setting.
- `TOOLPORT_CODE_MODE=1` - force-enable code mode (`toolport_run_script`) even if Settings
  has it off. Code mode is **on by default** (Settings kill switch turns it off). Each
  in-script tool call still respects profile scope and human approval; code mode is not a
  security boundary.

Every `TOOLPORT_*` name still accepts the pre-rename `CONDUIT_*` alias (for example
`CONDUIT_HTTP_TOKEN` continues to work). Prefer `TOOLPORT_*` in new configs.

**Destructive-call confirmation.** New installs enable `confirmDestructive` by
default. When a server marks a tool with `destructiveHint: true`, Toolport
returns an error result containing the arguments and a one-use, client-scoped
token. The agent must call `toolport_confirm` within 60 seconds to replay those
exact arguments. This is agent confirmation, not a human approval prompt.
Both desktop shells expose it in Settings. Tools without the destructive
annotation are not guaranteed to be caught.

Existing registries keep their previous confirmation setting. The bool was
serialized on every save, with no record of who chose it; neither a saved false
nor an absent field proves the user left the default untouched. Older registries
therefore deserialize with their old false default and an unacknowledged upgrade
notice. The app offers to turn confirmation on in Settings. Dismissing the notice
or choosing the confirmation setting persists `destructiveConfirmationNoticeSeen`
so the offer does not recur, including when switching desktop shells. New installs
start with this notice acknowledged. No 2.0 safety setting is introduced.

**Headless and non-interactive calls.** Agent confirmation does not wait for the
app or a prompt: it immediately returns the preview/error above. A headless agent
can replay the token; a client that cannot do so receives the error and the tool
never runs. Code-mode scripts cannot replay a token, so a destructive call fails
immediately with instructions to call it directly or enable human approval.

The independent **Require human approval** setting takes precedence over agent
confirmation for gated tools. Legacy clients and scripts use the authenticated
app approval broker. If no broker is published, they immediately fail closed with
an `unreachable` decision and an error asking whether the Toolport app is running.
Stale or unresponsive brokers fail closed; authentication and decision reads
use timeouts. A prompt that reaches the app auto-denies after 120 seconds.
Modern clients use MCP elicitation and receive a capability error when they cannot show that approval request.
Team-forced human approval or destructive-tool blocking still takes precedence;
changing the member's confirmation setting does not release those team locks.

**One gateway per host.** Client-spawned stdio gateways use a small adapter by
default; one host daemon owns the router and shares ordinary downstream
connections. Existing registry files without `gatewayTopology` select this
topology. Set `"gatewayTopology": "legacy"` in `registry.json` and restart
connected AI clients to keep separate in-process gateways. For one client,
`TOOLPORT_GATEWAY_TOPOLOGY=legacy` overrides the registry without editing it.
The explicit legacy choices remain available for a release cycle. The adapter
uses an in-process gateway only when the operating system proves daemon launch
failed. An ambiguous startup failure or a failure after the session opens
returns an error rather than starting or replaying against a second gateway.

**Discovery mode per HTTP client.** The stdio gateway resolves one discovery mode for
the client that spawned it. The headless HTTP/OpenAPI bridge serves several clients at
once, so it also honors `clientDiscovery[<http-client-id>]` for the client its bearer
token resolves to. Set `"full"` for a client that already has native tool search (Claude
Code, Codex) and `"lazy"` for one that does not, in the same bridge process. Only `full`
and `lazy` are per-client: `grouped` stays process-global, and a client without an entry
inherits the process mode.

**Server instructions per profile.** The gateway sends a block of instructions to every
client when it connects. Some clients, including Claude Code, add each server's
instructions to the model's context, so connecting one gateway several times (one entry
per profile) repeats the same text. Set `instructions` on a profile in `registry.json`
to replace what connections scoped to it receive, or set it to `""` to send none. This
keeps the built-in text on `infrastructure` and sends nothing for `postgres`:

```json
{
  "profiles": [
    { "id": "infrastructure", "name": "Infrastructure", "enabledServerIds": ["proxmox"] },
    {
      "id": "postgres",
      "name": "Postgres",
      "enabledServerIds": ["postgres"],
      "instructions": ""
    }
  ]
}
```

A top-level `gatewayInstructions` works the same way for every profile that doesn't set
its own. Leave both out to keep the built-in text everywhere.

The profile is the one the connection is scoped to: a registered HTTP client's
`profile`, or the stdio client's profile. A connection without one uses the gateway's
own profile, which is the active profile unless `TOOLPORT_PROFILE` sets another.
Changes apply the next time a client connects and don't restart any servers.

**Code mode limits.** Execution, validation, and saved routines run Boa in a separate
worker process. The parent enforces the 60-second wall-clock budget even during pure
JavaScript and permits at most four simultaneous runs. Saved routines can set lower
execution limits. Each worker has a 512 MiB allocation budget: Linux uses `RLIMIT_AS`
before exec, Windows assigns the suspended child to a memory-limited Job Object, and
macOS uses a worker-only Rust allocator cap because Darwin's `RLIMIT_AS` is advisory.
The macOS cap covers Boa's Rust heap, not the process's total resident memory. Allocation
refusal terminates the worker and returns a script error; the gateway stays available.
On all supported platforms, a worker allocation guard uses a dedicated exit code even
for fallible buffer allocations. Memory-budget audit attribution comes from the
worker's exit status, not error text thrown by JavaScript.

Source is capped at 256 KiB, `data` or `input` at 4 MiB of JSON, and `inputSchema` at
64 KiB on both stdio and HTTP. HTTP's 4 MiB whole-request cap still applies. Worker
messages, including host results and the final aggregate, are capped at 16 MiB per
frame. Oversized messages fail explicitly. Host calls still run through the parent's
scope, approval, content-defense, and rate-limit checks. On failure, completed calls
and the last checkpoint remain available; calls already in flight are cancelled where
supported and must not be automatically retried.

**Semantic search (optional).** Lazy discovery ranks tools lexically by default. Point it
at any `/v1/embeddings` endpoint (LM Studio, Ollama, or a cloud provider) to blend in
embedding similarity for paraphrased queries: `TOOLPORT_SEMANTIC=on`,
`TOOLPORT_EMBED_ENDPOINT`, `TOOLPORT_EMBED_MODEL`, plus optional `TOOLPORT_EMBED_KEY`
(endpoint auth) and `TOOLPORT_EMBED_BLEND`.

**Multiple accounts for the same service.** Credentials belong to a server, not a
profile. To use, say, a work and a personal GitHub, add GitHub twice as two
servers ("GitHub (work)", "GitHub (personal)"), authenticate each with its own
account, and enable one in each profile. A client scoped to the work profile
(`TOOLPORT_PROFILE`) then only ever sees the work account. Tool names are
namespaced per server, so the two never collide even in the same profile.
