# Working on Toolport

See [DESIGN.md](DESIGN.md) for component ownership, runtime boundaries, and
data flow, and [CONTRIBUTING.md](CONTRIBUTING.md) for development setup.

## Setup and verification

- Run `npm run doctor` for tools, dependencies, browser, and artifact paths.
- Install JavaScript dependencies with `npm ci`. Use stable Rust. Headless Linux
  builds need pkg-config plus D-Bus and OpenSSL development packages; desktop
  builds also need GTK/WebKit headers.
- Run `npm run verify` for formatting, lint, locale validation, frontend
  build/tests, startup bundle budget, browser and localization fixture smoke,
  headless Rust tests, and gateway smoke.
- Use `npm run verify:frontend` or `npm run verify:headless` for scoped changes.
  The headless path skips GTK/WebKit. Desktop-specific changes also need
  default-feature Rust checks (`npm run test:rust`) and platform build validation;
  headless checks do not cover desktop modules.
- Verification defaults to `.verify/`. Set `TOOLPORT_VERIFY_DIR` to an external
  artifact directory when logs/screenshots must stay outside the checkout; set
  `TOOLPORT_BUILD_DIR` for external Vite output (also used by `bench:bundle`).
  Each verification run writes step logs and a JSON summary, stops at the first
  failure, and prints its log path. Do not call a partial run a full pass.
- The gateway is not rebuilt by `tauri dev`. After gateway changes, run
  `npm run build:gateway` and restart connected clients.
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
when available. Browser artifacts use `TOOLPORT_VERIFY_DIR` or `.verify/`.

Worktrees have independent Node dependencies and Cargo artifacts by default. A
shared `CARGO_TARGET_DIR` reuses Rust builds but Cargo serializes access to it.
The gateway smoke and latency tools honor `CARGO_TARGET_DIR`,
`TOOLPORT_GATEWAY_BIN`, and `TOOLPORT_MOCK_BIN`. Both allocate temporary data;
the HTTP smoke also allocates loopback ports. Latency RPCs have a ten-second
deadline and clean up child processes and fixture data on failure.
Use `registry::DataDirOverride` and the existing data-dir test lock in Rust tests
that touch logs; never point tests at the user's installed data directory.

## Localization changes

Follow [CONTRIBUTING.md#localization](CONTRIBUTING.md#localization) for message
ownership, interpolation, and translation boundaries. Run `npm run check:locales`
and `npm run smoke:localization` after changing UI text or language behavior;
both are included in `verify:frontend`. Verify that switching languages preserves
unsaved input and does not repeat backend reads. Keep protocol identifiers,
keyboard comparisons, and React keys independent of translated display text.

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
  baseline failures separately. Commit, push task branches, and open PRs within
  authorized work without repeated approval. Merges require the relevant CI checks;
  releases require authorization and validation of their candidate artifacts.

## Public communication and external services

Keep public posts short and human. Never use em dashes. Inspect the exact title
and description before creating or updating a pull request; exclude terminal
transcripts, scratch notes, and full logs. Use a draft until both code and copy
are ready. After writing a PR, fetch its live title, description, diff, branch,
and checks, and correct unexpected content. Access Linear through Toolport.
