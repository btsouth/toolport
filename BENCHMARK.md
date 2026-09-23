# Toolport token benchmark

**Routing MCP servers through Toolport's lazy discovery cut provider-reported total tokens 74-91% at the
SAME task success rate**, measured on a frontier model and graded for _correct answers_,
not just completion. Every task completed correctly in both modes, and the savings grow
as you add servers. The reduction comes from not loading every tool's schema into the
model's context on every request.

Reproduce it yourself: [`benchmark/`](benchmark/).

## Method

- **Two modes**, same tasks, same model:
  - **flat**, every downstream tool exposed directly (`TOOLPORT_DISCOVERY=full`), the normal MCP setup.
  - **lazy**, Toolport advertises 4 meta-tools (`toolport_status`, `toolport_search_tools`,
    `toolport_call_tool`, `toolport_fetch_result`) and the agent searches/calls on demand
    (`TOOLPORT_DISCOVERY=lazy`). (The headline reduction below was originally measured on the
    earlier 3-meta-tool set; 886 was a historical UTF-8 bytes/4 estimate for a
    four-tool definition set, not a current invariant or provider token count.)
- **Model:** GPT-5.5 (frontier, via the Vercel AI Gateway), so model capability is not the
  variable, both modes can actually complete every task.
- **Tasks (5 runs each):** list Stripe products; list Neon projects; list Vercel projects
  (a two-step that needs a team id first).
- **Graded for correctness:** a run counts only if the agent's final answer contains the
  real items from the account, so "completed" can't hide a wrong or "I couldn't" answer.
- **Swept across catalog size** (3 and 6 connected servers) to show how the gap scales.

## Results

Provider-reported end-to-end tokens to complete the three tasks (median of 5 runs), both modes graded. Search calls and their returned content are included:

| Servers | Tools | Flat tokens | Lazy tokens | Reduction | Correct (flat / lazy) |
| ------- | ----- | ----------- | ----------- | --------- | --------------------- |
| 3       | 63    | 179,181     | 47,095      | **74%**   | 15/15 · 15/15         |
| 6       | 183   | 471,775     | 40,354      | **91%**   | 15/15 · 15/15         |

Two things stand out:

- **Identical task success.** Every task completed _correctly_ in both modes, 30/30.
  Lazy discovery did not trade accuracy for tokens.
- **The savings grow with your catalog.** Flat's cost more than doubled as servers went
  3 → 6 (it re-sends every tool schema on every call), while lazy's actually _dropped_
  (47K → 40K), it pays a flat ~450-token meta-tool overhead no matter how many servers
  you connect. Per-request tool-definition overhead: flat **19,002 → 51,533**, lazy a
  constant **451** in those model runs. These historical values depend on that
  tool set and harness.

## Why flat is so expensive

In the measured harness, flat mode exposed every tool schema on **every** LLM call, so a multi-step task paid that
overhead several times before counting any real work, and it climbed with each server.
Other MCP clients may gate or cache definitions differently. Lazy mode advertised a smaller
catalog and searched for what it needed. The
more tools you connect and the more calls a task takes, the wider the gap.

## Measured on a real 14-server catalog

The 63-tool test above is deliberately small. The historical local estimate below
used serialized definition bytes divided by four on a 14-server, **415-tool** catalog.
It describes MCP payload size, not model usage. To reproduce on the current gateway,
capture `tools/list` for the same client in full and lazy modes and pass both JSON
responses to [`benchmark/token-cost.mjs`](benchmark/token-cost.mjs).

|                              | Per request                   |
| ---------------------------- | ----------------------------- |
| Full catalog (historical)    | **≈164,880 token-equivalent** |
| Four meta-tools (historical) | **≈886 token-equivalent**     |
| Reduction                    | **99.5%**                     |

(≈660 token-equivalent / 99.6% for the original three-tool set. Toolport now
measures the actual arrays for each client; there is no fixed lazy floor.)

The cost is dominated by a few large servers:

| Server                     | Tools | Estimated token-equivalent of definitions |
| -------------------------- | ----- | ----------------------------------------- |
| RevenueCat                 | 93    | 42,370                                    |
| GitHub                     | 44    | 27,913                                    |
| Resend                     | 83    | 26,045                                    |
| Cloudflare (observability) | 8     | 5,948                                     |
| Stripe                     | 11    | 5,214                                     |
| Vercel                     | 20    | 5,029                                     |
| Supabase                   | 29    | 4,897                                     |
| (5 more)                   | ...   | ...                                       |

At ≈165k token-equivalent of serialized definitions, this catalog is large.
The actual model context cost depends on the MCP harness, provider serialization,
tool gating, and caching. Toolport's local telemetry now reports exact full and
exposed tool-array bytes per load, plus search response bytes separately.

The estimated average here is ~397 token-equivalent per tool, consistent with the ~387 the
public [calculator](https://toolport.app/calculator) uses.

## Latency: the gateway is not the bottleneck

Tokens are the headline, but a gateway adds a hop, so does it cost you time? Measure
it with [`benchmark/latency.mjs`](benchmark/latency.mjs), which spawns the gateway
against an instant mock downstream so the number is purely Toolport's own overhead
(no model, no network, no API keys):

| Operation                                                    | Median        |
| ------------------------------------------------------------ | ------------- |
| Handshake (one-time, per gateway start)                      | ~21 ms        |
| `tools/list` (lazy, 4 tools)                                 | ~0.2 ms       |
| `toolport_search_tools`                                      | ~0.1 ms       |
| A tool call through Toolport vs. calling the server directly | **+~0.75 ms** |

Toolport adds well under a millisecond to a tool call. Real MCP servers take tens to
hundreds of ms each (a process or a network API), so that overhead is noise, and it
buys the ~90% token reduction above. (Numbers from a dev laptop over 200 iterations;
run it on yours: `node benchmark/latency.mjs`.)

## Honest caveats

- **Scope:** one frontier model (GPT-5.5), one machine, three read-only "list" tasks, 5
  runs each. Treat the _direction_ (a large, consistent reduction at equal correctness) as
  the signal, not the exact percentage. Serialized UTF-8 byte measurements are exact
  at the MCP boundary; bytes/4 token equivalents are estimates. The end-to-end
  result table uses provider-reported model usage.
- **Correctness is graded, not eyeballed.** A run counts only if the answer contains the
  account's real items, so the 30/30 is "right," not just "finished." Token counts come
  from the model's reported `usage`.
- **Lazy adds search round-trips.** The total-token figures are already net of that. The
  trade-off only pays off past a handful of tools; for a single tiny server it's overkill.
- **Savings scale with your tool surface**, and that's the point: 74% at 63 tools, 91% at
  183, 99.5% definition-overhead at 415. The more you connect, the wider the gap.
