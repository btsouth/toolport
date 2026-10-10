# Live client discovery benchmark

This opt-in driver runs authenticated CLI clients against a disposable gateway and a deterministic local mock. It uses the public 1,707-tool catalog and only the search evaluation development split. Results measure real client discovery and dispatch against fictional data, not production services.

Supply an existing gateway build and authenticated client executable:

```sh
node scripts/live-client-bench.mjs \
  --catalog /path/to/catalog.json --dev /path/to/dev.json \
  --gateway /path/to/toolport-gateway --executable /path/to/client \
  --client codex --mode lazy --out /path/outside/the/repo
```

Clients: `claude-code`, `codex`, `cursor`, `opencode`. Modes: `full`, `lazy`, `grouped`. Use `--model` to pin the CLI model and `--timeout` for the per-task deadline in milliseconds. The default deadline is 90 seconds. Every model invocation has its own registry, data directory and MCP configuration. Credentials are read by the existing CLI login and are never copied by the driver. Codex and Cursor use a read-only filesystem with disposable runtime writes. Cursor loads a workspace MCP file while its real global MCP file is hidden.

- `--resume` skips completed summaries and preserves cancelled artifacts before rerunning an incomplete task.
- `--tasks r4-001,r4-002` selects development cases; `--limit 2` bounds the selection.
- `--start` measures a trivial first turn with no requested tool call.
- `--notification` adds a capability after the first Slack call. A bounded response barrier keeps the change inside the active turn.
- `--delay 8000 --single-server` delays the first downstream catalog without prewarming. The single server is named `catalog`, so its exposed tool names have that extra prefix.
- Cold Codex runs use a loopback bridge that launches the gateway only when the client connects. This preserves a cold start under filesystem containment.
- `--adapter-id codex` tests the installed adapter identity. The historical benchmark identity is `client:codex`.
- `--recovery` asks the model to inspect status and retry scoped discovery twice. `--bootstrap` explicitly directs code mode to inspect gateway helpers first. Keep these diagnostic runs separate from the ordinary matrix.
- `--debug` saves detailed CLI diagnostics. Codex retains its transcript only in the disposable runtime overlay.
- `--fixture-approvals` disables Codex's inner approval sandbox for this public fixture. Its outer filesystem containment remains active. Do not use it for real servers.

Evidence includes full JSON-RPC exchanges, downstream calls, CLI output, config metadata before and after, and summaries. Keep all evidence outside the repository. Config metadata from concurrent clients can change independently; use the recorded paths and containment evidence to distinguish those changes.

Score requires a completed turn, correct ordered downstream calls, schema validity and the requested argument values. It accepts the development split's file-read alternatives and refund shapes, and permits surrounding text when posting a returned URL. An extra unrelated call fails the task. Invalid public schemas fail validation without being rewritten. No-match cases require explicit abstention.

```sh
node --test scripts/live-client-bench.test.mjs
node scripts/live-client-bench-report.mjs \
  --catalog /path/to/catalog.json --dev /path/to/dev.json \
  --runs /path/to/evidence/matrix --out /path/to/evidence/analysis.json
```

The report sums and compares CLI-reported usage. Codex input includes cached tokens. The other CLI input fields exclude separate cache reads, and Claude also reports cache writes. Missing usage stays unknown. Latency includes process startup and model time. Do not treat concurrent samples as controlled performance or billing estimates. Discovery defaults are not selected by this driver.
