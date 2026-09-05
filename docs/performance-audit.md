# Performance and agent verification audit

Measured locally on Linux on 2026-09-04. The PR branch was verified against
`main` at `2149d34`.

## Findings and results

| Path                                        |       Before |     After | What changed                                                            |
| ------------------------------------------- | -----------: | --------: | ----------------------------------------------------------------------- |
| Static startup JavaScript                   |    649.25 kB | 551.65 kB | Vendored logos load as local image assets instead of eager SVG strings  |
| Startup JavaScript, gzip                    |    210.86 kB | 170.40 kB | About 19% fewer compressed startup bytes                                |
| Unchanged audit stats, release median       |      8.00 ms |   0.42 ms | Compare exact log bytes and reuse the aggregate                         |
| Rewrite plus audit stats, release median    |      7.75 ms |   7.01 ms | Aggregate rows as they are parsed instead of keeping every JSON tree    |
| Audit fixture peak process RSS              |   20,272 KiB | 9,744 KiB | Separate uncached/streamed processes, including fixture setup           |
| Activity refreshes during 60 seconds hidden | 20 scheduled |         0 | Native visibility and window focus gate the refresh timer               |
| Frontend tests, two workers                 |      24.95 s |   20.78 s | Pure tests use Node; only UI tests initialize jsdom and Testing Library |

Startup bytes were remeasured against the PR base using the original components
and the same production build settings. The test timings compare the initial
audit checkout with the optimized configuration; the final PR branch ran 657
tests in 22.79 seconds. Test timings varied while the machine was in use.

The audit benchmark uses 10,000 synthetic rows occupying 3,466,600 bytes. The
uncached reference is the previous `read_all` plus aggregation path. Its final
comparison runs both paths in the same optimized executable. Each timed workload
gets five warmups and 50 samples, or 20 samples when rewriting the log. Final
unchanged-stats p95 was 10.87 ms uncached and 0.44 ms cached. Rewrite-plus-stats p95
was 8.97 ms uncached and 7.45 ms streamed. The filesystem cache was warm; these are
not cold-disk or production API measurements. This machine was also in active use.

A preliminary debug build measured unchanged stats at 49.59 ms before and 0.65 ms
after caching. That is diagnostic evidence, not a shipped-build latency claim.
The first cache implementation made changed snapshots somewhat slower. Streaming
aggregation replaced that implementation and removed the measured regression.

The cache retains at most one normal-sized log snapshot, up to 4 MiB, plus its
summary. It still reads the file on each request. Exact byte comparison catches
same-size rewrites without trusting modification timestamps. Missing files return
empty stats, read failures still fail, and oversized logs aggregate without being
cached. No policy or permission decisions are cached.

Logo masks preserve inherited monochrome colors. The mixed-color Amazon mark
remains inline. Screenshots of all client logos in both themes were compared with
the original components: 61 of 1,116,000 pixels differed by more than 8 in a color
channel, consistent with rasterization differences. Local SVG files keep the
existing CSP. Native WebKit rendering still needs platform verification.

The startup byte reduction is measured. A specific cold-start time improvement is
not claimed. Browser fixtures run development React and mocked IPC, so their load
time cannot stand in for installed desktop startup latency.

## Verification and agent DX

- `npm run doctor` identifies missing dependencies and browser/native tooling.
- `npm run verify` runs the full frontend and headless flow, stopping on failure
  with per-stage logs and a machine-readable summary under `.verify/`.
- `verify:frontend` and `verify:headless` support scoped work. `typecheck` is
  available separately; `npm test -- --project logic` skips browser setup.
- `smoke:browser` starts an isolated loopback server on a free port. It checks
  seeded Servers and Activity screens, validates logo asset loads, blocks
  external requests, and captures screenshots plus a Playwright trace. Failures
  retain HTML, screenshots, and errors. It needs no credentials or real registry.
- `dev:fixture -- --port 1430` supports interactive inspection in a worktree.
- `bench:bundle` measures every statically imported startup chunk, so merely
  splitting the entry chunk does not evade the byte budget.
- `bench:audit` provides reproducible timing and separate-process Linux RSS cases.
- Gateway smoke and latency tools respect custom Cargo target directories and
  binary overrides. The latency harness now isolates data and environment,
  rejects RPC errors, times out hung requests, and cleans up failed child runs.
  Both immediate process exit and a nonresponding mock were exercised; the latter
  failed in about ten seconds and left no benchmark directory behind.
- CI now checks the startup bundle budget and offline browser smoke, retaining
  browser diagnostics on failure. Remote CI results are tracked on the PR.
- Shared UI test cleanup now drains Radix's delayed focus restoration before
  jsdom teardown. A focused regression fails with the original setup and passes
  with the fix, covering the unhandled Event-realm error found during this audit.
- The verification runner terminates its owned process tree on cancellation or
  timeout. A nested test process was confirmed stopped after cancelling a run.
- `AGENTS.md` documents setup, relevant code paths, scoped checks, worktrees,
  fixture isolation, and the limits of the available evidence.

The complete `npm run verify` flow passed on the PR branch: formatting, lint,
typechecked production build, 657 frontend tests, startup bundle budget, browser
smoke, 1,715 headless Rust tests (one pre-existing ignored test), gateway build,
and all ten headless smoke checks. Lint still reports pre-existing warnings.
Headless gateway overhead against the mock measured about 0.44 ms median in a
debug build; no before/after improvement is claimed for that path.

## Experiments not retained and remaining work

No final optimization was removed for lack of benefit. The initial cache that
materialized every row was replaced by the faster streamed version. Whole-file
recent-entry reads measured about 0.4 ms for 200 rows from the 3.5 MB fixture, so a
reverse-file reader was not implemented. Its complexity was not justified by that
measurement. Existing catalog indexing and route splitting already avoid several
obvious sources of repeated work.

The startup bundle still includes closed dialog implementations and substantial
React/Radix code. Deferring dialogs is a remaining opportunity, but needs checks
for focus restoration, keyboard opening, form state, and async failures. Long
lists also merit profiling with realistic registry/catalog sizes before adding
virtualization. Other sidebar polling continues independently of Activity.

Further verification would benefit from authenticated staging MCP accounts,
representative large registries and log distributions, installed Windows/macOS
builds, native GTK/WebKit traces, and cold-cache startup profiles. These are
needed to measure real downstream API latency, keychain prompts, packaging, and
native window behavior. The offline fixture and headless checks do not replace
those platform checks.
