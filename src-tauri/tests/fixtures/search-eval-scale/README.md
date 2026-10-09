# Public MCP search evaluation

The catalog has 1,707 real tools from 18 public servers. `sources.json` pins
source revisions, package versions where available, licenses and selection.
Only namespacing is added. Descriptions and input schemas are preserved.
Two duplicate public Stripe names retain their first definition; their names
and the deterministic Clerk filler selection are recorded in sources.
No installed Toolport catalog, credentials or customer data was used.

Round 1 froze 408 manually authored requests: 285 dev and 123 held-out. The
held-out queries have since been seen, so round 2 treats **both files as dev**.
The original files, labels, historical family split and hashes remain unchanged
in `frozen.sha256`. Keeping `held_out.json` records that provenance; it is no
longer blind evidence. Original compact capture hashes remain in `sources.json`.
Prettier normalized fixture whitespace after freeze with parsed equality checked.

A separate author supplies the new blind set only to the lead. The ordinary
Rust suite skips blind scoring unless `TOOLPORT_SEARCH_BLIND_INTENTS` names a
JSON array with the same intent shape as `dev.json`. Run the slot on devbox:

```bash
TOOLPORT_SEARCH_BLIND_INTENTS=/absolute/path/new-blind-intents.json \
  cargo test --locked --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features test-support --bin toolport-gateway \
  search_scale_blind_quality_gate -- --nocapture
```

The original 66-intent gate remains a regression check. The combined 408-intent
set is an author-written historical diagnostic, with its lower scores reported;
it no longer supplies the ranking acceptance target. The development sanity gate
uses dev-v2 recall at 3 and 10 plus no-match honesty. The blind scoring slot
remains separate, and an absent path prints a skip notice.

Top-1, top-3 and MRR use 338 resolvable requests with labelled alternatives.
Ambiguity honesty requires low confidence and at least two labelled returned
candidates. No-match precision and recall use `total == 0`; no-match honesty also
accepts a low-confidence menu. Undefined precision is null, never a perfect score.
The default adaptive menu starts at three and widens to 25 for uncertain queries.
Explicit limits are honored. This evaluates retrieval, not native tool execution,
model choice, owner decisions or current provider behavior.

Run the ignored `search_scale_measure` test with `SEARCH_SCALE_SPLIT=both` and
`SEARCH_SCALE_OUTPUT=/external/report.json`. Optional `SEARCH_SCALE_LIMIT` selects
a fixed limit; zero or absence measures the adaptive policy. The output includes
raw response text, exact `o200k_base` token counts, warm ranker/projection latency,
first-dispatch latency and cold index build time. `held_out` selects only the old
held-out dev subset; it never opens a new blind file.

Run the ignored `search_scale_resources` test in `--release` with
`SEARCH_RESOURCE_OUTPUT=/external/resources.json` for Linux process RSS before and
after catalog parsing, cold index construction and warm search. It deliberately
runs without token counting or dispatch caches. Compare separate processes with
`SEARCH_LEXICAL_ONLY=1` for the lexical baseline. These are benchmark-only controls,
not shipped settings. Model alternatives can use `SEARCH_STATIC_MODEL_DIR` as
documented in `src-tauri/assets/search/README.md`. Heavy work runs on devbox.

The public AutomateLab-tech capture is CC-BY-4.0, with attribution and exact links
in `sources.json`. Other schemas retain their listed upstream licenses. Snapshots
are offline and CI does not contact providers or execute third-party packages.
Labels were authored from public definitions. They are development evidence,
not observed model traffic or an independent benchmark.

Round 3 adds an independently authored development set, `dev-v2.json`: 450
requests, including 330 ranked, 70 ambiguous and 50 unavailable operations,
covering 493 distinct expected tools. Its author read the catalog rather than
the ranker or older labels. Expected tools must perform the requested job with
inputs the query can supply; a related endpoint is not a correct answer.

Round 4 replaces the request-hash split with connected expected tool families.
A family is the server plus the first resource token after the operation token,
with camelCase splitting, repeated namespace removal and plural stemming. All
labelled alternatives are joined, then the lowest 30% of component hashes are
self-check. No expected tool or resource family appears in both subsets.
`dev-v2-split.json` freezes 341 tuning and 109 self-check requests; both it and
`dev-v2.json` are pinned in `frozen.sha256`. These are development checks, not
unseen data. Only the lead sees the blind set.

The harness normalizes every input schema with production's schema compatibility
normalizer and local reference inliner. Recall is measured at 1, 3, 5, 8, 10, 12 and 25.
Confident precision is correct ranked #1 answers divided by all confident answers;
confident coverage is confident answers divided by all requests. Ambiguous or
unavailable requests marked confident count against precision. Correct #1 answers
flagged uncertain are also counted and reported. Undefined precision is null.
The family self-check floors are recall at 3 >=55%, recall at 10 >=75%,
no-match honesty >=95%, and explicit rejection precision >=90% when defined.
These sit below development measurements and do not imply blind acceptance.
Confidence precision is diagnostic only; it no longer gates payloads or quality.
The menu token gate uses o200k: p95 <=600 for the menu and <=3212 for the entire
response, matching upstream 1fec712a on these 450 normalized requests.

Use `SEARCH_SCALE_SPLIT=external` and `SEARCH_SCALE_INTENTS` with an absolute
path to measure supplied development data. Ranking diagnostics use depth 25;
production always returns the first 10 ranked candidates, filling a short menu
from the visible catalog when evidence is absent. Rows carry the exposed name,
a short description and every required parameter name. Row 1 also carries the
complete input schema. Repeated schema fragments use local definitions without
losing constraints or documentation; exact-name lookups preserve the original
complete definition and existing lossless paging. Description lines allow 100
characters with small schemas and 24 with large schemas to meet the token budget.
The informational low_confidence flag never changes menu size or schema hydration.

The `search-static` Cargo feature is default on for ordinary desktop builds.
With `--no-default-features`, pass `--features test-support,search-static` to
measure it on and `--features test-support` for a model-free binary.

Low confidence reports close scores or missing query evidence. It is not a
calibrated probability and never directs a client to trust the first candidate.
Development measurements are not a guarantee for new catalogs or the lead's
blind set. Live client discovery and valid dispatch are the execution target.
