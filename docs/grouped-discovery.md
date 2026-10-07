# Grouped discovery mode

Toolport has three tool-discovery modes, selected per client by the
`TOOLPORT_DISCOVERY` environment variable (falling back to the registry's
`lazy_discovery` setting when unset):

| Mode             | `tools/list` advertises                                                                                                                                                                                                  | Best for                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------- |
| `lazy` (default) | The core meta-tools (`toolport_status`, `toolport_search_tools`, `toolport_call_tool`, `toolport_fetch_result`), plus the Code Mode tool when it is enabled (off by default, so the default exposes the four core tools) | Capable models: minimal, constant context regardless of server count                     |
| `grouped`        | The core meta-tools **plus** a per-server `help_<server>` browse tool                                                                                                                                                    | Weaker / local models: an _enumerable_ server choice instead of inventing a search query |
| `full`           | `toolport_status`, `toolport_fetch_result`, and the scoped namespaced catalog (`server__tool`)                                                                                                                           | Debugging, or small setups where full schemas are affordable                             |

These are the core definitions. Code Mode, confirmation, agent-control,
and negotiated MCP Apps settings can add or change definitions for a particular
client. The exact measurement uses that client's resulting tool array.

Measured with `tiktoken o200k_base`, the four core tools measure about 940 tokens
including the gateway's instructions, and enabling Code Mode adds `toolport_run_script`
for a surface that stays under 2,000. The floor is flat regardless of server count, so lazy discovery costs
more than a flat client until the catalog passes roughly 10 to 25 tools.

## Why grouped exists

`lazy` mode is ideal for a strong model: it exposes a single
`toolport_search_tools` and the model invents a query to find what it needs. A
weaker or local model (e.g. a 7B) often struggles to invent a good query from a
blank slate.

Grouped mode keeps context small but replaces the blank-slate search with an
**enumerable** choice: the model sees one `help_<server>` tool per connected
server (`help_github`, `help_stripe`, ...). It picks a server by name, calls
`help_<server>` to list that server's tools (optionally filtered by a `query`),
then runs the chosen tool with `toolport_call_tool` using the exact name the
listing returned.

Context cost is roughly `4 + (number of servers)` tool definitions plus any Code
Mode tools, so grouped
mode is the sweet spot for a **handful of tool-heavy servers** (e.g. Stripe's
587 tools collapse to one `help_stripe`). It is not worth it for many tiny
servers, where the per-server tools approach the full catalog.

## How it works (and why it's safe)

- `help_<server>` is a thin rewrite: internally it runs `toolport_search_tools`
  scoped to that server, reusing the exact ranking, truncation, and schema
  handling. `help_<server>(query: "refund")` on Stripe returns
  `stripe__create_refund`, ready to call.
- Tool execution still goes through `toolport_call_tool`, so the audited call
  path is unchanged: content-defense screening, human-in-the-loop approval, the
  destructive-tool confirm gate, per-client scope enforcement, and result
  shaping all apply exactly as they do in lazy and full mode. Grouped mode adds
  **no new execution surface.**
- The `help_<server>` tools are scoped to the client's allowed servers, so a
  registered HTTP client never sees a browse tool for a server outside its
  scope.

## Catalog exposure measurements

The headline says **tokens saved**. Hover it for the method: offline
`cl100k_base` tokenization of the serialized full and exposed MCP tool arrays
under the same client scope and policy, minus every recorded discovery response.
The signed total includes extra catalog exposure and can be negative on small
catalogs or after many searches. It describes this MCP boundary, not provider
billing, prompt-cache savings, or savings over a client's native tool search.

A distinct scoped full/exposed catalog hash is counted once per client session,
even when `tools/list` is reloaded concurrently. A changed surface earns a new
exposure; returning to a previously seen surface does not. Stdio connections and
HTTP session records own their counters. Session expiry/reconnection starts a
new counter. Sessionless modern HTTP conservatively counts once per listener
lifetime and client label plus scoped catalog hash because it has no conversation
session. This counts catalog exposure, not inferred conversation turns.

Counts use the actual compact serialized arrays, including JSON punctuation;
there is no fixed per-tool average. Tokenization and vocabulary initialization run on the existing background
telemetry writer. The bundled vocabulary needs no runtime download. The request
thread serializes and hashes the catalog, or queues the discovery text; repeated
catalogs skip tokenization. The existing bounded queue's synchronous fallback
also applies if the writer is unavailable or overloaded. Raw text used for
tokenization is removed before persistence.

New rows have `v: 3` in `savings-v3.jsonl`. Exact byte measurements and signed
token totals survive rotation. Old pre-2.0 `savings.jsonl` estimates and
`savings-v2.jsonl` bytes/4 estimates still load, but are shown separately and
excluded from the headline. They cannot be reconstructed as tokenizer measurements.
Older gateways can rotate their historical files without touching new records.
Clearing Activity removes all three files; it does not credit another reload in
an existing session. Team server attribution remains a catalog-only allocation, not net savings.

Discovery response text includes its lead and guidance text. Every response
recorded by `record_discovery` is subtracted; a full-mode session does not incur
lazy discovery costs. Tool-result retrieval is ordinary task output rather than
catalog discovery and is not treated as avoided definitions.

Prometheus exposes the headline as the signed `toolport_tokens_saved` gauge,
with `tokenizer="cl100k_base"` and `method="net_of_discovery"` labels. Its
component token counters are separate. The existing `toolport_tokens_saved_total`
and `toolport_tool_definition_tokens_estimated_avoided_total` retain historical
estimates; neither includes new tokenizer measurements.

## Enabling it

Per client, set the env in that client's MCP server config:

```
TOOLPORT_DISCOVERY=grouped
```

## Roadmap

Grouped is the stateless, universally-compatible foundation. A future opt-in
enhancement ("dynamic drill-in") could, for clients that reliably honor
`notifications/tools/list_changed`, swap in a server's real flat tools on
activation so weak models call them with top-level arguments. That is gated on
verified client support because a client that caches `tools/list` for the
session would break it; grouped mode works everywhere today.

## Tokenizer cost

On the shared Linux x64 devbox, matching release builds on the supervisor base
grew from 30,285,720 to 32,199,592 bytes (+1,913,872 bytes, 6.3%). A release probe
initialized the bundled cl100k_base vocabulary in 38.2 ms on the telemetry worker;
a preserved 150,558-byte catalog capture took 6.1 ms to count. This is the trimmed
audit capture, not a reconstruction of the audit's full 166,913-byte response.
All 15 preserved compact catalog captures matched Python tiktoken with the same
bundled vocabulary offline.

Five paired 200-iteration gateway runs had median tools/list latency 0.284 to
0.271 ms, search 0.344 to 0.374 ms, and routed calls 0.243 to 0.241 ms. All paired
warm p95 values remained below 0.5 ms. Median handshake time was 52.3 to 52.4 ms;
cold catalog-ready time was 347 to 311 ms. Shared-machine load makes startup
differences noisy, not evidence of a tokenizer speedup. Both variants prime cold
discovery with tools/list, as the supervisor requires.

The 10,000-row debug audit benchmark's cached aggregation median was 0.460 to
0.527 ms (uncached: 52.1 to 55.5 ms; record enqueue: 5.472 to 5.482 microseconds).
The binary growth is acceptable for offline measurements, with vocabulary
initialization and counting deferred to the telemetry worker.
