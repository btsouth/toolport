---
name: verify
description: Boot the changed Toolport, drive it and look at the result before committing or calling a Toolport change done. Use while iterating, not only at the end. Pick the cheapest tier that shows the change - React/CSS/locales in src/ use the browser fixture (tier 1); the GTK shell in src-tauri/src/linux_native or anything it shows uses the GTK desktop in omabox (tier 2); gateway, router, registry, audit or other headless Rust uses the headless verify and gateway smoke (tier 3). Shared Rust that both shells show needs tier 2 plus tier 3.
---

# Verify a Toolport change

Smoke test harness for agents: see the change working with your own eyes, fast,
before you commit. Heavy work (installs, Cargo, test suites) runs on devbox; the
workstation only looks (headless browser, omabox). `scripts/verify-devbox.sh`
does the syncing, building and launching; run it from the worktree root.

Your uncommitted edits are what gets tested: every command first copies the
worktree (tracked and untracked files, not ignored ones) to
`devbox-bts:/workspace/worktrees/toolport-verify-<worktree name>`.

## Pick the tier

| Changed                                                                                               | Tier                                       |
| ----------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| `src/**` (React, CSS, locales, `src/lib/api.ts` wrappers)                                             | 1 browser fixture                          |
| `src-tauri/src/linux_native/**`, or shared Rust whose result the GTK app shows                        | 2 GTK desktop                              |
| `src-tauri/src/bin/toolport-gateway.rs`, `router.rs`, `registry*.rs`, `audit.rs`, other headless Rust | 3 headless                                 |
| Tauri-only Rust (`src-tauri/src/desktop.rs`, `main.rs`)                                               | 3, and say the Tauri window was not opened |
| Docs, CI, packaging, release scripts, tests only                                                      | none: CI covers these                      |

## Tier 1: browser fixture (about 15 s to first screenshot)

```bash
URL=$(scripts/verify-devbox.sh fixture)        # sync, npm ci if needed, vite on devbox, tunnel here
S=verify-$(basename "$PWD")
READY="async page => { await page.getByRole('button', { name: 'Servers', exact: true }).waitFor(); }"
playwright-cli -s=$S open "$URL"               # headless, clean profile
playwright-cli -s=$S resize 1240 900
playwright-cli -s=$S run-code "$READY"         # first load waits for vite's dependency pass
playwright-cli -s=$S find "<text on the changed screen>"   # refs for click/fill; or: snapshot
playwright-cli -s=$S click <ref>               # walk to the changed screen
playwright-cli -s=$S screenshot --filename "$PWD/.verify/shots/<screen>-dark.png"
playwright-cli -s=$S localstorage-set toolport-theme light
playwright-cli -s=$S reload && playwright-cli -s=$S run-code "$READY"
#   ...find and click your way back (a reload starts on Servers and renumbers refs)...
playwright-cli -s=$S screenshot --filename "$PWD/.verify/shots/<screen>-light.png"
```

Read each PNG to look at it. The fixture starts dark; the theme is the app's own
setting (`toolport-theme` in localStorage: `light`, `dark` or `system`), so
`set-color-scheme` alone changes nothing. Add the query flag to `$URL` before
`open` when you need a seeded state, for example `${URL}?approvals`, `?setup`,
`?connected-import`, `?long-names`, `?personal-sync=<scenario>` (grep
`URLSearchParams` in `src/test/browser-fixture.tsx` for the rest). Fixture data
is in memory; an unknown IPC command throws, so add the response to the
fixture rather than working around it.

After an edit: `scripts/verify-devbox.sh sync`, then `playwright-cli -s=$S
reload` (vite on devbox picks the files up). Done: `playwright-cli -s=$S close`
and `scripts/verify-devbox.sh fixture-stop`.

Wider regression check of all seeded screens: `npm run smoke:browser` on
devbox (see tier 3).

## Tier 2: GTK desktop in omabox (first build about 3 min, then about 15 s)

Load the omabox skill first if you have not this session; every GUI launch,
click and screenshot happens in a box, never on the real desktop.

```bash
scripts/verify-devbox.sh gtk-build             # sync, cargo build on devbox under heavy-run, strip, fetch
scripts/verify-devbox.sh gtk-launch --fresh    # box up (--net isolated), seeded data, app drawn; lists windows
omabox shot --window Toolport --fit 1456       # a PNG path: Read it
```

- What is isolated: the box has its own HOME (`/home/sbx`), session bus and
  keyring, and `--net isolated` keeps it off the host's loopback services. The
  app runs with `TOOLPORT_DATA_DIR=/home/sbx/toolport-data`, seeded with one
  stdio server, "Mock", running the fetched `mock-mcp-server` (Servers shows it
  "Ready", 9 tools). Never point it at the real data dir or keyring.
  Clients lists tools found on your PATH (a box keeps it, e.g. OpenCode);
  connecting one writes only to the box HOME.
- After an edit: `gtk-build`, then `gtk-launch` (stops the old process, keeps
  the box's data; `--fresh` reseeds; `--empty` starts as a new user, so the
  "Toolport setup" window opens over the main one: select windows by address
  from `omabox windows` then). The box HOME is writable from here at `omabox path --home`
  for seeding other states (edit `toolport-data/registry.json`, then relaunch).
- Navigate by GTK action rather than clicking the rail when navigation is not
  under test:
  `omabox run -- busctl --user call com.tsout.Toolport /com/tsout/Toolport org.gtk.Actions Activate 'sava{sv}' show-servers 0 0`
  (also `show-clients`, `show-skills`, `show-activity`, `show-catalog`,
  `show-teams`, `show-settings`). Then `omabox wait still`.
- Drive the changed flow by text: `omabox tree --window Toolport --find TEXT`
  prints each match as `role "name" @X,Y WxH` in window pixels; click its
  centre with `omabox click --window Toolport --wait X+W/2 Y+H/2`, type with
  `omabox keys --window Toolport --wait ...`. Coordinates read off a `--fit`
  shot need `--in THAT.png`. Look, act once, look again.
- Light and dark: the GTK shell follows the box's Omarchy theme, which starts
  as your desktop's, so set each one explicitly.
  `omabox run -- omarchy-theme-set flexoki-light` and
  `omabox run -- omarchy-theme-set tokyo-night` switch it live; shoot each.
- Save evidence with `omabox shot --window Toolport -o "$PWD/.verify/shots/gtk-<screen>-<theme>.png"`.
- App log: `omabox log run` (or the log path `gtk-launch` printed). Gdk
  `vkCreateSwapchainKHR` warnings there are the box's renderer and harmless
  when the shot looks right. Done: `omabox down`.

The binary is built on devbox (Ubuntu, glibc 2.43, GTK 4.22) and runs here
(Arch, newer glibc and GTK); that direction works. The same steps run from
RogAlly (`ssh rogally`, a checkout in `~/Projects/toolport`), which has omabox
and reaches devbox, so GUI checks can run there instead of the workstation.

## Tier 3: gateway and backend

While iterating, run only the Rust tests for the area you touched:

```bash
scripts/verify-devbox.sh sync
ssh devbox-bts 'cd /workspace/worktrees/toolport-verify-<worktree name> && export PATH=$HOME/.cargo/bin:$PATH &&
  heavy-run -- cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-support,search-static --lib <filter>'
```

CI runs the full suite on every push, so do not wait on it locally for an
ordinary change. Run the full headless check (about 14 min, almost all of it
the Rust suite) only when the change spans several backend areas or CI failed
in a way you need to reproduce:

```bash
scripts/verify-devbox.sh headless
```

It runs `npm run verify:headless` on devbox (headless Rust tests, gateway
build, `smoke:headless`: the HTTP gateway against a mock server on temporary
data) and copies `.verify/run-*/` back here, printing the `summary.json` path.
Do not edit `scripts/verify-devbox.sh` while it runs. The browser regression
smoke is the same pattern with `heavy-run -- npm run smoke:browser` (about 1
min; screenshots in the devbox directory's `.verify/browser-*`).

## Parity report: GTK vs React (about 1 min after the first build)

When a change touches a screen the parity tool covers (Servers or Activity) in
either shell, run it before calling the change done:

```bash
scripts/verify-devbox.sh parity              # or: parity --only servers-empty
```

It builds the GTK test binary on devbox, renders each scenario in
`parity/scenarios/` with the real GTK page under xvfb and in the React browser
fixture from the same data, and prints every difference (screen, region,
element, both values) to `.verify/parity/run-*/report.txt` and `report.json`.
It is report-only: differences never fail it, only a harness problem does (a
self-check, an empty menu, a scenario the fixture cannot answer). Fix a new
difference in the shell that drifted, or, when the difference is intended, add
an entry with a reason to `parity/known-differences.json`; the report lists it
separately and notes entries that no longer match anything.

## Evidence to report

- Tier 1 and 2: the screenshot paths under `.verify/shots/` (both themes for a
  visual change) and one line on what each shows.
- Tier 3: the `.verify/run-*/summary.json` path and pass/fail per step.
- What was not exercised (Tauri window, real keychain, other platforms).

## When it keeps failing

If a check still fails after three rounds of fixes, stop. Report the command,
the last output or screenshot path and what you tried, instead of looping. Do
not weaken a test, timeout or fixture to get a pass.

## Troubleshooting

- `gtk-build` failed: the script prints the error and warning lines and the
  path of the full log on devbox (`<devbox dir>.gtk-build.log`).
- `gtk-launch` exit 124: the window did not settle in 20 s. `omabox windows`,
  `omabox log run`; a second `gtk-launch` replaces the old process.
- Fixture not answering: the script prints the devbox vite log; a port clash
  is retried by running `fixture` again (it picks a new random port).
