# Toolport design

Toolport manages MCP servers locally and presents them through one gateway to
multiple AI clients. Server configuration, profiles, tool policies, and activity
are shared locally; credentials are resolved from the OS keychain. User setup is
documented in [README.md](README.md); development and verification requirements
are in [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Components and boundaries

| Component                                                                      | Responsibility                                                                                                                                    |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| React/Vite frontend (`src/`)                                                   | Server, client, policy, activity, and settings views. `src/lib/api.ts` wraps Tauri IPC; the frontend does not own downstream MCP connections.     |
| Tauri shell (`src-tauri/src/desktop.rs`)                                       | Desktop commands, native integrations, and orchestration of shared Rust services. This is the default Cargo `desktop` feature.                    |
| GTK shell (`src-tauri/src/linux_native/`, `src-tauri/src/bin/toolport-gtk.rs`) | Optional Linux-native UI behind `gtk-desktop`, using the shared controllers and runtime. It is separate from the React interface.                 |
| Gateway (`src-tauri/src/bin/toolport-gateway.rs`)                              | Upstream MCP protocol, discovery/search, profile scoping, call policy, and audit orchestration. Runs as a separate binary over stdio or HTTP.     |
| Router and transports (`router.rs`, `downstream.rs`, `remote.rs`)              | Aggregate and namespace tools, map calls back to original names, and communicate with downstream stdio or remote HTTP/SSE servers.                |
| Registry and credentials (`registry.rs`, `secrets.rs`, `oauth.rs`)             | Persist configuration and resolve local credentials. Shared controllers such as `registry_controller.rs` enforce mutation policy for both shells. |
| Client adapters (`clients.rs`, `rules.rs`, `instructions.rs`)                  | Discover client configuration, connect clients to the gateway, and preview/apply managed agent rules and instructions.                            |
| Activity (`audit.rs`, `observability_controller.rs`)                           | Record calls as bounded local JSONL history and aggregate it for the Activity UI.                                                                 |
| Teams (`teams.rs`)                                                             | Synchronize shared configuration and policies into the local registry while keeping each member's server credentials local.                       |

Rust module paths in the table are relative to `src-tauri/src/` unless shown in
full. `http_bridge.rs` supervises the HTTP gateway child process, authenticated
readiness, and shutdown; shells retain persistence and user-facing policy.

## Data flow

1. The UI invokes a shell command through `src/lib/api.ts`. Rust controllers
   validate mutations and update the local registry or keychain. Client adapters
   write the selected client's gateway connection in its native config format.
2. An AI client connects to the gateway. The active profile determines its server
   scope; the gateway loads registry configuration and resolves credentials for
   downstream connections. Registry changes trigger tool-list change notifications
   for clients that support them.
3. Discovery exposes namespaced tools or a compact set of discovery/call helpers
   in lazy mode. Search retrieves definitions on demand instead of placing the
   entire catalog in the client's initial context.
4. Calls pass gateway policy checks, including tool overrides, approval, and
   integrity controls, then route to the owning downstream server. Results return
   to the client and call activity is recorded for the local Activity view.
5. Optional Teams synchronization merges shared non-secret configuration into the
   same registry; members authenticate downstream servers with their own secrets.

## Invariants and adopted decisions

- **Separate shell and gateway processes.** Clients can spawn the stdio gateway
  independently of the UI; shells can supervise an HTTP gateway. The gateway
  builds without default desktop features, avoiding the Tauri/WebKit dependency
  set for headless use. The tradeoff is a separately built and bundled binary;
  frontend hot reload does not rebuild it.
- **One shared Rust core.** Feature gates isolate Tauri and GTK dependencies while
  controllers centralize security-sensitive mutations. The GTK interface has its
  own view implementation, so React features do not automatically apply to it.
- **Local configuration, separate secrets.** Registry and client configuration
  hold non-secret configuration and secret references, not keychain credential
  values. Credentials are injected into downstream environments or HTTP auth.
  Teams shares configuration rather than server credentials; headless deployments
  still need a working credential store as described in [docs/headless.md](docs/headless.md).
- **Concurrent persistence is coordinated.** Registry mutations use an OS advisory
  lock and atomic writes so desktop, gateway, and synchronization processes cannot
  overwrite each other's edits. Debug and release data directories are separate;
  test data overrides isolate fixtures from installed user data.
- **Stable routing identities.** Exposed tool names are sanitized to
  `[A-Za-z0-9_]` for client compatibility. The router preserves a reverse mapping
  to the server's original name. Profile scope and policy enforcement belong to
  the gateway, not translated UI labels.
- **Lazy discovery trades context size for discovery calls.** Clients search and
  invoke tools on demand. This avoids eagerly exposing every schema, with less
  benefit for small catalogs; [BENCHMARK.md](BENCHMARK.md) records measurements.
- **Bounded local history.** Audit logging trims older entries to limit disk use;
  Activity is a local operational history rather than indefinite archival storage.
- **Bundled, on-demand localization.** `src/lib/i18n.tsx` owns a shared external
  language store; `src/locales/` contains grouped dictionaries. English source
  messages are fallback text, and non-English dictionaries load via dynamic
  imports, keeping them outside the static startup dependency graph. Language
  choice persists in localStorage as `toolport-language`; failed storage still
  permits a session choice. A generation counter prevents stale asynchronous
  loads from replacing a newer selection. Dictionary failures show English and
  an error. React interpolation avoids HTML strings. Localization changes display
  text without changing protocol identifiers or user/server-supplied content;
  native tray, GTK, and raw backend diagnostics remain outside this layer.

The browser fixture (`src/test/browser-fixture.tsx`) substitutes in-memory IPC
responses and blocks external origins. It verifies frontend behavior, including
language switching, but does not exercise the native backend or OS keychain.
