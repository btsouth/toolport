# Token budget fixtures

Run `npm run bench:tokens` on devbox. This uses actual gateway dispatch and the
bundled `tiktoken-rs` `o200k_base` vocabulary, with no API calls or real client data.
Outputs, including exact serialized payloads, go into `.verify/token-budget.json`.
The ordinary Rust `token_budget_regression` test checks fixed OpenAI token budgets.

`shape.json` contains only anonymous description/schema byte-size pairs from a
read-only 1,639-tool cache snapshot on 2026-10-09. The original server counts were
587, 357, 333, 224, 115, 15, 8. The synthetic 1,707-tool catalog scales the other
large groups to 618, 376, 333, 236, 121, 15, 8, preserving the stated 333-tool server.
No private names, descriptions, schemas, arguments, results or settings appear here.
The generator spreads size samples deterministically and builds repeated property
objects from public text. It approximates byte shape, not private token frequency
or semantic search accuracy. The existing 411-tool search eval remains the accuracy gate.

`public-tools.json` includes eight complete archived reference Slack input
definitions, two filesystem input-schema reconstructions from Zod source, and
one GitHub issue_read input subset without output schema or pagination. Slack
read-only annotations are fixture annotations, not upstream fields. It is a
representative public subset, not a current complete hosted catalog. Sources:

- [Slack reference](https://github.com/modelcontextprotocol/servers-archived/blob/9be4674d1ddf8c469e6461a27a337eeb65f76c2e/src/slack/index.ts)
- [Filesystem reference](https://github.com/modelcontextprotocol/servers/blob/5abed86c5317b833dd59907492d56c65981642aa/src/filesystem/index.ts)
- [GitHub issue tools](https://github.com/github/github-mcp-server/blob/eb47a99ddb866ca2b8a162920e6bda9521f33ebb/pkg/github/issues.go)

Claude counts are explicitly `ceil(Unicode characters / 4)` approximations.
They are uncalibrated, especially for JSON and newer Claude tokenizers. No
Anthropic counting key was configured for this audit. Native-search name/header
and loaded-definition measurements are comparable components, not measured
Claude Code or Codex sessions. Vendor framing, native search instructions,
server instruction policies, cached billing and tool-selection behavior remain
outside the benchmark. Full MCP wire size must not be called native model cost.

Exact-name search is Toolport's describe operation. A successful top match already
has its schema, so the separate describe round is optional. Full passthrough calls
can be direct; native deferral adds its own search round before direct dispatch.
The large-result whole-fetch measurement bypasses normal page size only to total
the stored body. Actual clients use bounded pages; projection measures one row.
