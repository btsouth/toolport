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
  - **lazy**, Toolport advertises a small, fixed set of meta-tools and the agent searches and
    calls on demand (`TOOLPORT_DISCOVERY=lazy`). With Code Mode on (the default) that is seven
    tools; the core four are `toolport_status`, `toolport_search_tools`, `toolport_call_tool`,
    and `toolport_fetch_result`.
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
  (47K → 40K), it pays a fixed tool-definition floor no matter how many servers you
  connect. Measured with `tiktoken o200k_base`, the lazy floor is about 2,200 tokens
  with Code Mode on (about 940 for the core four tools) and is flat regardless of
  server count.

## Claude Code with a 1,707-tool catalog

A second test runs real AI clients against a large fixture catalog instead of calling a
model directly. It uses Toolport 2.0.0-preview.9 (release build of `639fc0af`), measured
2026-10-11.

- **Catalog:** 1,707 tools from public MCP server schemas across 18 namespaces, served by
  local mock servers. No real accounts or services.
- **Tasks:** 30 fixed development tasks, one run per task in each mode. A task passes only
  if the client makes the expected downstream calls with the expected arguments; the
  scorer is strict and unchanged, and no failure was rescored.
- **Clients:** Claude Code 2.1.296 with `claude-haiku-5-5`, and opencode 1.19.0 with
  `deepseek-v4.1-flash`.
- **Modes:** Full advertises the whole catalog (Claude Code can still use its own tool
  search over it). Lazy advertises Toolport's search and call tools.
- **Tokens:** cumulative input the client reported, including cache reads and writes.
  Runs that failed still count; missing usage is not counted as zero.

| Client      | Mode | Passed | Mean reported input |
| ----------- | ---- | ------ | ------------------- |
| Claude Code | Full | 25/30  | 132,372             |
| Claude Code | Lazy | 28/30  | 24,505              |
| opencode    | Full | 0/2    | not reported        |
| opencode    | Lazy | 27/30  | 14,540              |

In Claude Code, Lazy used **81.5% less reported input** than Full and passed three more
tasks. opencode with the full catalog did not finish either of two bounded attempts (one
timed out, one was rejected by the provider), so no opencode reduction can be computed.

Failures include three runs where the fixture's own Cloudflare schema is invalid and five
tied to an ambiguous Supabase prompt; both affect Full and Lazy runs. Each cell is one
run per task on a development split, not a held-out evaluation. This does not show the
same percentage for other clients, models or catalogs, and it says nothing about billing,
quota use or latency. In 2.0, Codex and Cursor default to Full. The driver is on the
[`bench/live-clients`](https://github.com/btsouth/toolport/blob/bench/live-clients/docs/live-client-benchmark.md)
branch.

## Why flat is so expensive

In the measured harness, flat mode exposed every tool schema on **every** LLM call, so a multi-step task paid that
overhead several times before counting any real work, and it climbed with each server.
Other MCP clients may gate or cache definitions differently. Lazy mode advertised a smaller
catalog and searched for what it needed. The
more tools you connect and the more calls a task takes, the wider the gap.

## Measured tool-definition cost

The 63-tool test above is deliberately small. To measure the static catalog without a
model, capture `tools/list` and the `initialize` instructions for the same client in
full and lazy modes and pass the JSON responses to
[`benchmark/token-cost.mjs`](benchmark/token-cost.mjs). Measured with
`tiktoken o200k_base` (the GPT-4o/GPT-5 family tokenizer) on a registry of up to 20
servers, 416 tools in full mode:

| Advertised set               | Tools | Tools + instructions (o200k) |
| ---------------------------- | ----- | ---------------------------- |
| Full catalog (20 servers)    | 416   | ≈38,800                      |
| Lazy, Code Mode on (default) | 7     | ≈2,200                       |
| Lazy, core four only         | 4     | ≈940                         |

The lazy floor is flat: about 2,200 tokens whether one server or twenty are connected.
Because the floor is fixed, lazy discovery only pays off once the catalog it replaces is
large enough. The same sweep measured 78% fewer tool-definition tokens at 5 servers, 90%
at 10, and 95% at 20; savings start at roughly 10 to 25 tools, and a single small server
(fetch or time) costs slightly more with lazy mode than without it. The in-app Activity
figure is a separate bytes/4 estimate; the table above is tokenizer output.

## Latency: the gateway is not the bottleneck

Tokens are the headline, but a gateway adds a hop, so does it cost you time? Measure
it with [`benchmark/latency.mjs`](benchmark/latency.mjs), which spawns the gateway
against an instant mock downstream so the number is purely Toolport's own overhead
(no model, no network, no API keys):

| Operation                                                    | Median        |
| ------------------------------------------------------------ | ------------- |
| Handshake (one-time, per gateway start)                      | ~21 ms        |
| `tools/list` (lazy)                                          | ~0.2 ms       |
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
- **Savings scale with your tool surface**, and that's the point: 74% at 63 tools and 91%
  at 183 end-to-end, and 78% at 5 servers, 90% at 10, 95% at 20 for the static tool
  definitions. The more you connect, the wider the gap.
