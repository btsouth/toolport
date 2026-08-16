# Headless / container gateway

**Gateway-only compile:** CI and headless builds use
`cargo build --no-default-features --bin toolport-gateway` to skip the Tauri
desktop shell and WebKit dependencies. The default feature set (`desktop`) is
for the full app.

Run `toolport-gateway` without the desktop app — for Docker hosts, sandboxed
coding agents, and Open WebUI. The desktop app stays the local-first product
(client config writers, HITL approvals, OAuth UX). This path is the same binary
with HTTP enabled.

## What you get

| Endpoint                             | Use                                                                  |
| ------------------------------------ | -------------------------------------------------------------------- |
| `GET /openapi.json` + `POST /{tool}` | Open WebUI, n8n, LibreChat (OpenAPI)                                 |
| `POST /mcp`                          | MCP clients over streamable-HTTP (Claude Code, Cursor remote, Pi, …) |
| `GET /mcp`                           | Legacy MCP session listen stream                                     |
| `GET /`                              | Short help text                                                      |

Current streamable-HTTP scope:

- Modern MCP `2026-07-28` requests are sessionless: every `POST /mcp` carries its
  protocol `_meta`, `MCP-Protocol-Version`, and `Mcp-Method`/`Mcp-Name` routing headers.
- Modern list-change and resource-update notifications use `subscriptions/listen` over POST.
- Legacy clients still initialize, receive `Mcp-Session-Id`, and reuse it on later requests.
- `GET /mcp` and `DELETE /mcp` remain available for valid legacy sessions; modern requests
  receive `405 Method Not Allowed`.
- `POST /mcp` returns JSON-RPC responses as JSON by default.
- If `Accept` prefers `text/event-stream`, `POST /mcp` returns a single SSE `message` event and closes.
- **Server-initiated RPC passthrough** (#167): legacy clients use the session listen stream;
  modern clients receive input requests through MRTR `input_required` results and retry with
  `inputResponses` plus `requestState`.

Auth is the same bearer token as today (`TOOLPORT_HTTP_TOKEN` or a registered
`httpClients[]` entry). Non-loopback binds **require** a token.

## Quick start (binary)

```bash
export TOOLPORT_HTTP_HOST=0.0.0.0
export TOOLPORT_HTTP_TOKEN="$(openssl rand -hex 24)"
export TOOLPORT_REGISTRY=/path/to/registry.json
# optional: encrypted vault
# export TOOLPORT_SECRET_KEY=...

toolport-gateway --http 8765
```

Point Open WebUI at `http://host:8765` with the bearer token as the API key.
Point an MCP client at `http://host:8765/mcp` (streamable-HTTP).

### Prometheus metrics (opt-in)

Off by default. Set `TOOLPORT_METRICS=1` on the gateway process, then scrape:

```bash
curl -s -H "Authorization: Bearer $TOOLPORT_HTTP_TOKEN" \
  http://127.0.0.1:8765/metrics
```

Emits counters for tool calls (`server`, `tool`, `client`, `ok`), held
destructive calls, duration sum/count, lazy-discovery tokens saved, and a
quarantine gauge. Labels are ids only (never arguments). Same auth as OpenAPI.

An instance that has not run a tool yet scrapes 200 with the gauges at zero. If a
local stat file exists but cannot be read (permissions, a sharing lock), the
endpoint answers `500` instead of a 200 with no series, so the scrape fails and
Prometheus `up` goes to 0 rather than the instance looking idle.

### Modern MCP request (curl)

```bash
curl -s -X POST http://127.0.0.1:8765/mcp \
  -H "Authorization: Bearer $TOOLPORT_HTTP_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: tools/list" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"curl","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}'
```

No initialize request or session header is used. `Mcp-Session-Id`, if sent by an old
intermediary, is ignored on this modern path.

### Legacy MCP handshake (curl)

```bash
# 1) initialize — capture Mcp-Session-Id from the response headers
curl -sD - -o /tmp/init.json -X POST http://127.0.0.1:8765/mcp \
  -H "Authorization: Bearer $TOOLPORT_HTTP_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'

# 2) tools/list (reuse the session id)
curl -s -X POST http://127.0.0.1:8765/mcp \
  -H "Authorization: Bearer $TOOLPORT_HTTP_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: <session-from-step-1>" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

## Docker

### Pull from GHCR (recommended)

After the first CI publish, make the package public (GitHub → Packages →
`toolport-gateway` → Package settings → Change visibility). Then:

```bash
docker pull ghcr.io/tsouth89/toolport-gateway:latest
mkdir -p data
cp data/registry.json.example data/registry.json
cp docker-compose.example.yml docker-compose.yml
# create .env with at least TOOLPORT_HTTP_TOKEN=...
docker compose up -d
```

### Build locally

From source (slow — compiles inside Docker):

```bash
docker build -f Dockerfile.source -t toolport-gateway .
```

Or use the runtime Dockerfile after building the binary on the host:

```bash
cargo build --release --bin toolport-gateway --manifest-path src-tauri/Cargo.toml --no-default-features
cp src-tauri/target/release/toolport-gateway toolport-gateway-bin
docker build -t toolport-gateway .
```

Image defaults:

- `TOOLPORT_HTTP_HOST=0.0.0.0`
- `TOOLPORT_REGISTRY=/data/registry.json`
- port `8765`
- volume `/data`

## Secrets without the OS keychain

Resolution order when a server marks `env[].secret: true`:

1. Process env `TOOLPORT_SECRET_<KEY>` (preferred in compose)
2. Process env `<KEY>` when `TOOLPORT_ALLOW_BARE_SECRET_ENV=1` (opt-in; enabled in the compose example)
3. Encrypted `secrets.enc` when `TOOLPORT_SECRET_KEY` is set
4. OS keychain (desktop)

Example `.env`:

```env
TOOLPORT_HTTP_TOKEN=replace-me
TOOLPORT_SECRET_STRIPE_SECRET_KEY=sk_live_...
# or bare name with explicit opt-in (set in compose example):
# TOOLPORT_ALLOW_BARE_SECRET_ENV=1
# STRIPE_SECRET_KEY=sk_live_...
```

## Minimal `registry.json`

Start from [`data/registry.json.example`](../data/registry.json.example) (remote
MCP server — works in a minimal container with no extra runtime). Or copy a full
`registry.json` from a machine that already runs the desktop app.

A valid headless registry needs `profiles` and `activeProfileId`, not just
`servers`:

```json
{
  "version": 1,
  "servers": [
    {
      "id": "stripe",
      "name": "Stripe",
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@stripe/mcp"],
      "env": [{ "key": "STRIPE_SECRET_KEY", "secret": true }],
      "source": "manual"
    }
  ],
  "profiles": [
    {
      "id": "default",
      "name": "Default",
      "enabledServerIds": ["stripe"]
    }
  ],
  "activeProfileId": "default"
}
```

Stdio servers inside the container need their runtimes (`node`/`npx`, `uv`,
etc.) installed in the image or reached via another container on the same
network. Remote MCP servers (`url` + streamable-HTTP downstream) need no extra
runtime in the gateway image.

## Production checklist

Use this before exposing a headless gateway beyond a trusted host or LAN.

### Network and auth

- [ ] **Bearer token set** — `TOOLPORT_HTTP_TOKEN` with at least 24 bytes of
      entropy (`openssl rand -hex 24`), or a registered scoped HTTP client. The
      process refuses any bind without configured authentication unless an operator
      explicitly passes `--insecure-loopback` for isolated local development.
- [ ] **Firewall** — only trusted clients can reach the port. Do not publish
      `:8765` to the public internet without a reverse proxy.
- [ ] **TLS in front** — the gateway speaks plain HTTP. Terminate TLS at nginx,
      Caddy, Traefik, or a cloud load balancer. Never send the bearer token over
      untrusted HTTP.
- [ ] **Scoped HTTP clients** — if the registry lists `httpClients[]`, give each
      caller its own token and profile scope instead of sharing one global token.
- [ ] **Request deadlines accounted for** — the gateway allows 10 seconds for complete
      headers and 30 seconds for the request body, then closes the connection with 408.
      Keep reverse-proxy deadlines at least as strict when exposing the gateway remotely.

### Secrets and registry

- [ ] **Vault passphrase** — set `TOOLPORT_SECRET_KEY` and use `secrets.enc`, or
      inject via `TOOLPORT_SECRET_<KEY>` env vars. Prefer prefixed names over bare
      `STRIPE_SECRET_KEY` unless you understand `TOOLPORT_ALLOW_BARE_SECRET_ENV`.
- [ ] **`.env` permissions** — mode `600`, never commit, rotate if leaked.
- [ ] **Registry on a volume** — persist `/data/registry.json`; back up before
      upgrades. A corrupt file is quarantined, not silently wiped (#224).
- [ ] **Disable HITL** — set `humanApproval: false` in the registry (and leave
      team-forced approval off). Without the desktop app's approval broker,
      gated tools **fail closed** with "approval service unreachable".

### Container hygiene

- [ ] **Non-root** — the published image runs as user `toolport` (uid 10001).
- [ ] **GHCR visibility** — make the package public only if you want anonymous
      pulls; otherwise configure registry auth.
- [ ] **Pin the image** — use a digest or version tag in production, not only
      `:latest`, once you have a known-good deploy.
- [ ] **Healthcheck token** — compose passes `TOOLPORT_HTTP_TOKEN` into the
      healthcheck; ensure logs don't echo env vars.

### Runtime expectations

- [ ] **OAuth** — browser OAuth still needs the desktop app. Use API keys /
      pre-vaulted secrets for headless servers.
- [ ] **npx/uvx cold start** — first connect can take up to ~2 minutes while a
      package downloads; this is normal (v1.6.0+).
- [ ] **HTTP downstream MCP** — some remote servers need server-initiated RPC
      outside an SSE `POST` response; those may not work until downstream
      `GET /mcp` listen ships. Prefer stdio or remote servers that answer inline.

## Security

### What already existed (desktop HTTP bridge)

The headless path reuses the same HTTP/OpenAPI server that shipped earlier:
bearer auth, per-client profile scoping, 4 MB request body cap, spawn-command
screening, downstream SSRF guards on OAuth, destructive-tool governance, and
fail-closed approval when the broker is missing. Those paths were hardened in
the v1.5.1–1.5.2 audit batch (#203–#207).

### What is new in 1.6.0

| Area                     | Risk                                                                 | Mitigations in code                                                         |
| ------------------------ | -------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| **Network exposure**     | Anyone with token + network path can invoke all scoped tools         | Non-loopback requires token; scope via `httpClients[]` / profiles           |
| **MCP streamable-HTTP**  | New `POST /mcp`, `GET /mcp` SSE, session ids                         | Random 128-bit session ids, 24h TTL, 4096 session cap, id format validation |
| **Server-initiated RPC** | Downstream can prompt upstream client (sampling, elicitation, roots) | Gated on client capabilities declared at `initialize`; 120s timeout         |
| **Container secrets**    | Env vars in process memory / compose files                           | `TOOLPORT_SECRET_*` prefix; encrypted `secrets.enc` option                  |
| **Long-lived SSE**       | Idle connections, queue growth                                       | Keepalive comments; session cleanup on TTL                                  |

**Known limitations (not bugs, but deploy constraints):**

- No built-in TLS or rate limiting — use a reverse proxy.
- `--insecure-loopback` warns and starts an unauthenticated **local-only** listener:
  any local process (including a malicious web page via browser) can call tools.
  Prefer a token even on localhost if browsers run on the same machine.
- Headless + human approval on = destructive calls blocked, not prompted.
- MCP HTTP test coverage is thinner than the stdio gateway path (see ROADMAP).

### Do you need a separate security audit?

**For a typical solo/small-team LAN or VPN deploy:** a disciplined walk through
the [production checklist](#production-checklist) above is the minimum. You do
not need a third-party audit before shipping 1.6.0 to users who already trust
the desktop app with the same credentials.

**Before internet-facing or multi-tenant production**, do a **focused review**
(not necessarily a full pentest) of:

1. Token handling and TLS termination at the proxy
2. Registry + secrets file permissions on the volume
3. Which servers/tools are enabled (principle of least privilege)
4. Whether HITL and team policies match headless mode

A full external audit makes sense if you are selling headless gateway as a
managed service or putting customer API keys on a shared host. The highest-risk
delta is **operational** (exposing the existing HTTP surface on `0.0.0.0`), not
a wholly new trust model.

Optional internal pass: re-run the gateway HTTP + MCP integration tests, smoke
`POST /mcp` initialize → `tools/list` with and without auth, and confirm
unauthenticated listeners are rejected at startup. Confirm `--insecure-loopback`
works only on loopback when the local-development escape hatch is required.

## Environment Variables

Toolport reads the following environment variables. This is the complete reference across all components.

Prefer the `TOOLPORT_*` names. Every name below still accepts the pre-rename
`CONDUIT_*` alias (for example `CONDUIT_HTTP_TOKEN` continues to work) so existing
headless and Docker configs do not break on upgrade.

| Name                             | Purpose                                                                                             | Default          | Where it applies          |
| -------------------------------- | --------------------------------------------------------------------------------------------------- | ---------------- | ------------------------- |
| `TOOLPORT_ALLOW_BARE_SECRET_ENV` | Opt-in to read bare secret keys (e.g. `STRIPE_KEY`) from the environment.                           | None             | Headless                  |
| `TOOLPORT_CLIENT_ID`             | Identifies the client to the gateway for live profile resolution.                                   | None             | Clients                   |
| `TOOLPORT_DATA_DIR`              | Override the full path to the Toolport config directory.                                            | OS config root   | Everywhere                |
| `TOOLPORT_DEBUG`                 | Enable trace and debug logging.                                                                     | None             | Everywhere                |
| `TOOLPORT_CODE_MODE`             | Force-enable code mode (`toolport_run_script`) even if Settings/registry has it off.                | Off (force)      | Gateway                   |
| `TOOLPORT_DISCOVERY`             | Override discovery mode (`lazy`, `grouped`, `full`).                                                | Registry setting | Everywhere                |
| `TOOLPORT_EMBED_BLEND`           | Semantic search embedding blend weight (float).                                                     | Registry setting | Gateway / semantic search |
| `TOOLPORT_EMBED_ENDPOINT`        | Semantic search embedding endpoint URL.                                                             | Registry setting | Gateway / semantic search |
| `TOOLPORT_EMBED_KEY`             | API key for the semantic search embedding endpoint.                                                 | None             | Gateway / semantic search |
| `TOOLPORT_EMBED_MODEL`           | Semantic search embedding model name.                                                               | Registry setting | Gateway / semantic search |
| `TOOLPORT_HTTP`                  | Direct port override or boolean flag to enable HTTP.                                                | None             | Gateway                   |
| `TOOLPORT_HTTP_HOST`             | Host IP to bind for the HTTP endpoint.                                                              | `127.0.0.1`      | Gateway                   |
| `TOOLPORT_HTTP_PORT`             | Port for the HTTP endpoint (when `TOOLPORT_HTTP` is a boolean).                                     | `8765`           | Gateway                   |
| `TOOLPORT_HTTP_TOKEN`            | Bearer token for HTTP authentication.                                                               | None             | Gateway                   |
| `TOOLPORT_METRICS`               | Opt-in Prometheus `GET /metrics` (`1` / `true` / `yes`). Off by default.                            | Off              | Gateway (HTTP mode)       |
| `TOOLPORT_PROFILE`               | Initial profile value for a scoped client install; live scope is resolved via `TOOLPORT_CLIENT_ID`. | None             | Clients                   |
| `TOOLPORT_REGISTRY`              | Override the path to `registry.json`.                                                               | Config root      | Everywhere                |
| `TOOLPORT_RESULT_BUDGET`         | Byte budget before large tool results get shaped/truncated (0 to disable).                          | `49152`          | Everywhere                |
| `TOOLPORT_SECRET_<KEY>`          | Process env override for a specific scoped secret.                                                  | None             | Headless                  |
| `TOOLPORT_SECRET_KEY`            | Passphrase to activate the `secrets.enc` file backend.                                              | None             | Headless                  |
| `TOOLPORT_SEMANTIC`              | Toggle semantic search (`on` or `off`).                                                             | Registry setting | Gateway / semantic search |
| `CONDUIT_TARGET_TRIPLE`          | Packaged fallback target architecture (internal build-time only).                                   | None             | Desktop                   |

## Notes

- **HITL approvals** need the desktop app’s approval broker. Leave human
  approval off (or expect fail-closed) in pure headless mode.
- **Client config writers** (Cursor/Claude local JSON) still need the desktop
  app or a one-time manual URL in the client config — which is what sandboxed
  setups usually want anyway.
- **Code mode** (`toolport_run_script`) is **on by default** in the registry
  (Settings kill switch / `"codeMode": false`). It is not a security boundary:
  agents supply JS that can call many tools in one round-trip; each call still
  hits the same scope and approval gates. Shared multi-tenant gateways that do
  not want the surface should set `"codeMode": false` in the registry.
- Open WebUI details: [openwebui.md](./openwebui.md).
