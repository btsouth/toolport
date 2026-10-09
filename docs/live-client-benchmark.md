# Live client discovery benchmark

This opt-in driver measures real model turns against an isolated Toolport gateway
and a deterministic public catalog mock. It never uses the installed gateway,
changes discovery defaults, or copies credentials. Outputs belong outside the repo.

Build the baseline gateway with `test-support` and no default features. Supply the
1,707-tool public `catalog.json` and only `dev.json` from `feat/search-eval-scale`.
The 30 requests use development intents, fictional arguments, three multi-step
flows, and three no-match requests. No other evaluation split is read.

```sh
node scripts/live-client-bench.mjs \
  --catalog /absolute/fixture/catalog.json --dev /absolute/fixture/dev.json \
  --gateway /absolute/bin/toolport-gateway \
  --executable /absolute/bin/claude --client claude-code --mode lazy \
  --out /absolute/private-evidence/matrix
```

Use `claude-code`, `codex`, or `opencode`, and `full`, `lazy`, or `grouped`.
Each run needs a new output subdirectory. `--tasks r4-196` selects one request;
`--limit 2` bounds pilots; `--model` overrides the CLI model. Claude defaults to
Haiku, keeps native ToolSearch, and caps each task at $0.50. Other clients use
their default models. `--timeout` sets a per-task millisecond deadline, default
90 seconds. Two consecutive failures without model usage stop that invocation.

Normal runs warm and verify the complete 1,707-tool catalog before starting the
CLI. `--delay 3000`, `8000`, or `15000` skips warming and delays each downstream's
first tools/list response for cold-catalog acceptance. `--start` runs one trivial
turn for reported session input. `--notification` triggers tools/list_changed
after a Slack call and asks the model to find the new capability in the same turn.
The notification probe is separate from development-task scoring.

Claude uses strict per-run MCP configuration. OpenCode uses a temporary config
directory with its own normal CLI auth mechanism. Codex ignores user config and
uses inline MCP overrides. On Linux, bubblewrap gives its normal home a disposable
writable overlay, with the original auth and config files mounted read-only.
The CLI reads its existing login directly; no auth file is copied or refreshed.
Runtime state and logs use temporary paths. Metadata snapshots record concurrent
host changes, which do not imply a write through the isolated mount. Codex needs
unprivileged overlayfs and bubblewrap; missing support is a failure, never a
fallback to writable real-home operation. Cursor is excluded until an isolated
per-run MCP mechanism is verified.

Codex marks this disposable MCP server required so an optional startup grace
cannot silently omit it. The default invocation retains Codex's approval policy.
`--fixture-approvals` opts into the CLI's approval bypass for this canned server;
the entire host filesystem stays read-only except the run's temporary directory,
evidence output, and disposable Codex overlay. This distinguishes task performance
from noninteractive approval-policy failures. It is not production acceptance of
the CLI's normal approval settings.

The private output records model usage, time, gateway discovery requests, actual
downstream calls, JSON Schema validity, requested argument values, wrong calls,
notification delivery, and re-list counts. Invalid public schemas fail strict
validity without changing the served catalog. Dev-approved refund alternatives
retain their distinct input shapes. Successful multi-step tasks need the intended
order and successful no-match tasks need a completed turn without downstream
calls and an explicit unavailable response. Wire payload bytes are not model tokens. Aggregate usage is not peak
context, and cache categories differ across providers; retain each raw usage
object when comparing results.

Run `node --test scripts/live-client-bench.test.mjs` for scorer regressions.
Raw CLI outputs, gateway data, private reports, and auth artifacts must never be
committed. The deterministic fixture cannot establish live-service or GUI success.
