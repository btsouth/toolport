# Implementation plan: one heavy gateway per host

Companion to [`one-gateway-per-host.md`](one-gateway-per-host.md). That document is the
design; this one is the build order. It is a multi-PR architecture change, not a single
gateway edit, and it should be read as a checklist rather than a commitment to land it in
one release.

## Current state

Grounded in the code as of 2026-09-16 (through #894). Anything marked "not started" below
is the honest next work, not a claim about ordering.

Landed:

- Phase 0. `src-tauri/src/topology.rs` defines `GatewayRole`, `CompatKey`, `LaunchKey`,
  `TopologySnapshot`, and the topology assertions.
- P2.1 rendezvous primitives (`src-tauri/src/daemon.rs`, #880), plus the daemon roles:
  P2.2a identity and P2.2b the full host runtime on the internal endpoint (#881), with a
  cold-start test (#882).
- P2.2c the stdio adapter, `--stdio-adapter` (#888), with a bounded worker pool for
  concurrent requests (#891) and recovery after the daemon dies (#893). Opt-in only; the
  default stdio role is untouched.
- P2.3 daemon idle exit (#892) and adapter crash recovery (#893). The lease is the open
  connection: the daemon exits once nothing has been in flight for `DAEMON_IDLE_GRACE`,
  and the adapter re-rendezvouses and replays the client handshake on the next request
  after a failure, without replaying the call that failed. Session TTL reaping
  (`reap_stale_mcp_sessions`) predates this work.
- P1.2 first increment: `session_store::SessionStore` (#894), a TTL and cap store for the
  session-scoped maps, as a tested module with nothing wired to it yet.
- P1.2 maps and owner: the PII pseudonym map, shaped-result cursors, and modern HITL
  approvals moved off their ad-hoc process globals onto `SessionStore`, and a
  `SessionTables` owner now holds the PII and HITL tables (the shaped stash is owned by
  `shaping`). Behavior is unchanged; the tables are now TTL- and cap-bounded, with
  reap-on-close and cap tests.
- P1.2 transport unification: `McpSession` (HTTP) and `StdioUpstream` (stdio) are now one
  `SessionState` with a transport face, and `StdioUpstream` is deleted. The upstream
  call/correlation logic and the notification fanout have a single implementation; the
  stdio face writes straight to stdout, the HTTP face queues for the listen stream.
- P1.2 threading, first increment: the stdio client's declared capabilities and its
  `${ROOT}` project root moved from `GatewayState` onto the stdio `SessionState`, and the
  server-request handler and roots refresh read them there.
- P1.2 threading, second increment: `MODERN_STDIO_UPSTREAM` moved off the process onto
  the stdio `SessionState`, and the notification and resource-updated paths now carry that
  session instead of a bare stdout, so the sink a frame is written to and the era that
  decides whether it may be written come from one owner. The stdio client's progress
  hand-off queue became session state (the shared progress dispatch now closes over the
  stdio session instead of capturing a process stdout), and the search and confirm guards
  moved onto `SessionState`, with the listener-level pair kept only for requests that carry
  no session record (a modern request, or an OpenAPI call). The `GatewayState.stdout` field
  is gone with them. Progress routes stay host-scoped by decision, see below.

Still open:

- P1.2 threading, remainder: the three `STDIO_*` handshake statics landed. `STDIO_CLIENT_READY`,
  `STDIO_RESPONDED`, and `STDIO_DEFERRED_LIST_CHANGED` are gone; the flags and the deferral
  queue are fields on the stdio `SessionState`, read through `stdio_client_ready()`,
  `stdio_responded()`, `mark_stdio_responded()`, and `stdio_may_speak()`. The test module's
  `StdioHandshakeGuard` (and the shared-state reset it did on drop) is deleted, because each
  test now builds its own session and cannot observe another's handshake. This closes the
  half of the single-stdio assumption that said a second connection inherits the first one's
  handshake.
  `stdout_broken` followed in the same shape: it was one `Arc<AtomicBool>` owned by `main` and
  threaded through `write_stdio_response`, `handle_stdio_request`, and the worker spawn (10
  occurrences across 6 distinct sites, none in tests), and it is now
  `SessionState::stdio_broken` (read via `stdio_broken()`, set via `mark_stdio_broken()`).
  Both callers lost the parameter. The flag is genuinely per-connection: a write failure on
  one stdio client used to stop every reader loop on the host.
  The stdio reader's cancellation and in-flight cap followed in the same shape: the
  `CancelRegistry` and its `Arc<AtomicUsize>` were created in `main` and threaded into
  `handle_stdio_request`, and they are now `SessionState::cancellations` and
  `SessionState::stdio_inflight`. The registry is keyed by the client's own JSON-RPC ids,
  which are client-chosen, so shared across a host one client cancelling its id 7 would have
  cancelled another client's id 7, and one client's queue depth would have throttled another's.
  `handle_stdio_request` lost the parameter.
  One single-stdio assumption remains and is NOT part of these moves: stdio PII/HITL lookups
  collapse to `PII_LOCAL_SESSION`, so two stdio clients on one host would share one pseudonym
  map and clearing one would clear the other. That one is a policy decision about identity
  rather than a field move, because a second stdio client has no asserted identity to key on
  until the daemon session protocol supplies one.
- P1.3 `HostState` (in progress). The host runtime now lives on `HostState` (registry and
  its trust flag, router, catalog snapshot, routine candidates and advisor, ready/dirty
  flags, rebuild lock, listener config, server handler, resource subscriptions and the
  `resources/updated` sink), together with its session table, its daemon runtime (daemon
  flag and activity lease), its rebuild streak map, its quarantine read flag, its code-mode
  switch, and its discovery mode. What remains outside is the
  principal-keyed `session_tables()` store and the `PROGRESS_*` dispatch and routes, which
  are read inside the dispatch core (see the P1.3 section for why those need the core's
  signatures changed rather than a field move).
  `GatewayState.stdio_upstream` is also constructed unconditionally, including in
  HTTP/daemon mode where there is no connection.
- Discovery and code mode are host policy, not session state, by decision. Both are
  resolved from the registry (which the watcher refreshes live) plus a process env
  override, so every session on one host sees the same switch; the per-client part of
  discovery already resolves per request from the caller's client id, and a daemon session
  will resolve it from the identity asserted at session open. Moving them onto
  `SessionState` would give each session a private copy of a host-wide setting.
- Progress routing is host state by decision: one token table per host, with every entry
  recording the session key that minted its token (a real session id for an HTTP client,
  the `RESOURCE_SUB_STDIO` sentinel for the stdio client). The stdio half of it is still
  single-client: the dispatch closes over the gateway's stdio session and that sentinel is
  a constant, so a second stdio client needs its own route identity. What was
  session-shaped about it (the hand-off queue and the stdout it writes to) is now owned by
  the stdio session.
- Progress is not era-gated, and never was: `deliver_progress` writes a bare
  `notifications/progress` frame through the hand-off, without the `list_changed` /
  `resources/updated` check on the peer's declared era. Pre-existing, unchanged by the
  threading work, and on the list so it is not read as an oversight.
- No topology feature flag in the registry.
- Two tests resolved the data directory per call on paths `DataDirOverride` was not
  guarding, so the gateway suite wrote into the developer's real data dir: the audit writer
  (`audit::audit_path`) and the search-trace writer (`searchtrace::path`). A full run
  appended 41 audit rows and 25 search-trace rows; the audit half also let one test's
  fixture row land inside another test's scratch log, which is what failed
  `mcp_http_audit_entry_records_client_and_client_name` intermittently on CI. Fixed by a
  test-only `DataDirTestEnv` guard (ENV_LOCK plus a scratch override) on every test that
  can reach either writer. Any future per-call `conduit_dir()` resolution needs the same
  treatment, or the leak returns under a third name.
  The same class has a state-dependent shape, found by a reviewer's sabotage: a test that
  dispatches a refused `run_script` writes no row while the code-mode gate is off, because the
  refusal returns before `audit::record_timed`, but force the gate on and the dispatch reaches
  it and appends to the developer's real dev log.
  `code_mode_flag_fails_closed_when_registry_load_fails` was that test and now holds the same
  scratch guard, so the row stays local whichever way the gate is forced.
  A third name, found by a reviewer running the suite in isolation: the dev gateway log.
  `~/.config/Toolport-dev/gateway.log` gains the suite's own fixture lines (tool-catalog and
  pagination warnings) on every lib run, on this tree and on main alike, so it is pre-existing
  and broader than either earlier fix. It is not a file the app reads back, but it is the same
  per-call `conduit_dir()` resolution, and it makes that log useless as a signal.
- Unrelated and still open: several tests leak their own scratch directories under the temp
  dir, because a panicking test skips its cleanup and a failing run leaves the directory
  behind. A long local session accumulated about 1,900 of them (`toolport-pii-release-*` was
  the largest group). Worth one small cleanup pass with a Drop guard on those specific
  tests; it does not affect correctness, but it makes the temp dir useless as a signal.
- The adapter has not been dogfooded against a real client (P4.1). It does have the
  synthetic per-session-count measurement in the delivery-shape section above, which is a
  process-count signal on a one-downstream fixture and not a substitute for the real run.
- Reusable primitives that already exist: the approval broker's `EndpointDescriptor`
  (`approval.rs`), `registry::atomic_write`, and the registry cross-process `FileLock`.
- Client launch: `clients.rs::gateway_entry` builds the stdio entry and sets
  `TOOLPORT_CLIENT_ID`. The desktop app spawns `--http <port>` through
  `http_bridge.rs::start_with_token_at` and kills it on exit.

## Delivery shape

| PR  | Slice                                                                                                                                                                           | Behavior change                             | Status                                                                                                  |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| 1   | P2.1 rendezvous primitives (library module, tested)                                                                                                                             | none (new module only)                      | landed (#880)                                                                                           |
| 2   | P2.2a identity role; P2.2b host runtime on the internal endpoint                                                                                                                | none (explicit flag only)                   | landed (#881)                                                                                           |
| 3   | P1.2 session tables on `SessionStore`; transports unified on `SessionState`; era, progress, guards, handshake, broken-stdout latch, cancellation, and in-flight cap per session | none default; HTTP confirm scoping narrowed | landed; one stdio assumption remains (PII)                                                              |
| 4   | P1.3 `HostState` extracted; `GatewayState` becomes a thin facade                                                                                                                | none                                        | in progress: five increments (the fifth in this PR), the session store and the progress dispatch remain |
| 5   | P2.2c stdio adapter speaks the daemon session protocol, behind flag                                                                                                             | opt-in only                                 | landed (#888, #891, #893)                                                                               |
| 6   | P2.3 session lifecycle, TTL, crash/EOF handling, fallback                                                                                                                       | opt-in only                                 | landed (#892, #893)                                                                                     |
| 7   | P3.1 union catalog built once, allowed-set enforced per session                                                                                                                 | opt-in only                                 | not started                                                                                             |
| 8   | P3.2 downstream pooling by `LaunchKey` and `${ROOT}` sharding                                                                                                                   | opt-in only, the big win                    | not started                                                                                             |
| 9   | P4.1 dogfood flag, telemetry, acceptance run                                                                                                                                    | opt-in only                                 | not started                                                                                             |
| 10  | P4.2 adapter topology becomes default; legacy kill switch remains                                                                                                               | default flip                                | not started                                                                                             |
| 11  | P4.3 desktop Shared HTTP converges onto a daemon service lease                                                                                                                  | separate, later                             | not started                                                                                             |

Each of 1 through 8 must leave the default topology untouched and all existing suites
green. The only PRs that change what a user gets are 10 and 11.

### Early dogfood signal (synthetic, not the P4.1 acceptance run)

The primary success metric is "another ordinary client session adds no router and no
root-independent downstream copy", so the structural claim was measured across session
counts rather than at one point. One local fixture: a private `TOOLPORT_DATA_DIR` and
registry, one stdio downstream (`mock-mcp-server`, 9 tools), the real gateway binary, and
N concurrent client sessions driven with a real MCP handshake and a real `tools/call` on
each. Both arms answered every call.

| sessions | legacy gateways / downstream copies | daemon + adapters / downstream copies |
| -------- | ----------------------------------- | ------------------------------------- |
| 1        | 1 / 1                               | 2 / 1                                 |
| 3        | 3 / 3                               | 4 / 1                                 |
| 6        | 6 / 6                               | 7 / 1                                 |

Downstream copies track the session count exactly on the legacy arm and stay at 1 on the
daemon arm, which is the metric moving as designed. The six-session row reproduced
identically on three separate runs, and the three-session row on two (process counts, not
just assertions).

Two honest caveats. At one session the daemon arm is strictly worse on process count
(2 against 1), because there is nothing yet to share and the adapter is pure overhead; the
crossover is somewhere between one and three sessions, and this fixture does not pinpoint
it. And this measures process counts and the pooling factor only. It does not reproduce the
Phase 0 baseline's order of magnitude, because that machine ran ~9 downstream servers per
client against this fixture's one, so it says nothing about the resident-memory win or
about cold-start and first-call latency. Those still need the real-machine run.

## Phase 1: explicit HostState, SessionState, RequestContext

Split `GatewayState` along the boundary the design already names, without changing
behavior. This is the prerequisite for sharing a runtime safely.

### P1.1 RequestContext

Largely satisfied already by SBS-551, so there is no separate PR for it. The
per-request fields (`upstream_version`, `upstream_capabilities`, `mcp_session`,
`upstream_transport`) live in one `ActiveRequestContext`, are installed at the dispatch
boundary (`handle_request_with_cancel` enters the era and capabilities guards;
`process_request` enters the transport; the HTTP path enters the session), and every
read goes through the context (`serving_modern_client`, `active_mcp_session`,
`active_upstream_is_stdio`, `modern_client_supports_server_rpc`). The design permits the
thread-local as a scoped adapter as long as it is populated from the explicit value and
cannot outlive the request, which the guards already ensure.

What remains is not per-request but session- and host-scoped: `PROGRESS_*` and the host
policy. Those move in P1.2 (`SessionState`) and P1.3 (`HostState`), so the isolation work
lands with the types that own it instead of as a mechanical rewrite of the request path. The PII and HITL tables already
moved onto a `SessionStore` owner, the two transport types are unified, and the stdio
client's protocol era, its progress hand-off queue, and the search and confirm guards are
session state; the handshake flags and deferral queue moved onto the session in the same
increment.

### P1.2 SessionState

Status: store landed (#894), the three session-scoped maps moved onto it, the two
transport types are unified, and the stdio session now owns the client's capabilities and
root, its declared protocol era, its progress hand-off, its search and confirm guards, and
its own handshake flags and deferral queue. The remaining globals are the host policy and
routing that P1.3 takes.

- Introduce `SessionState` owning exactly what the design lists: session id, principal and
  audit label, effective scope, protocol version and capabilities, roots and `${ROOT}`,
  upstream request correlation, outbound queue, cancellation, subscriptions, search guard,
  confirm guard, connection-local notification eligibility. Today it owns the transport
  face, owner, upstream request correlation, outbound queue, subscriptions, root, the
  stdio client's declared capabilities, its 2026-07-28 era flag, its progress hand-off
  queue, its guard pair, its handshake flags and deferral queue, its broken-stdout latch, and its
  cancellation registry and in-flight cap; discovery/code mode plus
  the progress routes are host-scoped by decision.
- `McpSession` becomes the HTTP transport face of `SessionState`; the stdio path gets the
  same type with a stdio transport face, deleting `StdioUpstream` as a separate concept.
  Landed: one `SessionState` with `SessionTransportFace::{Http, Stdio}`. The upstream
  call/correlation and the list_changed and resources/updated fanout each have a single
  implementation, and the notification path reads the sink and the era off the session.
- PII maps, shaped-result cursors, and modern HITL approvals stay keyed by principal, but
  live behind a `SessionStore` with explicit TTL and cap. Landed: `session_store::SessionStore`
  (#894) plus the PII map, shaped-result cursors, and modern HITL approvals moved onto it,
  with reap-on-close and reap-on-TTL/cap tests, owned by `SessionTables`.
- Guards: the search-thrash streak and the pending destructive confirmations are session
  state. Landed: `SessionState` owns them for both faces, a request with an MCP session id
  on the HTTP bridge uses that session's pair, and only a request with no session record
  keeps the listener-level pair. Tests cover a second session neither inheriting the streak
  nor redeeming the token. Consequence worth knowing: a confirmation is now redeemable only
  from the session that minted it (a session closed or re-initialized inside the
  confirmation window loses its pending token) and only on the surface that minted it
  (`/mcp` with a session id vs. the OpenAPI path, which mints against the listener pair).
- Known duplication: `watch_registry`/`watch_tick` still take the stdio session and its
  `${ROOT}` separately, although the root is a field of that session since #898. Harmless
  (both come from the same startup resolution) and it collapses with the `HostState` move.

### P1.3 HostState

Status: five increments, the fifth landing in this PR (the slice tracker in #910 still lists
three increments and is updated separately). `HostState` owns the host runtime the gateway already
resolved once per process, and `GatewayState` is now a facade over it: a `Deref` impl keeps
the host-scoped call sites reading `state.registry`, `state.router`, and friends, so moving
ownership did not rewrite several hundred lines.

- Landed: `HostState` holds the registry and its trust flag, the live router, the catalog
  snapshot, the routine candidate registry and advisor ledger, the ready and dirty flags,
  the rebuild lock, the listener configuration (`lazy`, `http`, bind host, allowed
  origins), the server-request handler, the resource subscription table, and the
  `resources/updated` dispatch sink. `GatewayState` keeps the session-side fields: the
  profile handle, the MCP session table, the stdio client's session, and its client id and
  boot profile. One invariant test asserts that a second facade shares the host, so one
  host still has exactly one live router and one registry.
- Landed, second increment: the host now also owns its session table and its daemon
  runtime. `mcp_sessions` moved onto `HostState` (the readers did
  not change at all, which is what the `Deref` facade buys), `daemon_mode` and
  `last_activity_ms` replaced the process statics of the same names with `touch_activity()`
  and `idle_for()` on the host, and the progress token counter moved onto `ProgressRoutes`
  so the table mints its own tokens instead of reading a process-wide sequence. The table
  itself is still reached through the process-global `PROGRESS_ROUTES` until the
  dispatch-core threading lands, so today this is a per-table counter on a still-global
  table; it is the right home either way, because a token is only ever resolved against the
  table that minted it, so uniqueness is needed within a table and nowhere else. Three
  tests pin the new ownership: a second host sees neither the first one's daemon flag nor
  its activity lease, each table starts its own token sequence, and the daemon identity
  route follows the host's own flag rather than any global.
- Landed, third increment: the host owns the rebuild streak map and the quarantine read
  flag, and carries them into the background threads. `preserve_collapsed_servers_guarded`
  became `HostState::preserve_collapsed_servers_guarded`, `effective_quarantine` and
  `reconcile_quarantine` take the flag, the two functions that reach them (`watch_tick`
  and `watch_registry`) take the host, and `persist_and_emit_with_sessions` became a host
  method called as `host.persist_and_emit_with_sessions(...)`. `main` now builds the host before it
  spawns the build thread and the registry watcher, so both carry `Arc<HostState>`; the
  four initializers they used to clone (server handler, rebuild lock, resource
  subscriptions, resources/updated sink) are read back off the host. One test drives a
  collapsed catalog through one host and asserts the streak accumulates there, that a
  second host's map stays empty, and that a store failure on one host does not mark
  another's read as failed.
- Fourth increment, in this PR: the host owns its code-mode switch. `CODE_MODE` is gone, and with
  it `CODE_MODE_TEST_LOCK`, `CodeModeGuard`, and `set_code_mode_flag`. `code_mode_enabled()` is
  `HostState::code_mode_enabled()`; the watcher publishes the registry's switch through
  `HostState::set_code_mode`. The fail-closed boot rule survives as
  `seed_code_mode_after_registry_load(loaded) -> bool`, which returns the value the field
  starts from rather than setting one, because the registry load happens above the host
  construction. Eight functions take `host: &HostState` (the six readers plus `handle_request`
  and `handle_request_with_cancel`), 89 call sites moved with them (13 production and 76 test,
  61 of those the test wrapper), and the tests that used to
  flip a process global now build the host they dispatch with. Two tests pin the ownership: one
  asserts two hosts advertise `toolport_run_script` differently, one drives the watcher's
  reload and asserts a second host's flag is untouched. Both were checked by sabotage, and
  between them each assertion is load-bearing. Worth knowing for the same move next time: the
  migrated off-path assertions cannot detect a gate that is silently always-off (they assert
  refusal, which a dead gate also produces), so the switch is kept honest by the new pair, by
  `run_script_respects_live_code_mode_flag`, and by the three migrated tests that assert the on
  path (`flattened_routine_tools_are_advertised_and_run`,
  `routine_write_opt_in_defaults_off_and_controls_advertisement`, and
  `immutable_code_run_returns_promotion_candidate_without_retaining_input`).
- Fifth increment, in this PR: the host owns its discovery mode. `DISCOVERY_MODE` is gone,
  and with it `DISCOVERY_MODE_TEST_LOCK`, `DiscoveryModeGuard`, and the free
  `discovery_mode()` / `set_discovery_mode()` / `grouped_discovery()`. `HostState` owns a
  `discovery: AtomicU8` read through those same three names as methods, `main` seeds it from
  the same `resolve_discovery_mode()` outcome it used to store into the static, and the
  watcher publishes on the host it was handed. Seven production reads moved with it
  (`enabled_summary`'s status line, which is why that function gained the host parameter,
  `watch_tick`'s compare, the `process_request` arguments in `handle_stdio_request` and
  `main`, and the three `grouped_discovery()` call sites), plus the test-only wrapper, plus
  the watcher's set and the bootstrap that initializes the field, the four test setter sites
  this moved (six calls, one of them a three-mode loop) and the five more the new tests and
  the status assertion added, eight test `enabled_summary` call sites, and the three guard
  sites, which are deleted rather than moved. Two tests pin the ownership, the discovery twin
  of the pair the code-mode increment added: one asserts two hosts advertise differently, one
  drives the watcher's reload and asserts a second host's mode is untouched. Sabotage-checked
  both ways: re-globalizing the value behind the new names fails both, and dropping the
  watcher's publish fails the second alone. This is the second of the two host-policy statics;
  the dispatch core still has the session store and the routes.
  Coverage limits, stated rather than implied: `main`'s wiring of the resolved mode into the
  host field is not observable from a unit test (nothing calls `main`), the
  `process_request` mode arguments are not driven with a non-default host mode, and the watcher
  test derives its expected mode from its own fixture so an ambient `TOOLPORT_DISCOVERY`
  override cannot fail it. The hazard this increment uncovered is closed: the HTTP/OpenAPI
  fallback read a boot-frozen `lazy` bit and only followed grouped switches live, so a switch
  involving `lazy` waited for a restart, unlike stdio and the daemon. The two fields are
  collapsed (the bridge resolves its own requests from `HostState::discovery`, and
  `gateway_capabilities` takes the resolved mode instead of a bool), and
  `http_bridge_follows_a_live_discovery_switch` flips the mode on one host and asserts the
  bridge's OpenAPI spec follows it both ways; freezing the read back to a constant fails it.
- Remaining: the principal-keyed session store and the
  `PROGRESS_*` dispatch and routes. The session store
  is read deep inside the dispatch core (`execute_call`,
  `handle_request_with_cancel`), which deliberately takes narrow parameters rather than
  the whole state, so moving them means threading a host handle through that core. That
  threading is wider than it looks: `handle_request` is a test-only wrapper with 63 call
  sites, all of them tests, and `execute_call` is reached through the routine and script
  dispatch helpers, so the slice needs a deliberate decision about how the test helper gets
  its host. The code-mode increment answered it (see below); the rest can reuse the answer.
  `PROGRESS_DISPATCH` and `PROGRESS_ROUTES` stay where they are for the same reason: the
  dispatch is read by `prepare_progress`, three layers below anything that holds the state,
  and the design publishes exactly one progress dispatch per host, so a process global is
  the host's in effect. Both are recorded here as deliberate, not overlooked; they should
  move with the dispatch-core threading if a process ever hosts two runtimes at once.
- Watch item: `mcp_sessions` moved onto `HostState` in this increment, which closes the
  trap the first increment's review flagged: a facade can no longer be constructed per
  session and end up with an empty table.
- Naming note: `codemode.rs` has its own private `HostState` for the QuickJS sandbox host.
  Unrelated to this one; the plan's name wins here because this is the type the design's
  ownership split is about.
- Introduce `HostState` owning registry and watcher, router and rebuild lock, catalog
  snapshot and cache writes, downstream pool and circuit-breaker state, quarantine and
  rate-limit bindings, audit/metrics/savings, `server_handler`, and
  `resource_updated_sink`.
- `GatewayState` shrinks to a facade holding `HostState` plus a `SessionStore`, so the
  existing 300 call sites keep compiling while P2 moves ownership.
- Tests: the topology assertions in `topology.rs` stay green; a host with one router and
  two sessions reports `router_owners == 1`.

#### Next slice: the dispatch core

This PR moves the discovery mode as the fifth increment; these are the holders left, and the shape the next
attempt should reuse rather than re-derive.

- Remaining holders, with the readers that keep them off `HostState`: the
  `session_tables()` store, whose nine production sites are inside `clear_pii_session`,
  `with_pii_session`, and the `modern_hitl_*` family. The HTTP session-close and
  re-handshake paths reach those helpers as callers rather than reading `session_tables()`
  themselves, so threading the store means threading those eight helpers.
- Measured size of what remains, so the next attempt starts from it rather than
  rediscovering it. `session_tables` has 22
  production and 41 test call sites across its eight helpers. On top of the call sites, each
  reader currently
  takes its inputs as separate parameters (`reg`, `router`, `cached`) rather than a host, so
  moving a holder means changing those signatures as well, and every `HostState` construction
  site (three: one production, two in tests) needs the new fields initialized.
  Adding the field first and migrating the readers afterwards is the tempting half-step and
  it must not be committed that way: while both the static and the host field exist, there
  are two sources of truth and whichever the readers still call wins silently. Land the
  field and its readers in one pass, or leave the static alone.
- The decision, taken for `CODE_MODE` and to be reused here: keep `handle_request` as the test
  wrapper and give it a `host: &HostState` parameter. Retiring it in favour of
  `handle_request_with_cancel` is the tidier end state but it is a 62-site change to an
  18-argument call, and nothing is blocked on it. The wrapper keeps its other parameters,
  which is deliberate: tests pass `lazy`, their own `reg` and `router`, and a profile, and the
  wrapper builds the `CatalogSearchIndex` those need. The host it is handed supplies
  host-scoped state and nothing else, so tests bind one host per body (the `dispatch_host`
  helper) rather than one per call: 5 tests dispatch twice or more under one code-mode state
  (`routine_write_opt_in_defaults_off_and_controls_advertisement` 2 calls,
  `code_mode_flag_fails_closed_when_registry_load_fails` 2,
  `toolport_extension_reports_active_features_without_gating_core_tools` 3,
  `a_corrupt_quarantine_store_keeps_the_current_set_instead_of_un_blocking` 23,
  `watch_tick_marks_a_recovered_registry_untrusted` 21), and a per-call host would reset the
  store between them and quietly weaken exactly those tests.
- Two nested test helpers still build a host per call rather than taking one:
  `fn dispatch` (20 call sites across 11 test bodies) and `fn search_text` (9 across 4), both
  fixtures whose callers dispatch several times in one body. Harmless while the dispatch core
  reads exactly one host-scoped value from the host, and a silent weakening the moment
  the session store is threaded through it. Give them a host parameter in that slice, not this
  one. (The discovery increment left them alone for a narrower reason than "no changed site
  inside them": both helpers call `handle_request` with `lazy = true`, which short-circuits the
  mode before any host read, so their per-call hosts never carry discovery state.)
- What the `CODE_MODE` half cost, measured, so the remaining two can be sized against a
  number that actually landed. 8 functions gained a host parameter: the 6 readers
  (`gateway_capabilities`, `grouped_tool_defs`, `append_routine_tool_defs`,
  `save_routine_dispatch`, `save_routine_promotion_dispatch`, `advise_after_direct_call`,
  which between them carried 11 production and 11 test call sites) plus `handle_request` and
  `handle_request_with_cancel`. 89 call sites moved in all, 13 production and 76 test, the test
  wrapper being 61 of them and 63 in the tree now. 53
  test bodies build a host they now own; the 18 `CodeModeGuard::acquire()` sites and 19
  `set_code_mode_flag` sites in tests are 21 setter calls on that host now, and three of the
  guard sites came out without a setter (two lean on the value the host is built with, one never
  set the flag). Two tests pin the
  ownership: `code_mode_is_per_host`, and
  `watch_tick_refreshes_code_mode_on_the_host_it_was_given`. That is a slice, not an edit to
  fold into another change, and the store half is larger again.
- `handle_request`'s 63 call sites are all tests. That count stood at 61 through the third
  increment and this slice's `code_mode_is_per_host` adds one, so re-measure it rather than
  trusting either number. The `#[cfg(test)]` definition sits outside `mod tests`, which is why
  counting the identifier alone reads one higher (64 here; a first pass on this doc wrote that
  occurrence count down as the call-site count, hence the old 62), and
  `execute_call` is reached through `run_routine_dispatch`, `execute_script_dispatch`, and
  `execute_script_dispatch_with_candidate`. Threading `host: &HostState` through that chain
  is mechanical except for how the tests receive their host. Tests that assert PII or HITL
  continuity across calls (for example
  `clearing_a_pii_session_drops_the_previous_conversations_map`) need one host for the
  whole test, so the fixture-shaped answer is a host built once in the test body and passed
  to every call; a per-call `&http_state(false)` would silently reset the store between
  calls and quietly weaken exactly those tests.
- Done in the third increment: `watch_tick` and `watch_registry` used to take the host _and_ clones of
  its own fields (registry, trust flag, router, catalog, dirty flag, server handler,
  session table, resource subscriptions, rebuild lock, `resources/updated` sink), so a
  caller could pair one host with another host's router or cache. Those parameters are gone;
  every host-scoped value is read off the single `host` argument. `watch_tick` went 19 → 10
  parameters and `watch_registry` 18 → 9. The four watcher tests that passed a throwaway
  `http_state(false)` as the host (7 call sites across those four) beside their own locals now
  build one host from their own handles (a `host_from_parts` test helper), which is what makes
  the collapse assert the same thing it used to. The only parameter kept is `resource_updated_override`, because the watcher tests
  need to drive a rebuild with no sink wired, which a host field cannot express.
- Sequencing question, for the maintainer rather than for the code: this increment is all
  Phase 1 has left apart from the one remaining stdio assumption noted above (the PII
  fallback), and it is not a prerequisite for the pooling work. The remaining P1.3
  holders are host-scoped by decision rather than isolation gaps (host policy, one table per
  host, one dispatch per host), and the P1.2
  remainder is now only the local-session PII fallback. P3.1 and P3.2 are therefore free to start
  first; the plan keeps the original order by preference, so that pooling is built on state
  that is already fully host-owned. That is a choice about risk, not a dependency.

## Phase 2: rendezvous and the stdio adapter

### P2.1 Rendezvous primitives (landed, #880)

New library module `src-tauri/src/daemon.rs`, no gateway wiring and no behavior change yet:

- `DaemonDescriptor` with `endpoint`, `token`, `pid`, `compat` (from `CompatKey`),
  `protocol`, `created_at_ms`. Written with `registry::atomic_write` and user-only
  permissions, beside the existing `approval-endpoint.json`.
- `descriptor_path(data_dir, compat)` keyed by the compat fingerprint, so mismatched
  versions and data dirs can never read each other's descriptor.
- `probe_identity(descriptor)` performing an authenticated `GET /host/identity` and
  returning the daemon's `DaemonIdentity`; `is_compatible_with` verifies the complete
  compatibility identity (version, data dir, protocol generation). Never trusts PID or an
  open port.
- `Rendezvous::ensure(spawn)` doing read, claim-check, probe, then election with a
  version-keyed `registry::lock_at_for`, a recheck under the lock, a single `spawn`, and a
  bounded readiness wait while the lock is held.
- `serve_identity(...)` for the daemon side: a tiny authenticated loopback listener that
  publishes the descriptor and answers identity.

Tests: descriptor path is compat-keyed; probe succeeds and rejects a wrong token; an
identity from one build is not compatible with another; the descriptor is owner-only; 8
concurrent cold starts elect exactly one daemon; a stale descriptor is replaced.

### P2.2 Host runtime and adapter

- P2.2a (landed, #881): `--daemon` is accepted and the P2.1 identity listener serves
  `/host/identity`.
- P2.2b (landed, #881): `--daemon` runs the full host runtime on an ephemeral loopback endpoint
  with a random internal bearer and publishes the descriptor. The internal
  `/host/identity` route is daemon-only, so the user-facing HTTP bridge never exposes the
  compat fingerprint or build. Still explicit-flag only, off the default startup path, and
  with the same registry, router, watcher, audit, and session tables as the HTTP bridge.
- P2.2c (landed, #888, #891, #893): `--stdio-adapter` performs `Rendezvous::ensure` and
  speaks the daemon session protocol with Toolport's Streamable HTTP/SSE: one session
  open, bidirectional JSON-RPC translation, cancellation, oversized frames, and
  server-initiated RPC correlated back to the originating session. Requests run on a
  bounded worker pool (notifications stay on the reader, so a cancellation stays ahead of
  what is queued behind it); a request that fails at the transport level is reported and
  never replayed, and the next one re-rendezvouses and replays the client handshake. No
  Node and no `mcp-remote`. The default role is still the existing in-process stdio
  gateway.

### P2.3 Lifecycle and failure (landed, #892, #893)

- Session lease on connect: the lease is the open connection. The adapter holds a long-lived
  `GET /mcp` listen stream while it is connected and deletes the session on client EOF, so
  per-session state is released at once. `reap_stale_mcp_sessions` still covers an adapter
  that dies without the DELETE.
- Daemon idle exit after `DAEMON_IDLE_GRACE`: landed (#892). It keys on nothing being in
  flight for the whole grace rather than on the session table, so a session row left behind
  by a crashed adapter cannot pin the process. Discovery is withdrawn before the exit is
  final and put back if a client connected in that window. `TOOLPORT_DAEMON_IDLE_GRACE_MS`
  overrides the grace for tests.
- Daemon crash: landed (#893). The affected call fails with an error and is never replayed;
  the next request re-runs the rendezvous and replays the client's `initialize` and
  `notifications/initialized`, so the replacement gets an equivalent session. Healthy calls
  share a read gate and run concurrently; recovery takes the write gate.
- Rollback: `--stdio-adapter` is opt-in; the legacy in-process role stays the default.

## Phase 3: downstream launch pooling

### P3.1 Union catalog with per-session scope

- Build the catalog once for the union of enabled servers and enforce each session's
  allowed set on every list, call, prompt, resource, subscription, and server-initiated
  path. The HTTP bridge already proves the filtering model; make it the only model.

### P3.2 Pool by `LaunchKey`

- Reuse one downstream launch per `LaunchKey` (server id plus launch-affecting fingerprint
  plus resolved root context). `${ROOT}` servers shard per distinct root; registry and
  secret generations retire the old key after in-flight calls finish.
- This is the measurable win: adding an ordinary client session must not add a router or a
  root-independent downstream copy.

### P3.3 Concurrency and isolation tests

- Concurrent clients with different identities, profiles, protocol eras, capabilities,
  roots, and overlapping request ids. Assertions must prove both sharing and isolation
  from the design's verification matrix.

## Phase 4: dogfood, default, convergence

- P4.1 registry feature flag, telemetry/diagnostics, and the real-machine acceptance run
  (heavy gateways, adapters, descendants, memory, cold-start and first-call latency).
- P4.2 default flip only after parity suites pass on Windows, macOS, and Linux, keeping a
  documented legacy kill switch for at least one release.
- P4.3 desktop Shared HTTP adopts a daemon service lease; app exit releases the lease
  instead of killing the process.

## Conformance harness

Every row of the design's verification matrix is a named case in
`src-tauri/tests/one_gateway_conformance.rs`, so each daemon phase lands against an
executable checklist instead of a prose table. The cases drive real processes — the real
gateway binary in `--daemon` and `--stdio-adapter` roles, the real rendezvous files, and
the real `mock-mcp-server` fixture as the downstream — and every wait is bounded, so a
row that hangs fails its own deadline rather than the CI job. Cases count downstream
launches two ways: as live processes and as `initialize` lines in the child's transcript.

The fixture registry writes full discovery (`lazy_discovery: false`), because the matrix
rows are full-catalog rows and `Registry::default()` runs lazy discovery, where
`tools/list` serves only the gateway's meta-tools. Cases that assert a downstream tool
is exposed poll `tools/list` bounded: the daemon serves immediately while its router
builds on a background thread, and a real client learns the finished catalog from
`notifications/tools/list_changed`.

| Matrix row                                                            | Case                                                                                                                                                        | Phase     | Status on main        |
| --------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | --------------------- |
| 20 simultaneous adapters cold-start exactly one daemon                | `matrix_cold_start_twenty_simultaneous_adapters_elect_exactly_one_daemon`                                                                                   | P2.1      | passing               |
| Version/data-dir mismatch creates separate daemons without cross-talk | `matrix_partitioning_separate_data_dirs_run_separate_daemons_without_cross_talk`, `matrix_partitioning_a_foreign_compat_descriptor_is_rejected_not_adopted` | P2.1      | passing               |
| Startup never waits indefinitely when rendezvous fails                | `matrix_lifecycle_a_stale_descriptor_does_not_stall_startup`                                                                                                | P2.1      | passing               |
| Crashes/EOF clean up session-owned resources after close or TTL       | `matrix_lifecycle_adapter_eof_releases_the_session_and_lets_the_daemon_exit`                                                                                | P2.3      | passing               |
| N clients on one ordinary stdio server create one downstream child    | `matrix_pooling_sessions_share_one_downstream_child`                                                                                                        | P3.2      | passing               |
| Two `${ROOT}` values create two children, equal roots share one       | `matrix_pooling_root_sharding_two_roots_two_children`                                                                                                       | P3.2      | pending (`#[ignore]`) |
| Profiles cannot list or call outside their scope                      | `matrix_routing_profiles_cannot_reach_servers_outside_their_scope`                                                                                          | P3.1      | pending (`#[ignore]`) |
| Identical JSON-RPC ids from different sessions never collide          | `matrix_routing_identical_request_ids_stay_per_session`                                                                                                     | P3.3      | passing               |
| Session-scoped surfaces reach only the originating session            | `matrix_routing_session_scoped_notifications_reach_only_the_subscriber`                                                                                     | P3.1/P3.3 | pending (`#[ignore]`) |

Pending rows are `#[ignore]`d acceptance criteria, not absent ones: they fail loudly with
`--ignored` today (root sharding falls back to the daemon's cwd; a shared daemon serves
the first session's profile to every session; `tools/list_changed` broadcasts to all
sessions), and the attribute comes off in the same PR that lands the phase — the pattern
`tests/spec_conformance.rs` used while the dual-era work was in flight.

Commands (the in-file `CASE_LOCK` already serializes cases; `--test-threads=1` keeps the
output readable):

```sh
# every row that main must satisfy (default CI-green set)
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test one_gateway_conformance -- --test-threads=1

# the pending rows, to watch a phase close (they fail until it lands)
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test one_gateway_conformance -- --ignored --test-threads=1

# one case by name
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --test one_gateway_conformance matrix_pooling_sessions_share_one_downstream_child \
  -- --exact --test-threads=1
```

Rows the harness deliberately does not duplicate: crash recovery and no-auto-replay are
pinned in `tests/stdio_adapter.rs`, daemon idle exit in `tests/daemon_idle_exit.rs`, and
version-keyed descriptor isolation in the `daemon.rs` unit tests (the compile-time
version makes a real-process version mismatch impossible to stage; the data-dir cases
cover real-process partitioning, and the foreign-compat case covers the wrong-domain
descriptor a version bump produces).

## Acceptance mapping

The design's verification matrix maps to: P2.1 (cold start election, mismatch isolation,
stale descriptor), P2.3 (crash, EOF, TTL cleanup), P3.3 (scope isolation, id collisions,
routing to the originating session), P4.1 (process and memory counts). The primary success
metric stays: another ordinary client session adds no router and no root-independent
downstream copy.
