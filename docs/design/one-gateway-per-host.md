# Design: One heavy gateway per host

Status: PROPOSED for SBS-551. This document is the design pass requested before an
implementation estimate. It does not change the current gateway topology.

## Goal

Stop every stdio AI-client session from starting its own router and copy of every
downstream MCP server. A machine with many agent sessions should normally have one
Toolport host daemon and one instance of each compatible downstream launch, plus a tiny
stdio adapter for each client connection that requires stdio.

"One gateway per host" therefore means **one heavy gateway runtime per compatible
Toolport version and data directory**, not literally one operating-system process. A
stdio client owns its child process's stdin and stdout, so a lightweight adapter process
at that boundary is both necessary and desirable. The adapter must not load the registry,
build a router, or spawn downstream servers.

## Current architecture

The existing code already proves most of the shared-runtime model, but the pieces are
owned at the wrong boundaries:

- Normal integration entries execute `toolport-gateway` directly and pass
  `TOOLPORT_CLIENT_ID` plus an optional bootstrap profile (`clients.rs`,
  `gateway_entry`). Every client launch therefore builds a complete gateway.
- Shared HTTP mode builds the union of registered clients' servers and serves multiple
  authenticated, scoped callers through one `GatewayState` (`toolport-gateway.rs`,
  `resolve_http_caller`, `serve_http`). This proves that one router can safely filter
  catalogs and calls per caller.
- Clients without a native remote-MCP configuration use `npx -y mcp-remote` in today's
  opt-in Shared HTTP mode (`clients.rs`, `client_uses_mcp_remote_bridge`). Making that
  optional path the default would trade the Toolport process explosion for per-session
  Node bridge processes and a third-party runtime dependency.
- The desktop app owns one fixed-port `toolport-gateway --http` child and kills it on app
  exit (`desktop.rs`, `start_http_bridge_at`). It fails when the port is occupied rather
  than discovering an existing compatible process, so it is not a host daemon.
- `GatewayState` mixes host state (registry, router, catalog, rebuild lock) with
  connection state (stdout, profile, root, upstream capabilities and server-request
  routing). Process globals also encode single-client assumptions: discovery/code mode,
  stdio presence and era, progress dispatch, PII maps, result stash, and pending modern
  HITL approvals.
- Streamable HTTP already has useful session primitives: authenticated owner and scope,
  client capabilities, outbound queues, upstream request correlation, subscriptions,
  expiry, and cleanup.

The measured failure mode is therefore structural, not a missing lock: N client sessions
currently create N routers and approximately N copies of every enabled stdio server.
Cross-process file locks protect individual shared files, but cannot remove that duplicated
work or make in-memory state coherent.

## Decision

Use the existing gateway binary in two explicit roles:

1. **Host daemon (`--daemon`)**: owns registry watching, the router pool, downstream
   transports, caches, policy, audit, and all session tables. It listens only on an
   authenticated loopback endpoint selected during rendezvous.
2. **Stdio adapter (`--stdio-adapter`, eventually the default no-argument role)**: owns
   exactly one client's stdin/stdout. It discovers or starts the compatible daemon,
   establishes one daemon session, and translates JSON-RPC in both directions. It contains
   no router and spawns no downstream MCP servers.

Native remote-MCP clients may connect directly to the same daemon protocol once desktop
bridge convergence lands. Stdio-only clients use the Toolport adapter, never
`mcp-remote`. The existing standalone stdio gateway remains available during rollout as
an escape hatch.

Use authenticated loopback TCP and the gateway's Streamable HTTP semantics for the first
implementation. This reuses code that already handles concurrent requests, legacy SSE,
modern requests, server-to-client RPC, cancellation, session expiry, and scoped callers.
Named pipes / Unix-domain sockets would reduce the network surface, but would require a
second framing and transport implementation before topology risk is retired. The endpoint
is loopback-only, requires a random bearer secret, rejects foreign browser origins, and is
not exposed as the user-configured HTTP endpoint.

## Runtime boundaries

### Host state

Owned once by the daemon:

- loaded registry and registry watcher;
- downstream pool and reconnect/circuit-breaker state;
- catalog snapshots and persistent cache writes;
- router rebuild coordination;
- integrity/quarantine and shared rate-limit state;
- audit, metrics, savings, and activity emission;
- daemon session registry, progress routes, resource ownership, and subscription fanout.

### Session state

Owned once per adapter/native connection and removed on disconnect or TTL:

- opaque daemon session id;
- stable client principal (`client:{id}`) and audit label;
- effective profile/server scope, recomputed from the registry;
- MCP protocol version and declared capabilities;
- project roots and the resolved `${ROOT}` context;
- upstream request correlation and outbound queue;
- cancellation registry, modern subscription filters, and resource subscriptions;
- search-thrash guard and pending destructive confirmations;
- connection-local notification eligibility and stdio/HTTP transport kind.

PII maps and shaped-result cursors remain keyed by stable client principal to preserve the
current policy across reconnects. Ending one of two concurrent sessions with the same
principal may clear their shared PII map; the existing deliberate over-clear policy remains
safer than retaining PII. Entries still need explicit TTL/cap cleanup because the daemon is
long-lived.

### Request context

Protocol version, modern capabilities, active session, caller identity, effective scope,
root context, and notification target must travel as an explicit request context. The
current thread-local/process-global shortcuts are not safe once requests from different
clients share workers. Temporary thread-local adapters are acceptable inside a synchronous
call stack only if they are populated from, and cannot outlive, the explicit context.

Downstream server-initiated requests must be correlated to the client request that caused
them. The daemon records `downstream request id -> upstream session id` before forwarding.
If no live originating session exists, Toolport refuses the request clearly; it must never
guess a recipient or broadcast elicitation/sampling/roots requests.

## Downstream pooling and `${ROOT}`

Most downstream connections can be shared across all profiles because authorization is
enforced when catalogs and calls are exposed. Profile membership does not by itself change
how a server is launched.

`${ROOT}` is the important exception. Two sessions can resolve the same server's cwd to
different projects, and one child process cannot have two working directories. The daemon
therefore pools downstream launches by a **launch key**:

`server id + launch-affecting config fingerprint + resolved root context`

The fingerprint includes transport, command/args, cwd, URL/auth mode, environment and any
other value that changes the child or connection. Secrets contribute a keyed digest or
generation marker, never plaintext. A registry or secret change retires the old launch key
after its in-flight calls finish and creates the new one. Servers without root-dependent
configuration converge to one instance; root-dependent servers converge per distinct root.
This is the honest boundary for sharing and avoids silently running a tool in the wrong
project.

## Rendezvous and startup

Each daemon compatibility domain has a rendezvous key derived from:

- canonical Toolport data-directory path;
- daemon protocol generation;
- exact gateway version/build identity.

Exact-version isolation makes upgrades safe: existing sessions keep using their old daemon,
while a new adapter starts or joins the new daemon. Old daemons exit after becoming idle.
No new binary talks to an older daemon merely because both understand HTTP.

The data directory contains a version-keyed descriptor with only endpoint, bearer token,
PID, protocol/build identity, and creation time. It is written atomically and inherits the
data directory's user-only permissions. Startup is:

1. Read the descriptor and perform an authenticated handshake that verifies the complete
   compatibility identity. Never trust PID existence or an open port alone.
2. If unavailable, acquire a version-keyed cross-process election lock using the existing
   registry file-lock primitive.
3. Recheck after acquiring the lock. Exactly one contender spawns `--daemon`; all others
   wait for its bounded readiness handshake.
4. The daemon binds an ephemeral loopback port, atomically publishes the descriptor, and
   keeps running independently of the adapter and desktop app.
5. A stale descriptor may be replaced only while holding the election lock and only after
   its authenticated endpoint cannot be reached. Never kill a process solely because a PID
   or port appears in a stale file.

Client identity is asserted in the authenticated session-open request and resolved against
the daemon's registry; the adapter does not supply an authoritative server allowlist. The
bearer token protects against browser/local-port access. This follows Toolport's existing
same-user data-directory trust boundary and does not claim to isolate hostile processes
running as the same OS user.

## Lifecycle and failure semantics

- Every connected adapter/native client holds a session lease. The desktop app may later
  hold a service lease for the user-facing shared HTTP endpoint.
- The daemon exits after a conservative idle grace period only when it has no live leases,
  in-flight calls, pending approvals/server requests, or active subscriptions. Start with
  five minutes and make the constant testable; this is an operational default, not a user
  setting in the first release.
- Adapter EOF closes its daemon session immediately. Daemon TTL reaping handles crashed
  adapters and performs the same subscription, PII, confirmation, and pending-request
  cleanup as an explicit close.
- If the daemon dies, adapters stay attached to their clients, fail the affected in-flight
  request with a clear Toolport error, and perform rendezvous again before the next request.
  They never replay an ambiguous tool call automatically. Initialize, ping, and list reads
  may be retried only before the daemon accepted them.
- One daemon crash affects more clients than one legacy gateway crash. Bounded startup,
  panic containment at request workers, downstream circuit breakers, and no automatic
  write replay are required mitigations. The legacy topology flag is the rollback path.
- The app updater/reaper must understand daemon compatibility domains. It may retire an old
  idle daemon, but must not kill a live shared daemon merely because the app is exiting.

## Desktop HTTP convergence

Do not combine daemon rollout with changing the public Shared HTTP contract.

Initially, the internal daemon endpoint is private rendezvous infrastructure and the
existing opt-in desktop HTTP bridge remains unchanged. After stdio sharing is stable, the
desktop app can acquire a service lease and publish the configured port/token through the
same host runtime. At that point `start_http_bridge_at` discovers/adopts the daemon and app
exit releases its lease instead of killing the process.

This sequencing keeps fixed-port behavior, registered client tokens, LAN exposure options,
and user expectations out of the first migration. It also avoids accidentally exposing the
internal host bearer as a copyable integration credential.

## Implementation sequence

### Phase 0: measurement and invariants

- Add a diagnostic snapshot for gateway role, daemon compatibility key, session count,
  downstream launch count, and launch keys without secret material.
- Add an integration fixture that starts multiple logical clients and measures peak daemon,
  adapter, and downstream child counts.
- Record current cold-start, first-list, first-call, steady memory, and process counts as a
  baseline. The ticket's real multi-session machine is the acceptance scenario.

### Phase 1: make session ownership explicit without changing topology

- Introduce `HostState`, `SessionState`, and `RequestContext` types.
- Move discovery/code mode, root, capabilities, search guard, confirm guard, cancellation,
  progress target, and notification routing off process-global/single-stdio assumptions.
- Key long-lived PII, shaping, and modern-approval stores explicitly and add TTL/cap tests.
- Keep normal stdio and HTTP behavior unchanged. This phase should be independently
  reviewable and establishes isolation before any process sharing.

### Phase 2: daemon rendezvous and native adapter, behind a flag

- Add versioned descriptor, election lock, authenticated health/identity handshake,
  `--daemon`, and `--stdio-adapter`.
- Implement the adapter with Toolport's Streamable HTTP/SSE protocol; do not invoke Node or
  `mcp-remote`.
- Keep the existing in-process stdio gateway as automatic startup fallback during dogfood,
  but never fall back after a request may have reached the daemon.
- Cover simultaneous cold starts, stale descriptor, wrong token/version, daemon crash,
  adapter EOF, oversized frames, cancellation, and server-initiated RPC.

### Phase 3: downstream launch pooling

- Build the union catalog once and enforce the session's allowed server set on every list,
  call, prompt, resource, subscription, and server-initiated path.
- Pool by launch key, including `${ROOT}` sharding and registry/secret generations.
- Add concurrent clients with different identities, profiles, protocol eras, capabilities,
  roots, and overlapping request ids. Assertions must prove both sharing and isolation.

### Phase 4: dogfood, default, and desktop convergence

- Ship opt-in telemetry/diagnostics and dogfood under a registry feature flag.
- Make the adapter topology default only after process/memory reduction and parity suites
  pass on Windows, macOS, and Linux.
- Converge the desktop Shared HTTP supervisor onto a daemon service lease in a later PR.
- Retain a documented legacy-topology kill switch for at least one release cycle.

## Verification matrix

The change is not complete on unit coverage alone. Required tests include:

- 20 simultaneous adapters cold-start exactly one compatible daemon;
- exact-version/data-dir mismatch creates separate daemons without cross-talk;
- N clients using the same ordinary stdio server create one downstream child;
- two `${ROOT}` values create two children, while equal roots share one;
- profiles cannot list/call/subscribe to servers outside their scope;
- identical JSON-RPC ids from different sessions never collide;
- confirmations, shaped cursors, PII tokens, cancellation, progress, subscriptions,
  sampling, roots, and both elicitation modes route only to the originating principal and
  session as appropriate;
- adapter/daemon/client crashes clean every session-owned resource after close or TTL;
- registry edits rebuild once and preserve in-flight calls on the retired pool;
- no request that may have executed is automatically replayed after a transport failure;
- standalone/headless startup never waits indefinitely when rendezvous fails;
- existing stdio, native HTTP, and Shared HTTP protocol suites remain green.

Acceptance metrics should report heavy gateway processes, adapter processes, downstream
children, total descendant count, steady private memory, cold-start latency, and first-call
latency. The primary success criterion is that adding another ordinary client session does
not add another router or another copy of root-independent downstream servers.

## Risks and non-goals

- This design reduces process duplication; it does not promise one downstream instance
  when launch-affecting context differs.
- It does not isolate mutually hostile processes running as the same OS user.
- It does not make in-flight tool calls replayable or durable across daemon crashes.
- It does not replace the approval broker, public Shared HTTP configuration, or remote
  headless deployment in the first implementation.
- A shared daemon increases blast radius and makes state-isolation bugs more serious. That
  is why explicit session context lands before pooling and why the rollout is reversible.

## Estimate after design

This is a multi-PR architecture change, not a single large gateway edit. A reasonable
delivery shape is four implementation phases plus dogfood, with Phase 1 carrying most of
the correctness refactor and Phases 2-3 carrying most of the process/lifecycle risk. The
work should not be estimated as complete until the cross-session isolation matrix and
real-machine process-count acceptance run exist.
