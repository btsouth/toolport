# Working on Toolport

Toolport has a React/Vite frontend, a Tauri desktop shell, an optional GTK shell,
and a Rust headless gateway. `src-tauri/src/bin/toolport-gateway.rs` owns the gateway
protocol and catalog search; `router.rs` routes calls; `audit.rs` records and
aggregates Activity history. Frontend IPC wrappers are in `src/lib/api.ts`.

## Setup and verification

- Run `npm run doctor` for tools, dependencies, browser, and artifact paths.
- Install JavaScript dependencies with `npm ci`. Use stable Rust. Headless Linux
  builds need pkg-config plus D-Bus and OpenSSL development packages; desktop
  builds also need GTK/WebKit headers.
- Run `npm run verify` for formatting, lint, frontend build/tests, startup bundle
  budget, browser fixture smoke, headless Rust tests, and gateway smoke.
- Use `npm run verify:frontend` or `npm run verify:headless` for scoped changes.
  The headless path skips GTK/WebKit. Desktop-specific changes still need the
  desktop Rust checks from CI (`npm run test:rust`).
- Every verification run writes step logs and a JSON summary under `.verify/`.
  A failed command stops the run and prints its log path. Do not call a partial
  run a full pass.
- `npm test -- --project logic` runs pure tests without jsdom. Component tests
  use `--project ui`; individual file filters also work. Local tests default to
  two workers; override `--maxWorkers` when appropriate.

## Browser fixtures and worktrees

`npm run smoke:browser` starts its own server on a free loopback port and uses an
isolated headless browser. It checks the seeded Servers and Activity screens and
captures both logo themes. Unknown IPC commands fail the fixture; add intentional
responses in `src/test/browser-fixture.tsx` as coverage grows. Requests to external
origins are blocked. These tests do not exercise the native backend or keychain.

For manual inspection, run `npm run dev:fixture -- --port 1430`. Choose a different
port in each worktree. This fixture has in-memory data and cannot change your real
registry. It is a separate development entry, absent from production builds.
Install Chromium with `npx playwright install chromium`, or set
`TOOLPORT_BROWSER_BIN` to a compatible browser. Linux also uses `/usr/bin/chromium`
when available. Failed browser checks and screenshots go under `.verify/`.

Worktrees have independent Node dependencies and Cargo artifacts by default. A
shared `CARGO_TARGET_DIR` reuses Rust builds but Cargo serializes access to it.
The gateway smoke and latency tools honor `CARGO_TARGET_DIR`,
`TOOLPORT_GATEWAY_BIN`, and `TOOLPORT_MOCK_BIN`. Both allocate temporary data;
the HTTP smoke also allocates loopback ports. Latency RPCs have a ten-second
deadline and clean up child processes and fixture data on failure.
Use `registry::DataDirOverride` and the existing data-dir test lock in Rust tests
that touch logs; never point tests at the user's installed data directory.

## Performance evidence

- `npm run build && npm run bench:bundle` measures the full static startup JS
  dependency graph and checks its raw and gzip budgets.
- `npm run bench:audit` measures a synthetic 10,000-row audit log. Add `-- --release`
  for optimized measurements. Run the release example with `--memory-uncached`
  and `--memory-streamed` separately for Linux peak RSS comparisons.
  The benchmark compares the original uncached
  aggregation with caching, including equal-length log rewrites.
- Keep before/after data and state the build profile and fixture size. Browser
  fixtures verify UI behavior; they are not native desktop performance evidence.
- Inspect `git status` before editing. Preserve existing work and report unrelated
  baseline failures separately. Do not commit, push, or publish without approval.

## Public communication and external services

Keep public posts short and human. Never use em dashes. Inspect the exact title
and description before creating or updating a pull request; exclude terminal
transcripts, scratch notes, and full logs. Use a draft until both code and copy
are ready. After writing a PR, fetch its live title, description, diff, branch,
and checks, and correct unexpected content. Access Linear through Toolport.
