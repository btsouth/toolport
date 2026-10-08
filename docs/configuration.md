# Configuration

Discovery defaults to Auto per identified client, based on native search and
late tool-list refresh support. Choose an override in Clients. See
[client capabilities and permission limits](client-discovery.md). Safety is a
separate global policy, enforced in every discovery mode. Client identity is
written automatically when you connect:

- `TOOLPORT_CLIENT_ID=<id>` - identifies this client for live profile resolution
  (written automatically when you Connect a client).
- `TOOLPORT_PROFILE=<name>` - initial profile scope for a scoped install. Unset =
  follow Default access (resolved live via `TOOLPORT_CLIENT_ID`).
- `TOOLPORT_DISCOVERY=lazy|full|grouped` - optional process default for a standalone
  gateway. Shared daemon sessions use their per-client choice or Auto.
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
- `TOOLPORT_GATEWAY_TOPOLOGY` - retired in 2.0. A `legacy` value is ignored and
  recorded once at gateway startup.
- `TOOLPORT_CODE_MODE=1` - force-enable code mode (`toolport_run_script`) even if Settings
  has it off. Code mode is **off by default**; opt in under Advanced in Settings. Each
  in-script tool call still respects profile scope and human approval; code mode is not a
  security boundary.

Every `TOOLPORT_*` name still accepts the pre-rename `CONDUIT_*` alias (for example
`CONDUIT_HTTP_TOKEN` continues to work). Prefer `TOOLPORT_*` in new configs.

**One gateway per host.** Client-spawned stdio gateways use a small adapter by
default; one host daemon owns the router and shares ordinary downstream
connections. The 2.0 upgrade drops a stored `gatewayTopology: "legacy"`, and the
legacy environment override is ignored, with a notice in the gateway log. Existing Shared HTTP client entries
and their authentication remain unchanged at startup. Connecting, resetting or
migrating a client explicitly writes the stdio adapter entry. Settings > Integrations
still provides the HTTP/OpenAPI bridge.
The adapter retains its in-process fallback when daemon spawning fails and its
private gateway fallback when the shared daemon is unhealthy. Ambiguous startup
failures still refuse to start a competing gateway.

**Discovery mode per HTTP client.** The stdio gateway resolves its client's
per-client override or Auto. The HTTP/OpenAPI bridge honors all three explicit
`clientDiscovery[<http-client-id>]` values for the bearer identity, including
Grouped. An absent entry uses Auto. Anonymous connections use the global mode.

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
own access set, which is Default access unless `TOOLPORT_PROFILE` sets another.
Changes apply the next time a client connects and don't restart any servers.

**Code mode limits.** Execution and validation run Boa in a separate
worker process. The parent enforces the 60-second wall-clock budget even during pure
JavaScript and permits at most four simultaneous runs. Each worker has a 512 MiB allocation budget: Linux uses `RLIMIT_AS`
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

**Upgrading from 1.x.** The first 2.0 start upgrades `registry.json` to schema v2.
Before changing it, Toolport saves the 1.x file next to it as
`registry.json.v1-<time>.bak`. User data from features 2.0 removed is copied to
`exports/` in the data directory, never overwriting a file there: personal agent
rules as `rules-<date>.md`, saved routines as `routines-<date>.json` (the original
`routines.json` stays), and agent permission rules as `agent-permissions-<date>.json`.
Client files are not edited. The upgrade sets one safety level (Strict if you blocked
destructive tools, quarantined drift or blocked injection, otherwise Ask) and turns
Code Mode off. Released 1.x builds do not check the schema version, so 2.0 keeps the
1.x safety toggles in step with the level: a 1.x process still running during the
upgrade enforces the same policy. To go back to 1.x, restore the `.bak` file.

### Downstream lifecycle

Toolport starts downstream servers when a client first uses them. A saved tool
catalog answers tool discovery without starting the server. If no catalog exists,
the first discovery request starts the visible servers to learn their tools.
The first prompt or resource list waits for the server's complete catalog.
Every dispatch to a stopped server waits up to 30 seconds for startup, including
resource reads, prompts, completions and tools approved after a long hold.
Uncertain failed operations are never replayed automatically.

A server stays warm for five minutes after its last completed use. Active calls,
suspended requests and resource subscriptions keep it running. After that idle
period, Toolport stops the connection and keeps its catalog; the next use starts
it again. The idle period is a constant, not a setting. An idle connection loses
process-local sessions.

A connection failure retries in the background with exponential backoff from
two seconds to five minutes, with jitter. A demand call can retry once 15 seconds
have passed since the last attempt. Reaching Ready resets the failure count.
Sign-in failures wait for credentials to change. Registry edits reuse servers
whose effective connection spec is unchanged, preserving their processes and
calls in flight. Changed servers get a new supervisor and start on their next
use, or immediately when they have active resource subscriptions. The new
connection restores those subscriptions. Policy-only edits do not restart
connections. Fresh catalogs pass the integrity gate before calls can use them.
