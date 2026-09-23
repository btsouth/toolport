# Toolport token benchmark

Compares lazy discovery with full catalog exposure in the same agent harness.
It runs the **same tasks** in both modes and records model-reported tokens,
tool calls, and completion. Client gating and provider caching can differ.

It's framed the same way as the [mcpico benchmark](https://github.com/lxg2it/mcpico/blob/main/BENCHMARK.md),
so the numbers are directly comparable: lazy mode makes **more tool calls**
(search round-trips) but may use fewer model tokens when the harness sends a
large full catalog on each request.

## No-model catalog report (`token-cost.mjs`)

To measure the current MCP catalog without a model, capture `tools/list` JSON
responses for the same client and configuration in full and lazy/grouped modes.
`token-cost.mjs` parses each response and measures the canonical reserialized
UTF-8 `tools` array. JSON escapes or numeric spelling can differ from the
captured bytes. It reports a clearly labeled bytes/4 token equivalent; the
gateway's local telemetry measures its own emitted arrays directly.

`compare-local.mjs` also times repeated `tools/list` calls after the catalog is
ready. Use `--products=toolport --sizes=1700 --iterations=20 --json` for a large
fixture regression check; the JSON records median and p95 list latency.

```bash
node benchmark/token-cost.mjs full-tools-list.json lazy-tools-list.json
```

A single cache or tools-list file prints only its own size; it cannot establish
exposure avoided. Search response bytes and provider model usage are separate.

## Run the agent-loop benchmark

```bash
# 1. Build the gateway
npm run build:gateway        # or: cargo build --release --bin toolport-gateway

# 2. Connect a few servers in Toolport, and edit the TASKS in bench.js to match them.

# 3. Start a local OpenAI-compatible LLM
#    LM Studio: load a model and start the server (default http://localhost:1234)
#    Ollama:    it serves an OpenAI-compatible API on http://localhost:11434/v1

# 4. Run
node benchmark/bench.js
MODEL="qwen2.5-7b-instruct" node benchmark/bench.js
LLM_URL="http://localhost:11434/v1/chat/completions" MODEL="qwen2.5:7b" node benchmark/bench.js
```

## Deterministic latency and startup report (`latency.mjs`)

This offline benchmark uses the bundled mock MCP server to isolate Toolport's own
cost. It measures the gateway handshake, time until the downstream catalog is
searchable, lazy `tools/list`, search, and routed-call overhead versus calling the
same server directly.

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml --bins
node benchmark/latency.mjs 200
node benchmark/latency.mjs 200 --json
node benchmark/latency.mjs 200 --check
```

`--json` is intended for storing and comparing results across commits and competing
local gateways. `--check` enforces the deliberately generous regression ceilings in
`latency-budget.json`; tighten them only with evidence from multiple machines.

## Native MCP pagination regression (`mcp-native.mjs`)

This end-to-end harness launches Toolport over stdio with a deterministic MCP
fixture that paginates tools, resources, resource templates, and prompts at two
items per page. It verifies that Toolport discovers every downstream page,
exposes the complete aggregated lists without forcing existing clients to
paginate, can invoke/read/fetch entries that appeared only on the final page,
routes expanded template URIs, and forwards `completion/complete` for prompt
and resource-template references (with namespaced prompt names remapped to the
downstream name). It also checks first-writer collision ownership across
servers and repeated-cursor safety.

Resource templates refresh on `notifications/resources/list_changed` because
MCP defines no separate templates list-change notification. Incomplete
downstream pagination keeps the previous complete snapshot (covered in Rust
unit tests and documented here).

### Resource subscriptions (SOU-394)

Toolport always advertises `resources.subscribe` and proxies
`resources/subscribe` / `resources/unsubscribe` through the same first-writer
ownership path as `resources/read` (concrete URI, then template expansion),
with the same HTTP scope rules. Downstream subscribe is reference-counted per
URI; `notifications/resources/updated` is forwarded only to subscribed
upstream clients (stdio + HTTP SSE), and is kept distinct from
`resources/list_changed`. Unknown or out-of-scope URIs fail closed. The fixture
exposes `emit_resource_updated` so the harness can trigger a real downstream
update after subscribe.

```bash
npm run build:gateway
npm run bench:mcp-native
npm run bench:mcp-native -- --json
```

## Side-by-side local gateway comparison (`compare-local.mjs`)

This is the competitive harness. It generates one deterministic MCP catalog, launches
the same dependency-free fixture server behind each gateway, and drives both products
through their public stdio MCP interfaces. It compares:

- cold start until a known capability is searchable;
- the count and estimated token cost of always-exposed gateway tools plus MCP
  initialization instructions;
- search median/p95, returned-payload size, recall@K, and mean reciprocal rank
  over shared queries with plausible near-match distractors;
- schema-ready@K plus the total response tokens and round trips needed to obtain
  the exact input schema and make a discovered result ready to invoke (so a
  smaller first response is not rewarded when it forces another lookup);
- routed-call median/p95 overhead against calling the fixture directly.

Ratel Local is pinned in `compare-local.config.json` and installed into the operating
system's temporary directory, not this repository:

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml --bins
npm run bench:compare -- --install-ratel

# Subsequent offline/cached runs
npm run bench:compare
npm run bench:compare -- --sizes=25,100,500 --iterations=200
npm run bench:compare -- --products=toolport --check
npm run bench:compare -- --products=toolport --profile-calls
npm run bench:compare -- --settle-ms=1000
npm run bench:compare -- --json --out=benchmark/local-compare.json

# Toolport-only regression run
npm run bench:compare -- --products=toolport
```

The comparison prefers Toolport's release binary and records the selected path,
SHA-256, build profile, Git revision, and dirty-worktree state in JSON. A debug
binary is accepted for development smoke tests, but do not publish performance
numbers from a debug-vs-release run. Each catalog size also gets a disposable
Toolport data directory, so existing audit logs, traces, caches, and result
cursors cannot influence the timings. `--check` enforces the cross-machine
regression ceilings in `compare-local.config.json`; it checks Toolport only,
even when a competitor is included in the report.

`--profile-calls` enables Toolport's opt-in routed-call stage timer and adds
preflight, downstream, post-processing, audit, and total p50/p95 timings for
successful routed calls to the report. The gateway writes those diagnostics to
stderr, never the MCP protocol stream, and includes no arguments or result data.
Keep it off for headline latency comparisons because emitting one diagnostic
line per call adds its own measurement overhead.

The report deliberately separates deterministic gateway mechanics from model-graded
agent accuracy. Use `bench-sweep.mjs` for end-to-end model tasks; do not present this
synthetic retrieval fixture as an agent-accuracy benchmark.

## What it reports

Per task and as totals, for each mode:

- **tokens**: summed from the LLM's `usage.total_tokens` across every request in the agent loop. This is the headline metric.
- **tool calls**: how many tool invocations the agent made (lazy mode includes its search calls).
- **completion**: whether the agent produced a final answer. Eyeball the printed answers for actual correctness; this is a coarse success flag, not a grader.

And the summary line: tools exposed (flat vs 3), total-token delta as a percent, and tasks completed.

## Reading the results honestly

- **Small sample.** A handful of tasks and single runs are noisy. Run it a few times; treat the _direction_ as the signal, not the exact percentage.
- **The trade-off is real and intentional.** Lazy mode trades extra tool calls (search → call) for fewer total tokens. The table shows both so you're not hiding the round-trip cost.
- **Flat mode may error** on small models, dumping every tool schema can overflow the context window. That's a finding, not a bug: it's exactly the failure lazy discovery avoids. The harness records it as an error rather than crashing.
- **Savings concentrate on smaller models** (mcpico saw ~60% on a 9B, ~8% on a 35B). Run it on a small _and_ a mid model to show that, it's the local-model story.

## Caveats

- Token counts depend on your runtime reporting `usage` (LM Studio and Ollama do).
- Tasks must match servers you actually have connected; the defaults assume Resend / Neon / Vercel.
- This measures the agent loop, not just the static tool-definition size. The static
  size (3 schemas vs hundreds) is the upper bound; the loop shows what you actually pay.
