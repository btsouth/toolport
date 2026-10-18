# Toolport token benchmarks

Toolport's lazy discovery replaces every downstream tool definition with four small
search and call tools. Whether that saves tokens depends on the client:

- **Clients that send every tool definition on every request** (opencode 1.x, and the
  GPT-5.5 harness below) see large reductions at the same accuracy.
- **Clients with their own tool search** (Claude Code, Codex, Cursor, opencode 2.x)
  already defer definitions. Toolport's Auto mode gives Codex and Cursor the full list,
  and Claude Code too at 120 tools or fewer. In the REA test below, Claude Code at 207 and
  375 tools used more input through Toolport than with its own search.

Reproduce it yourself: [`benchmark/`](benchmark/).

## REA (Reverse Engineer Anything), 2026-10-18

[REA](https://github.com/morluto/rea) 6.7.0 with Ghidra 12.1.4: 207 tools, about 366K
Claude tokens of definitions. Two stripped x86-64 crackmes (derive a serial, recover a
hidden password), graded by running the real binary with the agent's answer. Five runs
per task per setup, Toolport 2.2.0 in its default mode for each client, REA's own skill
installed as `rea setup` installs it. Input tokens include cache reads and writes,
summed over the session.

OPENCODE_SECTION

**Claude Code 2.1.286, Sonnet 5.5.** "Every definition" is Claude Code with
`ENABLE_TOOL_SEARCH=false`, standing in for a client without tool search. "REA + 11" adds
playwright, chrome-devtools, github, filesystem, memory, sequential-thinking, notion,
context7, mongodb, git, fetch and time (375 tools).

| Setup | Solved | Startup tokens | Input per task | Cost per task |
| ----- | -----: | -------------: | -------------: | ------------: |
| REA, every definition | 10/10 | 365,912 | 2,052,477 | $0.70 |
| REA, Claude Code tool search | 10/10 | 19,098 | 187,063 | $0.20 |
| REA through Toolport | 10/10 | 15,786 | 255,749 | $0.22 |
| REA + 11, Claude Code tool search | 10/10 | 21,859 | 179,121 | $0.19 |
| REA + 11 through Toolport | 10/10 | 15,788 | 261,361 | $0.25 |

Every run in every setup solved its task. Against every definition, Toolport used 8.0x
fewer input tokens. Against Claude Code's own search it used 37% and 46% more: each
Toolport run loaded REA's skill (about 6K tokens, carried on later requests) because REA's
tools were not visible by name, and made two to five searches. These are short tasks
(about 30 seconds, 5 to 10 tool calls) on one model; cost is list price with prompt
caching across runs.

## GPT-5.5 harness: method

- **Two modes**, same tasks, same model:
  - **flat**, every downstream tool exposed directly (`TOOLPORT_DISCOVERY=full`), as a client without its own tool search sees it.
  - **lazy**, Toolport advertises a small, fixed set of meta-tools and the agent searches and
    calls on demand (`TOOLPORT_DISCOVERY=lazy`). With Code Mode off (the default) that is the
    four core tools: `toolport_status`, `toolport_search_tools`, `toolport_call_tool`, and
    `toolport_fetch_result`; enabling Code Mode adds `toolport_run_script`.
- **Model:** GPT-5.5 (frontier, via the Vercel AI Gateway), so model capability is not the
  variable, both modes can actually complete every task.
- **Tasks (5 runs each):** list Stripe products; list Neon projects; list Vercel projects
  (a two-step that needs a team id first).
- **Graded for correctness:** a run counts only if the agent's final answer contains the
  real items from the account, so "completed" can't hide a wrong or "I couldn't" answer.
- **Swept across catalog size** (3 and 6 connected servers) to show how the gap scales.

## GPT-5.5 harness: results

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
  connect. Measured with `tiktoken o200k_base`, the lazy floor is about 870 tokens
  with Code Mode on and about 500 for the core four tools, and is flat regardless of
  server count.

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

| Advertised set            | Tools | Tools + instructions (o200k) |
| ------------------------- | ----- | ---------------------------- |
| Full catalog (20 servers) | 416   | ≈38,800                      |
| Lazy, Code Mode on        | 5     | ≈870                         |
| Lazy, core four (default) | 4     | ≈500                         |

The lazy floor is flat: about 500 tokens with Code Mode off (about 870 with it on), whether one server or twenty are connected.
Because the floor is fixed, lazy discovery only pays off once the catalog it replaces is
large enough. The same sweep measured 78% fewer tool-definition tokens at 5 servers, 90%
at 10, and 95% at 20; savings start at roughly 10 to 25 tools, and a single small server
(fetch or time) costs slightly more with lazy mode than without it. The in-app Activity
figure uses cl100k_base, net of discovery responses and counted once per session
and catalog hash. Historical bytes/4 estimates are excluded; the table above
is tokenizer output.

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
hundreds of ms each (a process or a network API), so that overhead is noise. (Numbers from a dev laptop over 200 iterations;
run it on yours: `node benchmark/latency.mjs`.)

## Honest caveats

- **Scope:** the GPT-5.5 harness is one frontier model, one machine, three read-only
  "list" tasks, 5 runs each, against a baseline that sends every definition on every call.
  Treat the _direction_ (a large reduction at equal correctness for such clients) as the
  signal, not the exact percentage. Clients with their own tool search are a different
  comparison; see the REA results. Serialized UTF-8 byte measurements are exact
  at the MCP boundary; bytes/4 token equivalents are estimates. The end-to-end
  result table uses provider-reported model usage.
- **Correctness is graded, not eyeballed.** A run counts only if the answer contains the
  account's real items, so the 30/30 is "right," not just "finished." Token counts come
  from the model's reported `usage`.
- **Lazy adds search round-trips.** The total-token figures are already net of that. The
  trade-off only pays off past a handful of tools; for a single tiny server it's overkill.
- **Against clients that load everything, savings scale with your tool surface**: 74% at 63 tools and 91%
  at 183 end-to-end, and 78% at 5 servers, 90% at 10, 95% at 20 for the static tool
  definitions. The more you connect, the wider the gap.
